//! Poll backoff and retry policy.
//!
//! Transport posture mirrors upstream's `httpClientOption` defaults
//! (`retryMax=4`, `retryWaitMax=30s`, 5min long-poll timeout) over
//! `retryablehttp.DefaultRetryPolicy`: retry connection failures, 429, and
//! 5xx except 501; the admin-connection handshake additionally retries
//! 401/403 (`getActionsServiceAdminConnectionRequest`). Rate-limit waits
//! reuse [`GitHubRateLimitStatus`](crate::protocol::GitHubRateLimitStatus)
//! so `Retry-After`/`x-ratelimit-reset` are honored before the backoff.

use std::time::Duration;

use reqwest::header::HeaderMap;
use reqwest::StatusCode;

use crate::protocol::GitHubRateLimitStatus;

/// Upstream default: at most 4 retries after the first attempt.
pub const DEFAULT_RETRY_MAX: u32 = 4;
/// Upstream default: backoff waits cap at 30s.
pub const DEFAULT_RETRY_WAIT_MAX: Duration = Duration::from_secs(30);
/// `retryablehttp` default floor (upstream never overrides it).
pub const DEFAULT_RETRY_WAIT_MIN: Duration = Duration::from_secs(1);
/// Upstream default HTTP timeout = long-poll window (5min).
pub const DEFAULT_LONG_POLL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Retry policy mirroring `httpClientOption` retry fields.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub wait_min: Duration,
    pub wait_max: Duration,
    /// Per-request timeout (long polls need the 5min window).
    pub timeout: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_RETRY_MAX,
            wait_min: DEFAULT_RETRY_WAIT_MIN,
            wait_max: DEFAULT_RETRY_WAIT_MAX,
            timeout: DEFAULT_LONG_POLL_TIMEOUT,
        }
    }
}

impl RetryPolicy {
    /// True when another attempt is allowed after `attempts` failures
    /// (`max_retries=4` → 5 total attempts, mirroring `retryMax`).
    #[must_use]
    pub fn may_retry(&self, attempts: u32) -> bool {
        attempts < self.max_retries
    }

    /// Mirror of `retryablehttp.DefaultRetryPolicy` status classification,
    /// plus the admin-handshake 401/403 extension.
    #[must_use]
    pub fn retryable_status(status: StatusCode, admin_handshake: bool) -> bool {
        if admin_handshake
            && (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN)
        {
            return true;
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return true;
        }
        status.is_server_error() && status != StatusCode::NOT_IMPLEMENTED
    }

    /// Exponential backoff `wait_min * 2^attempt`, capped at `wait_max`.
    /// Upstream adds jitter in `DefaultBackoff`; the cap and growth match,
    /// jitter is intentionally omitted (hides thundering-herd math, not
    /// contract).
    #[must_use]
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let shift = attempt.min(10);
        let scaled = self.wait_min.saturating_mul(1 << shift);
        scaled.min(self.wait_max)
    }

    /// Rate-limit wait from response headers, reusing the shared
    /// [`GitHubRateLimitStatus`](crate::protocol::GitHubRateLimitStatus)
    /// exhaustion rule (429, or 403 with `remaining=0`/`Retry-After`).
    #[must_use]
    pub fn rate_limit_delay(
        status: StatusCode,
        headers: &HeaderMap,
        now_epoch: u64,
        default: Duration,
    ) -> Duration {
        let rate = GitHubRateLimitStatus {
            retry_after_seconds: header_u64(headers, reqwest::header::RETRY_AFTER.as_str()),
            rate_limit_reset_epoch: header_u64(headers, "x-ratelimit-reset"),
            remaining: header_u64(headers, "x-ratelimit-remaining"),
        };
        if !rate.is_limited(status.as_u16()) {
            return default;
        }
        rate.reset_epoch_or_retry_after(now_epoch)
            .map_or(default, |epoch| {
                Duration::from_secs(epoch.saturating_sub(now_epoch)).max(default)
            })
    }
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse().ok())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_upstream_options() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 4);
        assert_eq!(policy.wait_max, Duration::from_secs(30));
        assert_eq!(policy.timeout, Duration::from_secs(300));
        assert!(policy.may_retry(0));
        assert!(policy.may_retry(3));
        assert!(!policy.may_retry(4));
    }

    #[test]
    fn retryable_statuses_match_default_policy() {
        assert!(RetryPolicy::retryable_status(
            StatusCode::TOO_MANY_REQUESTS,
            false
        ));
        assert!(RetryPolicy::retryable_status(
            StatusCode::SERVICE_UNAVAILABLE,
            false
        ));
        assert!(RetryPolicy::retryable_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            false
        ));
        assert!(!RetryPolicy::retryable_status(
            StatusCode::NOT_IMPLEMENTED,
            false
        ));
        assert!(!RetryPolicy::retryable_status(
            StatusCode::BAD_REQUEST,
            false
        ));
        assert!(!RetryPolicy::retryable_status(
            StatusCode::UNAUTHORIZED,
            false
        ));
        assert!(RetryPolicy::retryable_status(
            StatusCode::UNAUTHORIZED,
            true
        ));
        assert!(RetryPolicy::retryable_status(StatusCode::FORBIDDEN, true));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay_for_attempt(0), Duration::from_secs(1));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_secs(2));
        assert_eq!(policy.delay_for_attempt(2), Duration::from_secs(4));
        assert_eq!(policy.delay_for_attempt(99), Duration::from_secs(30));
    }

    #[test]
    fn retry_after_extends_backoff() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "45".parse().unwrap());
        let delay = RetryPolicy::rate_limit_delay(
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            1_000,
            Duration::from_secs(2),
        );
        assert_eq!(delay, Duration::from_secs(45));
    }

    #[test]
    fn permission_403_keeps_default_delay() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-remaining", "10".parse().unwrap());
        let delay = RetryPolicy::rate_limit_delay(
            StatusCode::FORBIDDEN,
            &headers,
            1_000,
            Duration::from_secs(2),
        );
        assert_eq!(delay, Duration::from_secs(2));
    }
}

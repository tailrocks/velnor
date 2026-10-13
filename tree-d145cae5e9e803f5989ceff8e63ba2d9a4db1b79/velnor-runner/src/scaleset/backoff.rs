//! Poll backoff and retry policy.
//!
//! Transport posture mirrors upstream's `httpClientOption` defaults
//! (`retryMax=4`, `retryWaitMax=30s`, 5min long-poll timeout) over
//! `retryablehttp.DefaultRetryPolicy`: retry connection failures, 429, and
//! 5xx except 501; the admin-connection handshake additionally retries
//! 401/403 (`getActionsServiceAdminConnectionRequest`). Rate-limit waits
//! use `Retry-After` for 429/503 exactly as `retryablehttp.DefaultBackoff`;
//! the Scale Set client does not apply GitHub REST quota-reset headers.

use std::time::Duration;
use std::time::SystemTime;

use reqwest::header::HeaderMap;
use reqwest::StatusCode;

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
        status.as_u16() >= 500 && status != StatusCode::NOT_IMPLEMENTED
    }

    /// Match retryablehttp's non-retryable transport cases: request build and
    /// redirect failures, deadline/timeout, and TLS certificate validation.
    #[must_use]
    pub fn retryable_transport_error(error: &reqwest::Error) -> bool {
        let rendered = format!("{error:#}").to_ascii_lowercase();
        retryable_transport_flags(
            error.is_builder(),
            error.is_redirect(),
            error.is_timeout(),
            is_certificate_verification_failure(&rendered),
        )
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

    /// Delay before the next poll after `outcome`, given
    /// `consecutive_errors` failed polls in a row (reset on any `Ok`).
    ///
    /// Messages drain immediately (the server long-poll paces us); nils
    /// wait a flat [`IdlePolicy::nil_delay`] — upstream loops at once, but
    /// an instant-202 server (mock, outage page) would hot-spin the loop
    /// without this floor; errors back off exponentially per
    /// [`RetryPolicy::delay_for_attempt`].
    #[must_use]
    pub fn poll_delay(
        &self,
        outcome: PollOutcomeClass,
        consecutive_errors: u32,
        idle: &IdlePolicy,
    ) -> Duration {
        match outcome {
            PollOutcomeClass::Message => Duration::ZERO,
            PollOutcomeClass::Nil => idle.nil_delay,
            PollOutcomeClass::Error => self.delay_for_attempt(consecutive_errors.saturating_sub(1)),
        }
    }

    /// Parse the upstream retryablehttp Retry-After override. The server hint
    /// applies only to 429/503 and bypasses the exponential wait cap.
    #[must_use]
    pub fn retry_after_delay(
        status: StatusCode,
        headers: &HeaderMap,
        now: SystemTime,
    ) -> Option<Duration> {
        if status != StatusCode::TOO_MANY_REQUESTS && status != StatusCode::SERVICE_UNAVAILABLE {
            return None;
        }
        let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
        if let Ok(seconds) = value.parse::<i64>() {
            return u64::try_from(seconds).ok().map(Duration::from_secs);
        }
        let retry_at = parse_http_date(value)?;
        Some(retry_at.duration_since(now).unwrap_or(Duration::ZERO))
    }
}

fn retryable_transport_flags(
    builder: bool,
    redirect: bool,
    timeout: bool,
    certificate_verification: bool,
) -> bool {
    !(builder || redirect || timeout || certificate_verification)
}

fn is_certificate_verification_failure(error: &str) -> bool {
    const MARKERS: &[&str] = &[
        "invalid peer certificate",
        "certificate is not trusted",
        "certificate verify failed",
        "certificate verification failed",
        "certificate has expired",
        "certificate expired",
        "unknownissuer",
        "notvalidforname",
        "notvalidyet",
        "unknown ca",
        "self-signed certificate",
    ];
    MARKERS.iter().any(|marker| error.contains(marker))
}

/// Poll-result class driving [`RetryPolicy::poll_delay`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollOutcomeClass {
    Message,
    Nil,
    Error,
}

/// Idle-poll pacing. The flat nil floor only binds when polls return
/// instantly; ordinary 5-minute server long-polls never see it.
#[derive(Debug, Clone)]
pub struct IdlePolicy {
    pub nil_delay: Duration,
}

impl Default for IdlePolicy {
    fn default() -> Self {
        Self {
            nil_delay: Duration::from_secs(1),
        }
    }
}

fn parse_http_date(value: &str) -> Option<SystemTime> {
    let format = time::format_description::parse_borrowed::<1>(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT",
    )
    .ok()?;
    let date = time::PrimitiveDateTime::parse(value, &format)
        .ok()?
        .assume_utc();
    let timestamp = date.unix_timestamp_nanos();
    let seconds = u64::try_from(timestamp.div_euclid(1_000_000_000)).ok()?;
    let nanoseconds = u32::try_from(timestamp.rem_euclid(1_000_000_000)).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(seconds, nanoseconds))
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
        assert!(RetryPolicy::retryable_status(
            StatusCode::from_u16(599).unwrap(),
            false
        ));
        assert!(!RetryPolicy::retryable_status(
            StatusCode::from_u16(499).unwrap(),
            false
        ));
        assert!(RetryPolicy::retryable_status(
            StatusCode::from_u16(600).unwrap(),
            false
        ));
        assert!(RetryPolicy::retryable_status(
            StatusCode::from_u16(999).unwrap(),
            false
        ));
    }

    #[test]
    fn transport_retryability_matches_non_retryable_upstream_cases() {
        assert!(retryable_transport_flags(false, false, false, false));
        assert!(!retryable_transport_flags(true, false, false, false));
        assert!(!retryable_transport_flags(false, true, false, false));
        assert!(!retryable_transport_flags(false, false, true, false));
        assert!(!retryable_transport_flags(false, false, false, true));
        assert!(is_certificate_verification_failure(
            "error sending request: invalid peer certificate: UnknownIssuer"
        ));
        assert!(is_certificate_verification_failure(
            "certificate is not trusted"
        ));
        assert!(!is_certificate_verification_failure("connection refused"));
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
    fn retry_after_seconds_are_exact_and_ignore_backoff_cap() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "90".parse().unwrap());
        let delay = RetryPolicy::retry_after_delay(
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(delay, Some(Duration::from_secs(90)));

        headers.insert("retry-after", "1".parse().unwrap());
        let short = RetryPolicy::retry_after_delay(
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(short, Some(Duration::from_secs(1)));
    }

    #[test]
    fn retry_after_http_date_is_supported_for_429_and_503() {
        let value = "Wed, 21 Oct 2015 07:28:00 GMT";
        let retry_at = parse_http_date(value).unwrap();
        let now = retry_at - Duration::from_secs(60);
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", value.parse().unwrap());

        assert_eq!(
            RetryPolicy::retry_after_delay(StatusCode::TOO_MANY_REQUESTS, &headers, now),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            RetryPolicy::retry_after_delay(StatusCode::SERVICE_UNAVAILABLE, &headers, now),
            Some(Duration::from_secs(60))
        );

        assert_eq!(
            RetryPolicy::retry_after_delay(
                StatusCode::SERVICE_UNAVAILABLE,
                &headers,
                retry_at + Duration::from_secs(1)
            ),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn poll_delay_drains_messages_and_backs_off_errors() {
        let retry = RetryPolicy::default();
        let idle = IdlePolicy::default();
        assert_eq!(
            retry.poll_delay(PollOutcomeClass::Message, 0, &idle),
            Duration::ZERO
        );
        assert_eq!(
            retry.poll_delay(PollOutcomeClass::Nil, 0, &idle),
            Duration::from_secs(1)
        );
        assert_eq!(
            retry.poll_delay(PollOutcomeClass::Error, 1, &idle),
            Duration::from_secs(1)
        );
        assert_eq!(
            retry.poll_delay(PollOutcomeClass::Error, 3, &idle),
            Duration::from_secs(4)
        );
        assert_eq!(
            retry.poll_delay(PollOutcomeClass::Error, 99, &idle),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn retry_after_does_not_override_other_statuses_or_github_quota_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "40".parse().unwrap());
        headers.insert("x-ratelimit-remaining", "10".parse().unwrap());
        let delay =
            RetryPolicy::retry_after_delay(StatusCode::FORBIDDEN, &headers, SystemTime::UNIX_EPOCH);
        assert_eq!(delay, None);
        assert_eq!(
            RetryPolicy::retry_after_delay(
                StatusCode::INTERNAL_SERVER_ERROR,
                &headers,
                SystemTime::UNIX_EPOCH
            ),
            None
        );
    }
}

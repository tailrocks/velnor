//! Bounded per-scope broker recovery state.
//!
//! The controller owns one coordinator for a scope. It is deliberately pure:
//! transport code reports a classified signal, and the coordinator decides
//! whether to stay healthy, refresh credentials, recreate a session, back
//! off, or quarantine. This prevents every slot from independently reacting
//! to the same broker failure.

use std::time::Duration;

use crate::protocol::BrokerPollErrorClass;

const MAX_RETRY_STREAK: u32 = 8;
const MAX_BACKOFF: Duration = Duration::from_secs(600);
const RETRY_BUDGET: u32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryState {
    Healthy,
    MissingSession,
    Backoff,
    Quarantined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    None,
    RefreshCredentials,
    RecreateSession,
    Quarantine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverySignal {
    Empty,
    Message,
    Error(BrokerPollErrorClass),
}

#[derive(Debug, Clone)]
pub struct RecoveryCoordinator {
    state: RecoveryState,
    retry_streak: u32,
    retry_budget_used: u32,
    retry_at: Duration,
    quarantine_until: Option<Duration>,
    last_error_at: Option<Duration>,
}

impl Default for RecoveryCoordinator {
    fn default() -> Self {
        Self {
            state: RecoveryState::Healthy,
            retry_streak: 0,
            retry_budget_used: 0,
            retry_at: Duration::ZERO,
            quarantine_until: None,
            last_error_at: None,
        }
    }
}

impl RecoveryCoordinator {
    #[must_use]
    pub fn state(&self) -> RecoveryState {
        self.state
    }

    #[must_use]
    pub fn retry_streak(&self) -> u32 {
        self.retry_streak
    }

    #[must_use]
    pub fn retry_budget_used(&self) -> u32 {
        self.retry_budget_used
    }

    #[must_use]
    pub fn retry_at(&self) -> Duration {
        self.retry_at
    }

    #[must_use]
    pub fn quarantine_until(&self) -> Option<Duration> {
        self.quarantine_until
    }

    /// Observe one result at monotonic `now`, returning the one recovery
    /// authority's action. Repeated signals never create concurrent actions.
    pub fn observe(&mut self, signal: RecoverySignal, now: Duration) -> RecoveryAction {
        match signal {
            RecoverySignal::Empty | RecoverySignal::Message => {
                if self.state != RecoveryState::Quarantined {
                    self.recovered(now);
                }
                RecoveryAction::None
            }
            RecoverySignal::Error(BrokerPollErrorClass::Authentication) => {
                if self.last_error_at == Some(now) {
                    return RecoveryAction::None;
                }
                self.last_error_at = Some(now);
                if self.state == RecoveryState::Backoff {
                    return self.schedule_retry(now);
                }
                self.state = RecoveryState::Backoff;
                self.retry_at = now.saturating_add(Duration::from_secs(5));
                RecoveryAction::RefreshCredentials
            }
            RecoverySignal::Error(BrokerPollErrorClass::MissingSession) => {
                if self.last_error_at == Some(now) {
                    return RecoveryAction::None;
                }
                self.last_error_at = Some(now);
                if self.state == RecoveryState::MissingSession {
                    return self.schedule_retry(now);
                }
                self.state = RecoveryState::MissingSession;
                self.retry_at = now.saturating_add(Duration::from_secs(5));
                RecoveryAction::RecreateSession
            }
            RecoverySignal::Error(BrokerPollErrorClass::Conflict)
            | RecoverySignal::Error(BrokerPollErrorClass::Forbidden)
            | RecoverySignal::Error(BrokerPollErrorClass::Client)
            | RecoverySignal::Error(BrokerPollErrorClass::RateLimited)
            | RecoverySignal::Error(BrokerPollErrorClass::Server)
            | RecoverySignal::Error(BrokerPollErrorClass::Transport) => {
                if self.last_error_at == Some(now) {
                    return RecoveryAction::None;
                }
                self.last_error_at = Some(now);
                self.schedule_retry(now)
            }
        }
    }

    /// Return whether a recovery attempt is currently permitted.
    #[must_use]
    pub fn due(&self, now: Duration) -> bool {
        now >= self.retry_at && self.quarantine_until.is_none_or(|deadline| now >= deadline)
    }

    /// Record successful session/credential recovery.
    pub fn recovered(&mut self, now: Duration) {
        if self.state == RecoveryState::Quarantined {
            return;
        }
        self.state = RecoveryState::Healthy;
        self.retry_streak = 0;
        self.retry_budget_used = 0;
        self.retry_at = now;
        self.quarantine_until = None;
        self.last_error_at = None;
    }

    fn schedule_retry(&mut self, now: Duration) -> RecoveryAction {
        self.retry_streak = self.retry_streak.saturating_add(1).min(MAX_RETRY_STREAK);
        self.retry_budget_used = self.retry_budget_used.saturating_add(1);
        if self.retry_budget_used > RETRY_BUDGET {
            self.state = RecoveryState::Quarantined;
            let until = now.saturating_add(MAX_BACKOFF);
            self.quarantine_until = Some(until);
            self.retry_at = until;
            return RecoveryAction::Quarantine;
        }
        let multiplier = 1_u64 << self.retry_streak.saturating_sub(1).min(7);
        let backoff = Duration::from_secs(5_u64.saturating_mul(multiplier)).min(MAX_BACKOFF);
        self.state = RecoveryState::Backoff;
        self.retry_at = now.saturating_add(backoff);
        RecoveryAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_session_has_one_recreate_action() {
        let mut coordinator = RecoveryCoordinator::default();
        assert_eq!(
            coordinator.observe(
                RecoverySignal::Error(BrokerPollErrorClass::MissingSession),
                Duration::from_secs(10),
            ),
            RecoveryAction::RecreateSession
        );
        assert_eq!(coordinator.state(), RecoveryState::MissingSession);
        assert_eq!(
            coordinator.observe(
                RecoverySignal::Error(BrokerPollErrorClass::MissingSession),
                Duration::from_secs(10),
            ),
            RecoveryAction::None
        );
    }

    #[test]
    fn transient_failures_backoff_and_eventually_quarantine() {
        let mut coordinator = RecoveryCoordinator::default();
        for attempt in 0..=RETRY_BUDGET {
            let now = Duration::from_secs(1_000 + u64::from(attempt) * 600);
            let action =
                coordinator.observe(RecoverySignal::Error(BrokerPollErrorClass::Server), now);
            if attempt == RETRY_BUDGET {
                assert_eq!(action, RecoveryAction::Quarantine);
                assert_eq!(coordinator.state(), RecoveryState::Quarantined);
                assert!(!coordinator.due(now));
            }
        }
    }

    #[test]
    fn successful_idle_poll_resets_backoff_only_when_healthy() {
        let mut coordinator = RecoveryCoordinator::default();
        coordinator.observe(
            RecoverySignal::Error(BrokerPollErrorClass::Server),
            Duration::ZERO,
        );
        assert_eq!(coordinator.retry_streak(), 1);
        coordinator.recovered(Duration::from_secs(10));
        coordinator.observe(RecoverySignal::Empty, Duration::from_secs(10));
        assert_eq!(coordinator.retry_streak(), 0);
        assert_eq!(coordinator.state(), RecoveryState::Healthy);
    }

    #[test]
    fn quarantine_is_not_cleared_by_another_idle_signal() {
        let mut coordinator = RecoveryCoordinator::default();
        for attempt in 0..=RETRY_BUDGET {
            coordinator.observe(
                RecoverySignal::Error(BrokerPollErrorClass::Server),
                Duration::from_secs(1_000 + u64::from(attempt) * 600),
            );
        }
        assert_eq!(coordinator.state(), RecoveryState::Quarantined);
        assert_eq!(
            coordinator.observe(RecoverySignal::Empty, Duration::from_secs(99_999)),
            RecoveryAction::None
        );
        assert_eq!(coordinator.state(), RecoveryState::Quarantined);
    }

    #[test]
    fn every_broker_failure_class_is_bounded_and_scheduled() {
        let classes = [
            BrokerPollErrorClass::Authentication,
            BrokerPollErrorClass::Forbidden,
            BrokerPollErrorClass::MissingSession,
            BrokerPollErrorClass::Conflict,
            BrokerPollErrorClass::RateLimited,
            BrokerPollErrorClass::Client,
            BrokerPollErrorClass::Server,
            BrokerPollErrorClass::Transport,
        ];
        for class in classes {
            let mut coordinator = RecoveryCoordinator::default();
            let now = Duration::from_secs(500);
            let action = coordinator.observe(RecoverySignal::Error(class), now);
            assert!(
                matches!(
                    (class, action),
                    (
                        BrokerPollErrorClass::Authentication,
                        RecoveryAction::RefreshCredentials
                    ) | (
                        BrokerPollErrorClass::MissingSession,
                        RecoveryAction::RecreateSession
                    ) | (_, RecoveryAction::None)
                ),
                "unexpected action for {class:?}: {action:?}"
            );
            assert!(coordinator.retry_at() > now);
            assert!(coordinator.retry_at() <= now + MAX_BACKOFF);
            assert!(coordinator.retry_streak() <= MAX_RETRY_STREAK);
        }
    }

    #[test]
    fn sibling_failures_same_control_second_consume_one_retry_budget() {
        let mut coordinator = RecoveryCoordinator::default();
        let now = Duration::from_secs(500);
        coordinator.observe(RecoverySignal::Error(BrokerPollErrorClass::Server), now);
        coordinator.observe(RecoverySignal::Error(BrokerPollErrorClass::Server), now);
        assert_eq!(coordinator.retry_budget_used(), 1);
        assert_eq!(coordinator.retry_streak(), 1);
        assert_eq!(coordinator.state(), RecoveryState::Backoff);
    }

    #[test]
    fn one_missing_session_emits_one_coordinated_recreate() {
        let mut coordinator = RecoveryCoordinator::default();
        let now = Duration::from_secs(500);
        assert_eq!(
            coordinator.observe(
                RecoverySignal::Error(BrokerPollErrorClass::MissingSession),
                now,
            ),
            RecoveryAction::RecreateSession
        );
        assert_eq!(
            coordinator.observe(
                RecoverySignal::Error(BrokerPollErrorClass::MissingSession),
                now,
            ),
            RecoveryAction::None
        );
        assert_eq!(coordinator.retry_budget_used(), 0);
        assert_eq!(coordinator.state(), RecoveryState::MissingSession);
    }
}

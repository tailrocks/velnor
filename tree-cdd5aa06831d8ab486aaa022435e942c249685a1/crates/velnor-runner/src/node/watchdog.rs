//! systemd notify is gated on a completed local control cycle.

use crate::sd_notify;

/// Outcome of one bounded supervision cycle. Watchdog and READY fire only
/// when `completed` is true. An independent timer must not call this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalCycle {
    pub completed: bool,
}

impl LocalCycle {
    #[must_use]
    pub fn finished() -> Self {
        Self { completed: true }
    }

    #[must_use]
    pub fn incomplete() -> Self {
        Self { completed: false }
    }
}

/// Feed systemd only after a finished local cycle. Returns whether a ping
/// was sent (or would be sent when `$NOTIFY_SOCKET` is set).
#[must_use]
pub fn feed_after_cycle(cycle: LocalCycle, announce_ready: bool) -> bool {
    if !cycle.completed {
        return false;
    }
    if announce_ready {
        sd_notify::ready();
    }
    // Interval is advisory: systemd still requires WATCHDOG=1 after a real
    // cycle. Reading it keeps the helper live without a detached timer.
    let _interval = sd_notify::watchdog_interval();
    sd_notify::watchdog_ping();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_timer_must_not_feed() {
        assert!(!feed_after_cycle(LocalCycle::incomplete(), true));
    }

    #[test]
    fn completed_cycle_feeds() {
        assert!(feed_after_cycle(LocalCycle::finished(), true));
    }
}

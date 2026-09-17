//! Adapter telemetry: loop, acquire, and reconcile counters.
//!
//! Plain atomics behind a cheap-clone handle; the daemon scrapes
//! [`Metrics::snapshot`] for its health endpoint. No I/O, no clock, safe to
//! touch from every step of the §5.1 loop.

use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Default)]
struct Counters {
    polls: AtomicU64,
    nil_polls: AtomicU64,
    poll_errors: AtomicU64,
    messages: AtomicU64,
    scale_errors: AtomicU64,
    acks: AtomicU64,
    offers_observed: AtomicU64,
    offers_granted: AtomicU64,
    offers_declined: AtomicU64,
    acquire_batches: AtomicU64,
    acquired_ids: AtomicU64,
    missing_ids: AtomicU64,
    uncertain_batches: AtomicU64,
    provision_intents: AtomicU64,
    reconcile_runs: AtomicU64,
    unknown_events: AtomicU64,
    stale_grants_reset: AtomicU64,
    last_advertised_capacity: AtomicU32,
    last_message_id: AtomicI32,
}

/// Cloneable adapter metrics handle.
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    inner: Arc<Counters>,
}

impl Metrics {
    /// Increment a counter. Each step calls exactly the counters it owns so
    /// a scraped snapshot attributes work to the step that did it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inc_polls(&self) {
        self.inner.polls.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_nil_polls(&self) {
        self.inner.nil_polls.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_poll_errors(&self) {
        self.inner.poll_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_messages(&self) {
        self.inner.messages.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_scale_errors(&self) {
        self.inner.scale_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_acks(&self) {
        self.inner.acks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_offers_observed(&self, count: u64) {
        self.inner
            .offers_observed
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_offers_granted(&self, count: u64) {
        self.inner
            .offers_granted
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_offers_declined(&self, count: u64) {
        self.inner
            .offers_declined
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn inc_acquire_batches(&self) {
        self.inner.acquire_batches.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_acquired_ids(&self, count: u64) {
        self.inner.acquired_ids.fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_missing_ids(&self, count: u64) {
        self.inner.missing_ids.fetch_add(count, Ordering::Relaxed);
    }

    pub fn inc_uncertain_batches(&self) {
        self.inner.uncertain_batches.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_provision_intents(&self) {
        self.inner.provision_intents.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_reconcile_runs(&self) {
        self.inner.reconcile_runs.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_unknown_events(&self, count: u64) {
        self.inner
            .unknown_events
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_stale_grants_reset(&self, count: u64) {
        self.inner
            .stale_grants_reset
            .fetch_add(count, Ordering::Relaxed);
    }

    /// Last `X-ScaleSetMaxCapacity` value sent on a poll.
    pub fn set_advertised_capacity(&self, capacity: u32) {
        self.inner
            .last_advertised_capacity
            .store(capacity, Ordering::Relaxed);
    }

    /// Highest ACKed message ID (cursor high-water mark).
    pub fn set_last_message_id(&self, message_id: i32) {
        self.inner
            .last_message_id
            .store(message_id, Ordering::Relaxed);
    }

    /// Point-in-time copy of every counter and gauge.
    #[must_use]
    pub fn snapshot(&self) -> MetricSnapshot {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        MetricSnapshot {
            polls: load(&self.inner.polls),
            nil_polls: load(&self.inner.nil_polls),
            poll_errors: load(&self.inner.poll_errors),
            messages: load(&self.inner.messages),
            scale_errors: load(&self.inner.scale_errors),
            acks: load(&self.inner.acks),
            offers_observed: load(&self.inner.offers_observed),
            offers_granted: load(&self.inner.offers_granted),
            offers_declined: load(&self.inner.offers_declined),
            acquire_batches: load(&self.inner.acquire_batches),
            acquired_ids: load(&self.inner.acquired_ids),
            missing_ids: load(&self.inner.missing_ids),
            uncertain_batches: load(&self.inner.uncertain_batches),
            provision_intents: load(&self.inner.provision_intents),
            reconcile_runs: load(&self.inner.reconcile_runs),
            unknown_events: load(&self.inner.unknown_events),
            stale_grants_reset: load(&self.inner.stale_grants_reset),
            last_advertised_capacity: self.inner.last_advertised_capacity.load(Ordering::Relaxed),
            last_message_id: self.inner.last_message_id.load(Ordering::Relaxed),
        }
    }
}

/// Scrape-friendly copy of [`Metrics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricSnapshot {
    pub polls: u64,
    pub nil_polls: u64,
    pub poll_errors: u64,
    pub messages: u64,
    pub scale_errors: u64,
    pub acks: u64,
    pub offers_observed: u64,
    pub offers_granted: u64,
    pub offers_declined: u64,
    pub acquire_batches: u64,
    pub acquired_ids: u64,
    pub missing_ids: u64,
    pub uncertain_batches: u64,
    pub provision_intents: u64,
    pub reconcile_runs: u64,
    pub unknown_events: u64,
    pub stale_grants_reset: u64,
    pub last_advertised_capacity: u32,
    pub last_message_id: i32,
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
    fn snapshot_starts_at_zero() {
        let snapshot = Metrics::new().snapshot();
        assert_eq!(snapshot.polls, 0);
        assert_eq!(snapshot.acks, 0);
        assert_eq!(snapshot.last_advertised_capacity, 0);
        assert_eq!(snapshot.last_message_id, 0);
    }

    #[test]
    fn counters_accumulate_and_clones_share() {
        let metrics = Metrics::new();
        let shared = metrics.clone();
        metrics.inc_polls();
        shared.inc_polls();
        metrics.add_acquired_ids(3);
        metrics.set_advertised_capacity(9);
        metrics.set_last_message_id(41);
        let snapshot = shared.snapshot();
        assert_eq!(snapshot.polls, 2);
        assert_eq!(snapshot.acquired_ids, 3);
        assert_eq!(snapshot.last_advertised_capacity, 9);
        assert_eq!(snapshot.last_message_id, 41);
    }
}

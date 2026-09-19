//! Production [`CapacityLedger`][crate::scaleset::capacity::CapacityLedger]
//! over the ONE host-wide C2 ledger.
//!
//! The loop programs against the [`CapacityLedger`][cap] trait; this module
//! is the thin adapter that binds it to
//! [`velnor_control::permit_ledger::PermitLedger`] — the same SQLite file
//! the native lane acquires from, the same `N`, the same generation
//! fencing, the same reconcile-before-advertise rule. A permit held by set
//! 7 denies set 9 exactly as it denies a native acquisition, and the poll
//! loop's `X-ScaleSetMaxCapacity` advertisement is `N − occupied_global`
//! across both lanes ([`advertise_free`][crate::scaleset::capacity::advertise_free]).
//!
//! The adapter deliberately exposes no `set_max_jobs`/`begin_epoch`: only
//! the daemon startup path resizes or re-epochs the ledger. Scale-set
//! holders record no pid (`None`): workers are containers, not host
//! processes, so pid liveness can neither adopt nor sweep them.
//!
//! [cap]: crate::scaleset::capacity::CapacityLedger

use std::path::Path;

use velnor_control::permit_ledger::{
    AcquireOutcome as ControlAcquire, LedgerError as ControlError, PermitDemand,
    PermitLane as ControlLane, PermitLedger, PermitState as ControlState,
    ReconcileReport as ControlReport,
};

use crate::scaleset::capacity::{
    AcquireOutcome, CapacityLedger, LedgerHolder, LedgerLane, LedgerPermitState, ReconcileReport,
};

fn to_control_lane(lane: LedgerLane) -> ControlLane {
    match lane {
        LedgerLane::Native => ControlLane::Native,
        LedgerLane::ScaleSet => ControlLane::ScaleSet,
    }
}

fn from_control_lane(lane: ControlLane) -> LedgerLane {
    match lane {
        ControlLane::Native => LedgerLane::Native,
        ControlLane::ScaleSet => LedgerLane::ScaleSet,
    }
}

pub(crate) fn to_control_state(state: LedgerPermitState) -> ControlState {
    match state {
        LedgerPermitState::Reserved => ControlState::Reserved,
        LedgerPermitState::Acquiring => ControlState::Acquiring,
        LedgerPermitState::Provisioning => ControlState::Provisioning,
        LedgerPermitState::Assignable => ControlState::Assignable,
        LedgerPermitState::Running => ControlState::Running,
        LedgerPermitState::Cleaning => ControlState::Cleaning,
        LedgerPermitState::Uncertain => ControlState::Uncertain,
    }
}

fn from_control_state(state: ControlState) -> LedgerPermitState {
    match state {
        ControlState::Reserved => LedgerPermitState::Reserved,
        ControlState::Acquiring => LedgerPermitState::Acquiring,
        ControlState::Provisioning => LedgerPermitState::Provisioning,
        ControlState::Assignable => LedgerPermitState::Assignable,
        ControlState::Running => LedgerPermitState::Running,
        ControlState::Cleaning => LedgerPermitState::Cleaning,
        ControlState::Uncertain => LedgerPermitState::Uncertain,
    }
}

fn from_control_outcome(outcome: ControlAcquire) -> AcquireOutcome {
    match outcome {
        ControlAcquire::Acquired => AcquireOutcome::Acquired,
        ControlAcquire::AlreadyHeld => AcquireOutcome::AlreadyHeld,
        ControlAcquire::Full | ControlAcquire::Deferred | ControlAcquire::Closed => {
            AcquireOutcome::Full
        }
        ControlAcquire::StaleGeneration => AcquireOutcome::StaleGeneration,
        ControlAcquire::NotConfigured => AcquireOutcome::NotConfigured,
    }
}

fn from_control_report(report: ControlReport) -> ReconcileReport {
    ReconcileReport {
        adopted: report.adopted,
        marked_uncertain: report.marked_uncertain,
        confirmed: report.confirmed,
    }
}

/// [`CapacityLedger`] over the shared host-wide [`PermitLedger`] file.
///
/// Owns one SQLite connection; every method is one immediate transaction,
/// so concurrent native-lane handles on the same file stay consistent.
#[derive(Debug)]
pub struct SharedLedger {
    inner: PermitLedger,
}

impl SharedLedger {
    /// Bind to the host-wide ledger file. The path MUST be the same file
    /// the native lane uses (production: `permit-ledger.db` next to the
    /// state db — resolved once by the daemon startup, never per lane).
    pub fn open(path: &Path) -> Result<Self, ControlError> {
        Ok(Self {
            inner: PermitLedger::open(path)?,
        })
    }

    /// The bound ledger file.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.inner.path()
    }

    /// Record a Scale Set offer in the host-wide demand queue. Pass the
    /// durable first-seen time from the lane's offer record; redelivery
    /// refreshes liveness without changing queue age or global sequence.
    pub fn observe_demand(
        &mut self,
        holder: &str,
        lane: LedgerLane,
        scope: &str,
        first_seen_unix: u64,
        observed_unix: u64,
    ) -> Result<PermitDemand, ControlError> {
        self.inner.observe_demand(
            holder,
            to_control_lane(lane),
            scope,
            first_seen_unix,
            observed_unix,
        )
    }

    /// Stop an upstream offer from blocking later work after the lane has
    /// confirmed it is no longer eligible. Held permits must first pass
    /// through the cleanup release API.
    pub fn cancel_demand(&mut self, holder: &str) -> Result<bool, ControlError> {
        self.inner.cancel_demand(holder)
    }

    /// Release after confirmed retry/handoff cleanup while preserving the
    /// demand's original position in the global queue.
    pub fn release_to_eligible(&mut self, holder: &str) -> Result<bool, ControlError> {
        self.inner.release_to_eligible(holder)
    }

    /// Release after confirmed upstream cancellation.
    pub fn release_cancelled(&mut self, holder: &str) -> Result<bool, ControlError> {
        self.inner.release_cancelled(holder)
    }

    /// Keep occupancy after cleanup uncertainty and close the served demand
    /// in the same transaction.
    pub fn retain_uncertain(&mut self, holder: &str, generation: u64) -> Result<(), ControlError> {
        self.inner.retain_uncertain(holder, generation)
    }
}

impl CapacityLedger for SharedLedger {
    type Error = ControlError;

    fn generation(&self) -> Result<u64, Self::Error> {
        self.inner.generation()
    }

    fn advertised_free(&self) -> Result<Option<u32>, Self::Error> {
        self.inner.advertised_free()
    }

    fn occupied(&self) -> Result<u32, Self::Error> {
        self.inner.occupied()
    }

    fn holders(&self) -> Result<Vec<LedgerHolder>, Self::Error> {
        Ok(self
            .inner
            .holders()?
            .into_iter()
            .map(|holder| LedgerHolder {
                holder: holder.holder,
                lane: from_control_lane(holder.lane),
                state: from_control_state(holder.state),
                generation: holder.generation,
            })
            .collect())
    }

    fn holder_state(&self, holder: &str) -> Result<Option<LedgerPermitState>, Self::Error> {
        Ok(self.inner.holder_state(holder)?.map(from_control_state))
    }

    fn observe_demand(
        &mut self,
        holder: &str,
        lane: LedgerLane,
        scope: &str,
        first_seen_unix: u64,
        observed_unix: u64,
    ) -> Result<(), Self::Error> {
        self.inner.observe_demand(
            holder,
            to_control_lane(lane),
            scope,
            first_seen_unix,
            observed_unix,
        )?;
        Ok(())
    }

    fn cancel_demand(&mut self, holder: &str) -> Result<bool, Self::Error> {
        self.inner.cancel_demand(holder)
    }

    fn acquire(
        &mut self,
        holder: &str,
        lane: LedgerLane,
        state: LedgerPermitState,
        generation: u64,
    ) -> Result<AcquireOutcome, Self::Error> {
        Ok(from_control_outcome(self.inner.acquire(
            holder,
            to_control_lane(lane),
            to_control_state(state),
            generation,
            None,
        )?))
    }

    fn transition(
        &mut self,
        holder: &str,
        state: LedgerPermitState,
        generation: u64,
    ) -> Result<(), Self::Error> {
        self.inner
            .transition(holder, to_control_state(state), generation)
    }

    fn release(&mut self, holder: &str) -> Result<bool, Self::Error> {
        self.inner.release(holder)
    }

    fn release_to_eligible(&mut self, holder: &str) -> Result<bool, Self::Error> {
        self.inner.release_to_eligible(holder)
    }

    fn release_cancelled(&mut self, holder: &str) -> Result<bool, Self::Error> {
        self.inner.release_cancelled(holder)
    }

    fn retain_uncertain(&mut self, holder: &str, generation: u64) -> Result<(), Self::Error> {
        self.inner.retain_uncertain(holder, generation)
    }

    fn reconcile(
        &mut self,
        alive: &[(&str, LedgerLane, LedgerPermitState)],
    ) -> Result<ReconcileReport, Self::Error> {
        let attested: Vec<(&str, ControlLane, ControlState)> = alive
            .iter()
            .map(|(holder, lane, state)| {
                (*holder, to_control_lane(*lane), to_control_state(*state))
            })
            .collect();
        Ok(from_control_report(self.inner.reconcile(&attested)?))
    }

    fn is_stale_generation(error: &Self::Error) -> bool {
        matches!(error, ControlError::StaleGeneration { .. })
    }
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
    use crate::scaleset::capacity::{advertise_free, reserve_for_offer, ReserveOutcome};

    fn temp_ledger_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-shared-ledger-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("permit-ledger.db")
    }

    fn configured(path: &Path, max_jobs: u32) -> SharedLedger {
        let mut raw = PermitLedger::open(path).unwrap();
        raw.set_max_jobs(max_jobs).unwrap();
        raw.begin_epoch().unwrap();
        drop(raw);
        let mut ledger = SharedLedger::open(path).unwrap();
        ledger.reconcile(&[]).unwrap();
        ledger
    }

    #[test]
    fn unreconciled_advertises_nothing() {
        let path = temp_ledger_path("unreconciled");
        let mut raw = PermitLedger::open(&path).unwrap();
        raw.set_max_jobs(4).unwrap();
        raw.begin_epoch().unwrap();
        drop(raw);
        let ledger = SharedLedger::open(&path).unwrap();
        assert_eq!(ledger.advertised_free().unwrap(), None);
        assert_eq!(advertise_free(&ledger), 0);
    }

    #[test]
    fn reserve_counts_against_shared_n() {
        let path = temp_ledger_path("reserve");
        let mut ledger = configured(&path, 1);
        assert_eq!(advertise_free(&ledger), 1);
        let generation = ledger.generation().unwrap();
        let (outcome, _) = reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        assert_eq!(outcome, ReserveOutcome::Reserved);
        assert_eq!(advertise_free(&ledger), 0);
        let (full, _) = reserve_for_offer(&mut ledger, "scaleset/7/2", generation).unwrap();
        assert_eq!(full, ReserveOutcome::CapacityExhausted);
    }

    #[test]
    fn duplicate_holder_holds_once() {
        let path = temp_ledger_path("dup");
        let mut ledger = configured(&path, 1);
        let generation = ledger.generation().unwrap();
        let (first, _) = reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        let (again, _) = reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        assert_eq!(first, ReserveOutcome::Reserved);
        assert_eq!(again, ReserveOutcome::Reserved);
        assert_eq!(ledger.occupied().unwrap(), 1);
    }

    #[test]
    fn native_row_denies_scaleset_acquire() {
        let path = temp_ledger_path("x-lane");
        let mut ledger = configured(&path, 1);
        let generation = ledger.generation().unwrap();
        let native = ledger
            .acquire(
                "native/broker-9",
                LedgerLane::Native,
                LedgerPermitState::Running,
                generation,
            )
            .unwrap();
        assert_eq!(native, AcquireOutcome::Acquired);
        let (outcome, _) = reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        assert_eq!(outcome, ReserveOutcome::CapacityExhausted);
        let holders = ledger.holders().unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].lane, LedgerLane::Native);
    }

    #[test]
    fn shared_demand_api_orders_native_and_scaleset_before_acquire() {
        let path = temp_ledger_path("global-demand");
        let mut ledger = configured(&path, 2);
        let generation = ledger.generation().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        ledger
            .observe_demand("native/older", LedgerLane::Native, "scope-a", now, now)
            .unwrap();
        ledger
            .observe_demand(
                "scaleset/7/younger",
                LedgerLane::ScaleSet,
                "set-7",
                now + 1,
                now + 1,
            )
            .unwrap();

        assert_eq!(
            ledger
                .acquire(
                    "scaleset/7/younger",
                    LedgerLane::ScaleSet,
                    LedgerPermitState::Reserved,
                    generation,
                )
                .unwrap(),
            AcquireOutcome::Full
        );
        let raw = PermitLedger::open(&path).unwrap();
        assert_eq!(
            raw.demand("scaleset/7/younger").unwrap().unwrap().state,
            velnor_control::permit_ledger::DemandState::Eligible
        );
        drop(raw);

        assert_eq!(
            ledger
                .acquire(
                    "native/older",
                    LedgerLane::Native,
                    LedgerPermitState::Running,
                    generation,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            ledger
                .acquire(
                    "scaleset/7/younger",
                    LedgerLane::ScaleSet,
                    LedgerPermitState::Reserved,
                    generation,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
    }

    #[test]
    fn stale_generation_retries_then_reports() {
        let path = temp_ledger_path("stale");
        let mut ledger = configured(&path, 2);
        let stale = ledger.generation().unwrap();
        PermitLedger::open(&path).unwrap().begin_epoch().unwrap();
        // `reserve_for_offer` re-reads once and lands on the fresh epoch.
        let (outcome, landed) = reserve_for_offer(&mut ledger, "scaleset/7/1", stale).unwrap();
        assert_eq!(outcome, ReserveOutcome::Reserved);
        assert_eq!(landed, stale + 1);
        // A raw fenced transition on the old epoch reports stale.
        let mut raw = PermitLedger::open(&path).unwrap();
        let error = raw
            .transition("scaleset/7/1", ControlState::Running, stale)
            .unwrap_err();
        assert!(SharedLedger::is_stale_generation(&error));
    }

    #[test]
    fn reconcile_adopts_live_and_marks_missing_uncertain() {
        let path = temp_ledger_path("reconcile");
        let mut ledger = configured(&path, 4);
        let generation = ledger.generation().unwrap();
        ledger
            .acquire(
                "scaleset/7/1",
                LedgerLane::ScaleSet,
                LedgerPermitState::Running,
                generation,
            )
            .unwrap();
        let report = ledger
            .reconcile(&[(
                "scaleset/7/2",
                LedgerLane::ScaleSet,
                LedgerPermitState::Running,
            )])
            .unwrap();
        assert_eq!(report.adopted, vec!["scaleset/7/2".to_owned()]);
        assert_eq!(report.marked_uncertain, vec!["scaleset/7/1".to_owned()]);
        assert_eq!(
            ledger.holder_state("scaleset/7/1").unwrap(),
            Some(LedgerPermitState::Uncertain)
        );
        // Nothing deleted: both rows still occupy N.
        assert_eq!(ledger.occupied().unwrap(), 2);
    }

    #[test]
    fn release_after_cleanup_frees_exactly_once() {
        let path = temp_ledger_path("release");
        let mut ledger = configured(&path, 1);
        let generation = ledger.generation().unwrap();
        reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        assert!(ledger.release("scaleset/7/1").unwrap());
        assert!(!ledger.release("scaleset/7/1").unwrap());
        assert_eq!(advertise_free(&ledger), 1);
    }

    #[test]
    fn state_round_trips_both_directions() {
        for state in [
            LedgerPermitState::Reserved,
            LedgerPermitState::Acquiring,
            LedgerPermitState::Provisioning,
            LedgerPermitState::Assignable,
            LedgerPermitState::Running,
            LedgerPermitState::Cleaning,
            LedgerPermitState::Uncertain,
        ] {
            assert_eq!(from_control_state(to_control_state(state)), state);
        }
        assert_eq!(
            from_control_lane(to_control_lane(LedgerLane::Native)),
            LedgerLane::Native
        );
        assert_eq!(
            from_control_lane(to_control_lane(LedgerLane::ScaleSet)),
            LedgerLane::ScaleSet
        );
    }
}

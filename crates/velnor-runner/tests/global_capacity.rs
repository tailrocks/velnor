//! Native and Scale Set ingress share durable demand ordering and one permit
//! count. These assertions cover the active adapters, not a lane-local queue.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests may panic"
)]
#![cfg(feature = "test-support")]

use std::path::{Path, PathBuf};

use velnor_control::permit_ledger::{unix_now, PermitLane, PermitLedger};
use velnor_runner::permit_guard::{native_permit_holder, NativePermitGuard};
use velnor_runner::scaleset::capacity::{
    AcquireOutcome, CapacityLedger, LedgerLane, LedgerPermitState,
};
use velnor_runner::scaleset::SharedLedger;

const SCOPE_NATIVE: &str = "https://github.com/o/native";
const SCOPE_SCALE_SET: &str = "scale-set-7";

fn temp_ledger(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "velnor-global-capacity-it-{name}-{}-{}",
        std::process::id(),
        unix_now()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("permit-ledger.db")
}

fn configure(path: &Path, max_jobs: u32) -> u64 {
    let mut ledger = PermitLedger::open(path).unwrap();
    ledger.set_max_jobs(max_jobs).unwrap();
    let generation = ledger.begin_epoch().unwrap();
    ledger.reconcile(&[]).unwrap();
    generation
}

#[test]
fn native_and_scaleset_start_in_shared_observation_order() {
    let path = temp_ledger("cross-lane-order");
    let generation = configure(&path, 2);
    let observed = unix_now();
    let native_holder = native_permit_holder("older");
    let scaleset_holder = "scaleset/7/younger";

    // Match native broker ingress: persist its observation before the
    // shared acquisition transaction can race another lane.
    let mut native_ledger = PermitLedger::open(&path).unwrap();
    native_ledger
        .observe_demand(
            &native_holder,
            PermitLane::Native,
            SCOPE_NATIVE,
            observed,
            observed,
        )
        .unwrap();

    // Match Scale Set offer ingress. N=2 leaves a spare permit, but the
    // younger offer still cannot start before the older ungranted demand.
    let mut scaleset = SharedLedger::open(&path).unwrap();
    scaleset
        .observe_demand(
            scaleset_holder,
            LedgerLane::ScaleSet,
            SCOPE_SCALE_SET,
            observed,
            observed,
        )
        .unwrap();
    assert_eq!(
        scaleset
            .acquire(
                scaleset_holder,
                LedgerLane::ScaleSet,
                LedgerPermitState::Reserved,
                generation,
            )
            .unwrap(),
        AcquireOutcome::Full
    );
    assert_eq!(PermitLedger::open(&path).unwrap().occupied().unwrap(), 0);

    // Native acquires first; that atomic grant leaves the second global
    // slot available to the younger Scale Set demand.
    let native = NativePermitGuard::acquire(&path, native_holder.clone(), SCOPE_NATIVE)
        .unwrap()
        .expect("oldest native demand should acquire");
    assert_eq!(
        scaleset
            .acquire(
                scaleset_holder,
                LedgerLane::ScaleSet,
                LedgerPermitState::Reserved,
                generation,
            )
            .unwrap(),
        AcquireOutcome::Acquired
    );
    assert_eq!(PermitLedger::open(&path).unwrap().occupied().unwrap(), 2);

    // Native redelivery must retain its original global queue identity.
    let persisted = PermitLedger::open(&path)
        .unwrap()
        .demand(&native_holder)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.first_seen_unix, observed);
    assert_eq!(
        persisted.state,
        velnor_control::permit_ledger::DemandState::Granted
    );

    native.release();
    scaleset.release(scaleset_holder).unwrap();
    assert_eq!(PermitLedger::open(&path).unwrap().occupied().unwrap(), 0);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn older_native_demand_blocks_younger_scaleset_with_one_slot() {
    let path = temp_ledger("single-slot-order");
    let generation = configure(&path, 1);
    let observed = unix_now();
    let native_holder = native_permit_holder("older");
    let scaleset_holder = "scaleset/7/younger";

    let mut native_ledger = PermitLedger::open(&path).unwrap();
    native_ledger
        .observe_demand(
            &native_holder,
            PermitLane::Native,
            SCOPE_NATIVE,
            observed,
            observed,
        )
        .unwrap();
    let mut scaleset = SharedLedger::open(&path).unwrap();
    scaleset
        .observe_demand(
            scaleset_holder,
            LedgerLane::ScaleSet,
            SCOPE_SCALE_SET,
            observed,
            observed,
        )
        .unwrap();

    assert_eq!(
        scaleset
            .acquire(
                scaleset_holder,
                LedgerLane::ScaleSet,
                LedgerPermitState::Reserved,
                generation,
            )
            .unwrap(),
        AcquireOutcome::Full
    );
    let native = NativePermitGuard::acquire(&path, native_holder, SCOPE_NATIVE)
        .unwrap()
        .expect("older native demand should get the only slot");
    assert_eq!(
        scaleset
            .acquire(
                scaleset_holder,
                LedgerLane::ScaleSet,
                LedgerPermitState::Reserved,
                generation,
            )
            .unwrap(),
        AcquireOutcome::Full
    );
    assert_eq!(PermitLedger::open(&path).unwrap().occupied().unwrap(), 1);

    native.release();
    assert_eq!(
        scaleset
            .acquire(
                scaleset_holder,
                LedgerLane::ScaleSet,
                LedgerPermitState::Reserved,
                generation,
            )
            .unwrap(),
        AcquireOutcome::Acquired
    );
    scaleset.release(scaleset_holder).unwrap();
    assert_eq!(PermitLedger::open(&path).unwrap().occupied().unwrap(), 0);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

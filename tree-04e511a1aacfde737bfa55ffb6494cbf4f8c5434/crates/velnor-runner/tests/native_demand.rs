//! Native oldest-observed admission conformance: the demand fence and the
//! real [`NativePermitGuard`] share one ledger file, and grant order follows
//! durable age across every scope — never fence-call order, never scope.
//!
//! Time is explicit (`now_unix` arguments), so every ordering assertion is
//! deterministic: no sleeps, no wall-clock races.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
#![cfg(feature = "test-support")]

use std::path::{Path, PathBuf};

use velnor_control::permit_ledger::PermitLedger;
use velnor_runner::native_demand::{
    fence_admission, FenceOutcome, NativeDemandStore, STALE_AFTER_SECS,
};
use velnor_runner::permit_guard::{native_permit_holder, NativePermitGuard};

const SCOPE_A: &str = "https://github.com/o/repo-a";
const SCOPE_B: &str = "https://github.com/o/repo-b";
const SCOPE_C: &str = "https://github.com/o/repo-c";

fn temp_ledger(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "velnor-native-demand-it-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("permit-ledger.db")
}

fn configure(path: &Path, max_jobs: u32) {
    let mut ledger = PermitLedger::open(path).unwrap();
    ledger.set_max_jobs(max_jobs).unwrap();
    ledger.begin_epoch().unwrap();
    ledger.reconcile(&[]).unwrap();
}

fn fence(path: &Path, request: &str, scope: &str, now: u64) -> FenceOutcome {
    fence_admission(path, request, scope, now, STALE_AFTER_SECS)
}

fn acquire(path: &Path, request: &str, scope: &str) -> NativePermitGuard {
    NativePermitGuard::acquire(path, native_permit_holder(request), scope)
        .unwrap()
        .unwrap_or_else(|| panic!("expected a grant for {request}"))
}

fn occupied(path: &Path) -> u32 {
    PermitLedger::open(path).unwrap().occupied().unwrap()
}

/// The core invariant: with N=1 and two scopes, the younger offer defers
/// until the older is served — then grants. Age crosses scopes.
#[test]
fn younger_defers_until_older_is_served_across_scopes() {
    let path = temp_ledger("defer");
    configure(&path, 1);

    assert_eq!(fence(&path, "old", SCOPE_A, 1_000), FenceOutcome::Grant);
    assert_eq!(
        fence(&path, "young", SCOPE_B, 1_001),
        FenceOutcome::Defer { older: 1 }
    );

    // Old takes the only permit (granted, no longer eligible): the fence
    // grants — nothing older is eligible — and the LEDGER refuses young.
    // Capacity, not order, blocks young here.
    let old = acquire(&path, "old", SCOPE_A);
    assert_eq!(occupied(&path), 1);
    assert_eq!(fence(&path, "young", SCOPE_B, 1_002), FenceOutcome::Grant);
    assert!(
        NativePermitGuard::acquire(&path, native_permit_holder("young"), SCOPE_B)
            .unwrap()
            .is_none()
    );

    // Old is served and released; young is now oldest and grants.
    old.release();
    assert_eq!(occupied(&path), 0);
    assert_eq!(fence(&path, "young", SCOPE_B, 1_003), FenceOutcome::Grant);
    let young = acquire(&path, "young", SCOPE_B);
    assert_eq!(occupied(&path), 1);
    young.release();
    assert_eq!(occupied(&path), 0);
}

/// Grant order follows durable age, not fence-call order: three offers
/// fenced youngest-first still serve oldest-first.
#[test]
fn grant_order_follows_age_not_fence_call_order() {
    let path = temp_ledger("order");
    configure(&path, 1);
    let mut store = NativeDemandStore::open(&path).unwrap();
    store.submit_offer("a", SCOPE_A, 1_000).unwrap();
    store.submit_offer("b", SCOPE_B, 1_001).unwrap();
    store.submit_offer("c", SCOPE_C, 1_002).unwrap();
    drop(store);

    // Fence youngest-first: c sees two older, b sees one, a sees none.
    assert_eq!(
        fence(&path, "c", SCOPE_C, 1_003),
        FenceOutcome::Defer { older: 2 }
    );
    assert_eq!(
        fence(&path, "b", SCOPE_B, 1_003),
        FenceOutcome::Defer { older: 1 }
    );
    assert_eq!(fence(&path, "a", SCOPE_A, 1_003), FenceOutcome::Grant);

    // Serve strictly oldest-first; each completion promotes the next.
    acquire(&path, "a", SCOPE_A).release();
    assert_eq!(fence(&path, "b", SCOPE_B, 1_004), FenceOutcome::Grant);
    assert_eq!(
        fence(&path, "c", SCOPE_C, 1_004),
        FenceOutcome::Defer { older: 1 }
    );
    acquire(&path, "b", SCOPE_B).release();
    assert_eq!(fence(&path, "c", SCOPE_C, 1_005), FenceOutcome::Grant);
    acquire(&path, "c", SCOPE_C).release();
    assert_eq!(occupied(&path), 0);
}

/// No per-scope N: three scopes on N=2 grant exactly two permits total.
/// The fence defers the third scope while two older offers are eligible;
/// once the older offers hold permits, the fence grants (nothing older is
/// eligible) and the LEDGER refuses — the hard guarantee behind the
/// advisory fence.
#[test]
fn scopes_share_one_n_without_reservation() {
    let path = temp_ledger("scopes");
    configure(&path, 2);

    assert_eq!(fence(&path, "a", SCOPE_A, 1_000), FenceOutcome::Grant);
    assert_eq!(fence(&path, "b", SCOPE_B, 1_001), FenceOutcome::Grant);
    // Older-than-c is {a, b} = 2 and free is 2: defer.
    assert_eq!(
        fence(&path, "c", SCOPE_C, 1_002),
        FenceOutcome::Defer { older: 2 }
    );

    // Two scopes hold the whole N; the third scope is refused by the
    // ledger no matter which scope it is (no per-scope reserve).
    let held_a = acquire(&path, "a", SCOPE_A);
    let held_b = acquire(&path, "b", SCOPE_B);
    assert_eq!(occupied(&path), 2);
    assert_eq!(fence(&path, "c", SCOPE_C, 1_003), FenceOutcome::Grant);
    assert!(
        NativePermitGuard::acquire(&path, native_permit_holder("c"), SCOPE_C)
            .unwrap()
            .is_none()
    );

    // One release frees exactly one grant for the waiting scope.
    held_a.release();
    assert_eq!(fence(&path, "c", SCOPE_C, 1_004), FenceOutcome::Grant);
    let held_c = acquire(&path, "c", SCOPE_C);
    assert_eq!(occupied(&path), 2);
    held_b.release();
    held_c.release();
    assert_eq!(occupied(&path), 0);
}

/// Redelivery retains the original age: a re-fenced young offer never
/// jumps ahead of an older offer it once deferred behind.
#[test]
fn redelivery_retains_original_age() {
    let path = temp_ledger("redelivery");
    configure(&path, 1);

    assert_eq!(fence(&path, "old", SCOPE_A, 1_000), FenceOutcome::Grant);
    assert_eq!(
        fence(&path, "young", SCOPE_B, 1_001),
        FenceOutcome::Defer { older: 1 }
    );
    // Young is re-offered twice more; its age never moves.
    assert_eq!(
        fence(&path, "young", SCOPE_B, 1_002),
        FenceOutcome::Defer { older: 1 }
    );
    assert_eq!(
        fence(&path, "young", SCOPE_B, 1_003),
        FenceOutcome::Defer { older: 1 }
    );
    let store = NativeDemandStore::open(&path).unwrap();
    let row = store.get("young").unwrap().unwrap();
    assert_eq!(row.first_seen_unix, 1_001);
    assert_eq!(row.sequence, 2);
    // ... while its liveness refreshes, so live demand never goes stale.
    assert_eq!(row.updated_unix, 1_003);
}

/// Stale demand presumed dead upstream never blocks younger demand
/// forever — but a later redelivery revives it with the original age.
#[test]
fn stale_demand_yields_and_redelivery_revives_with_age() {
    let path = temp_ledger("stale");
    configure(&path, 1);

    assert_eq!(fence(&path, "old", SCOPE_A, 1_000), FenceOutcome::Grant);
    // 301s with no redelivery touch: old is presumed dead, young grants.
    assert_eq!(fence(&path, "young", SCOPE_B, 1_301), FenceOutcome::Grant);

    // Old is redelivered after all: its original age stands, and young —
    // still waiting — now defers behind it again.
    assert_eq!(fence(&path, "old", SCOPE_A, 1_302), FenceOutcome::Grant);
    assert_eq!(
        fence(&path, "young", SCOPE_B, 1_303),
        FenceOutcome::Defer { older: 1 }
    );
    let store = NativeDemandStore::open(&path).unwrap();
    assert_eq!(store.get("old").unwrap().unwrap().first_seen_unix, 1_000);
}

/// A re-offer of a served request proceeds (the ledger and run-service
/// idempotency decide, exactly as before demand existed) and never
/// wedges the queue: younger demand still grants around it.
#[test]
fn reoffer_of_served_demand_proceeds_without_wedging_young() {
    let path = temp_ledger("reoffer");
    configure(&path, 2);

    assert_eq!(fence(&path, "old", SCOPE_A, 1_000), FenceOutcome::Grant);
    acquire(&path, "old", SCOPE_A).release();
    // Served rows never defer: young grants immediately.
    assert_eq!(fence(&path, "young", SCOPE_B, 1_001), FenceOutcome::Grant);
    // The served request is re-offered: proceed, terminal row untouched.
    assert_eq!(fence(&path, "old", SCOPE_A, 1_002), FenceOutcome::Grant);
    let store = NativeDemandStore::open(&path).unwrap();
    assert_eq!(
        store.get("old").unwrap().unwrap().state,
        velnor_runner::native_demand::DemandState::Terminal
    );
}

/// Ordering blindness never corrupts capacity: a broken demand path
/// degrades to Blind, and the ledger behind it still enforces N.
#[test]
fn blind_fence_leaves_capacity_enforcement_to_the_ledger() {
    let broken = temp_ledger("blind-broken");
    std::fs::create_dir_all(&broken).unwrap();
    // A directory is not a database: ordering degrades, jobs proceed.
    assert!(matches!(
        fence(&broken, "req-1", SCOPE_A, 1_000),
        FenceOutcome::Blind { .. }
    ));

    // ... while a healthy ledger still refuses over-N natively.
    let path = temp_ledger("blind-ledger");
    configure(&path, 1);
    let _held = acquire(&path, "holder", SCOPE_A);
    assert!(
        NativePermitGuard::acquire(&path, native_permit_holder("other"), SCOPE_B)
            .unwrap()
            .is_none()
    );
}

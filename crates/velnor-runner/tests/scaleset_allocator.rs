//! Shared-allocator conformance: the scale-set lane and the native lane
//! spend from the ONE host-wide `max_jobs=N` ledger.
//!
//! Both lanes drive their REAL guards here — [`ScaleSetAllocator`] and the
//! native [`NativePermitGuard`] (with its demand lockstep) — against one
//! ledger file. Races run on threads with a barrier start so grants
//! genuinely contend inside immediate transactions.

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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Barrier};

use velnor_runner::permit_guard::{native_permit_holder, NativePermitGuard};
use velnor_runner::scaleset::allocator::{startup_reconcile, ScaleSetAllocator};
use velnor_runner::scaleset::permit_holder;

fn temp_ledger(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "velnor-alloc-race-{name}-{}-{}",
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
    use velnor_control::permit_ledger::PermitLedger;
    let mut ledger = PermitLedger::open(path).unwrap();
    ledger.set_max_jobs(max_jobs).unwrap();
    ledger.begin_epoch().unwrap();
    ledger.reconcile(&[]).unwrap();
}

#[test]
fn thundering_herd_grants_exactly_one_per_permit() {
    let path = temp_ledger("herd");
    configure(&path, 2);
    let allocator = Arc::new(ScaleSetAllocator::open(&path));
    let ledger_path = Arc::new(path.clone());

    // 16 racers (8 scale-set + 8 native guards) on N=2: exactly 2 win.
    // Each round lets every pending demand retry, so FIFO deferrals do not
    // strand the next oldest offer after its first attempt.
    let start = Arc::new(Barrier::new(16));
    let attempted = Arc::new(Barrier::new(16));
    let checked = Arc::new(Barrier::new(16));
    let wins = Arc::new(AtomicU32::new(0));
    let mut threads = Vec::new();
    for index in 0..8 {
        let (allocator, start, attempted, checked, wins) = (
            Arc::clone(&allocator),
            Arc::clone(&start),
            Arc::clone(&attempted),
            Arc::clone(&checked),
            Arc::clone(&wins),
        );
        threads.push(std::thread::spawn(move || {
            let holder = permit_holder(7, 1000 + index);
            let mut held = None;
            for _ in 0..16 {
                start.wait();
                if held.is_none() {
                    held = allocator.acquire(&holder).unwrap();
                    if held.is_some() {
                        wins.fetch_add(1, Ordering::SeqCst);
                    }
                }
                attempted.wait();
                let full = allocator.occupied().unwrap() == 2;
                checked.wait();
                if full {
                    break;
                }
            }
            drop(held);
        }));
    }
    for index in 0..8 {
        let (allocator, ledger_path, start, attempted, checked, wins) = (
            Arc::clone(&allocator),
            Arc::clone(&ledger_path),
            Arc::clone(&start),
            Arc::clone(&attempted),
            Arc::clone(&checked),
            Arc::clone(&wins),
        );
        threads.push(std::thread::spawn(move || {
            let holder = native_permit_holder("scope-test", &format!("herd-{index}"));
            let mut held = None;
            for _ in 0..16 {
                start.wait();
                if held.is_none() {
                    held = NativePermitGuard::acquire(&ledger_path, holder.clone(), "scope-test")
                        .unwrap();
                    if held.is_some() {
                        wins.fetch_add(1, Ordering::SeqCst);
                    }
                }
                attempted.wait();
                let full = allocator.occupied().unwrap() == 2;
                checked.wait();
                if full {
                    break;
                }
            }
            // Winners drop only once every lane sees full occupancy.
            drop(held);
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(wins.load(Ordering::SeqCst), 2);
    // Every winner's guard dropped on thread exit (release-on-drop runs
    // for both lanes): the ledger is empty again.
    assert_eq!(allocator.occupied().unwrap(), 0);
}

#[test]
fn occupancy_never_exceeds_n_under_churn() {
    let path = temp_ledger("churn");
    configure(&path, 4);
    let allocator = Arc::new(ScaleSetAllocator::open(&path));
    let ledger_path = Arc::new(path.clone());
    let barrier = Arc::new(Barrier::new(8));
    let violations = Arc::new(AtomicU32::new(0));

    let mut threads = Vec::new();
    for lane in 0..8 {
        let (allocator, ledger_path, barrier, violations) = (
            Arc::clone(&allocator),
            Arc::clone(&ledger_path),
            Arc::clone(&barrier),
            Arc::clone(&violations),
        );
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            for round in 0..25 {
                if lane % 2 == 0 {
                    let holder = permit_holder(7, i64::from(lane * 1000 + round));
                    if let Some(guard) = allocator.acquire(&holder).unwrap() {
                        if allocator.occupied().unwrap() > 4 {
                            violations.fetch_add(1, Ordering::SeqCst);
                        }
                        guard.transition_running();
                        guard.release();
                    }
                } else {
                    let holder =
                        native_permit_holder("scope-test", &format!("churn-{lane}-{round}"));
                    if let Some(mut guard) =
                        NativePermitGuard::acquire(&ledger_path, holder, "scope-test").unwrap()
                    {
                        if allocator.occupied().unwrap() > 4 {
                            violations.fetch_add(1, Ordering::SeqCst);
                        }
                        guard.transition_running();
                        guard.release().unwrap();
                    }
                }
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(violations.load(Ordering::SeqCst), 0);
    assert_eq!(allocator.occupied().unwrap(), 0);
}

#[test]
fn native_occupancy_denies_scaleset_and_vice_versa() {
    let path = temp_ledger("shared");
    configure(&path, 2);
    let allocator = ScaleSetAllocator::open(&path);

    // Native fills N: scale-set is refused (no per-lane reserve).
    let native_a =
        NativePermitGuard::acquire(&path, native_permit_holder("scope-a", "a"), "scope-a")
            .unwrap()
            .expect("native grant a");
    let native_b =
        NativePermitGuard::acquire(&path, native_permit_holder("scope-b", "b"), "scope-b")
            .unwrap()
            .expect("native grant b across scopes");
    assert!(allocator.acquire(&permit_holder(7, 1)).unwrap().is_none());

    // A nonterminal native drop requeues its older demand, so it retains
    // priority over the newer scale-set offer.
    drop(native_a);
    assert!(allocator.acquire(&permit_holder(7, 1)).unwrap().is_none());
    let retry = NativePermitGuard::acquire(&path, native_permit_holder("scope-a", "a"), "scope-a")
        .unwrap()
        .expect("older native demand keeps its place");
    retry.release().unwrap();

    // Terminal native release closes that demand and frees one grant.
    let guard = allocator
        .acquire(&permit_holder(7, 1))
        .unwrap()
        .expect("shared N frees one grant");
    // And the scale-set hold now denies native.
    assert!(
        NativePermitGuard::acquire(&path, native_permit_holder("scope-a", "c"), "scope-a")
            .unwrap()
            .is_none()
    );
    guard.release();
    let native_c =
        NativePermitGuard::acquire(&path, native_permit_holder("scope-a", "c"), "scope-a")
            .unwrap()
            .expect("native grant c after scale-set release");
    native_b.release().unwrap();
    native_c.release().unwrap();
    assert_eq!(allocator.occupied().unwrap(), 0);
}

#[test]
fn stale_generation_grants_nothing() {
    use velnor_control::permit_ledger::{AcquireOutcome, PermitLane, PermitLedger, PermitState};
    let path = temp_ledger("fence");
    configure(&path, 2);
    let allocator = ScaleSetAllocator::open(&path);

    // Read the generation, then move the epoch out from under it.
    let mut ledger = PermitLedger::open(&path).unwrap();
    let stale = ledger.generation().unwrap();
    ledger.begin_epoch().unwrap();
    assert_eq!(
        ledger
            .acquire(
                &permit_holder(7, 9),
                PermitLane::ScaleSet,
                PermitState::Acquiring,
                stale,
                None
            )
            .unwrap(),
        AcquireOutcome::StaleGeneration
    );
    assert_eq!(allocator.occupied().unwrap(), 0);

    // The allocator retries internally: its own acquire still grants.
    assert!(allocator.acquire(&permit_holder(7, 9)).unwrap().is_some());
}

#[test]
fn reconcile_before_advertise_marks_epoch_once() {
    use velnor_control::permit_ledger::{PermitLane, PermitLedger, PermitState};
    let path = temp_ledger("adv");
    let mut ledger = PermitLedger::open(&path).unwrap();
    ledger.set_max_jobs(3).unwrap();
    ledger.begin_epoch().unwrap();
    let allocator = ScaleSetAllocator::open(&path);
    assert_eq!(allocator.advertised_free().unwrap(), None);

    // A crash-window row (unreconciled) is adopted, not deleted.
    let generation = PermitLedger::open(&path).unwrap().generation().unwrap();
    PermitLedger::open(&path)
        .unwrap()
        .acquire(
            &permit_holder(7, 4242),
            PermitLane::ScaleSet,
            PermitState::Running,
            generation,
            None,
        )
        .unwrap();
    let report =
        startup_reconcile(&path, &[("scaleset/7/4242", PermitState::Running)], &[]).unwrap();
    assert_eq!(report.confirmed, vec!["scaleset/7/4242".to_string()]);
    assert_eq!(allocator.advertised_free().unwrap(), Some(2));

    // Unattested rows go uncertain (counted), never vanish.
    let report = startup_reconcile(&path, &[], &[]).unwrap();
    assert_eq!(report.marked_uncertain, vec!["scaleset/7/4242".to_string()]);
    assert_eq!(allocator.occupied().unwrap(), 1);
    assert_eq!(allocator.advertised_free().unwrap(), Some(2));
}

#[test]
fn startup_reconcile_retains_uncertain_rows_without_teardown_proof() {
    use velnor_control::permit_ledger::{PermitLedger, PermitState};
    let path = temp_ledger("sweep");
    configure(&path, 4);
    let allocator = ScaleSetAllocator::open(&path);

    // One uncertain scale-set row (cleanup failure), one uncertain native
    // row from a process that no longer exists.
    let guard = allocator
        .acquire(&permit_holder(7, 4242))
        .unwrap()
        .expect("grants");
    guard.mark_uncertain_and_disarm();
    let mut ledger = PermitLedger::open(&path).unwrap();
    let generation = ledger.generation().unwrap();
    ledger
        .acquire(
            "native/dead",
            velnor_control::permit_ledger::PermitLane::Native,
            PermitState::Running,
            generation,
            Some(u32::MAX),
        )
        .unwrap();
    ledger.retain_uncertain("native/dead", generation).unwrap();
    ledger.begin_epoch().unwrap();

    // Process death does not prove cleanup or peer-daemon handoff.
    let report = startup_reconcile(&path, &[], &[]).unwrap();
    assert!(report.adopted.is_empty());
    let ledger = PermitLedger::open(&path).unwrap();
    assert_eq!(ledger.occupied().unwrap(), 2);
    assert_eq!(
        ledger.holder_state(&permit_holder(7, 4242)).unwrap(),
        Some(PermitState::Uncertain)
    );
    assert_eq!(
        ledger.holder_state("native/dead").unwrap(),
        Some(PermitState::Uncertain)
    );
    assert_eq!(allocator.occupied().unwrap(), 2);
}

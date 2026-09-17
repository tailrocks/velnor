//! Shared-allocator conformance: the scale-set lane and the native lane
//! spend from the ONE host-wide `max_jobs=N` ledger.
//!
//! The native lane is a mock driving [`PermitLane::Native`] acquisitions
//! directly against the ledger file (the C2 native wiring merges
//! separately; the mock exercises the same ledger authority the real
//! guard will). Races run on threads with a barrier start so grants
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

use velnor_control::permit_ledger::{AcquireOutcome, PermitLane, PermitLedger, PermitState};
use velnor_runner::scaleset::allocator::{
    scaleset_permit_holder, startup_reconcile, ScaleSetAllocator,
};

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
    let mut ledger = PermitLedger::open(path).unwrap();
    ledger.set_max_jobs(max_jobs).unwrap();
    ledger.begin_epoch().unwrap();
    ledger.reconcile(&[]).unwrap();
}

/// Native-lane mock: the C2 guard's ledger footprint (Native lane,
/// pid-attributed rows) without the C2 wiring.
struct NativeMock {
    ledger_path: PathBuf,
}

impl NativeMock {
    fn acquire(&self, holder: &str) -> AcquireOutcome {
        let mut ledger = PermitLedger::open(&self.ledger_path).unwrap();
        for _ in 0..3 {
            let generation = ledger.generation().unwrap();
            match ledger
                .acquire(
                    holder,
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(std::process::id()),
                )
                .unwrap()
            {
                AcquireOutcome::StaleGeneration => continue,
                other => return other,
            }
        }
        AcquireOutcome::StaleGeneration
    }

    fn release(&self, holder: &str) -> bool {
        PermitLedger::open(&self.ledger_path)
            .unwrap()
            .release(holder)
            .unwrap()
    }
}

#[test]
fn thundering_herd_grants_exactly_one_per_permit() {
    let path = temp_ledger("herd");
    configure(&path, 2);
    let allocator = Arc::new(ScaleSetAllocator::open(&path));
    let native = Arc::new(NativeMock {
        ledger_path: path.clone(),
    });

    // 16 racers (8 scale-set + 8 native mock) on N=2: exactly 2 win.
    // The second barrier holds every winner past every loser's attempt,
    // so no release can precede a grant — the count is exact, not racy.
    let start = Arc::new(Barrier::new(16));
    let attempted = Arc::new(Barrier::new(16));
    let wins = Arc::new(AtomicU32::new(0));
    let mut threads = Vec::new();
    for index in 0..8 {
        let (allocator, start, attempted, wins) = (
            Arc::clone(&allocator),
            Arc::clone(&start),
            Arc::clone(&attempted),
            Arc::clone(&wins),
        );
        threads.push(std::thread::spawn(move || {
            start.wait();
            let holder = scaleset_permit_holder(7, 1000 + index);
            let guard = allocator.acquire(&holder).unwrap();
            if guard.is_some() {
                wins.fetch_add(1, Ordering::SeqCst);
            }
            attempted.wait();
            drop(guard);
        }));
    }
    for index in 0..8 {
        let (native, start, attempted, wins) = (
            Arc::clone(&native),
            Arc::clone(&start),
            Arc::clone(&attempted),
            Arc::clone(&wins),
        );
        threads.push(std::thread::spawn(move || {
            start.wait();
            let holder = format!("native/herd-{index}");
            let won = native.acquire(&holder) == AcquireOutcome::Acquired;
            if won {
                wins.fetch_add(1, Ordering::SeqCst);
            }
            attempted.wait();
            if won {
                native.release(&holder);
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(wins.load(Ordering::SeqCst), 2);
    // Scale-set winners' guards dropped on thread exit (release-on-drop);
    // native winners released explicitly: the ledger is empty again.
    assert_eq!(allocator.occupied().unwrap(), 0);
}

#[test]
fn occupancy_never_exceeds_n_under_churn() {
    let path = temp_ledger("churn");
    configure(&path, 4);
    let allocator = Arc::new(ScaleSetAllocator::open(&path));
    let native = Arc::new(NativeMock {
        ledger_path: path.clone(),
    });
    let barrier = Arc::new(Barrier::new(8));
    let violations = Arc::new(AtomicU32::new(0));

    let mut threads = Vec::new();
    for lane in 0..8 {
        let (allocator, native, barrier, violations) = (
            Arc::clone(&allocator),
            Arc::clone(&native),
            Arc::clone(&barrier),
            Arc::clone(&violations),
        );
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            for round in 0..25 {
                if lane % 2 == 0 {
                    let holder = scaleset_permit_holder(7, i64::from(lane * 1000 + round));
                    if let Some(guard) = allocator.acquire(&holder).unwrap() {
                        if allocator.occupied().unwrap() > 4 {
                            violations.fetch_add(1, Ordering::SeqCst);
                        }
                        guard.transition_running();
                        drop(guard);
                    }
                } else {
                    let holder = format!("native/churn-{lane}-{round}");
                    if native.acquire(&holder) == AcquireOutcome::Acquired {
                        if allocator.occupied().unwrap() > 4 {
                            violations.fetch_add(1, Ordering::SeqCst);
                        }
                        native.release(&holder);
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
    let native = NativeMock {
        ledger_path: path.clone(),
    };

    // Native fills N: scale-set is refused (no per-lane reserve).
    assert_eq!(native.acquire("native/a"), AcquireOutcome::Acquired);
    assert_eq!(native.acquire("native/b"), AcquireOutcome::Acquired);
    assert!(allocator
        .acquire(&scaleset_permit_holder(7, 1))
        .unwrap()
        .is_none());

    // One native release frees exactly one scale-set grant.
    assert!(native.release("native/a"));
    let guard = allocator
        .acquire(&scaleset_permit_holder(7, 1))
        .unwrap()
        .expect("shared N frees one grant");
    // And the scale-set hold now denies native.
    assert_eq!(native.acquire("native/c"), AcquireOutcome::Full);
    guard.release();
    assert_eq!(native.acquire("native/c"), AcquireOutcome::Acquired);
}

#[test]
fn stale_generation_grants_nothing() {
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
                &scaleset_permit_holder(7, 9),
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
    assert!(allocator
        .acquire(&scaleset_permit_holder(7, 9))
        .unwrap()
        .is_some());
}

#[test]
fn reconcile_before_advertise_marks_epoch_once() {
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
            &scaleset_permit_holder(7, 4242),
            PermitLane::ScaleSet,
            PermitState::Running,
            generation,
            None,
        )
        .unwrap();
    let (report, swept) = startup_reconcile(
        &path,
        &[("scaleset/7/4242", PermitState::Running)],
        &[],
        &|_| false,
    )
    .unwrap();
    assert_eq!(report.confirmed, vec!["scaleset/7/4242".to_string()]);
    assert!(swept.is_empty());
    assert_eq!(allocator.advertised_free().unwrap(), Some(2));

    // Unattested rows go uncertain (counted), never vanish.
    let (report, _) = startup_reconcile(&path, &[], &[], &|_| false).unwrap();
    assert_eq!(report.marked_uncertain, vec!["scaleset/7/4242".to_string()]);
    assert_eq!(allocator.occupied().unwrap(), 1);
    assert_eq!(allocator.advertised_free().unwrap(), Some(2));
}

#[test]
fn sweep_never_frees_scaleset_rows() {
    let path = temp_ledger("sweep");
    configure(&path, 4);
    let allocator = ScaleSetAllocator::open(&path);
    let native = NativeMock {
        ledger_path: path.clone(),
    };

    // One uncertain scale-set row (cleanup failure), one uncertain native
    // row with a dead pid, both unprotected.
    let guard = allocator
        .acquire(&scaleset_permit_holder(7, 4242))
        .unwrap()
        .expect("grants");
    guard.mark_uncertain_and_disarm();
    assert_eq!(native.acquire("native/dead"), AcquireOutcome::Acquired);
    let mut ledger = PermitLedger::open(&path).unwrap();
    let generation = ledger.generation().unwrap();
    ledger
        .transition("native/dead", PermitState::Uncertain, generation)
        .unwrap();

    // Every pid reads dead; nothing is protected.
    let (_, swept) = startup_reconcile(&path, &[], &[], &|_| false).unwrap();
    // The native row is swept; the scale-set row survives (pid-less rows
    // are never swept — holder recovery converges them).
    assert_eq!(swept, vec!["native/dead".to_string()]);
    let ledger = PermitLedger::open(&path).unwrap();
    assert_eq!(
        ledger
            .holder_state(&scaleset_permit_holder(7, 4242))
            .unwrap(),
        Some(PermitState::Uncertain)
    );
    assert_eq!(allocator.occupied().unwrap(), 1);
}

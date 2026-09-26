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
            let holder = permit_holder(7, 1000 + index);
            let guard = allocator.acquire(&holder).unwrap();
            if guard.is_some() {
                wins.fetch_add(1, Ordering::SeqCst);
            }
            attempted.wait();
            drop(guard);
        }));
    }
    for index in 0..8 {
        let (ledger_path, start, attempted, wins) = (
            Arc::clone(&ledger_path),
            Arc::clone(&start),
            Arc::clone(&attempted),
            Arc::clone(&wins),
        );
        threads.push(std::thread::spawn(move || {
            start.wait();
            let holder = native_permit_holder(&format!("herd-{index}"));
            let guard = NativePermitGuard::acquire(&ledger_path, holder, "scope-test").unwrap();
            if guard.is_some() {
                wins.fetch_add(1, Ordering::SeqCst);
            }
            attempted.wait();
            // Winners drop here (release-on-drop); losers hold nothing.
            drop(guard);
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
fn deferred_waiter_outlives_stalled_head() {
    // N=2 with an older demand whose grant lands 500ms late. A younger
    // single-shot waiter must still grant: departing on Deferred would
    // leave the head eligible-but-departed, ghosting every younger waiter
    // and stranding the free permit (the thundering-herd under-count).
    // Budget math: 32 blind spins resolve in milliseconds even on slow
    // disks (old code departs → None → this fails); the 5s wait budget
    // covers the stall with 10x margin (new code grants → this passes).
    stalled_head_grants_younger(false);
    stalled_head_grants_younger(true);
}

fn stalled_head_grants_younger(native: bool) {
    use std::time::Duration;
    use velnor_control::permit_ledger::{
        unix_now, AcquireOutcome, PermitLane, PermitLedger, PermitState,
    };

    let path = temp_ledger(if native {
        "stall-native"
    } else {
        "stall-scaleset"
    });
    configure(&path, 2);
    let allocator = ScaleSetAllocator::open(&path);
    let (lane, scope) = if native {
        (PermitLane::Native, "scope-test")
    } else {
        (PermitLane::ScaleSet, "")
    };
    let head_holder = if native {
        native_permit_holder("stall-head")
    } else {
        permit_holder(7, 1)
    };
    let young_holder = if native {
        native_permit_holder("stall-young")
    } else {
        permit_holder(7, 2)
    };
    // Plant the head first so it owns the front of the queue.
    {
        let mut ledger = PermitLedger::open(&path).unwrap();
        let now = unix_now();
        ledger
            .observe_demand(&head_holder, lane, scope, now, now)
            .unwrap();
    }
    // The head grants 500ms late on a background thread. The raw ledger
    // call leaves the row held (no guard to drop), so the ledger ends at
    // 2/2 with the younger waiter's guard.
    let grant_path = path.clone();
    let head_thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        let mut ledger = PermitLedger::open(&grant_path).unwrap();
        let generation = ledger.generation().unwrap();
        let pid = if native {
            Some(std::process::id())
        } else {
            None
        };
        let outcome = ledger
            .acquire(&head_holder, lane, PermitState::Acquiring, generation, pid)
            .unwrap();
        assert_eq!(outcome, AcquireOutcome::Acquired);
    });
    // The younger single-shot waiter outlives the stall and grants the
    // second permit instead of departing behind the head.
    if native {
        let _young = NativePermitGuard::acquire(&path, young_holder, scope)
            .unwrap()
            .expect("younger waiter outlives stalled head");
        head_thread.join().unwrap();
        assert_eq!(allocator.occupied().unwrap(), 2);
    } else {
        let _young = allocator
            .acquire(&young_holder)
            .unwrap()
            .expect("younger waiter outlives stalled head");
        head_thread.join().unwrap();
        assert_eq!(allocator.occupied().unwrap(), 2);
    }
}

#[test]
fn full_departure_parks_without_blocking_younger() {
    // N=1: a waiter refused while full parks its demand instead of
    // ghosting the queue head. A younger waiter grants past it, and the
    // departed waiter's redelivery revives at its original age.
    parked_departure_yields_to_younger(false);
    parked_departure_yields_to_younger(true);
}

fn parked_departure_yields_to_younger(native: bool) {
    use velnor_control::permit_ledger::{DemandState, PermitLedger};

    let path = temp_ledger(if native {
        "park-native"
    } else {
        "park-scaleset"
    });
    configure(&path, 1);
    let allocator = ScaleSetAllocator::open(&path);
    let scope = "scope-test";
    let (first, second, third) = if native {
        (
            native_permit_holder("park-first"),
            native_permit_holder("park-second"),
            native_permit_holder("park-third"),
        )
    } else {
        (
            permit_holder(7, 11),
            permit_holder(7, 12),
            permit_holder(7, 13),
        )
    };
    let demand_state = |holder: &str| {
        PermitLedger::open(&path)
            .unwrap()
            .demand(holder)
            .unwrap()
            .map(|row| row.state)
    };

    // The first waiter holds the one permit; the second departs full and
    // parks instead of head-blocking younger demand.
    if native {
        let first_guard = NativePermitGuard::acquire(&path, first, scope)
            .unwrap()
            .expect("first grants");
        assert!(NativePermitGuard::acquire(&path, second.clone(), scope)
            .unwrap()
            .is_none());
        assert_eq!(demand_state(&second), Some(DemandState::Waiting));
        first_guard.release();
        // A younger waiter grants past the parked departure.
        let third_guard = NativePermitGuard::acquire(&path, third, scope)
            .unwrap()
            .expect("younger grants past parked departure");
        third_guard.release();
        // Redelivery revives the parked demand at its original age.
        let ticket = PermitLedger::open(&path)
            .unwrap()
            .demand(&second)
            .unwrap()
            .unwrap();
        let second_guard = NativePermitGuard::acquire(&path, second.clone(), scope)
            .unwrap()
            .expect("redelivery revives parked demand");
        let revived = PermitLedger::open(&path)
            .unwrap()
            .demand(&second)
            .unwrap()
            .unwrap();
        assert_eq!(revived.first_seen_unix, ticket.first_seen_unix);
        assert_eq!(revived.sequence, ticket.sequence);
        second_guard.release();
    } else {
        let first_guard = allocator.acquire(&first).unwrap().expect("first grants");
        assert!(allocator.acquire(&second).unwrap().is_none());
        assert_eq!(demand_state(&second), Some(DemandState::Waiting));
        first_guard.release();
        // A younger waiter grants past the parked departure.
        let third_guard = allocator
            .acquire(&third)
            .unwrap()
            .expect("younger grants past parked departure");
        third_guard.release();
        // Redelivery revives the parked demand at its original age.
        let ticket = PermitLedger::open(&path)
            .unwrap()
            .demand(&second)
            .unwrap()
            .unwrap();
        let second_guard = allocator
            .acquire(&second)
            .unwrap()
            .expect("redelivery revives parked demand");
        let revived = PermitLedger::open(&path)
            .unwrap()
            .demand(&second)
            .unwrap()
            .unwrap();
        assert_eq!(revived.first_seen_unix, ticket.first_seen_unix);
        assert_eq!(revived.sequence, ticket.sequence);
        second_guard.release();
    }
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
                    let holder = native_permit_holder(&format!("churn-{lane}-{round}"));
                    if let Some(mut guard) =
                        NativePermitGuard::acquire(&ledger_path, holder, "scope-test").unwrap()
                    {
                        if allocator.occupied().unwrap() > 4 {
                            violations.fetch_add(1, Ordering::SeqCst);
                        }
                        guard.transition_running();
                        // Terminal release: Drop after running retains uncertain
                        // occupancy and would leak counted rows across rounds.
                        guard.release();
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
    let native_a = NativePermitGuard::acquire(&path, native_permit_holder("a"), "scope-a")
        .unwrap()
        .expect("native grant a");
    let native_b = NativePermitGuard::acquire(&path, native_permit_holder("b"), "scope-b")
        .unwrap()
        .expect("native grant b across scopes");
    assert!(allocator.acquire(&permit_holder(7, 1)).unwrap().is_none());

    // Terminal native cleanup (not Drop): Drop would return native/a to
    // Eligible with its original age, and global demand order would grant
    // that older native demand again instead of the scale-set offer.
    native_a.release();
    let guard = allocator
        .acquire(&permit_holder(7, 1))
        .unwrap()
        .expect("shared N frees one grant");
    // And the scale-set hold now denies native.
    assert!(
        NativePermitGuard::acquire(&path, native_permit_holder("c"), "scope-a")
            .unwrap()
            .is_none()
    );
    guard.release();
    let native_c = NativePermitGuard::acquire(&path, native_permit_holder("c"), "scope-a")
        .unwrap()
        .expect("native grant c after scale-set release");
    native_b.release();
    native_c.release();
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
    use velnor_control::permit_ledger::{PermitLedger, PermitState};
    let path = temp_ledger("sweep");
    configure(&path, 4);
    let allocator = ScaleSetAllocator::open(&path);

    // One uncertain scale-set row (cleanup failure), one uncertain native
    // row with a dead pid, both unprotected.
    let guard = allocator
        .acquire(&permit_holder(7, 4242))
        .unwrap()
        .expect("grants");
    guard.mark_uncertain_and_disarm();
    let native = NativePermitGuard::acquire(&path, native_permit_holder("dead"), "scope-a")
        .unwrap()
        .expect("native grants");
    // Drop without release would free the row; disarm via the uncertain
    // path instead so the sweep has an uncertain row to converge. The
    // sweep's pid probe is stubbed dead below, as in production crashes.
    native.mark_uncertain_and_disarm();

    // Every pid reads dead; nothing is protected.
    let (_, swept) = startup_reconcile(&path, &[], &[], &|_| false).unwrap();
    // The native row is swept; the scale-set row survives (pid-less rows
    // are never swept — holder recovery converges them).
    assert_eq!(swept, vec!["native/dead".to_string()]);
    let ledger = PermitLedger::open(&path).unwrap();
    assert_eq!(
        ledger.holder_state(&permit_holder(7, 4242)).unwrap(),
        Some(PermitState::Uncertain)
    );
    assert_eq!(allocator.occupied().unwrap(), 1);
}

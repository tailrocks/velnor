//! Lifecycle drain unification with the flag unset (default off).
//!
//! The in-process test below is valid only when `VELNOR_JOURNAL_DRAIN` is
//! unset: it pins the static path behaviorally identical on marker-free
//! journals. When the suite itself runs with the flag forced on, that
//! precondition is gone and the test skips instead of failing. The
//! subprocess test at the bottom never skips: the child re-executes this
//! binary with the flag removed from its own environment.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]

use std::sync::Arc;

use velnor_control::journal::{reduce, Event, Journal};
use velnor_control::lifecycle::LifecycleService;
use velnor_control::ports::{MutationKind, MutationPort, MutationRequest};
use velnor_control::store::Store;
use velnor_model::{Generation, SlotId};
use velnor_runner::node::{run_controller, ControllerArgs, ControllerLifecycle};

/// Skip when the operator forced the flag on for the whole suite run.
fn require_flag_unset() -> bool {
    if std::env::var("VELNOR_JOURNAL_DRAIN").is_ok_and(|value| value == "1") {
        eprintln!("skipped: default-off test requires VELNOR_JOURNAL_DRAIN unset");
        return false;
    }
    true
}

fn scratch(label: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "velnor-drain-off-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// (c) With the flag off, a lifecycle `draining` desired state is ignored:
/// no marker is latched, no observed write lands, and permits still flow.
#[tokio::test]
async fn controller_ignores_desired_draining_with_flag_off() {
    if !require_flag_unset() {
        return;
    }
    let dir = scratch("ignored");
    std::fs::write(
        dir.join("execution.toml"),
        "[execution]\nbackend = \"docker\"\n",
    )
    .unwrap();
    let store = Arc::new(Store::open(dir.join("state.db")).unwrap());
    LifecycleService::with_store_for_instance(Arc::clone(&store), "primary")
        .unwrap()
        .mutate(MutationRequest {
            kind: MutationKind::Drain,
            target: "primary".to_owned(),
            reason: "test".to_owned(),
            idempotency_key: "off-primary".to_owned(),
            expected_version: None,
            scale_to: None,
        })
        .unwrap();

    run_controller(ControllerArgs {
        state_dir: dir.clone(),
        scope: "velnor".to_owned(),
        desired_ready: 1,
        once: true,
        spawn_slots: false,
        lifecycle: Some(ControllerLifecycle {
            store: Arc::clone(&store),
            instance: "primary".to_owned(),
        }),
    })
    .await
    .unwrap();

    let journal = Journal::open(dir.join("journal.db")).unwrap();
    let state = journal.materialized_state().unwrap();
    assert!(!state.drain_active);
    let slot = state
        .slots
        .iter()
        .find(|slot| slot.slot_id.0 == "velnor-1")
        .expect("permit reserved with the flag off");
    assert!(slot.permit_held);
    let row = store.lifecycle_instance("primary").unwrap().unwrap();
    assert_eq!(row.desired_state, "draining");
    assert_eq!(row.observed_state, "ready");
    assert_eq!(row.resource_version, 2);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Hidden subprocess entry for `controller_drains_on_stale_marker_with_flag_off`:
/// re-executed via `current_exe --exact <this name>` with
/// `VELNOR_DRAIN_PROBE=dir:<state dir>` and `VELNOR_JOURNAL_DRAIN` removed.
/// No-ops in-process (the variable is absent there). Runs the controller
/// with `once: false`: only a drain exit returns at all, so a prompt exit
/// proves the flag-off arm honored the marker (a regressed controller
/// would reconcile and loop until the parent kills it).
#[tokio::test]
async fn subprocess_probe_flag_off_with_stale_marker() {
    let spec = std::env::var("VELNOR_DRAIN_PROBE").unwrap_or_default();
    let Some(dir) = spec.strip_prefix("dir:") else {
        return;
    };
    run_controller(ControllerArgs {
        state_dir: std::path::PathBuf::from(dir),
        scope: "velnor".to_owned(),
        desired_ready: 1,
        once: false,
        spawn_slots: false,
        lifecycle: None,
    })
    .await
    .unwrap();
}

/// Fail-closed with the flag off: a marker latched by an earlier flag-on
/// run is still a drain order. Never skips: the child runs with the flag
/// removed from its environment, so the suite's own env cannot void the
/// precondition the way it can for the in-process test above.
#[test]
fn controller_drains_on_stale_marker_with_flag_off() {
    let dir = scratch("stale-marker");
    std::fs::write(
        dir.join("execution.toml"),
        "[execution]\nbackend = \"docker\"\n",
    )
    .unwrap();
    let mut journal = Journal::open(dir.join("journal.db")).unwrap();
    assert!(journal.set_drain(4).unwrap());
    drop(journal);

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("subprocess_probe_flag_off_with_stale_marker")
        .arg("--nocapture")
        .env("VELNOR_DRAIN_PROBE", format!("dir:{}", dir.display()))
        .env_remove("VELNOR_JOURNAL_DRAIN")
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "flag-off probe must drain and exit 0");
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("flag-off controller did not drain-exit within 60s (stale marker ignored?)");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Drained before any reconcile ran: marker intact, no permits, and no
    // ledger writes (fail-closed reads the marker; nothing writes flag-off).
    let journal = Journal::open(dir.join("journal.db")).unwrap();
    let state = journal.materialized_state().unwrap();
    assert!(state.drain_active);
    assert_eq!(state.drain_version, 4);
    assert!(state.slots.is_empty());
    // The reducer leg is unconditional too: new work is rejected against
    // the marker regardless of the flag.
    let outcome = reduce(
        state,
        Event::PermitReserved {
            slot_id: SlotId("velnor-1".to_owned()),
            generation: Generation::INITIAL,
        },
    );
    assert!(outcome.rejected);
    std::fs::remove_dir_all(dir).unwrap();
}

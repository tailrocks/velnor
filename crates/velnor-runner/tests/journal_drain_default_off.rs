//! Lifecycle drain unification with the flag unset (default off).
//!
//! Valid only when `VELNOR_JOURNAL_DRAIN` is unset: these tests pin the
//! static path byte-identical. When the suite itself runs with the flag
//! forced on, the precondition is gone and each test skips instead of
//! failing.

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

use velnor_control::journal::Journal;
use velnor_control::lifecycle::LifecycleService;
use velnor_control::ports::{MutationKind, MutationPort, MutationRequest};
use velnor_control::store::Store;
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

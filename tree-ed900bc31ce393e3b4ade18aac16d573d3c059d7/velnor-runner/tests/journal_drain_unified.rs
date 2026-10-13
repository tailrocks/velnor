//! Lifecycle drain unification with `VELNOR_JOURNAL_DRAIN=1`.
//!
//! Every test enables the flag explicitly (same value, so no races), which
//! makes this binary robust whether the suite runs with the flag on or off.

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

fn enable_flag() {
    unsafe { std::env::set_var("VELNOR_JOURNAL_DRAIN", "1") };
}

fn scratch(label: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "velnor-drain-unified-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn drain_mutation(store: &Arc<Store>, slug: &str) {
    LifecycleService::with_store_for_instance(Arc::clone(store), slug)
        .unwrap()
        .mutate(MutationRequest {
            kind: MutationKind::Drain,
            target: slug.to_owned(),
            reason: "test".to_owned(),
            idempotency_key: format!("unified-{slug}"),
            expected_version: None,
            scale_to: None,
        })
        .unwrap();
}

/// (b) A fresh lifecycle `draining` desired state drives the supervised
/// controller to latch the journal marker, record the observed projection
/// exactly once, and exit without reserving permits.
#[tokio::test]
async fn controller_drains_on_fresh_desired_draining() {
    enable_flag();
    let dir = scratch("desired");
    let store = Arc::new(Store::open(dir.join("state.db")).unwrap());
    drain_mutation(&store, "primary");

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
    assert!(state.drain_active);
    assert_eq!(state.drain_version, 2);
    assert!(state.slots.is_empty());
    let row = store.lifecycle_instance("primary").unwrap().unwrap();
    assert_eq!(row.desired_state, "draining");
    assert_eq!(row.observed_state, "draining");
    assert_eq!(row.resource_version, 3);
    std::fs::remove_dir_all(dir).unwrap();
}

/// (e) A marker latched by another process drains the controller through
/// the journal leg alone: no ledger, no rewrite, no permits.
#[tokio::test]
async fn controller_exits_on_latched_journal_marker_without_ledger() {
    enable_flag();
    let dir = scratch("marker");
    let mut journal = Journal::open(dir.join("journal.db")).unwrap();
    assert!(journal.set_drain(9).unwrap());
    drop(journal);

    run_controller(ControllerArgs {
        state_dir: dir.clone(),
        scope: "velnor".to_owned(),
        desired_ready: 1,
        once: true,
        spawn_slots: false,
        lifecycle: None,
    })
    .await
    .unwrap();

    let journal = Journal::open(dir.join("journal.db")).unwrap();
    let state = journal.materialized_state().unwrap();
    assert!(state.drain_active);
    assert_eq!(state.drain_version, 9);
    assert!(state.slots.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

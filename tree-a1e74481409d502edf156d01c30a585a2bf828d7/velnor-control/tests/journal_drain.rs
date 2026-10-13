//! Lifecycle drain unification: cross-handle contracts.
//!
//! Each test opens the same SQLite file through two independent handles,
//! which is the isolation boundary separate processes get (WAL plus
//! immediate transactions) — but both handles still live in one process.
//! True process-boundary coverage is the stale-marker subprocess test in
//! `velnor-runner/tests/journal_drain_default_off.rs`. No environment flag
//! is involved: the control crate gates on durable data, and the runner
//! gates the readers.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]

use velnor_control::journal::{read_drain_state, Event, Journal};
use velnor_control::store::{InstanceRow, Store};
use velnor_model::{Generation, SlotId, Timestamp};

fn scratch(label: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "velnor-journal-drain-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// (a) A drain marker latched through one handle is visible to a second
/// handle and to the zero-timeout (non-blocking) reader, and the second handle's reducer
/// rejects fresh permits and acquisitions against it.
#[test]
fn drain_marker_is_visible_across_handles_and_gates_the_second_writer() {
    let dir = scratch("cross-handle");
    let path = dir.join("journal.db");
    let mut first = Journal::open(&path).unwrap();
    assert!(first.set_drain(6).unwrap());

    let mut second = Journal::open(&path).unwrap();
    let state = second.materialized_state().unwrap();
    assert!(state.drain_active);
    assert_eq!(state.drain_version, 6);
    let marker = read_drain_state(&path).unwrap().unwrap();
    assert!(marker.active);
    assert_eq!(marker.version, 6);

    let generation = Generation::INITIAL;
    assert!(
        second
            .apply(Event::PermitReserved {
                slot_id: SlotId("velnor-1".to_owned()),
                generation,
            })
            .unwrap()
            .rejected
    );
    assert!(
        second
            .apply(Event::JobAcquisitionIntended {
                slot_id: SlotId("velnor-1".to_owned()),
                job_id: velnor_model::JobId("job-1".to_owned()),
                generation,
                message_id: "msg-1".to_owned(),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 1_000,
            })
            .unwrap()
            .rejected
    );
    // The rejections persisted nothing, so the marker still reads back.
    assert_eq!(read_drain_state(&path).unwrap().unwrap().version, 6);
    std::fs::remove_dir_all(dir).unwrap();
}

/// (d) An observed write with a stale version converges on the row another
/// handle committed: no error, no retry, the re-read row is returned.
#[test]
fn observed_write_converges_across_two_store_handles() {
    let dir = scratch("observed-handles");
    let path = dir.join("state.db");
    let first = Store::open(&path).unwrap();
    first
        .upsert_instance(&InstanceRow {
            instance_slug: "primary".to_owned(),
            host: "test-host".to_owned(),
            daemon_version: "test".to_owned(),
            slots_configured: 1,
            slots_busy: 0,
            updated_at: Timestamp::UNIX_EPOCH,
        })
        .unwrap();

    let second = Store::open(&path).unwrap();
    let (written, fresh) = first
        .record_lifecycle_observed("primary", "draining", 1)
        .unwrap();
    assert!(fresh);
    assert_eq!(written.resource_version, 2);

    // The second handle still believes version 1: it converges instead of
    // erroring or retrying.
    let (converged, updated) = second
        .record_lifecycle_observed("primary", "ready", 1)
        .unwrap();
    assert!(!updated);
    assert_eq!(converged.observed_state, "draining");
    assert_eq!(converged.resource_version, 2);

    // And a write at the converged version lands exactly once.
    let (rewritten, fresh) = second
        .record_lifecycle_observed("primary", "draining", 2)
        .unwrap();
    assert!(fresh);
    assert_eq!(rewritten.resource_version, 3);
    assert_eq!(
        first
            .lifecycle_instance("primary")
            .unwrap()
            .unwrap()
            .resource_version,
        3
    );
    std::fs::remove_dir_all(dir).unwrap();
}

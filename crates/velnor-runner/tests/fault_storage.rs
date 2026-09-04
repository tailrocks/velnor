//! Storage faults: a host that runs out of disk, a durable store another
//! process is holding, a corrupted journal, a corrupted artifact, and the
//! debris a killed writer leaves in the completion outbox.
//!
//! Disk pressure is driven through its own measurement-and-clock seam rather
//! than by filling a real filesystem: `DiskPressure::observe` takes the free
//! bytes and the wall-clock second as arguments, so a five-hour escalation is
//! a loop, and the assertion is on the state machine rather than on whether a
//! CI runner happened to have space.

mod fault_support;

use std::path::PathBuf;
use std::time::Duration;

use velnor_control::journal::Event;
use velnor_model::JobId;
use velnor_runner::execution::{
    hex_sha256, verify_microvm_artifacts, ArtifactChecksums, MemoryFs, MicroVmArtifactSet,
};
use velnor_runner::host_capacity::{
    docker_usage_bytes_from_df, DiskAction, DiskPolicy, DiskPressure, DiskState,
    DEFAULT_MIN_FREE_BYTES,
};

use fault_support::{fixture_slot, open_journal, prime_owned_job, TempRoot};

const GIB: u64 = 1024 * 1024 * 1024;

fn policy() -> DiskPolicy {
    DiskPolicy {
        min_free_bytes: 2 * GIB,
        degraded_deadline: Duration::from_secs(600),
        drain_deadline: Duration::from_secs(300),
    }
}

#[test]
fn a_host_near_the_floor_reclaims_before_it_refuses_work() {
    // "Near-full" must not be the same event as "full": the first response to
    // shrinking space is to reclaim, not to shed the slot.
    let mut pressure = DiskPressure::new(policy());
    assert_eq!(pressure.observe(8 * GIB, 1_000), DiskAction::Admit);
    assert_eq!(pressure.state(), DiskState::Healthy);

    let action = pressure.observe(GIB, 1_010);
    assert_eq!(
        action,
        DiskAction::Reclaim,
        "the first below-floor measurement must trigger reclaim, not refusal"
    );

    // Space came back before the reclaimer had to escalate.
    assert_eq!(pressure.observe(4 * GIB, 1_020), DiskAction::Admit);
    assert_eq!(
        pressure.state(),
        DiskState::Healthy,
        "recovery must be immediate rather than waiting out a cooldown"
    );
}

#[test]
fn a_full_host_escalates_through_bounded_states_to_deregistration() {
    // The property that matters is that no state means "sleep and retry
    // forever": every step has a deadline, and the sequence terminates.
    let policy = policy();
    let mut pressure = DiskPressure::new(policy);
    let mut seen = Vec::new();

    assert_eq!(pressure.observe(8 * GIB, 0), DiskAction::Admit);
    // One measurement per minute for three hours, with the disk never recovering.
    for minute in 1..=180_u64 {
        let action = pressure.observe(1024, minute * 60);
        if seen.last() != Some(&action) {
            seen.push(action);
        }
        if action == DiskAction::Deregister {
            break;
        }
    }

    assert!(
        matches!(seen.first(), Some(DiskAction::Reclaim)),
        "the escalation must start with reclaim, got {seen:?}"
    );
    assert_eq!(
        seen.last(),
        Some(&DiskAction::Deregister),
        "a host that never recovers must reach a terminal state, got {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|action| matches!(action, DiskAction::Drain)),
        "the slot must drain before it deregisters, got {seen:?}"
    );
    for action in &seen {
        if let DiskAction::RefuseUntil { remaining } = action {
            assert!(
                *remaining <= policy.degraded_deadline,
                "a refusal must carry a bounded remaining time, got {remaining:?}"
            );
        }
    }

    // Terminal means terminal: space coming back cannot silently revive a
    // deregistered slot behind the operator's back.
    assert_eq!(pressure.observe(64 * GIB, 100_000), DiskAction::Deregister);
    assert_eq!(pressure.state(), DiskState::Deregistered);
}

#[test]
fn admission_is_refused_before_a_job_is_acquired_rather_than_after() {
    // Acquiring first and discovering the host cannot hold the job is how a
    // doomed job is created. The decision has to be answerable from the state
    // alone, ahead of acquisition.
    let policy = policy();
    assert!(policy.admits(DiskState::Healthy));
    assert!(!policy.admits(DiskState::Reclaiming));
    assert!(!policy.admits(DiskState::Degraded {
        elapsed: Duration::ZERO
    }));
    assert!(!policy.admits(DiskState::Draining {
        elapsed: Duration::ZERO
    }));
    assert!(!policy.admits(DiskState::Deregistered));
}

#[test]
fn the_default_disk_floor_leaves_room_for_a_job_rather_than_a_byte() {
    assert_eq!(DiskPolicy::default().min_free_bytes, DEFAULT_MIN_FREE_BYTES);
    const {
        assert!(
            DEFAULT_MIN_FREE_BYTES >= GIB,
            "a floor below a gibibyte cannot hold one image pull"
        );
    }
}

#[test]
fn an_unreadable_docker_usage_report_is_absent_rather_than_zero() {
    // Zero is the dangerous answer: it says "Docker is using nothing", which
    // inflates the promisable budget exactly when the daemon is unhealthy.
    assert_eq!(docker_usage_bytes_from_df(""), None);
    assert_eq!(docker_usage_bytes_from_df("not json at all"), None);
    assert_eq!(docker_usage_bytes_from_df("{\"Images\":[]"), None);
}

#[test]
fn a_journal_another_process_holds_exclusively_fails_loudly_and_recovers_after() {
    let root = TempRoot::new("sqlite-busy");
    let job_id = JobId("job-busy".into());
    let slot = fixture_slot();
    let mut journal = open_journal(root.path());
    let generation = prime_owned_job(&mut journal, &slot, &job_id);

    let path = root.join("journal.db");
    let blocker = rusqlite::Connection::open(&path).expect("open the blocking connection");
    blocker
        .busy_timeout(Duration::from_millis(50))
        .expect("bound the blocker's own waiting");
    blocker
        .execute_batch("BEGIN EXCLUSIVE")
        .expect("take the exclusive lock");

    // A second process cannot even open the journal while it is held, and says
    // so in terms an operator can act on rather than as a bare SQLite code.
    let blocked_open =
        velnor_control::journal::Journal::open(&path).expect_err("a held journal must not open");
    fault_support::assert_actionable_diagnostic(&format!("{blocked_open}"), &["locked", "retry"]);

    // The already-open handle refuses the write rather than losing it.
    let error = journal
        .apply(Event::JobStarted {
            job_id: job_id.clone(),
            generation,
        })
        .expect_err("a locked journal must refuse the write, not lose it");
    fault_support::assert_actionable_diagnostic(&format!("{error}"), &["locked"]);

    // Recoverable durable state: the write was refused, not half-applied, and
    // the same handle succeeds once the lock is released.
    blocker
        .execute_batch("ROLLBACK")
        .expect("release the exclusive lock");
    drop(blocker);

    let outcome = journal
        .apply(Event::JobStarted {
            job_id: job_id.clone(),
            generation,
        })
        .expect("the journal is usable again once the lock is gone");
    assert!(!outcome.rejected);
    assert_eq!(
        fault_support::job_phase(&journal, &job_id),
        Some(velnor_model::ActorPhase::Running)
    );
}

#[test]
fn a_corrupted_journal_is_refused_rather_than_opened_and_believed() {
    let root = TempRoot::new("journal-corrupt");
    {
        let mut journal = open_journal(root.path());
        prime_owned_job(&mut journal, &fixture_slot(), &JobId("job-corrupt".into()));
    }

    let path = root.join("journal.db");
    let good = std::fs::read(&path).expect("read the healthy journal");
    assert!(!good.is_empty());
    std::fs::write(&path, b"this is not a SQLite database, it is a log file")
        .expect("corrupt the journal");
    // The WAL and shared-memory files would otherwise contradict the corrupted
    // main database in a way SQLite reports differently per platform.
    for suffix in ["-wal", "-shm"] {
        let mut companion = path.clone().into_os_string();
        companion.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(companion));
    }

    let opened = velnor_control::journal::Journal::open(&path);
    let error = opened.expect_err("a corrupted journal must not open");
    // The message names the physical fault and the remediation, though not the
    // journal itself; see the report accompanying this suite.
    fault_support::assert_actionable_diagnostic(&format!("{error}"), &["store", "not a database"]);
}

#[test]
fn a_corrupted_artifact_is_named_by_the_check_that_caught_it() {
    // The cache-corruption class in its most consequential form: a byte of a
    // pinned artifact changed, and the runner must refuse to boot a guest from
    // it rather than fail mysteriously later.
    let root = PathBuf::from("/packaged/microvm");
    let mut fs = MemoryFs::default();
    let contents: [(&str, &[u8]); 5] = [
        ("firecracker", b"firecracker-binary"),
        ("jailer", b"jailer-binary"),
        ("vmlinux", b"kernel-image"),
        ("rootfs.ext4", b"rootfs-image"),
        ("velnor-guest-agent", b"guest-agent-binary"),
    ];
    for (name, bytes) in contents {
        fs.files.insert(root.join(name), bytes.to_vec());
    }

    let checksums = ArtifactChecksums {
        firecracker_version: "pinned".into(),
        jailer_version: "pinned".into(),
        firecracker: hex_sha256(b"firecracker-binary"),
        jailer: hex_sha256(b"jailer-binary"),
        kernel: hex_sha256(b"kernel-image"),
        rootfs: hex_sha256(b"rootfs-image"),
        guest_agent: hex_sha256(b"guest-agent-binary"),
        snapshot: None,
    };
    let set = MicroVmArtifactSet::from_root(&root, checksums);
    verify_microvm_artifacts(&set, &fs).expect("an intact artifact set verifies");

    // One flipped byte in the root filesystem image.
    fs.files
        .insert(root.join("rootfs.ext4"), b"rootfs-imagf".to_vec());
    let error = verify_microvm_artifacts(&set, &fs)
        .expect_err("a corrupted artifact must not pass verification");
    fault_support::assert_actionable_diagnostic(&format!("{error}"), &["rootfs"]);

    // A truncated (rather than altered) artifact is the same class of fault and
    // must be caught by the same check.
    fs.files.insert(root.join("rootfs.ext4"), Vec::new());
    let truncated = verify_microvm_artifacts(&set, &fs)
        .expect_err("a truncated artifact must not pass verification");
    fault_support::assert_actionable_diagnostic(&format!("{truncated}"), &["rootfs"]);

    // A missing artifact must say it is missing, not that it mismatched.
    fs.files.remove(&root.join("rootfs.ext4"));
    let missing =
        verify_microvm_artifacts(&set, &fs).expect_err("a missing artifact must not verify");
    fault_support::assert_actionable_diagnostic(&format!("{missing}"), &["rootfs.ext4"]);
}

#[test]
fn debris_left_by_a_killed_writer_does_not_block_or_contaminate_a_fresh_publish() {
    // A process killed mid-publish leaves a partial temporary file in the
    // outbox directory. It must neither be mistaken for a payload nor stop the
    // next writer from publishing one.
    let root = TempRoot::new("outbox-debris");
    let outbox = root.join("outbox");
    std::fs::create_dir_all(&outbox).expect("create the outbox directory");
    std::fs::write(outbox.join("..job-stale.0.tmp-99999-1"), b"half a payload")
        .expect("leave debris from a killed writer");
    std::fs::write(outbox.join("job-orphan.7"), b"an abandoned payload")
        .expect("leave an orphaned payload from an older generation");

    let payload = br#"{"conclusion":"success"}"#;
    velnor_runner::node::cleanup::write_outbox(root.path(), "job-fresh", 1, payload)
        .expect("debris must not block a fresh publish");
    assert_eq!(
        velnor_runner::node::cleanup::read_outbox(root.path(), "job-fresh", 1)
            .expect("read the fresh payload"),
        payload
    );

    // The debris is inert: it is not readable as this job's payload, and
    // removing this job's payload does not touch it.
    assert!(velnor_runner::node::cleanup::read_outbox(root.path(), "job-stale", 0).is_err());
    velnor_runner::node::cleanup::remove_outbox(root.path(), "job-fresh", 1)
        .expect("remove the fresh payload");
    assert!(outbox.join("job-orphan.7").exists());
}

#[test]
fn removing_a_payload_that_is_already_gone_is_not_an_error() {
    // Crash recovery re-runs cleanup steps it may already have completed. If
    // deleting an absent payload failed, every recovery would end in a spurious
    // error and the slot would never be released.
    let root = TempRoot::new("outbox-idempotent-remove");
    velnor_runner::node::cleanup::remove_outbox(root.path(), "job-never-existed", 1)
        .expect("removing an absent payload is a no-op");

    velnor_runner::node::cleanup::write_outbox(root.path(), "job-twice", 1, b"payload")
        .expect("stage a payload");
    velnor_runner::node::cleanup::remove_outbox(root.path(), "job-twice", 1)
        .expect("remove it once");
    velnor_runner::node::cleanup::remove_outbox(root.path(), "job-twice", 1)
        .expect("removing it again is still a no-op");
    assert!(fault_support::staged_outbox_files(&root.join("outbox")).is_empty());
}

//! Process-death faults: `SIGKILL` at each job lifecycle boundary.
//!
//! `node::complete` already proves these boundaries against an in-process
//! journal restart, which is the right unit test and a weaker claim than the
//! one production needs: a dropped `Journal` handle runs SQLite's own shutdown,
//! while `SIGKILL` runs nothing. These tests extend that coverage rather than
//! repeat it — a real child process reaches the boundary, is killed by signal 9
//! with no unwinding and no flush, and a fresh process then has to recover from
//! whatever actually reached the disk.
//!
//! The determinism comes from the ordering, not from timing: the child commits
//! the boundary's durable step *before* it publishes the marker the parent
//! waits on, and then blocks forever. Whenever the kill lands, the durable
//! state is already exactly at the boundary under test.
//!
//! The child body runs inside this same test binary, re-executed with
//! `VELNOR_FAULT_CRASH_AT` set, so the suite needs no extra binary target and
//! no new dependency.

mod fault_support;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use velnor_control::journal::{payload_checksum, Event, Journal};
use velnor_model::{ActorPhase, Generation, JobId};

use fault_support::{fixture_slot, job_phase, open_journal, outbox_row, prime_owned_job, TempRoot};

/// Environment variable naming the boundary a re-executed child must die at.
const CRASH_AT: &str = "VELNOR_FAULT_CRASH_AT";
/// Environment variable naming the state directory the child works in.
const STATE_DIR: &str = "VELNOR_FAULT_STATE_DIR";
/// The child's name for the fixture job, shared with the parent.
const CRASH_JOB: &str = "job-crash-1";
/// Payload the completion boundaries stage. Byte-identical in parent and child
/// so the parent can assert the checksum recovery must match.
const CRASH_PAYLOAD: &[u8] = br#"{"conclusion":"success","result":"succeeded"}"#;

/// The lifecycle boundaries this suite kills a process at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::enum_variant_names,
    reason = "every boundary is named for the durable step it follows"
)]
enum Boundary {
    /// The permit is consumed and the slot is assigned, but the job is not yet
    /// durably owned.
    AfterAcquireBeforeDurableMarker,
    /// The job produced its terminal result; the completion payload has not
    /// been staged.
    AfterTerminalResultBeforeOutbox,
    /// The one permitted terminal send is claimed; the HTTP request has not
    /// been made.
    AfterSendClaimBeforeRequest,
    /// The request reached the run service; the acknowledgement was never
    /// journaled. Journal-identical to the previous boundary by construction —
    /// which is the point: the node cannot tell them apart, so recovery must be
    /// safe for both.
    AfterRequestBeforeAck,
}

impl Boundary {
    const fn token(self) -> &'static str {
        match self {
            Self::AfterAcquireBeforeDurableMarker => "after-acquire",
            Self::AfterTerminalResultBeforeOutbox => "after-terminal-result",
            Self::AfterSendClaimBeforeRequest => "after-send-claim",
            Self::AfterRequestBeforeAck => "after-request",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        [
            Self::AfterAcquireBeforeDurableMarker,
            Self::AfterTerminalResultBeforeOutbox,
            Self::AfterSendClaimBeforeRequest,
            Self::AfterRequestBeforeAck,
        ]
        .into_iter()
        .find(|boundary| boundary.token() == token)
    }
}

fn marker_path(state_dir: &Path) -> PathBuf {
    state_dir.join("boundary-reached")
}

/// Re-execution entry point.
///
/// Under the ordinary suite this observes no `VELNOR_FAULT_CRASH_AT` and
/// returns, so it costs one no-op test. Under a parent-launched child it drives
/// the journal to the requested boundary, publishes the marker, and then blocks
/// so the parent's `SIGKILL` is the only thing that ever ends it.
#[test]
fn crash_child_boundary_driver() {
    let Ok(token) = std::env::var(CRASH_AT) else {
        return;
    };
    let boundary = Boundary::parse(&token).expect("parent named a known boundary");
    let state_dir = PathBuf::from(std::env::var(STATE_DIR).expect("parent named a state dir"));
    drive_to_boundary(boundary, &state_dir);

    std::fs::write(marker_path(&state_dir), boundary.token())
        .expect("publish the boundary marker after the durable step");

    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn drive_to_boundary(boundary: Boundary, state_dir: &Path) {
    let job_id = JobId(CRASH_JOB.into());
    let slot = fixture_slot();
    let mut journal = open_journal(state_dir);

    if boundary == Boundary::AfterAcquireBeforeDurableMarker {
        let generation = fault_support::prime_ready_slot(&mut journal, &slot);
        let assigned = journal
            .apply(Event::Assigned {
                slot_id: slot,
                job_id,
                generation,
            })
            .expect("apply Assigned");
        assert!(!assigned.rejected);
        return;
    }

    let generation = prime_owned_job(&mut journal, &slot, &job_id);
    velnor_runner::node::complete::record_terminal_result(
        &mut journal,
        &job_id,
        generation,
        "success",
    )
    .expect("record the terminal result");
    if boundary == Boundary::AfterTerminalResultBeforeOutbox {
        return;
    }

    velnor_runner::node::cleanup::write_outbox(state_dir, &job_id.0, generation.0, CRASH_PAYLOAD)
        .expect("stage the completion payload");
    let intended = journal
        .apply(Event::CompletionIntended {
            job_id: job_id.clone(),
            generation,
            payload_sha256: payload_checksum(CRASH_PAYLOAD),
        })
        .expect("apply CompletionIntended");
    assert!(!intended.rejected);
    let claimed = journal
        .apply(Event::CompletionSendStarted { job_id, generation })
        .expect("apply CompletionSendStarted");
    assert!(!claimed.rejected);
}

/// Spawn this binary as a child that drives the journal to `boundary`, wait
/// until it says it is there, then kill it with `SIGKILL`.
///
/// Returns the state directory the killed child left behind.
fn kill_at(boundary: Boundary, root: &TempRoot) {
    let exe = std::env::current_exe().expect("locate the test binary for re-execution");
    let mut child: Child = Command::new(exe)
        .args(["--exact", "crash_child_boundary_driver", "--nocapture"])
        .env(CRASH_AT, boundary.token())
        .env(STATE_DIR, root.path())
        .env("RUST_BACKTRACE", "0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the crash child");

    let marker = marker_path(root.path());
    // Liveness bound only. The marker is written strictly after the boundary's
    // durable step, so the assertion does not depend on when it appears.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "the crash child never reached the {} boundary",
            boundary.token()
        );
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "the crash child exited on its own with {status} instead of reaching the {} boundary",
                boundary.token()
            );
        }
        std::thread::yield_now();
    }

    // SIGKILL: no unwinding, no destructors, no SQLite shutdown.
    child.kill().expect("SIGKILL the crash child");
    let status = child.wait().expect("reap the crash child");
    assert!(
        !status.success(),
        "a killed child must not report success ({status})"
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(9),
            "the child must die by SIGKILL, not by an ordinary exit"
        );
    }
}

#[test]
fn sigkill_after_acquire_and_before_the_durable_marker_leaves_nothing_to_replay() {
    let root = TempRoot::new("crash-after-acquire");
    kill_at(Boundary::AfterAcquireBeforeDurableMarker, &root);

    let journal = open_journal(root.path());
    let job_id = JobId(CRASH_JOB.into());

    // Correct GitHub-visible result: none is possible, and none is invented.
    // The job was never durably owned, so nothing may claim it concluded.
    assert_eq!(
        journal
            .recorded_terminal_conclusion(&job_id, Generation::INITIAL)
            .expect("read the terminal conclusion"),
        None,
        "a job killed before its durable marker must not acquire a conclusion"
    );

    // No orphaned resources: no completion payload was staged, so nothing is
    // left for a recovery sweep to send.
    assert!(journal
        .pending_outbox()
        .expect("read the pending outbox")
        .is_empty());
    assert!(
        fault_support::staged_outbox_files(&root.join("outbox")).is_empty(),
        "no completion payload may exist for a job that was never owned"
    );

    // Recoverable durable state: the slot's assignment is visible, so the
    // controller can fence and reuse the generation rather than losing capacity.
    let state = journal
        .materialized_state()
        .expect("materialize fleet state");
    assert_eq!(
        state.slots.len(),
        1,
        "the slot record must survive the kill"
    );
    assert!(
        !state
            .jobs
            .iter()
            .any(|job| job.phase == ActorPhase::Running),
        "no job may be left looking like it is running"
    );
}

#[test]
fn sigkill_after_the_terminal_result_and_before_the_outbox_keeps_the_real_conclusion() {
    let root = TempRoot::new("crash-after-result");
    kill_at(Boundary::AfterTerminalResultBeforeOutbox, &root);

    let journal = open_journal(root.path());
    let job_id = JobId(CRASH_JOB.into());

    // The correct GitHub-visible result. Without the pre-committed terminal
    // result, recovery could only invent a failure for a job that succeeded.
    assert_eq!(
        journal
            .recorded_terminal_conclusion(&job_id, Generation::INITIAL)
            .expect("read the terminal conclusion")
            .as_deref(),
        Some("success"),
        "a green job killed before its outbox write must stay green"
    );

    // No orphaned resources: nothing was staged, so nothing leaks.
    assert!(
        outbox_row(&journal, &job_id).is_none(),
        "no outbox row may exist before the payload is staged"
    );
    assert!(fault_support::staged_outbox_files(&root.join("outbox")).is_empty());

    // Recoverable: the job is still owned, so a replacement worker can build
    // and send the completion.
    assert_eq!(
        job_phase(&journal, &job_id),
        Some(ActorPhase::Completing),
        "recording the terminal result moves the job into Completing, which is \
         where a replacement worker picks the completion up"
    );
}

#[test]
fn sigkill_after_the_send_claim_and_before_the_request_replays_under_the_same_claim() {
    let root = TempRoot::new("crash-after-claim");
    kill_at(Boundary::AfterSendClaimBeforeRequest, &root);

    let mut journal = open_journal(root.path());
    let job_id = JobId(CRASH_JOB.into());
    let generation = Generation::INITIAL;

    // Recoverable durable state: the exact bytes the killed worker intended to
    // send survive, checksum-matched to the journal row.
    let row = outbox_row(&journal, &job_id).expect("the claimed completion row survives SIGKILL");
    assert!(row.intended);
    assert!(row.send_started, "the send claim must survive SIGKILL");
    assert!(!row.remote_acked);
    let durable = velnor_runner::node::cleanup::read_outbox(root.path(), &job_id.0, generation.0)
        .expect("read the staged payload back after SIGKILL");
    assert_eq!(durable, CRASH_PAYLOAD);
    assert_eq!(payload_checksum(&durable), row.payload_sha256);

    // Bounded behavior: a replacement worker cannot manufacture a second
    // terminal send, no matter how many times it retries.
    for _ in 0..3 {
        let second = journal
            .apply(Event::CompletionSendStarted {
                job_id: job_id.clone(),
                generation,
            })
            .expect("apply a replayed send claim");
        assert!(
            second.rejected,
            "recovery must reuse the surviving claim, never take a second one"
        );
    }

    // No cross-job contamination: the crashed job's payload is the only one
    // staged, and it is addressed by job id and generation.
    let staged = fault_support::staged_outbox_files(&root.join("outbox"));
    assert_eq!(staged.len(), 1, "exactly one payload is staged: {staged:?}");
    assert!(
        staged[0]
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(CRASH_JOB)),
        "the staged payload must be addressed by job id: {staged:?}"
    );
}

#[test]
fn sigkill_after_the_request_and_before_the_ack_terminalizes_without_a_second_send() {
    let root = TempRoot::new("crash-after-request");
    kill_at(Boundary::AfterRequestBeforeAck, &root);

    let mut journal = open_journal(root.path());
    let job_id = JobId(CRASH_JOB.into());
    let generation = Generation::INITIAL;

    // The node cannot know the request was delivered, so the durable state it
    // wakes up in is identical to the pre-request boundary. That is what makes
    // the remote's "I already have this" answer the only safe resolution.
    let row = outbox_row(&journal, &job_id).expect("the claimed completion row survives SIGKILL");
    assert!(row.send_started && !row.remote_acked);

    let observed = journal
        .apply(Event::RemoteObservedTerminal {
            job_id: job_id.clone(),
            generation,
        })
        .expect("apply the remote's terminal observation");
    assert!(
        !observed.rejected,
        "a replay that finds the job already terminal must be able to record it"
    );
    assert!(journal
        .has_remote_terminal_ack(&job_id, generation)
        .expect("read the terminal acknowledgement"));

    velnor_runner::node::cleanup::remove_outbox(root.path(), &job_id.0, generation.0)
        .expect("remove the acknowledged payload");

    // No orphaned resources once the disposition is known.
    assert!(journal
        .pending_outbox()
        .expect("read the pending outbox")
        .is_empty());
    assert!(fault_support::staged_outbox_files(&root.join("outbox")).is_empty());
}

#[test]
fn a_killed_worker_leaves_a_journal_a_fresh_process_can_still_open_and_extend() {
    // The failure mode this guards is not "the row is wrong" but "the database
    // is unusable": SIGKILL during a SQLite write leaves a hot WAL, and a
    // runner that cannot reopen its own journal has lost every slot on the host.
    let root = TempRoot::new("crash-journal-usable");
    kill_at(Boundary::AfterSendClaimBeforeRequest, &root);

    let mut journal: Journal = open_journal(root.path());
    let job_id = JobId(CRASH_JOB.into());
    let outcome = journal
        .apply(Event::CompletionAttemptFailed {
            job_id: job_id.clone(),
            generation: Generation::INITIAL,
            permanent: false,
        })
        .expect("a journal that survived SIGKILL must still accept writes");
    assert!(!outcome.rejected);

    let row = outbox_row(&journal, &job_id).expect("the row is still readable");
    assert!(
        row.attempts >= 1,
        "the durable attempt budget must have been charged, got {}",
        row.attempts
    );
}

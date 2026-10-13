//! Cancellation-race faults.
//!
//! Cancellation is the one fault the runner is *asked* to handle, and it is the
//! one that races by construction: the message arrives from a broker thread
//! while a step thread is mutating job state. The tests here pin the five races
//! that matter — during a step, at the edge of step completion, during a post
//! action, twice, and simultaneously with completion.
//!
//! Determinism comes from [`JobCancellation::recording`], which walks the real
//! ladder and runs registered hooks but signals nothing, and from driving the
//! fan-out synchronously wherever an exact invocation count is asserted. The
//! one test that must observe the *asynchronous* ladder waits on a condition
//! that is guaranteed to become true, never on a duration that decides the
//! assertion.

mod fault_support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use velnor_control::journal::Event;
use velnor_model::{ActorPhase, JobId};
use velnor_runner::execution::{
    forced_kill_delay, terminate, CancelLevel, CancelReason, ContainerRole, JobCancellation,
    TerminationLadder, TerminationTarget,
};

use fault_support::{fixture_slot, open_journal, prime_owned_job, TempRoot};

/// A hook target that records every level it was fired at.
fn recording_hook(label: &str, log: &Arc<Mutex<Vec<CancelLevel>>>) -> TerminationTarget {
    let log = Arc::clone(log);
    TerminationTarget::Hook {
        label: label.to_owned(),
        run: Arc::new(move |level| {
            log.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(level);
            Ok(())
        }),
    }
}

/// A hook that always refuses, so the suite can prove a *failing* teardown is
/// still reported rather than swallowed.
fn failing_hook(label: &str, detail: &'static str) -> TerminationTarget {
    TerminationTarget::Hook {
        label: label.to_owned(),
        run: Arc::new(move |_| Err(detail.to_owned())),
    }
}

/// Wait until `condition` holds. The condition is guaranteed to become true, so
/// the bound only distinguishes "slow host" from "hung forever" — it never
/// decides an assertion.
fn await_condition(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::yield_now();
    }
    panic!("{what} never happened within the liveness bound");
}

#[test]
fn cancel_during_a_step_terminates_the_step_work_once_and_keeps_the_registry_intact() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let token = JobCancellation::recording(Some(Duration::from_secs(600)));

    let _step_work = token.register(recording_hook("step-process-tree", &log));
    let _job_container = token.register(TerminationTarget::Container {
        name: "velnor-job-1".into(),
        role: ContainerRole::Job,
    });
    let _service = token.register(TerminationTarget::Container {
        name: "velnor-job-1-postgres".into(),
        role: ContainerRole::Service,
    });

    // `force` escalates without starting the asynchronous ladder thread, so the
    // fan-out under test is the synchronous one and the invocation counts below
    // are exact rather than sampled.
    token.force();
    assert_eq!(token.level(), CancelLevel::Forced);
    token.fan_out_once();

    let mut terminated: Vec<String> = token
        .outcomes()
        .into_iter()
        .map(|outcome| outcome.target)
        .collect();
    assert!(
        terminated.contains(&"hook:step-process-tree".to_owned()),
        "the cancelled step's own process tree must be terminated, got {terminated:?}"
    );
    let before_dedup = terminated.len();
    terminated.sort();
    terminated.dedup();
    assert_eq!(
        terminated.len(),
        before_dedup,
        "one cancellation must not signal the same target twice, got {terminated:?}"
    );
    assert_eq!(
        log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1,
        "the step hook must fire exactly once for one cancellation"
    );

    // No cross-contamination: the registry still names every live target, so
    // the forced pass cannot lose one.
    let mut keys = token.target_keys();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "container:velnor-job-1".to_owned(),
            "container:velnor-job-1-postgres".to_owned(),
            "hook:step-process-tree".to_owned(),
        ]
    );
}

#[test]
fn a_target_registered_after_cancellation_is_terminated_rather_than_leaked() {
    // The step-completion edge: cancellation lands between the decision to
    // spawn and the registration of what was spawned. Losing that race must
    // not leak a container.
    let log = Arc::new(Mutex::new(Vec::new()));
    let token = JobCancellation::recording(Some(Duration::from_secs(600)));

    // `force` escalates without starting the asynchronous ladder, so the only
    // fan-out in this test is the synchronous one `register` performs.
    token.force();
    assert_eq!(token.level(), CancelLevel::Forced);
    assert!(token.outcomes().is_empty());

    let _late = token.register(recording_hook("late-sidecar", &log));

    let fired = log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(
        fired.len(),
        1,
        "a target registered after cancellation must be terminated exactly once"
    );
    assert_eq!(
        token.outcomes().len(),
        1,
        "the late target's outcome must be recorded for forensics"
    );
    assert_eq!(token.outcomes()[0].target, "hook:late-sidecar");
}

#[test]
fn a_post_action_runs_under_a_token_its_parent_cannot_cancel() {
    let parent = JobCancellation::recording(Some(Duration::from_secs(600)));
    let post = parent.unlinked();
    let log = Arc::new(Mutex::new(Vec::new()));
    let _post_work = post.register(recording_hook("post-step", &log));

    parent.force();
    parent.fan_out_once();

    assert_eq!(post.level(), CancelLevel::None);
    assert!(!post.is_cancelled());
    assert!(
        log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "a cancelled parent must not reach into the post step's own targets"
    );
    assert!(
        parent.outcomes().is_empty(),
        "the parent has no targets of its own and must not adopt the post token's"
    );
}

#[test]
fn cancelling_twice_keeps_the_first_reason_and_starts_no_second_fan_out() {
    let token = JobCancellation::recording(Some(Duration::from_secs(600)));
    let log = Arc::new(Mutex::new(Vec::new()));
    let _work = token.register(recording_hook("step-process-tree", &log));

    assert!(token.request(CancelReason::ServerRequested));
    assert!(
        !token.request(CancelReason::JobTimeout),
        "a repeated cancellation must report that it did not cancel the job"
    );
    assert!(!token.request(CancelReason::DaemonShutdown));

    assert_eq!(
        token.reason(),
        Some(CancelReason::ServerRequested),
        "the first reason is the one the job is failed with"
    );
    assert_eq!(token.level(), CancelLevel::Requested);

    // A cancel storm must not become a signal storm: only the winning request
    // starts a fan-out, so the hook fires once no matter how many cancellations
    // arrive. Waiting for the first outcome is a liveness bound, not a timing
    // assumption — the later assertion is on an exact count that repeated
    // requests could only inflate.
    await_condition("the cancellation fan-out", || {
        !log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    });
    assert_eq!(
        log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1,
        "three cancellations must produce one fan-out"
    );
    assert_eq!(token.outcomes().len(), 1);
}

#[test]
fn concurrent_cancellations_elect_exactly_one_winner() {
    let token = JobCancellation::recording(Some(Duration::from_secs(600)));
    let winners = Arc::new(AtomicUsize::new(0));
    let reasons = [
        CancelReason::ServerRequested,
        CancelReason::JobTimeout,
        CancelReason::RegistrationLost,
        CancelReason::DaemonShutdown,
    ];

    let handles: Vec<_> = reasons
        .into_iter()
        .map(|reason| {
            let token = token.clone();
            let winners = Arc::clone(&winners);
            std::thread::spawn(move || {
                if token.request(reason) {
                    winners.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("cancellation thread panicked");
    }

    assert_eq!(
        winners.load(Ordering::SeqCst),
        1,
        "exactly one concurrent cancellation may own the job's reason"
    );
    assert!(token.reason().is_some());
    assert_eq!(token.level(), CancelLevel::Requested);
}

#[test]
fn cancellation_racing_completion_cannot_produce_a_second_terminal_send() {
    // The worst race in the system: GitHub cancels while the job is already
    // walking the completion path. Cancellation is an in-process signal; the
    // terminal send is a durable single-shot claim. The two must not compose
    // into two terminal sends.
    let root = TempRoot::new("cancel-vs-completion");
    let job_id = JobId("job-cancel-race".into());
    let slot = fixture_slot();
    let mut journal = open_journal(root.path());
    let generation = prime_owned_job(&mut journal, &slot, &job_id);

    let token = JobCancellation::recording(Some(Duration::from_secs(600)));

    velnor_runner::node::complete::record_terminal_result(
        &mut journal,
        &job_id,
        generation,
        "cancelled",
    )
    .expect("record the terminal result the completion will carry");

    let payload = br#"{"conclusion":"cancelled"}"#;
    velnor_runner::node::cleanup::write_outbox(root.path(), &job_id.0, generation.0, payload)
        .expect("stage the completion payload");
    let intended = journal
        .apply(Event::CompletionIntended {
            job_id: job_id.clone(),
            generation,
            payload_sha256: velnor_control::journal::payload_checksum(payload),
        })
        .expect("apply completion intent");
    assert!(!intended.rejected);

    // Cancellation arrives here, exactly between intent and the send claim.
    assert!(token.request(CancelReason::ServerRequested));

    let first_claim = journal
        .apply(Event::CompletionSendStarted {
            job_id: job_id.clone(),
            generation,
        })
        .expect("apply the first send claim");
    assert!(!first_claim.rejected, "the first send claim must succeed");

    let second_claim = journal
        .apply(Event::CompletionSendStarted {
            job_id: job_id.clone(),
            generation,
        })
        .expect("apply the second send claim");
    assert!(
        second_claim.rejected,
        "a cancellation-driven retry must never win a second terminal send"
    );

    // Recoverable durable state: while the completion is still in flight the
    // conclusion the job actually reached is readable, so a crashed worker's
    // replacement replays "cancelled" rather than inventing a failure.
    let conclusion = journal
        .recorded_terminal_conclusion(&job_id, generation)
        .expect("read the recorded terminal conclusion");
    assert_eq!(conclusion.as_deref(), Some("cancelled"));

    let acked = journal
        .apply(Event::RemoteAcked {
            job_id: job_id.clone(),
            generation,
        })
        .expect("apply the remote acknowledgement");
    assert!(!acked.rejected);

    assert!(
        journal
            .has_remote_terminal_ack(&job_id, generation)
            .expect("read the terminal acknowledgement"),
        "the cancelled job must still be terminal at the remote"
    );

    velnor_runner::node::cleanup::remove_outbox(root.path(), &job_id.0, generation.0)
        .expect("remove the acknowledged payload");

    // No orphaned resources: nothing pending is left behind for a recovery
    // sweep to re-send.
    assert!(
        journal
            .pending_outbox()
            .expect("read the pending outbox")
            .is_empty(),
        "an acknowledged completion must leave no pending outbox row"
    );
    assert_ne!(
        fault_support::job_phase(&journal, &job_id),
        Some(ActorPhase::Running),
        "a completed job must not still look running after a cancellation race"
    );
}

#[test]
fn the_termination_ladder_is_bounded_and_reports_a_target_it_could_not_stop() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let started = Instant::now();
    let outcome = terminate(
        &failing_hook("buildkit-solve", "buildx refused to abort the solve"),
        deadline,
    );
    let elapsed = started.elapsed();

    assert!(!outcome.gone);
    fault_support::assert_actionable_diagnostic(
        outcome
            .error
            .as_deref()
            .expect("a refused hook reports why"),
        &["buildx"],
    );
    assert_eq!(outcome.target, "hook:buildkit-solve");
    assert!(
        elapsed < Duration::from_secs(5),
        "a refusing hook must fail fast rather than burn the signal ladder's grace periods, took {elapsed:?}"
    );
}

#[test]
fn the_forced_kill_deadline_is_bounded_below_by_the_upstream_floor() {
    // An operator-supplied cancel timeout can be absurd in both directions; the
    // escalation deadline must stay inside the upstream window regardless, or a
    // cancelled job becomes an unbounded one.
    assert_eq!(
        forced_kill_delay(None),
        Duration::from_secs(45),
        "the default cancel window is the upstream 60s floor minus the 15s hard-kill lead"
    );
    assert_eq!(
        forced_kill_delay(Some(Duration::from_secs(1))),
        Duration::from_secs(45)
    );
    assert_eq!(
        forced_kill_delay(Some(Duration::ZERO)),
        Duration::from_secs(45)
    );
    assert_eq!(
        forced_kill_delay(Some(Duration::from_secs(600))),
        Duration::from_secs(585)
    );

    // The ladder itself is finite in every step.
    let ladder = TerminationLadder::default();
    assert!(ladder.sigint_grace > Duration::ZERO);
    assert!(ladder.sigterm_grace > Duration::ZERO);
    assert!(ladder.sigint_grace + ladder.sigterm_grace < Duration::from_secs(30));
}

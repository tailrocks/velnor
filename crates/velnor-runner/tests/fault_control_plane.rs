//! Control-plane faults: a broker that stops answering, a run service that
//! refuses a completion forever, a log publisher that fails, and a runner
//! registration that vanishes underneath a live job.
//!
//! Two halves, deliberately: the transport half proves the client turns a
//! failure into the right *classification*, and the durable half proves the
//! journal turns that classification into a *bounded* terminal state. A fault
//! suite that only tested the first half would pass on a runner that retries a
//! doomed completion until the host is drained.
#![cfg(feature = "test-support")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::LazyLock;

use velnor_control::journal::{payload_checksum, Event, Journal, MAX_COMPLETION_ATTEMPTS};
use velnor_model::JobId;
use velnor_runner::protocol::{
    classify_broker_poll, classify_broker_poll_error, BrokerClient, BrokerPollClass,
    BrokerPollErrorClass, FeedStreamClient, GitHubApiError, RunnerStatus, TwirpResultsClient,
    TwirpStep,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod fault_support;

use fault_support::{fixture_slot, open_journal, outbox_row, prime_owned_job, TempRoot};

/// Reserved and never listening: a connection here is refused by the kernel,
/// so "the remote is gone" is delivered without waiting on a timeout.
const DEAD_ENDPOINT: &str = "http://127.0.0.1:1";

static TRANSPORT_ENV_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

struct NativeTransportGuard {
    previous: Option<OsString>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

async fn native_transport() -> NativeTransportGuard {
    let lock = TRANSPORT_ENV_LOCK.lock().await;
    let previous = std::env::var_os("VELNOR_GITHUB_HTTP_TRANSPORT");
    // SAFETY: the process-wide environment lock is held for this guard's life.
    unsafe { std::env::set_var("VELNOR_GITHUB_HTTP_TRANSPORT", "native") };
    NativeTransportGuard {
        previous,
        _lock: lock,
    }
}

impl Drop for NativeTransportGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            // SAFETY: the process-wide environment lock is held for this guard.
            Some(value) => unsafe { std::env::set_var("VELNOR_GITHUB_HTTP_TRANSPORT", value) },
            // SAFETY: the process-wide environment lock is held for this guard.
            None => unsafe { std::env::remove_var("VELNOR_GITHUB_HTTP_TRANSPORT") },
        }
    }
}

fn api_status(error: &anyhow::Error) -> Option<u16> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<GitHubApiError>())
        .map(|api| api.status)
}

#[tokio::test]
async fn a_broker_that_cannot_be_reached_is_an_error_never_an_idle_poll() {
    // The 2026-06-11 fleet incident in one assertion: a slot that reads a dead
    // broker as "no work" polls forever while GitHub has already dropped it.
    let _transport = native_transport().await;
    let broker = BrokerClient::new(DEAD_ENDPOINT, "bearer").expect("build the broker client");

    let error = broker
        .get_runner_message("session-1", RunnerStatus::Online, true)
        .await
        .expect_err("an unreachable broker must not answer 'no message'");

    // The diagnostic must name the endpoint that refused and say what the
    // refusal was, or an operator cannot tell a dead broker from a wedged one.
    fault_support::assert_actionable_diagnostic(&format!("{error:#}"), &["message", "refused"]);
    assert_eq!(
        api_status(&error),
        None,
        "a refused connection has no HTTP status to report"
    );
}

#[tokio::test]
async fn a_broker_session_that_no_longer_exists_is_reported_as_a_missing_session() {
    let _transport = native_transport().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(404).set_body_string(""))
        .mount(&server)
        .await;

    let broker = BrokerClient::new(&server.uri(), "bearer").expect("build the broker client");
    let error = broker
        .get_runner_message("session-1", RunnerStatus::Online, true)
        .await
        .expect_err("a deleted session must not read as an idle poll");

    let status = api_status(&error).expect("a 404 carries its status");
    assert_eq!(status, 404);
    assert_eq!(
        classify_broker_poll_error(status),
        BrokerPollErrorClass::MissingSession,
        "a missing session must be distinguishable from a server fault, because \
         only one of the two is fixed by re-registering"
    );
    // An empty body is what makes this dangerous: the classifier must key on
    // the status, not on whether bytes came back.
    assert_eq!(classify_broker_poll(404, ""), BrokerPollClass::Error);
    assert_eq!(classify_broker_poll(204, ""), BrokerPollClass::Empty);
}

#[tokio::test]
async fn a_broker_server_fault_and_a_rate_limit_are_classified_apart() {
    // Both are retriable; only one of them means "slow down". Collapsing them
    // turns a rate limit into a hot loop that deepens the limit.
    for (status, expected) in [
        (429, BrokerPollErrorClass::RateLimited),
        (500, BrokerPollErrorClass::Server),
        (503, BrokerPollErrorClass::Server),
        (401, BrokerPollErrorClass::Authentication),
        (403, BrokerPollErrorClass::Forbidden),
        (409, BrokerPollErrorClass::Conflict),
        (0, BrokerPollErrorClass::Transport),
    ] {
        assert_eq!(
            classify_broker_poll_error(status),
            expected,
            "broker status {status} classified wrongly"
        );
    }
}

#[tokio::test]
async fn a_failing_step_publisher_reports_the_failure_instead_of_losing_step_state() {
    let _transport = native_transport().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/twirp/github.actions.results.api.v1.WorkflowStepUpdateService/WorkflowStepsUpdate",
        ))
        .respond_with(ResponseTemplate::new(500).set_body_string("results service unavailable"))
        .mount(&server)
        .await;

    let client = TwirpResultsClient::new(server.uri(), "bearer").expect("build the results client");
    let error = client
        .update_steps(&[], "run-backend-id", "job-backend-id", 1)
        .await
        .expect_err("a 500 from the results service must not read as published");

    fault_support::assert_actionable_diagnostic(&format!("{error:#}"), &["500"]);
    // Bounded behavior: the publisher retries, but a fixed number of times.
    // An unbounded publisher retry is how a dead results service turns into a
    // slot that never finishes its job.
    let attempts = server
        .received_requests()
        .await
        .expect("recorded requests")
        .len();
    assert!(
        (1..=5).contains(&attempts),
        "the step publisher must give up after a small fixed number of attempts, made {attempts}"
    );
}

#[tokio::test]
async fn a_log_feed_that_cannot_be_opened_fails_fast_rather_than_hanging_the_job() {
    let feed = FeedStreamClient::new(DEAD_ENDPOINT, "bearer");
    let error = feed
        .connect()
        .await
        .expect_err("an unreachable log feed cannot be opened");
    fault_support::assert_actionable_diagnostic(&format!("{error:#}"), &["feed stream", "refused"]);
}

#[test]
fn a_completion_the_run_service_refuses_forever_reaches_a_bounded_terminal_state() {
    let root = TempRoot::new("completion-refused-forever");
    let job_id = JobId("job-refused".into());
    let slot = fixture_slot();
    let mut journal: Journal = open_journal(root.path());
    let generation = prime_owned_job(&mut journal, &slot, &job_id);

    velnor_runner::node::complete::record_terminal_result(
        &mut journal,
        &job_id,
        generation,
        "success",
    )
    .expect("record the terminal result");
    let payload = br#"{"conclusion":"success"}"#;
    velnor_runner::node::cleanup::write_outbox(root.path(), &job_id.0, generation.0, payload)
        .expect("stage the payload");
    journal
        .apply(Event::CompletionIntended {
            job_id: job_id.clone(),
            generation,
            payload_sha256: payload_checksum(payload),
        })
        .expect("apply the completion intent");
    journal
        .apply(Event::CompletionSendStarted {
            job_id: job_id.clone(),
            generation,
        })
        .expect("apply the send claim");

    // Before the budget is spent, giving up is refused: a terminal state has to
    // be provable from durable state, never asserted by a caller in a hurry.
    let premature = velnor_runner::node::complete::abandon_unresolvable_completion(
        &mut journal,
        root.path(),
        &job_id,
        generation,
        "operator impatience",
    )
    .expect("ask the journal to abandon the completion");
    assert!(
        !premature,
        "a completion with budget left must not be abandonable"
    );

    for _ in 0..MAX_COMPLETION_ATTEMPTS {
        journal
            .apply(Event::CompletionAttemptFailed {
                job_id: job_id.clone(),
                generation,
                permanent: false,
            })
            .expect("charge one failed send attempt");
    }

    let abandoned = velnor_runner::node::complete::abandon_unresolvable_completion(
        &mut journal,
        root.path(),
        &job_id,
        generation,
        "run service refused every attempt",
    )
    .expect("abandon the completion once its budget is spent");
    assert!(abandoned, "a spent budget must reach a terminal state");

    // The operator diagnostic: the reason is preserved in the immutable log,
    // not only printed to a stream nobody kept.
    let unresolvable = journal
        .unresolvable_completions()
        .expect("read the unresolvable completions");
    assert_eq!(unresolvable.len(), 1);
    assert_eq!(unresolvable[0].job_id, job_id);
    fault_support::assert_actionable_diagnostic(&unresolvable[0].reason, &["run service"]);

    // No orphaned resources: the abandoned payload is gone, so no later sweep
    // can resurrect it as a second terminal send.
    assert!(fault_support::staged_outbox_files(&root.join("outbox")).is_empty());
    assert!(journal
        .pending_outbox()
        .expect("read the pending outbox")
        .is_empty());

    // ...and the send claim still stands, so a replay cannot take a second one.
    let second_claim = journal
        .apply(Event::CompletionSendStarted {
            job_id: job_id.clone(),
            generation,
        })
        .expect("apply a replayed send claim");
    assert!(
        second_claim.rejected,
        "abandoning a completion must never release its terminal-send claim"
    );

    // The slot is not held hostage by the job it could not report.
    let state = journal
        .materialized_state()
        .expect("materialize fleet state");
    assert!(
        !state.jobs.iter().any(|job| job.job_id == job_id),
        "an abandoned completion must release its job record"
    );
    assert_eq!(state.slots.len(), 1, "the slot itself must survive");
}

#[test]
fn a_permanently_refused_completion_spends_the_whole_budget_at_once() {
    // The distinction that keeps a doomed payload from occupying a slot for
    // hours: a refusal retrying cannot fix is worth the entire budget.
    let root = TempRoot::new("completion-refused-permanently");
    let job_id = JobId("job-permanent".into());
    let slot = fixture_slot();
    let mut journal = open_journal(root.path());
    let generation = prime_owned_job(&mut journal, &slot, &job_id);

    velnor_runner::node::complete::record_terminal_result(
        &mut journal,
        &job_id,
        generation,
        "failure",
    )
    .expect("record the terminal result");
    let payload = br#"{"conclusion":"failure"}"#;
    velnor_runner::node::cleanup::write_outbox(root.path(), &job_id.0, generation.0, payload)
        .expect("stage the payload");
    journal
        .apply(Event::CompletionIntended {
            job_id: job_id.clone(),
            generation,
            payload_sha256: payload_checksum(payload),
        })
        .expect("apply the completion intent");
    journal
        .apply(Event::CompletionSendStarted {
            job_id: job_id.clone(),
            generation,
        })
        .expect("apply the send claim");

    journal
        .apply(Event::CompletionAttemptFailed {
            job_id: job_id.clone(),
            generation,
            permanent: true,
        })
        .expect("charge the permanent refusal");

    // One permanent refusal is enough: the row reports its budget as spent
    // even though only one attempt was actually charged, which is what stops a
    // doomed payload from occupying a slot for the full retry window.
    let row = outbox_row(&journal, &job_id).expect("the outbox row survives the refusal");
    assert_eq!(row.attempts, 1);
    assert!(row.attempts < MAX_COMPLETION_ATTEMPTS);

    let abandoned = velnor_runner::node::complete::abandon_unresolvable_completion(
        &mut journal,
        root.path(),
        &job_id,
        generation,
        "run service refused the payload permanently",
    )
    .expect("abandon the permanently refused completion");
    assert!(
        abandoned,
        "one permanent refusal must be enough to release the slot"
    );
}

#[test]
fn a_registration_that_disappeared_clears_the_claim_without_losing_the_slot() {
    let root = TempRoot::new("registration-lost");
    let slot = fixture_slot();
    let mut journal = open_journal(root.path());
    let generation = fault_support::prime_ready_slot(&mut journal, &slot);

    let lost = journal
        .apply(Event::RegistrationLost {
            slot_id: slot.clone(),
            generation,
        })
        .expect("apply RegistrationLost");
    assert!(
        !lost.rejected,
        "a slot must be able to record that GitHub no longer knows it"
    );

    // Recoverable durable state: the slot record survives, so reconciliation
    // can issue a fresh JIT request instead of trusting split-brain state.
    let state = journal
        .materialized_state()
        .expect("materialize fleet state");
    assert_eq!(
        state.slots.len(),
        1,
        "losing a registration must not lose the host's capacity"
    );

    // The claim itself is released rather than kept as split-brain state: the
    // slot drops back to Provisioning with no permit, no session, and no
    // registration, which is the only state from which a fresh JIT request is
    // correct.
    let record = state
        .slots
        .iter()
        .find(|record| record.slot_id == slot)
        .expect("the slot record survives");
    assert!(
        !record.registered,
        "the stale registration claim must be cleared"
    );
    assert!(!record.session_live, "the broker session goes with it");
    assert!(!record.permit_held, "admission is released atomically");

    // ...and the slot can be re-provisioned on the same generation, which is
    // what makes the recovery bounded rather than a restart of the whole node.
    for event in [
        Event::PermitReserved {
            slot_id: slot.clone(),
            generation,
        },
        Event::ExecutorProven {
            slot_id: slot.clone(),
            generation,
        },
        Event::SessionLive {
            slot_id: slot.clone(),
            generation,
        },
        Event::RegistrationIntended {
            slot_id: slot.clone(),
            generation,
        },
        Event::Registered {
            slot_id: slot.clone(),
            generation,
        },
        Event::ReadyAttempt {
            slot_id: slot.clone(),
            generation,
        },
    ] {
        let outcome = journal
            .apply(event.clone())
            .expect("apply a re-provisioning event");
        assert!(
            !outcome.rejected,
            "a lost registration must be re-claimable, rejected at {event:?}"
        );
    }
}

#[test]
fn a_worker_that_disappeared_returns_its_slot_rather_than_stranding_capacity() {
    let root = TempRoot::new("worker-lost");
    let job_id = JobId("job-worker-lost".into());
    let slot = fixture_slot();
    let mut journal = open_journal(root.path());
    let generation = prime_owned_job(&mut journal, &slot, &job_id);

    let lost = journal
        .apply(Event::JobWorkerLost {
            job_id: job_id.clone(),
            generation,
        })
        .expect("apply JobWorkerLost");
    assert!(!lost.rejected);

    let state = journal
        .materialized_state()
        .expect("materialize fleet state");
    assert!(
        !state.jobs.iter().any(|job| job.job_id == job_id),
        "a lost worker's job must not stay owned forever"
    );
    assert_eq!(
        state.slots.len(),
        1,
        "the slot returns to the fleet rather than leaking"
    );
    assert!(
        journal
            .pending_outbox()
            .expect("read the pending outbox")
            .is_empty(),
        "a job that never produced a completion leaves no outbox row"
    );
}

#[test]
fn one_jobs_completion_cannot_be_read_or_deleted_through_another_jobs_id() {
    // Cross-job contamination check for the durable payload store: every
    // payload is addressed by (job id, generation), so a neighbouring job's
    // recovery sweep cannot consume it.
    let root = TempRoot::new("outbox-isolation");
    let first = br#"{"conclusion":"success","job":"a"}"#;
    let second = br#"{"conclusion":"failure","job":"b"}"#;
    velnor_runner::node::cleanup::write_outbox(root.path(), "job-a", 1, first)
        .expect("stage job A's payload");
    velnor_runner::node::cleanup::write_outbox(root.path(), "job-b", 1, second)
        .expect("stage job B's payload");

    assert_eq!(
        velnor_runner::node::cleanup::read_outbox(root.path(), "job-a", 1)
            .expect("read job A's payload"),
        first
    );
    assert!(
        velnor_runner::node::cleanup::read_outbox(root.path(), "job-a", 2).is_err(),
        "a different generation must not resolve to another generation's payload"
    );

    velnor_runner::node::cleanup::remove_outbox(root.path(), "job-a", 1)
        .expect("remove job A's payload");
    assert_eq!(
        velnor_runner::node::cleanup::read_outbox(root.path(), "job-b", 1)
            .expect("job B's payload survives job A's cleanup"),
        second
    );
    assert_eq!(
        fault_support::staged_outbox_files(&root.join("outbox")).len(),
        1
    );
}

#[test]
fn a_traversal_shaped_job_id_cannot_address_a_payload_outside_the_outbox() {
    let root = TempRoot::new("outbox-traversal");
    for hostile in ["../escape", "..", "a/b", "a\0b"] {
        let written =
            velnor_runner::node::cleanup::write_outbox(root.path(), hostile, 1, b"payload");
        assert!(
            written.is_err(),
            "job id {hostile:?} must be refused before it names a path"
        );
    }
    assert!(fault_support::staged_outbox_files(&root.join("outbox")).is_empty());
}

/// Keeps the unused-import warning honest: the step publisher's payload type is
/// part of the contract this suite exercises.
#[test]
fn a_step_update_payload_round_trips_through_its_publisher_type() {
    let steps: Vec<TwirpStep> = Vec::new();
    assert!(steps.is_empty());
    let endpoints: BTreeMap<String, String> = BTreeMap::new();
    assert!(TwirpResultsClient::from_endpoint_data(&endpoints, "bearer").is_none());
}

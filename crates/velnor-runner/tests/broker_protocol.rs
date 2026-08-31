#![cfg(feature = "test-support")]

use std::{
    collections::BTreeMap,
    ffi::OsString,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, LazyLock,
    },
    time::Duration,
};

use anyhow::Error;
use reqwest::StatusCode;
use serde_json::{json, Value};
use velnor_runner::protocol::{
    AcquireJobOutcome, BrokerClient, BrokerPoll, GitHubApiError, RunServiceClient,
    RunServiceCompleteJob, RunnerStatus, TaskAgentMessage, TaskAgentSession, TaskResult,
    RUNNER_JOB_REQUEST, RUNNER_VERSION,
};
use wiremock::{
    matchers::{header, method, path, query_param},
    Mock, MockServer, Request, ResponseTemplate,
};

const TOKEN: &str = "test-token";
const GITHUB_HTTP_TRANSPORT_ENV: &str = "VELNOR_GITHUB_HTTP_TRANSPORT";

static GITHUB_HTTP_TRANSPORT_ENV_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

struct NativeTransportGuard {
    previous: Option<OsString>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

async fn native_transport_guard() -> NativeTransportGuard {
    let lock = GITHUB_HTTP_TRANSPORT_ENV_LOCK.lock().await;
    let previous = std::env::var_os(GITHUB_HTTP_TRANSPORT_ENV);
    std::env::set_var(GITHUB_HTTP_TRANSPORT_ENV, "native");
    NativeTransportGuard {
        previous,
        _lock: lock,
    }
}

impl Drop for NativeTransportGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(GITHUB_HTTP_TRANSPORT_ENV, value),
            None => std::env::remove_var(GITHUB_HTTP_TRANSPORT_ENV),
        }
    }
}

#[tokio::test]
async fn broker_run_service_happy_path_acquires_and_completes_job() {
    let _transport_guard = native_transport_guard().await;
    let server = MockServer::start().await;
    let broker = BrokerClient::new(&format!("{}/broker", server.uri()), TOKEN).unwrap();
    let run_service = RunServiceClient::new(TOKEN).unwrap();
    let run_service_url = format!("{}/run/jobs/123", server.uri());

    Mock::given(method("POST"))
        .and(path("/broker/session"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "session-1",
            "ownerName": "host (PID: 1)",
            "agent": {
                "id": 42,
                "name": "velnor-test",
                "version": RUNNER_VERSION,
                "osDescription": std::env::consts::OS
            },
            "useFipsEncryption": false
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/broker/message"))
        .and(query_param("sessionId", "session-1"))
        .and(query_param("status", "Online"))
        .and(query_param("runnerVersion", RUNNER_VERSION))
        .and(query_param("os", std::env::consts::OS))
        .and(query_param("architecture", std::env::consts::ARCH))
        .and(query_param("disableUpdate", "true"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/broker/message"))
        .and(query_param("sessionId", "session-1"))
        .and(query_param("status", "Busy"))
        .and(query_param("disableUpdate", "false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(job_message(&run_service_url)))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/run/jobs/123/acquirejob"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "planId": "plan-1",
            "jobId": "job-1",
            "jobName": "test"
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/run/jobs/123/completejob"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let session = broker
        .create_session(&TaskAgentSession::new("host (PID: 1)", 42, "velnor-test"))
        .await
        .unwrap();
    assert_eq!(session.session_id.as_deref(), Some("session-1"));

    let BrokerPoll {
        status,
        message: idle,
    } = broker
        .get_runner_message("session-1", RunnerStatus::Online, true)
        .await
        .unwrap();
    assert_eq!(status, 204);
    assert!(idle.is_none());

    let poll = broker
        .get_runner_message("session-1", RunnerStatus::Busy, false)
        .await
        .unwrap();
    let message = poll.message.expect("job message");
    assert_eq!(message.message_id, 99);
    assert_eq!(message.message_type, RUNNER_JOB_REQUEST);

    let acquired = run_service
        .acquire_job(
            &run_service_url,
            "broker-message",
            std::env::consts::OS,
            Some("42"),
        )
        .await
        .unwrap();
    let AcquireJobOutcome::Acquired(job) = acquired else {
        panic!("expected acquired job");
    };
    assert_eq!(job["planId"], "plan-1");
    assert_eq!(job["jobId"], "job-1");

    run_service
        .complete_job(&run_service_url, complete_job())
        .await
        .unwrap();

    let requests = recorded_requests(&server).await;
    let session_body = body_for(&requests, "POST", "/broker/session");
    assert_eq!(session_body["ownerName"], "host (PID: 1)");
    assert_eq!(session_body["agent"]["id"], 42);
    assert_eq!(session_body["agent"]["name"], "velnor-test");
    assert_eq!(session_body["useFipsEncryption"], false);

    let acquire_body = body_for(&requests, "POST", "/run/jobs/123/acquirejob");
    assert_eq!(acquire_body["jobMessageId"], "broker-message");
    assert_eq!(acquire_body["runnerOS"], std::env::consts::OS);
    assert_eq!(acquire_body["billingOwnerId"], "42");

    let complete_body = body_for(&requests, "POST", "/run/jobs/123/completejob");
    assert_eq!(complete_body["planId"], "plan-1");
    assert_eq!(complete_body["jobId"], "job-1");
    assert_eq!(complete_body["conclusion"], "succeeded");
}

#[tokio::test]
async fn broker_poll_auth_failure_is_error_not_idle() {
    let _transport_guard = native_transport_guard().await;
    let server = MockServer::start().await;
    let broker = BrokerClient::new(&format!("{}/broker", server.uri()), TOKEN).unwrap();

    for status in [401, 403] {
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/broker/message"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;

        let error = broker
            .get_runner_message("session-1", RunnerStatus::Online, true)
            .await
            .expect_err("auth poll failure must be an error");
        assert_github_status(&error, status, "get broker message");
    }
}

#[tokio::test]
async fn broker_poll_empty_non_success_body_is_error_not_idle() {
    let _transport_guard = native_transport_guard().await;
    let server = MockServer::start().await;
    let broker = BrokerClient::new(&format!("{}/broker", server.uri()), TOKEN).unwrap();

    Mock::given(method("GET"))
        .and(path("/broker/message"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let error = broker
        .get_runner_message("session-1", RunnerStatus::Online, true)
        .await
        .expect_err("empty 500 poll must be an error");
    assert_github_status(&error, 500, "get broker message");
}

#[tokio::test]
async fn one_slow_broker_session_does_not_block_sibling_session() {
    let _transport_guard = native_transport_guard().await;
    let server = MockServer::start().await;
    let broker = BrokerClient::new(&format!("{}/broker", server.uri()), TOKEN).unwrap();

    Mock::given(method("GET"))
        .and(path("/broker/message"))
        .and(query_param("sessionId", "slow-session"))
        .respond_with(ResponseTemplate::new(404).set_delay(Duration::from_millis(300)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/broker/message"))
        .and(query_param("sessionId", "healthy-session"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let slow_broker = broker.clone();
    let slow = tokio::spawn(async move {
        slow_broker
            .get_runner_message("slow-session", RunnerStatus::Online, true)
            .await
    });
    tokio::task::yield_now().await;
    let healthy = tokio::time::timeout(
        Duration::from_millis(150),
        broker.get_runner_message("healthy-session", RunnerStatus::Online, true),
    )
    .await
    .expect("a slow sibling poll must not block the healthy session")
    .expect("healthy session poll");
    assert_eq!(healthy.status, 204);
    assert!(healthy.message.is_none());

    let slow_error = slow
        .await
        .expect("slow poll task")
        .expect_err("404 session must enter recovery path");
    assert_github_status(&slow_error, 404, "get broker message");
}

#[tokio::test]
async fn acquire_job_classifies_non_retriable_statuses() {
    let _transport_guard = native_transport_guard().await;
    let server = MockServer::start().await;
    let run_service = RunServiceClient::new(TOKEN).unwrap();
    let run_service_url = format!("{}/run/jobs/123", server.uri());

    for status in [
        StatusCode::NOT_FOUND,
        StatusCode::CONFLICT,
        StatusCode::UNPROCESSABLE_ENTITY,
    ] {
        server.reset().await;
        Mock::given(method("POST"))
            .and(path("/run/jobs/123/acquirejob"))
            .respond_with(
                ResponseTemplate::new(status.as_u16())
                    .insert_header("x-github-request-id", "request-1")
                    .set_body_string("skip"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let outcome = run_service
            .acquire_job(
                &run_service_url,
                "broker-message",
                std::env::consts::OS,
                None,
            )
            .await
            .unwrap();
        let AcquireJobOutcome::Skipped {
            status: actual,
            request_id,
            body,
        } = outcome
        else {
            panic!("expected skipped acquire");
        };
        assert_eq!(actual, status);
        assert_eq!(request_id.as_deref(), None);
        assert_eq!(body, "skip");
    }
}

#[tokio::test]
async fn complete_job_retries_5xx_and_succeeds() {
    let _transport_guard = native_transport_guard().await;
    let server = MockServer::start().await;
    let run_service = RunServiceClient::new(TOKEN).unwrap();
    let run_service_url = format!("{}/run/jobs/123", server.uri());
    let attempts = Arc::new(AtomicUsize::new(0));
    let responder_attempts = Arc::clone(&attempts);

    Mock::given(method("POST"))
        .and(path("/run/jobs/123/completejob"))
        .respond_with(move |_request: &Request| {
            if responder_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(500).set_body_string("temporary")
            } else {
                ResponseTemplate::new(204)
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    run_service
        .complete_job(&run_service_url, complete_job())
        .await
        .unwrap();

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn complete_job_does_not_retry_non_retryable_4xx() {
    let _transport_guard = native_transport_guard().await;
    let server = MockServer::start().await;
    let run_service = RunServiceClient::new(TOKEN).unwrap();
    let run_service_url = format!("{}/run/jobs/123", server.uri());

    Mock::given(method("POST"))
        .and(path("/run/jobs/123/completejob"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad completion"))
        .expect(1)
        .mount(&server)
        .await;

    let error = run_service
        .complete_job(&run_service_url, complete_job())
        .await
        .expect_err("400 completion must not retry");
    assert_github_status(&error, 400, "complete run-service job");

    assert_eq!(
        recorded_requests(&server)
            .await
            .iter()
            .filter(|request| request.url.path() == "/run/jobs/123/completejob")
            .count(),
        1
    );
}

fn job_message(run_service_url: &str) -> TaskAgentMessage {
    TaskAgentMessage {
        message_id: 99,
        message_type: RUNNER_JOB_REQUEST.to_string(),
        body: json!({
            "id": "broker-message",
            "runner_request_id": "request-1",
            "should_acknowledge": true,
            "run_service_url": run_service_url,
            "billing_owner_id": "42"
        })
        .to_string(),
        iv_base64: None,
    }
}

fn complete_job() -> RunServiceCompleteJob {
    RunServiceCompleteJob {
        plan_id: "plan-1".to_string(),
        job_id: "job-1".to_string(),
        conclusion: TaskResult::Succeeded,
        outputs: BTreeMap::new(),
        step_results: Vec::new(),
        annotations: Vec::new(),
        telemetry: Vec::new(),
        environment_url: None,
        billing_owner_id: Some("42".to_string()),
        infrastructure_failure_category: None,
    }
}

async fn recorded_requests(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("request recording enabled")
}

fn body_for(requests: &[Request], method: &str, path: &str) -> Value {
    let request = requests
        .iter()
        .find(|request| request.method.as_str() == method && request.url.path() == path)
        .unwrap_or_else(|| panic!("missing {method} {path}"));
    request.body_json().unwrap()
}

fn assert_github_status(error: &Error, status: u16, action: &str) {
    let api_error = error
        .downcast_ref::<GitHubApiError>()
        .unwrap_or_else(|| panic!("expected GitHubApiError, got {error:#}"));
    assert_eq!(api_error.status, status);
    assert_eq!(api_error.action, action);
}

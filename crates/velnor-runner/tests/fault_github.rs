//! GitHub-side faults: transient 5xx, rate limiting, expired credentials, and
//! a registration that disappeared underneath a live runner.
//!
//! Every case here is a real HTTP round trip against `wiremock`, because the
//! thing under test is not a pure classifier — it is what the client does with
//! a status line plus headers. The mock makes that deterministic: the response
//! is fixed before the request is made, and each assertion checks a bound (how
//! many requests were sent), a disposition (retriable or permanent), and the
//! diagnostic an operator would be handed.
#![cfg(feature = "test-support")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::LazyLock;

use velnor_runner::protocol::{
    completion_failure_is_permanent, github_api_quota_status, github_api_retry_delay,
    is_retriable_completion_status, GitHubApiError, GitHubScope, RegistrationClient,
    RunServiceClient, RunServiceCompleteJob, TaskResult,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod fault_support;

const PAT: &str = "fault-suite-pat";

static TRANSPORT_ENV_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Forces the native HTTP transport for the duration of one test, restoring
/// whatever the process had before. Loopback HTTP is only accepted under the
/// `test-support` feature, so this never widens the release surface.
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

/// A GitHub Enterprise-shaped scope pointed at the mock, so `api_base_url`
/// resolves to `<mock>/api/v3/`.
fn scope_for(server: &MockServer) -> GitHubScope {
    GitHubScope::parse(&format!("{}/tailrocks", server.uri())).expect("parse the mock GitHub scope")
}

fn api_error(error: &anyhow::Error) -> &GitHubApiError {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<GitHubApiError>())
        .expect("a GitHub HTTP failure carries a typed API error")
}

fn completion() -> RunServiceCompleteJob {
    RunServiceCompleteJob {
        plan_id: "plan-fault".into(),
        job_id: "job-fault".into(),
        conclusion: TaskResult::Succeeded,
        outputs: BTreeMap::new(),
        step_results: Vec::new(),
        annotations: Vec::new(),
        telemetry: Vec::new(),
        environment_url: None,
        billing_owner_id: None,
        infrastructure_failure_category: None,
    }
}

#[tokio::test]
async fn a_transient_five_hundred_is_surfaced_once_and_stays_retriable() {
    let _transport = native_transport().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/orgs/tailrocks/actions/runners"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream unavailable"))
        .mount(&server)
        .await;

    let error = RegistrationClient::new()
        .expect("build the registration client")
        .list_runners(&scope_for(&server), PAT)
        .await
        .expect_err("a 503 listing must not look like an empty fleet");

    // Bounded behavior: the listing does not retry inline. Its caller owns the
    // pacing, so one transient failure costs one request, not a storm.
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .len(),
        1,
        "a transient 5xx must not be retried inside the client"
    );

    let api = api_error(&error);
    assert_eq!(api.status, 503);
    // Correct disposition: 5xx is exactly the case where retrying is right.
    assert!(is_retriable_completion_status(api.status));
    assert!(!completion_failure_is_permanent(&error));
    // No fabricated wait: with no rate-limit headers the client must not
    // invent a back-off the caller would obey.
    assert_eq!(github_api_retry_delay(&error), None);
    assert_eq!(github_api_quota_status(&error), None);
    fault_support::assert_actionable_diagnostic(&format!("{error:#}"), &["503", "runners"]);
}

#[tokio::test]
async fn a_rate_limited_response_is_reported_with_the_wait_its_headers_ask_for() {
    let _transport = native_transport().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/orgs/tailrocks/actions/runners"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "137")
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-limit", "5000")
                .set_body_string("API rate limit exceeded"),
        )
        .mount(&server)
        .await;

    let error = RegistrationClient::new()
        .expect("build the registration client")
        .list_runners(&scope_for(&server), PAT)
        .await
        .expect_err("a 429 must not look like an empty fleet");

    assert_eq!(
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .len(),
        1,
        "a rate-limited client must not immediately spend another request"
    );

    // The GitHub-visible result an operator can act on: how long to wait, and
    // the proof that the budget — not a permission — is what ran out.
    let quota = github_api_quota_status(&error).expect("a 429 is a quota status");
    assert_eq!(quota.retry_after_seconds, Some(137));
    assert_eq!(quota.remaining, Some(0));
    assert!(quota.is_limited(429));

    let delay = github_api_retry_delay(&error).expect("a 429 with headers asks for a wait");
    assert!(
        delay.as_secs() >= 130 && delay.as_secs() <= 137,
        "the back-off must come from the header, got {delay:?}"
    );
    assert_eq!(api_error(&error).status, 429);
    assert!(!completion_failure_is_permanent(&error));
}

#[tokio::test]
async fn a_permission_forbidden_is_not_mistaken_for_a_rate_limit() {
    // GitHub sends rate-limit headers on permission failures too. Treating one
    // as exhaustion would idle the whole fleet on a token that simply lost a
    // scope, so the distinction is load-bearing.
    let _transport = native_transport().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/orgs/tailrocks/actions/runners"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-remaining", "4931")
                .insert_header("x-ratelimit-limit", "5000")
                .set_body_string("Resource not accessible by personal access token"),
        )
        .mount(&server)
        .await;

    let error = RegistrationClient::new()
        .expect("build the registration client")
        .list_runners(&scope_for(&server), PAT)
        .await
        .expect_err("a 403 must not look like an empty fleet");

    assert_eq!(api_error(&error).status, 403);
    assert_eq!(
        github_api_quota_status(&error),
        None,
        "a permission 403 with budget remaining is not a rate limit"
    );
    assert_eq!(
        github_api_retry_delay(&error),
        None,
        "a permission failure must never park the fleet on a back-off"
    );
    // Permanent for a completion: no amount of retrying grants a scope.
    assert!(completion_failure_is_permanent(&error));
}

#[tokio::test]
async fn expired_credentials_fail_closed_and_are_never_retried_as_transient() {
    let _transport = native_transport().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/completejob"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Bad credentials"))
        .mount(&server)
        .await;

    let error = RunServiceClient::new("expired-bearer")
        .expect("build the run-service client")
        .complete_job(&server.uri(), completion())
        .await
        .expect_err("an expired token cannot complete a job");

    // Bounded behavior: the completion retry loop must recognise 401 as
    // hopeless on the first answer rather than burning its six attempts.
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .len(),
        1,
        "expired credentials must be refused after exactly one attempt"
    );
    assert_eq!(api_error(&error).status, 401);
    assert!(!is_retriable_completion_status(401));
    assert!(
        completion_failure_is_permanent(&error),
        "a permanent refusal must spend the durable budget at once so the slot is freed"
    );
    fault_support::assert_actionable_diagnostic(&format!("{error:#}"), &["401", "complete"]);
}

#[tokio::test]
async fn a_registration_that_disappeared_reads_as_absent_rather_than_as_an_error() {
    // The runner is gone from GitHub while the node still believes it owns the
    // identity. A 404 must be a fact ("no such runner"), not a failure: only a
    // fact lets reconciliation clear the claim and issue a fresh JIT request.
    let _transport = native_transport().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/orgs/tailrocks/actions/runners/4242"))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"message":"Not Found"}"#))
        .mount(&server)
        .await;

    let looked_up = RegistrationClient::new()
        .expect("build the registration client")
        .get_runner(&scope_for(&server), PAT, 4242)
        .await
        .expect("a missing runner is an answer, not a transport failure");

    assert!(
        looked_up.is_none(),
        "a 404 runner lookup must be the absence of a runner, not a stale identity"
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .len(),
        1,
        "a definitive 404 must not be retried"
    );
}

#[tokio::test]
async fn a_runner_lookup_that_returns_the_wrong_identity_is_refused() {
    // Split brain the other way: GitHub answers, but with someone else's
    // runner. Accepting it would attach this node's job to another host's
    // registration — the worst kind of cross-job contamination.
    let _transport = native_transport().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/orgs/tailrocks/actions/runners/4242"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"id":9999,"name":"someone-elses-runner","status":"online","busy":false}"#,
        ))
        .mount(&server)
        .await;

    let error = RegistrationClient::new()
        .expect("build the registration client")
        .get_runner(&scope_for(&server), PAT, 4242)
        .await
        .expect_err("an identity mismatch must be refused");

    fault_support::assert_actionable_diagnostic(&format!("{error:#}"), &["4242", "mismatch"]);
}

#[tokio::test]
async fn a_lease_renewal_failure_is_reported_immediately_rather_than_retried_inline() {
    // Lease renewal runs on a cadence the caller owns. Retrying inside the
    // client would blur two different clocks and let one wedged renewal hold a
    // slot past its lease.
    let _transport = native_transport().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/renewjob"))
        .respond_with(ResponseTemplate::new(500).set_body_string("renew failed"))
        .mount(&server)
        .await;

    let error = RunServiceClient::new("bearer")
        .expect("build the run-service client")
        .renew_job(&server.uri(), "plan-1", "job-1")
        .await
        .expect_err("a 500 renewal is a failure the caller must see");

    assert_eq!(
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .len(),
        1,
        "renewal must not retry inside the client"
    );
    assert_eq!(api_error(&error).status, 500);
    fault_support::assert_actionable_diagnostic(&format!("{error:#}"), &["500", "renew"]);
}

#[tokio::test]
async fn a_run_service_that_drops_the_connection_is_transient_not_permanent() {
    // Disconnect, not a status line: the remote's answer is unknown. That is
    // precisely the case where giving up would risk losing a finished job's
    // result, so it must never classify as permanent.
    let _transport = native_transport().await;
    // Port 1 is reserved and never listening, so the connection is refused
    // rather than timing out: the fault is delivered by the kernel, not by a
    // clock the test would have to wait on.
    let error = RunServiceClient::new("bearer")
        .expect("build the run-service client")
        .renew_job("http://127.0.0.1:1", "plan-1", "job-1")
        .await
        .expect_err("a dead run service cannot renew a lease");

    assert!(
        !completion_failure_is_permanent(&error),
        "not knowing the remote's answer is never a permanent refusal"
    );
    assert!(
        github_api_retry_delay(&error).is_none(),
        "a transport failure carries no server-supplied back-off"
    );
}

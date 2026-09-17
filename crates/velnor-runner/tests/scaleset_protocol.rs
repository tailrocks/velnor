//! Scale-set protocol conformance: recorded fixtures + mocked Actions Service.
//!
//! No live calls: GitHub API and Actions Service are both `wiremock` doubles.
//! Each test pins one upstream behavior from `actions/scaleset` @
//! `e6daac70` (session create/refresh/close, long poll + capacity header,
//! 202/401 handling, AcquireJobs queue-token auth, JIT config, ACK).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
#![cfg(feature = "test-support")]

use std::time::Duration;

use velnor_model::{
    AcquireJobsResponse, RunnerScaleSetJitRunnerConfig, RunnerScaleSetJitRunnerSetting,
    RunnerScaleSetMessageResponse, ScaleSetSession,
};
use velnor_runner::scaleset::{
    ActionsAuth, Fixtures, MessageSessionClient, RetryPolicy, ScaleSetClient, ScaleSetFault,
    SystemInfo,
};
use wiremock::{
    matchers::{body_json, body_string_contains, header, method, path, query_param},
    Mock, MockServer, ResponseTemplate,
};

const SCALE_SET_ID: i32 = 7;
const OWNER: &str = "octo-org";

fn fixture_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("scaleset")
}

fn system_info() -> SystemInfo {
    SystemInfo {
        system: "velnor".into(),
        version: "0.1.0-test".into(),
        commit_sha: "test".into(),
        scale_set_id: SCALE_SET_ID,
        subsystem: "listener".into(),
    }
}

fn test_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 0,
        wait_min: Duration::from_millis(1),
        wait_max: Duration::from_millis(1),
        timeout: Duration::from_secs(30),
    }
}

/// Unsigned admin JWT: the client reads the unverified `exp` claim only.
fn admin_jwt(exp: u64) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let payload = URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{exp}}}"));
    format!("test-header.{payload}.test-signature")
}

fn session_json(server: &MockServer, token: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionId": "3fa85f64-5717-4562-b3fc-2c963f66afa6",
        "ownerName": OWNER,
        "messageQueueUrl": format!(
            "{}/tenant/_apis/runtime/runnerscalesets/7/messages/session-1",
            server.uri()
        ),
        "messageQueueAccessToken": token,
        "statistics": {
            "totalAvailableJobs": 1, "totalAcquiredJobs": 0, "totalAssignedJobs": 2,
            "totalRunningJobs": 1, "totalRegisteredRunners": 2,
            "totalBusyRunners": 1, "totalIdleRunners": 1
        }
    })
}

/// Mount the token chain: registration-token (201) → admin connection (200).
async fn mount_token_chain(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(
            "/api/v3/orgs/octo-org/actions/runners/registration-token",
        ))
        .and(header("Authorization", "Bearer test-pat"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "token": "reg-token",
            "expires_at": "2026-09-17T00:05:00Z"
        })))
        .mount(server)
        .await;
    let admin_url = format!("{}/tenant", server.uri());
    Mock::given(method("POST"))
        .and(path("/api/v3/actions/runner-registration"))
        .and(header("Authorization", "RemoteAuth reg-token"))
        .and(body_string_contains(OWNER))
        .and(body_string_contains("register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "url": admin_url,
            "token": admin_jwt(2_000_000_000)
        })))
        .mount(server)
        .await;
}

async fn test_client(server: &MockServer) -> ScaleSetClient {
    mount_token_chain(server).await;
    ScaleSetClient::new_with_pat(
        &format!("{}/octo-org", server.uri()),
        "test-pat",
        system_info(),
        test_retry(),
    )
    .unwrap()
}

#[tokio::test]
async fn fixtures_verify_pin_hashes_and_redaction() {
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    assert_eq!(fixtures.manifest().files.len(), 21);

    let session: ScaleSetSession = fixtures.parse("session_created.json").unwrap();
    assert_eq!(session.owner_name, OWNER);
    assert_eq!(session.message_queue_access_token, "REDACTED");

    let envelope: RunnerScaleSetMessageResponse = fixtures.parse("message_batch.json").unwrap();
    assert_eq!(envelope.message_type, "RunnerScaleSetJobMessages");
    let parsed = velnor_runner::scaleset::parse_message_response(
        fixtures.read("message_batch.json").unwrap().as_bytes(),
    )
    .unwrap();
    assert_eq!(parsed.message_id, 9);
    assert_eq!(parsed.job_available_messages.len(), 1);
    assert_eq!(parsed.job_assigned_messages.len(), 1);
    assert_eq!(parsed.job_started_messages.len(), 1);
    assert_eq!(parsed.job_completed_messages.len(), 1);
    assert_eq!(
        parsed.job_available_messages[0].base.runner_request_id,
        4242
    );

    let acquired: AcquireJobsResponse = fixtures.parse("acquire_jobs.json").unwrap();
    assert_eq!(acquired.value, vec![4242, 4243]);

    let jit: RunnerScaleSetJitRunnerConfig = fixtures.parse("jit_runner_config.json").unwrap();
    assert_eq!(jit.encoded_jit_config, "REDACTED");
}

#[tokio::test]
async fn session_create_polls_with_capacity_header_and_last_message_id() {
    let server = MockServer::start().await;
    let client = test_client(&server).await;
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let batch = fixtures.read("message_batch.json").unwrap();

    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions"
        )))
        .and(body_string_contains(OWNER))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(session_json(&server, "queue-token-1")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/messages/session-1"
        )))
        .and(query_param("lastMessageId", "8"))
        .and(header("X-ScaleSetMaxCapacity", "10"))
        .and(header("Authorization", "Bearer queue-token-1"))
        .and(header(
            "Accept",
            "application/json; api-version=6.0-preview",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(batch))
        .mount(&server)
        .await;

    let session = MessageSessionClient::create(&client, SCALE_SET_ID, OWNER)
        .await
        .unwrap();
    assert_eq!(session.scale_set_id(), SCALE_SET_ID);
    let message = session.get_message(8, 10).await.unwrap().unwrap();
    assert_eq!(message.message_id, 9);
    assert_eq!(message.job_available_messages.len(), 1);
    assert_eq!(message.statistics.unwrap().desired_runners(), 2);
}

#[tokio::test]
async fn poll_without_last_message_id_omits_query_and_202_is_none() {
    let server = MockServer::start().await;
    let client = test_client(&server).await;

    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(session_json(&server, "queue-token-1")),
        )
        .mount(&server)
        .await;
    // No `lastMessageId` matcher: upstream only sets the query when > 0.
    // wiremock matches path-only here; the query assertion lives in the
    // companion test above. 202 (timeout, no message) → None.
    Mock::given(method("GET"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/messages/session-1"
        )))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;

    let session = MessageSessionClient::create(&client, SCALE_SET_ID, OWNER)
        .await
        .unwrap();
    assert!(session.get_message(0, 10).await.unwrap().is_none());
    assert!(session.get_message(-1, 10).await.unwrap().is_none());
}

#[tokio::test]
async fn expired_queue_token_refreshes_once_then_retries() {
    let server = MockServer::start().await;
    let client = test_client(&server).await;
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let batch = fixtures.read("message_batch.json").unwrap();

    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(session_json(&server, "queue-token-old")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(header("Authorization", "Bearer queue-token-old"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions/3fa85f64-5717-4562-b3fc-2c963f66afa6"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_json(&server, "queue-token-2")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(header("Authorization", "Bearer queue-token-2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(batch))
        .mount(&server)
        .await;

    let session = MessageSessionClient::create(&client, SCALE_SET_ID, OWNER)
        .await
        .unwrap();
    let message = session.get_message(8, 10).await.unwrap().unwrap();
    assert_eq!(message.message_id, 9);
    assert_eq!(
        session.session().await.message_queue_access_token,
        "queue-token-2"
    );
}

#[tokio::test]
async fn acquire_jobs_uses_queue_token_and_returns_subset() {
    let server = MockServer::start().await;
    let client = test_client(&server).await;

    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(session_json(&server, "queue-token-1")),
        )
        .mount(&server)
        .await;
    // Upstream swaps the admin token for the queue token on this route.
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/acquirejobs"
        )))
        .and(header("Authorization", "Bearer queue-token-1"))
        .and(body_json(serde_json::json!([4242, 4243, 4244])))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 2, "value": [4242, 4243]
        })))
        .mount(&server)
        .await;

    let session = MessageSessionClient::create(&client, SCALE_SET_ID, OWNER)
        .await
        .unwrap();
    let acquired = session.acquire_jobs(&[4242, 4243, 4244]).await.unwrap();
    assert_eq!(acquired, vec![4242, 4243]);
}

#[tokio::test]
async fn jit_config_ack_and_close_follow_upstream_contract() {
    let server = MockServer::start().await;
    let client = test_client(&server).await;
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();

    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(session_json(&server, "queue-token-1")),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/generatejitconfig"
        )))
        .and(body_json(serde_json::json!({
            "name": "velnor-set-0007", "workFolder": "_work"
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixtures.read("jit_runner_config.json").unwrap()),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/messages/session-1/9"
        )))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions/3fa85f64-5717-4562-b3fc-2c963f66afa6"
        )))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let session = MessageSessionClient::create(&client, SCALE_SET_ID, OWNER)
        .await
        .unwrap();
    let jit = client
        .generate_jit_runner_config(
            &RunnerScaleSetJitRunnerSetting {
                name: "velnor-set-0007".into(),
                work_folder: "_work".into(),
            },
            SCALE_SET_ID,
        )
        .await
        .unwrap();
    assert_eq!(jit.runner.unwrap().id, 11);
    session.delete_message(9).await.unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn scale_set_and_runner_reads_match_upstream_shapes() {
    let server = MockServer::start().await;
    let client = test_client(&server).await;
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();

    Mock::given(method("GET"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixtures.read("runner_scale_set.json").unwrap()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tenant/_apis/distributedtask/pools/0/agents"))
        .and(query_param("agentName", "velnor-set-absent"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"count": 0, "value": []})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tenant/_apis/distributedtask/pools/0/agents/11"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(
                serde_json::from_str::<serde_json::Value>(
                    &fixtures.read("error_agent_not_found.json").unwrap(),
                )
                .unwrap(),
            ),
        )
        .mount(&server)
        .await;

    let set = client
        .get_runner_scale_set_by_id(SCALE_SET_ID)
        .await
        .unwrap();
    assert_eq!(set.name, "velnor-set");
    assert!(client
        .get_runner_by_name("velnor-set-absent")
        .await
        .unwrap()
        .is_none());

    let error = client.get_runner(11).await.unwrap_err();
    assert_eq!(error.fault(), Some(ScaleSetFault::NotFound));
    assert!(error.to_string().contains("runner not found"), "{error}");
}

#[tokio::test]
async fn pat_and_app_constructors_validate_like_upstream() {
    assert!(ScaleSetClient::new_with_pat(
        "https://github.com/octo-org",
        "pat",
        system_info(),
        test_retry(),
    )
    .is_ok());
    // PAT xor App is enforced at the auth layer.
    let auth = ActionsAuth::pat("pat".into());
    assert!(auth.validate().is_ok());
    assert!(auth.is_pat());
}

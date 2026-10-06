//! Scale-set protocol conformance: recorded fixtures + mocked Actions Service.
//!
//! No live calls: GitHub API and Actions Service are both `wiremock` doubles.
//! Each test pins one upstream behavior from `actions/scaleset` @
//! `e6daac70` (session create/refresh/close, long poll + capacity header,
//! 202/401 handling, AcquireJobs queue-token auth, JIT config, ACK,
//! PAT + App token chains, runner get/lookup).

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

use std::sync::Arc;
use std::time::Duration;

use velnor_model::{
    AcquireJobsResponse, RunnerReference, RunnerScaleSetJitRunnerConfig,
    RunnerScaleSetJitRunnerSetting, RunnerScaleSetMessageResponse, ScaleSetSession,
};
use velnor_runner::scaleset::{
    credentials::InstallationAccessToken, ActionsAuth, Fixtures, FnJwtProvider, JwtProvider,
    MessageSessionClient, RetryPolicy, ScaleSetClient, ScaleSetFault, SystemInfo,
};
use wiremock::{
    matchers::{body_json, body_string_contains, header, method, path, query_param},
    Match, Mock, MockServer, Request, ResponseTemplate,
};

const SCALE_SET_ID: i32 = 7;
const OWNER: &str = "octo-org";
/// `exp` of the unsigned test admin JWT served by the token-chain mocks.
const ADMIN_JWT_EXP: u64 = 2_000_000_000;

/// Upstream `getMessage` sets `lastMessageId` only when > 0
/// (`session_client.go`); `wiremock::matchers::path` ignores the query
/// string, so omission needs an explicit matcher — without it a client
/// that always sent `lastMessageId=0` would still pass.
struct NoLastMessageId;

impl Match for NoLastMessageId {
    fn matches(&self, request: &Request) -> bool {
        !request
            .url
            .query_pairs()
            .any(|(key, _)| key == "lastMessageId")
    }
}

/// Upstream sends the JSON `userAgent` (`system`, `version`, `commit_sha`,
/// `scale_set_id`, `subsystem`, `build_*`, `kind: "scaleset"`) on every
/// queue request (`setUserAgent`); parse the header and pin its shape.
struct ScaleSetUserAgent;

impl Match for ScaleSetUserAgent {
    fn matches(&self, request: &Request) -> bool {
        let Some(value) = request.headers.get("user-agent") else {
            return false;
        };
        let Ok(text) = value.to_str() else {
            return false;
        };
        let Ok(agent) = serde_json::from_str::<serde_json::Value>(text) else {
            return false;
        };
        agent.get("kind").and_then(serde_json::Value::as_str) == Some("scaleset")
            && agent.get("system").and_then(serde_json::Value::as_str) == Some("velnor")
            && agent
                .get("scale_set_id")
                .and_then(serde_json::Value::as_i64)
                == Some(i64::from(SCALE_SET_ID))
            && agent.get("subsystem").and_then(serde_json::Value::as_str) == Some("listener")
    }
}

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

/// Mount the admin handshake: `RemoteAuth <registration token>` (from the
/// recorded registration-token bytes) → admin connection (200).
///
/// `admin_connection.json` pins the upstream handshake response shape
/// (`actionsServiceAdminConnection`: exactly `{url, token}`); only the
/// environment-dependent values are substituted for the mock server.
async fn mount_admin_handshake(server: &MockServer, fixtures: &Fixtures) {
    let mut admin: serde_json::Value = fixtures.parse("admin_connection.json").unwrap();
    let fields = admin.as_object().unwrap();
    assert_eq!(fields.len(), 2, "handshake shape is exactly {{url, token}}");
    assert!(fields.contains_key("url") && fields.contains_key("token"));
    admin["url"] = serde_json::Value::String(format!("{}/tenant", server.uri()));
    admin["token"] = serde_json::Value::String(admin_jwt(ADMIN_JWT_EXP));
    Mock::given(method("POST"))
        .and(path("/api/v3/actions/runner-registration"))
        .and(header("Authorization", "RemoteAuth REDACTED"))
        .and(header("Content-Type", "application/json"))
        .and(body_string_contains(OWNER))
        .and(body_string_contains("register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(admin))
        .mount(server)
        .await;
}

/// Mount the token chain: registration-token (201, recorded bytes) →
/// admin connection (200). The recorded registration token (`REDACTED`)
/// flows into the handshake's `RemoteAuth` header exactly like live.
async fn mount_token_chain(server: &MockServer, fixtures: &Fixtures) {
    Mock::given(method("POST"))
        .and(path(
            "/api/v3/orgs/octo-org/actions/runners/registration-token",
        ))
        .and(header("Authorization", "Bearer test-pat"))
        .and(header("Content-Type", "application/vnd.github.v3+json"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(fixtures.read("registration_token.json").unwrap()),
        )
        .mount(server)
        .await;
    mount_admin_handshake(server, fixtures).await;
}

async fn test_client(server: &MockServer, fixtures: &Fixtures) -> ScaleSetClient {
    mount_token_chain(server, fixtures).await;
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

    // Token-chain shapes (`registrationToken`, `accessToken`,
    // `actionsServiceAdminConnection` in `client.go`): the wire tests serve
    // these bytes, so pin their fields here too.
    let registration: serde_json::Value = fixtures.parse("registration_token.json").unwrap();
    assert_eq!(registration["token"], "REDACTED");
    assert_eq!(registration["expires_at"], "2026-09-17T00:05:00Z");
    let installation: InstallationAccessToken = fixtures.parse("installation_token.json").unwrap();
    assert_eq!(installation.expires_at, "2026-09-17T01:00:00Z");
    let admin: serde_json::Value = fixtures.parse("admin_connection.json").unwrap();
    assert_eq!(admin.as_object().unwrap().len(), 2);

    let runner: RunnerReference = fixtures.parse("runner_reference.json").unwrap();
    assert_eq!(runner.id, 11);
    assert_eq!(runner.name, "velnor-set-0007");
    assert_eq!(runner.runner_scale_set_id, SCALE_SET_ID);

    // Session responses may embed the set (`runnerScaleSet`,
    // `omitempty`-present): the create/refresh decode path must accept it.
    let mut created: serde_json::Value = fixtures.parse("session_created.json").unwrap();
    created["runnerScaleSet"] = fixtures.parse("runner_scale_set.json").unwrap();
    let with_set: ScaleSetSession = serde_json::from_value(created).unwrap();
    assert_eq!(with_set.runner_scale_set.unwrap().id, SCALE_SET_ID);
}

#[tokio::test]
async fn session_create_polls_with_capacity_header_and_last_message_id() {
    let server = MockServer::start().await;
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let client = test_client(&server, &fixtures).await;
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
        .and(ScaleSetUserAgent)
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
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let client = test_client(&server, &fixtures).await;

    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(session_json(&server, "queue-token-1")),
        )
        .mount(&server)
        .await;
    // Upstream only sets the query when > 0 (`session_client.go`); the
    // matcher below fails the test if `lastMessageId` is sent at all.
    // 202 (timeout, no message) → None.
    Mock::given(method("GET"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/messages/session-1"
        )))
        .and(NoLastMessageId)
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
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let client = test_client(&server, &fixtures).await;
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
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let client = test_client(&server, &fixtures).await;

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
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let client = test_client(&server, &fixtures).await;

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
    // Upstream ACKs with the queue token + JSON content type
    // (`deleteMessage`); the session close reuses the admin bearer.
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/messages/session-1/9"
        )))
        .and(header("Content-Type", "application/json"))
        .and(header("Authorization", "Bearer queue-token-1"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions/3fa85f64-5717-4562-b3fc-2c963f66afa6"
        )))
        .and(header(
            "Authorization",
            format!("Bearer {}", admin_jwt(ADMIN_JWT_EXP)),
        ))
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
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let client = test_client(&server, &fixtures).await;

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
    assert_eq!(error.fault(), Some(ScaleSetFault::RunnerNotFound));
    assert!(error.to_string().contains("runner not found"), "{error}");
}

#[tokio::test]
async fn runner_reference_shapes_pin_get_and_lookup() {
    let server = MockServer::start().await;
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let client = test_client(&server, &fixtures).await;

    // `GetRunner` (200 → `RunnerReference`, verbatim recorded bytes).
    Mock::given(method("GET"))
        .and(path("/tenant/_apis/distributedtask/pools/0/agents/11"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixtures.read("runner_reference.json").unwrap()),
        )
        .mount(&server)
        .await;
    // `GetRunnerByName` count-1 → first entry; the list envelope
    // (`{count, value}`) wraps the same recorded reference bytes.
    let reference: serde_json::Value = fixtures.parse("runner_reference.json").unwrap();
    Mock::given(method("GET"))
        .and(path("/tenant/_apis/distributedtask/pools/0/agents"))
        .and(query_param("agentName", "velnor-set-0007"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1, "value": [reference]
        })))
        .mount(&server)
        .await;

    let runner = client.get_runner(11).await.unwrap();
    assert_eq!(runner.name, "velnor-set-0007");
    assert_eq!(runner.runner_scale_set_id, SCALE_SET_ID);
    let found = client
        .get_runner_by_name("velnor-set-0007")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, 11);
}

#[tokio::test]
async fn github_app_chain_fetches_installation_token_first() {
    let server = MockServer::start().await;
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    const INSTALLATION_ID: i64 = 42;

    // Upstream `fetchAccessToken`: App JWT Bearer → 201 `accessToken`
    // (recorded `installation_token.json` bytes verbatim).
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v3/app/installations/{INSTALLATION_ID}/access_tokens"
        )))
        .and(header("Authorization", "Bearer test-app-jwt"))
        .and(header("Content-Type", "application/vnd.github+json"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(fixtures.read("installation_token.json").unwrap()),
        )
        .mount(&server)
        .await;
    // The installation token authorizes the registration-token call.
    Mock::given(method("POST"))
        .and(path(
            "/api/v3/orgs/octo-org/actions/runners/registration-token",
        ))
        .and(header("Authorization", "Bearer REDACTED"))
        .and(header("Content-Type", "application/vnd.github.v3+json"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(fixtures.read("registration_token.json").unwrap()),
        )
        .mount(&server)
        .await;
    mount_admin_handshake(&server, &fixtures).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(session_json(&server, "queue-token-1")),
        )
        .mount(&server)
        .await;

    let provider: Arc<dyn JwtProvider> = Arc::new(FnJwtProvider(|| Ok("test-app-jwt".to_string())));
    let client = ScaleSetClient::new_with_jwt_provider(
        &format!("{}/octo-org", server.uri()),
        INSTALLATION_ID,
        provider,
        system_info(),
        test_retry(),
    )
    .unwrap();
    let session = MessageSessionClient::create(&client, SCALE_SET_ID, OWNER)
        .await
        .unwrap();
    assert_eq!(session.scale_set_id(), SCALE_SET_ID);
    assert_eq!(session.owner(), OWNER);
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

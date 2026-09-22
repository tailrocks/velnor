//! Scale-set loop conformance: the §5.1 9-step processor against a mocked
//! Actions Service (`wiremock`) + real stores + [`MemLedger`].
//!
//! No live calls. Each test pins one loop behavior: deferred offers without
//! capacity, redelivery idempotency, partial/uncertain acquire, 401 refresh
//! survival, stale-generation reset, order-independent observation folding,
//! and unknown-kind tolerance. Poll scripts come from the recorded
//! transcripts; wire payloads from the sanitized fixtures.

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

use std::sync::{Arc, Mutex};
use std::time::Duration;

use velnor_runner::scaleset::{
    grant_oldest, parse_message_response, startup, AcquireBatchStore, CapacityLedger,
    ClientSession, DemandState, DemandStore, Fixtures, IdlePolicy, Listener, LoopConfig, MemLedger,
    MessageSessionClient, Metrics, Processor, ProcessorConfig, ProvisionImages,
    ProvisionIntentStore, RetryPolicy, ScaleKind, ScaleSetClient, SessionStore, SystemInfo,
    WorkerLane,
};
use wiremock::{
    matchers::{body_json, body_string_contains, header, method, path, query_param},
    Mock, MockServer, ResponseTemplate,
};

const SCALE_SET_ID: i32 = 7;
const OWNER: &str = "octo-org";
const SESSION_ID: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

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

fn admin_jwt(exp: u64) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let payload = URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{exp}}}"));
    format!("test-header.{payload}.test-signature")
}

fn session_json(server: &MockServer, token: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionId": SESSION_ID,
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

fn temp_db(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "velnor-scaleset-loop-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("state.db")
}

#[derive(Debug, Default)]
struct LaneState {
    provisioned: Mutex<Vec<String>>,
    terminals: Mutex<Vec<i64>>,
}

#[derive(Debug, Default, Clone)]
struct RecordedLane {
    state: Arc<LaneState>,
}

#[derive(Debug)]
struct LaneError(String);

impl std::fmt::Display for LaneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lane: {}", self.0)
    }
}

impl std::error::Error for LaneError {}

impl WorkerLane for RecordedLane {
    type Error = LaneError;

    async fn provision(
        &mut self,
        intent: &velnor_runner::scaleset::ProvisionIntent,
    ) -> Result<(), Self::Error> {
        self.state
            .provisioned
            .lock()
            .unwrap()
            .push(intent.operation_id.clone());
        Ok(())
    }

    fn note_assigned(
        &mut self,
        _assigned: &velnor_model::ScaleSetJobAssigned,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn note_started(
        &mut self,
        _started: &velnor_model::ScaleSetJobStarted,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn note_terminal(
        &mut self,
        completed: &velnor_model::ScaleSetJobCompleted,
    ) -> Result<(), Self::Error> {
        self.state
            .terminals
            .lock()
            .unwrap()
            .push(completed.base.runner_request_id);
        Ok(())
    }
}

fn push_offer(id: i64, event: &str) -> velnor_model::ScaleSetJobAvailable {
    velnor_model::ScaleSetJobAvailable {
        acquire_job_url: String::new(),
        base: velnor_model::ScaleSetJobMessage {
            message_type: velnor_model::ScaleSetJobMessageType::JobAvailable,
            runner_request_id: id,
            repository_name: "velnor".to_owned(),
            owner_name: "tailrocks".to_owned(),
            job_id: format!("job-{id}"),
            job_workflow_ref: String::new(),
            job_display_name: String::new(),
            workflow_run_id: 0,
            event_name: event.to_owned(),
            request_labels: vec!["velnor".to_owned()],
            queue_time: String::new(),
            scale_set_assign_time: String::new(),
            runner_assign_time: String::new(),
            finish_time: String::new(),
        },
    }
}

fn images() -> ProvisionImages {
    ProvisionImages {
        runner_digest: "sha256:runner".to_owned(),
        dind_digest: "sha256:dind".to_owned(),
    }
}

fn queue_path() -> String {
    format!("/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/messages/session-1")
}

async fn mount_session_create(server: &MockServer, token: &str) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_json(server, token)))
        .mount(server)
        .await;
}

async fn mount_ack(server: &MockServer, message_id: i32) {
    Mock::given(method("DELETE"))
        .and(path(format!("{}/{message_id}", queue_path())))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
}

type TestListener = Listener<ClientSession, MemLedger, RecordedLane>;

async fn listener(
    server: &MockServer,
    db: &std::path::Path,
    max_jobs: u32,
) -> (TestListener, RecordedLane, Metrics) {
    let client = test_client(server).await;
    mount_session_create(server, "queue-token-1").await;
    let session = MessageSessionClient::create(&client, SCALE_SET_ID, OWNER)
        .await
        .unwrap();
    let queue = ClientSession::new(session);
    let metrics = Metrics::new();
    let ledger = MemLedger::new();
    ledger.set_max_jobs(max_jobs);
    let lane = RecordedLane::default();
    let processor = Processor::new(
        queue.clone(),
        ledger,
        lane.clone(),
        DemandStore::open(db).unwrap(),
        AcquireBatchStore::open(db).unwrap(),
        ProvisionIntentStore::open(db).unwrap(),
        metrics.clone(),
        ProcessorConfig {
            scale_set_id: SCALE_SET_ID,
            images: images(),
            max_acquire_batch: velnor_runner::scaleset::MAX_ACQUIRE_BATCH,
        },
    );
    let listener = Listener::new(
        queue,
        processor,
        SessionStore::open(db).unwrap(),
        metrics.clone(),
        LoopConfig {
            scale_set_id: SCALE_SET_ID,
            retry: test_retry(),
            idle: IdlePolicy {
                nil_delay: Duration::from_millis(1),
            },
        },
    );
    (listener, lane, metrics)
}

#[tokio::test]
async fn loop_fixtures_verify_and_transcripts_validate() {
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    assert_eq!(fixtures.manifest().files.len(), 21);

    // Redelivery is byte-identical to the deferred batch (same message ID).
    let deferred = fixtures.read("message_deferred_offer.json").unwrap();
    let redelivered = fixtures.read("message_redelivered.json").unwrap();
    assert_eq!(deferred, redelivered);
    let parsed = parse_message_response(deferred.as_bytes()).unwrap();
    assert_eq!(parsed.message_id, 20);
    assert_eq!(parsed.job_available_messages.len(), 2);
    assert_eq!(
        parsed.job_available_messages[0].base.runner_request_id,
        4244
    );

    // Scrambled batch order survives dispatch per kind.
    let reordered =
        parse_message_response(fixtures.read("message_reordered.json").unwrap().as_bytes())
            .unwrap();
    assert_eq!(reordered.job_completed_messages.len(), 1);
    assert_eq!(reordered.job_started_messages.len(), 1);
    assert_eq!(reordered.job_assigned_messages.len(), 1);

    // Unknown future kinds are captured, live offers still dispatch.
    let unknown = parse_message_response(
        fixtures
            .read("message_unknown_kind.json")
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(
        unknown.unknown_message_types,
        vec!["JobMigrated".to_owned()]
    );
    assert_eq!(unknown.job_available_messages.len(), 1);

    // Partial acquire shape: subset, not echo.
    let partial: velnor_model::AcquireJobsResponse =
        fixtures.parse("acquire_partial.json").unwrap();
    assert_eq!(partial.count, 1);
    assert_eq!(partial.value, vec![4244]);

    // Refresh answer: new session, redacted token.
    let refreshed: velnor_model::ScaleSetSession =
        fixtures.parse("session_refreshed.json").unwrap();
    assert_eq!(refreshed.message_queue_access_token, "REDACTED");
    assert!(!refreshed.session_id.is_empty());

    // Transcripts reference only manifest payloads.
    let nils = fixtures.transcript("transcript_nil_polls.json").unwrap();
    assert_eq!(nils.polls.len(), 3);
    assert_eq!(nils.polls[0].status, 202);
    assert_eq!(
        nils.polls[2].fixture.as_deref(),
        Some("message_stats_only.json")
    );
    let redelivery = fixtures.transcript("transcript_redelivery.json").unwrap();
    assert_eq!(redelivery.polls.len(), 3);

    let seed = fixtures.demand_seed("seed_stale_generation.json").unwrap();
    assert_eq!(seed.scale_set_id, SCALE_SET_ID);
    assert_eq!(seed.demand.len(), 2);
}

#[tokio::test]
async fn deferred_offer_queues_durably_without_capacity_then_acks() {
    let server = MockServer::start().await;
    let db = temp_db("deferred");
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let (mut listener, _lane, metrics) = listener(&server, &db, 0).await;

    // N=0 advertises 0: offers queue, nothing reserves.
    Mock::given(method("GET"))
        .and(path(queue_path()))
        .and(header("X-ScaleSetMaxCapacity", "0"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixtures.read("message_deferred_offer.json").unwrap()),
        )
        .mount(&server)
        .await;
    mount_ack(&server, 20).await;

    let outcome = listener.run_once().await.unwrap();
    assert!(matches!(
        outcome.kind,
        ScaleKind::Message { message_id: 20 }
    ));
    assert_eq!(outcome.offers_seen, 2);
    assert_eq!(outcome.granted, 1);
    assert!(outcome.acquired.is_empty());
    assert!(outcome.uncertain.is_empty());

    let demand = DemandStore::open(&db).unwrap();
    assert_eq!(
        demand.get(SCALE_SET_ID, 4244).unwrap().unwrap().state,
        DemandState::Granted
    );
    let pr = demand.get(SCALE_SET_ID, 4245).unwrap().unwrap();
    assert_eq!(pr.state, DemandState::Observed);
    assert_eq!(pr.decline_reason.as_deref(), Some("trust-inputs-missing"));

    // The queue call was never made: nothing reserved, nothing to acquire.
    let acquire_calls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/acquirejobs"))
        .count();
    assert_eq!(acquire_calls, 0);

    assert_eq!(metrics.snapshot().acks, 1);
    let cursors = SessionStore::open(&db).unwrap();
    assert_eq!(
        cursors.get(SCALE_SET_ID).unwrap().unwrap().last_message_id,
        20
    );
}

#[tokio::test]
async fn redelivery_after_failed_ack_replays_idempotently() {
    let server = MockServer::start().await;
    let db = temp_db("redelivery");
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let (mut listener, _lane, metrics) = listener(&server, &db, 2).await;
    let batch = fixtures.read("message_deferred_offer.json").unwrap();

    // Two identical polls (redelivery), then the high-water probe.
    // Same-matcher mocks fall through in mount order once `up_to_n_times`
    // exhausts; the query-specific probe wins by priority.
    Mock::given(method("GET"))
        .and(path(queue_path()))
        .and(query_param("lastMessageId", "20"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixtures.read("message_high_water.json").unwrap()),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(queue_path()))
        .respond_with(ResponseTemplate::new(200).set_body_string(batch.clone()))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(queue_path()))
        .respond_with(ResponseTemplate::new(200).set_body_string(batch))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // First ACK fails; the retry succeeds.
    Mock::given(method("DELETE"))
        .and(path(format!("{}/20", queue_path())))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{}/20", queue_path())))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    mount_ack(&server, 41).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/acquirejobs"
        )))
        .and(body_json(serde_json::json!([4244])))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1, "value": [4244]
        })))
        .mount(&server)
        .await;

    let failed = listener.run_once().await;
    assert!(failed.is_err(), "failed ACK must fail the poll");
    // Cursor did not advance past the un-ACKed message.
    let cursors = SessionStore::open(&db).unwrap();
    assert_eq!(
        cursors.get(SCALE_SET_ID).unwrap().unwrap().last_message_id,
        0
    );
    let before = DemandStore::open(&db)
        .unwrap()
        .get(SCALE_SET_ID, 4244)
        .unwrap()
        .unwrap();

    let replayed = listener.run_once().await.unwrap();
    assert_eq!(replayed.acquired, Vec::<i64>::new());
    let after = DemandStore::open(&db)
        .unwrap()
        .get(SCALE_SET_ID, 4244)
        .unwrap()
        .unwrap();
    assert_eq!(after.first_seen_at, before.first_seen_at);
    assert_eq!(after.sequence, before.sequence);

    let probed = listener.run_once().await.unwrap();
    assert!(matches!(probed.kind, ScaleKind::Message { message_id: 41 }));
    assert_eq!(metrics.snapshot().acks, 2);
    // Exactly one acquire call across both deliveries: no double-spend.
    let acquire_calls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/acquirejobs"))
        .count();
    assert_eq!(acquire_calls, 1);
}

#[tokio::test]
async fn partial_acquire_releases_missing_and_requeues_with_age() {
    let server = MockServer::start().await;
    let db = temp_db("partial");
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let (mut listener, _lane, metrics) = listener(&server, &db, 2).await;

    // A second grantable offer so the grant set is [4244, 4246].
    let mut demand = DemandStore::open(&db).unwrap();
    demand
        .submit_offer(SCALE_SET_ID, &push_offer(4246, "push"), 0)
        .unwrap();

    Mock::given(method("GET"))
        .and(path(queue_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixtures.read("message_deferred_offer.json").unwrap()),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/acquirejobs"
        )))
        .and(body_json(serde_json::json!([4246, 4244])))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixtures.read("acquire_partial.json").unwrap()),
        )
        .mount(&server)
        .await;
    mount_ack(&server, 20).await;

    let outcome = listener.run_once().await.unwrap();
    assert_eq!(outcome.acquired, vec![4244]);
    assert_eq!(outcome.missing, vec![4246]);
    assert_eq!(outcome.provisioned, vec![4244]);

    let demand = DemandStore::open(&db).unwrap();
    let missing = demand.get(SCALE_SET_ID, 4246).unwrap().unwrap();
    assert_eq!(missing.state, DemandState::Eligible);
    assert_eq!(missing.sequence, 1);
    assert_eq!(
        demand.get(SCALE_SET_ID, 4244).unwrap().unwrap().state,
        DemandState::ProvisionIntent
    );
    assert_eq!(listener.processor().ledger_ref().occupied().unwrap(), 1);
    assert_eq!(metrics.snapshot().missing_ids, 1);
}

#[tokio::test]
async fn uncertain_acquire_resolves_on_idle_reacquire() {
    let server = MockServer::start().await;
    let db = temp_db("uncertain");
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let (mut listener, lane, metrics) = listener(&server, &db, 2).await;

    Mock::given(method("GET"))
        .and(path(queue_path()))
        .and(query_param("lastMessageId", "20"))
        .respond_with(ResponseTemplate::new(202))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(queue_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixtures.read("message_deferred_offer.json").unwrap()),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/acquirejobs"
        )))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/acquirejobs"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1, "value": [4244]
        })))
        .mount(&server)
        .await;
    mount_ack(&server, 20).await;

    // Transport failure after intent: uncertain, counted, and ACKable.
    let outcome = listener.run_once().await.unwrap();
    assert_eq!(outcome.uncertain, vec![4244]);
    assert!(outcome.provisioned.is_empty());
    assert_eq!(metrics.snapshot().acks, 1);
    assert_eq!(listener.processor().ledger_ref().occupied().unwrap(), 1);

    // Age the batch past the re-acquire horizon, then idle-poll.
    let old = velnor_model::Timestamp::now()
        .minus(Duration::from_secs(3600))
        .to_rfc3339()
        .unwrap();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("UPDATE scaleset_acquire_batches SET created_at = ?1", [old])
        .unwrap();
    let idle = listener.run_once().await.unwrap();
    assert_eq!(idle.kind, ScaleKind::Nil);
    assert_eq!(idle.provisioned, vec![4244]);
    let demand = DemandStore::open(&db).unwrap();
    assert_eq!(
        demand.get(SCALE_SET_ID, 4244).unwrap().unwrap().state,
        DemandState::ProvisionIntent
    );
    assert_eq!(lane.state.provisioned.lock().unwrap().len(), 1);
    // Idle work resumes acquisition and provisioning. Occupancy stays 1.
    assert_eq!(listener.processor().ledger_ref().occupied().unwrap(), 1);
}

#[tokio::test]
async fn queue_401_refreshes_and_loop_continues() {
    let server = MockServer::start().await;
    let db = temp_db("refresh");
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let (mut listener, _lane, _metrics) = listener(&server, &db, 2).await;

    // The recorded refresh answer, re-pointed at the mock queue.
    let mut refreshed: serde_json::Value = fixtures.parse("session_refreshed.json").unwrap();
    refreshed["messageQueueUrl"] = serde_json::json!(format!("{}{}", server.uri(), queue_path()));
    refreshed["messageQueueAccessToken"] = serde_json::json!("queue-token-2");

    Mock::given(method("GET"))
        .and(path(queue_path()))
        .and(header("Authorization", "Bearer queue-token-1"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions/{SESSION_ID}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(refreshed))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(queue_path()))
        .and(header("Authorization", "Bearer queue-token-2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixtures.read("message_high_water.json").unwrap()),
        )
        .mount(&server)
        .await;
    mount_ack(&server, 41).await;

    let outcome = listener.run_once().await.unwrap();
    assert!(matches!(
        outcome.kind,
        ScaleKind::Message { message_id: 41 }
    ));
    let cursors = SessionStore::open(&db).unwrap();
    assert_eq!(
        cursors.get(SCALE_SET_ID).unwrap().unwrap().last_message_id,
        41
    );
}

#[tokio::test]
async fn stale_generation_seed_resets_and_regrants_with_age_kept() {
    let db = temp_db("stale-seed");
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let seed = fixtures.demand_seed("seed_stale_generation.json").unwrap();
    let metrics = Metrics::new();
    let mut ledger = MemLedger::new();
    ledger.set_max_jobs(4);
    let mut demand = DemandStore::open(&db).unwrap();
    let mut batches = AcquireBatchStore::open(&db).unwrap();

    // Prior epoch: submit + grant everything.
    let prior = ledger.generation().unwrap();
    for row in &seed.demand {
        demand
            .submit_offer(
                seed.scale_set_id,
                &push_offer(row.request_id, &row.event_name),
                prior,
            )
            .unwrap();
    }
    let granted = grant_oldest(&mut demand, seed.scale_set_id, prior, &metrics).unwrap();
    assert_eq!(granted.len(), 2);
    let ages: Vec<(String, i64)> = seed
        .demand
        .iter()
        .map(|row| {
            let stored = demand.get(SCALE_SET_ID, row.request_id).unwrap().unwrap();
            (stored.first_seen_at, stored.sequence)
        })
        .collect();

    // New epoch: startup resets the stale grants, then they re-grant fresh.
    ledger.begin_epoch();
    let report = startup(
        &mut ledger,
        &mut demand,
        &mut batches,
        seed.scale_set_id,
        &metrics,
    )
    .unwrap();
    assert_eq!(report.stale_grants_reset, 2);
    let current = ledger.generation().unwrap();
    assert_eq!(current, prior + 1);
    let granted = grant_oldest(&mut demand, seed.scale_set_id, current, &metrics).unwrap();
    assert_eq!(granted.len(), 2);
    for (row, (first_seen_at, sequence)) in seed.demand.iter().zip(ages.iter()) {
        let stored = demand.get(SCALE_SET_ID, row.request_id).unwrap().unwrap();
        assert_eq!(stored.state, DemandState::Granted);
        assert_eq!(stored.generation, current);
        assert_eq!(&stored.first_seen_at, first_seen_at);
        assert_eq!(&stored.sequence, sequence);
    }
}

#[tokio::test]
async fn reordered_batch_folds_order_independently() {
    let server = MockServer::start().await;
    let db = temp_db("reorder");
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let (mut listener, lane, _metrics) = listener(&server, &db, 2).await;

    // Track 4250 as acquired-with-permit before its observations arrive.
    let mut demand = DemandStore::open(&db).unwrap();
    demand
        .submit_offer(SCALE_SET_ID, &push_offer(4250, "push"), 0)
        .unwrap();
    demand
        .set_state(SCALE_SET_ID, 4250, DemandState::Acquired, None, 0)
        .unwrap();

    Mock::given(method("GET"))
        .and(path(queue_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixtures.read("message_reordered.json").unwrap()),
        )
        .mount(&server)
        .await;
    mount_ack(&server, 21).await;

    let outcome = listener.run_once().await.unwrap();
    assert_eq!(outcome.completed, 1);
    // Assigned/started replay against the terminal row: tracked, no-ops.
    assert_eq!(outcome.assigned, 1);
    assert_eq!(outcome.started, 1);

    let demand = DemandStore::open(&db).unwrap();
    assert_eq!(
        demand.get(SCALE_SET_ID, 4250).unwrap().unwrap().state,
        DemandState::Terminal
    );
    // Startup reconcile adopted the attested holder; completion moved it to
    // cleaning while the lane schedules export + cleanup.
    assert_eq!(
        listener
            .processor()
            .ledger_ref()
            .holder_state("scaleset/7/4250")
            .unwrap(),
        Some(velnor_runner::scaleset::LedgerPermitState::Cleaning)
    );
    assert_eq!(lane.state.terminals.lock().unwrap().as_slice(), &[4250]);
}

#[tokio::test]
async fn unknown_kind_counts_and_processes_live_offer() {
    let server = MockServer::start().await;
    let db = temp_db("unknown");
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    let (mut listener, _lane, metrics) = listener(&server, &db, 2).await;

    Mock::given(method("GET"))
        .and(path(queue_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixtures.read("message_unknown_kind.json").unwrap()),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/acquirejobs"
        )))
        .and(body_json(serde_json::json!([4260])))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1, "value": [4260]
        })))
        .mount(&server)
        .await;
    mount_ack(&server, 22).await;

    let outcome = listener.run_once().await.unwrap();
    assert_eq!(outcome.acquired, vec![4260]);
    assert_eq!(outcome.provisioned, vec![4260]);
    assert_eq!(metrics.snapshot().unknown_events, 1);
    assert_eq!(metrics.snapshot().acks, 1);
}

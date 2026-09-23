//! Worker-lane conformance: recorded profile fixture + full lifecycle.
//!
//! * The `worker_profile.json` fixture loads through the verified
//!   [`Fixtures`] bundle (pin + per-file hash + redaction) and matches
//!   the lane's compiled pins exactly.
//! * A scripted [`WorkerRunner`] drives acquire → provision → supervise →
//!   terminal → export → cleanup → release end to end: the permit is held
//!   across the whole lifecycle and freed only after confirmed cleanup.
//! * Cleanup failure retains the permit uncertain instead of releasing.

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

use std::collections::{BTreeMap, VecDeque};

use velnor_control::permit_ledger::{PermitLedger, PermitState};
use velnor_model::ScaleSetWorkerState;
use velnor_runner::scaleset::allocator::ScaleSetAllocator;
use velnor_runner::scaleset::permit_holder;
use velnor_runner::scaleset::worker::{dind, runner};
use velnor_runner::scaleset::worker::{
    provision_worker, DockerToolContentHook, HomogeneousProfile, OwnershipId, PinnedImage,
    ProvisionPlan, ScaleSetWorker, Supervision, SupervisionOutcome, ToolContentAttestation,
    ToolContentExpectation, ToolContentHook, VecEdgeSink, WorkerIdentity, WorkerOutput,
    WorkerRunner, DIND_DIGEST_AMD64, DIND_DIGEST_ARM64, DIND_INDEX_DIGEST, DIND_REPOSITORY,
    DIND_VERSION, RUNNER_DIGEST_AMD64, RUNNER_DIGEST_ARM64, RUNNER_INDEX_DIGEST, RUNNER_REPOSITORY,
    RUNNER_VERSION,
};
use velnor_runner::scaleset::Fixtures;

fn fixture_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("scaleset-worker")
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "velnor-worker-lane-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct ScriptRunner {
    results: VecDeque<WorkerOutput>,
    seen: Vec<Vec<String>>,
}

impl ScriptRunner {
    fn scripted(results: Vec<WorkerOutput>) -> Self {
        Self {
            results: results.into(),
            seen: Vec::new(),
        }
    }

    fn ok(stdout: &str) -> WorkerOutput {
        WorkerOutput {
            code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    fn fail(code: i32, stderr: &str) -> WorkerOutput {
        WorkerOutput {
            code,
            stdout: String::new(),
            stderr: stderr.to_string(),
        }
    }
}

impl WorkerRunner for ScriptRunner {
    fn run(&mut self, program: &str, args: &[String]) -> anyhow::Result<WorkerOutput> {
        assert_eq!(program, "docker");
        self.seen.push(args.to_vec());
        self.results
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("script exhausted at docker {}", args.join(" ")))
    }
}

/// Test-only hook: production uses DockerToolContentHook's real runner
/// provenance verifier; this lifecycle test isolates Docker ownership/order.
struct CompleteTestHook;

impl ToolContentHook for CompleteTestHook {
    fn verify(
        &self,
        runner: &mut dyn WorkerRunner,
        image: &PinnedImage,
        expected: &ToolContentExpectation,
    ) -> anyhow::Result<ToolContentAttestation> {
        DockerToolContentHook.verify(runner, image, expected)
    }

    fn verify_platform(
        &self,
        _runner: &mut dyn WorkerRunner,
        _image: &PinnedImage,
        _expected: &velnor_runner::scaleset::worker::ImagePlatform,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn verify_attestation(
        &self,
        _runner: &mut dyn WorkerRunner,
        _image: &PinnedImage,
        _expected: &ToolContentExpectation,
        _attestation: &ToolContentAttestation,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn verify_signature(
        &self,
        _runner: &mut dyn WorkerRunner,
        _image: &PinnedImage,
        _expected: &ToolContentExpectation,
        _attestation: &ToolContentAttestation,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

fn identity_labels(identity: &WorkerIdentity, role: &str) -> BTreeMap<String, String> {
    let mut labels = identity.labels();
    labels.insert("velnor.scaleset.role".to_owned(), role.to_owned());
    labels
}

fn owned_container_projection(identity: &WorkerIdentity, role: &str, id: &str) -> String {
    format!(
        "{}\t{}\n",
        serde_json::to_string(id).unwrap(),
        serde_json::to_string(&identity_labels(identity, role)).unwrap()
    )
}

fn owned_network_projection(identity: &WorkerIdentity, id: &str) -> String {
    format!(
        "{}\t{}\n",
        serde_json::to_string(id).unwrap(),
        serde_json::to_string(&identity.labels()).unwrap()
    )
}

fn holder_projection(identity: &WorkerIdentity, id: &str) -> String {
    let mounts = serde_json::json!([
        {"Type":"volume","Name":"anonymous-work","Source":"/var/lib/docker/volumes/anonymous-work/_data","Destination":"/home/runner/_work","Driver":"local","RW":true},
        {"Type":"volume","Name":"anonymous-tools","Source":"/var/lib/docker/volumes/anonymous-tools/_data","Destination":"/opt/hostedtoolcache","Driver":"local","RW":true},
        {"Type":"volume","Name":"anonymous-docker","Source":"/var/lib/docker/volumes/anonymous-docker/_data","Destination":"/var/lib/docker","Driver":"local","RW":true}
    ]);
    format!(
        "{}\t{}\t{}\tnull\t{}\t{}\t{}\n",
        serde_json::to_string(id).unwrap(),
        serde_json::to_string(&format!("{RUNNER_REPOSITORY}@{RUNNER_INDEX_DIGEST}")).unwrap(),
        serde_json::to_string(&identity_labels(identity, "volume-holder")).unwrap(),
        serde_json::to_string(&vec!["/bin/true"]).unwrap(),
        serde_json::to_string("created").unwrap(),
        mounts
    )
}

fn holder_volume_projection(identity: &WorkerIdentity, name: &str) -> String {
    let labels = identity_labels(identity, "volume-holder");
    [
        serde_json::to_string(name).unwrap(),
        serde_json::to_string("local").unwrap(),
        serde_json::to_string(&labels).unwrap(),
        "null".to_owned(),
        serde_json::to_string(&format!("/var/lib/docker/volumes/{name}/_data")).unwrap(),
    ]
    .join("\t")
}

fn holder_isolation_projection(identity: &WorkerIdentity, id: &str) -> String {
    let labels = identity_labels(identity, "volume-holder");
    let mounts = serde_json::json!([
        {"Type":"volume","Name":"anonymous-work","Source":"/var/lib/docker/volumes/anonymous-work/_data","Destination":"/home/runner/_work","Driver":"local","RW":true,"Propagation":""},
        {"Type":"volume","Name":"anonymous-tools","Source":"/var/lib/docker/volumes/anonymous-tools/_data","Destination":"/opt/hostedtoolcache","Driver":"local","RW":true,"Propagation":""},
        {"Type":"volume","Name":"anonymous-docker","Source":"/var/lib/docker/volumes/anonymous-docker/_data","Destination":"/var/lib/docker","Driver":"local","RW":true,"Propagation":""}
    ]);
    let requested = serde_json::json!([
        {"Type":"volume","Target":"/home/runner/_work","Source":""},
        {"Type":"volume","Target":"/opt/hostedtoolcache","Source":""},
        {"Type":"volume","Target":"/var/lib/docker","Source":""}
    ]);
    [
        serde_json::to_string(id).unwrap(),
        mounts.to_string(),
        "false".to_owned(),
        "{}".to_owned(),
        "false".to_owned(),
        "null".to_owned(),
        "null".to_owned(),
        serde_json::to_string(&vec!["/bin/true"]).unwrap(),
        serde_json::to_string("").unwrap(),
        serde_json::to_string(&labels).unwrap(),
        requested.to_string(),
    ]
    .join("\t")
}

fn container_isolation_projection(
    identity: &WorkerIdentity,
    id: &str,
    role: &str,
    state_dir: &std::path::Path,
) -> String {
    let mut labels = identity_labels(identity, role);
    labels.insert(
        "velnor.scaleset.state-source".to_owned(),
        state_dir.display().to_string(),
    );
    let mounts = serde_json::json!([
        {"Type":"volume","Name":"anonymous-work","Source":"/var/lib/docker/volumes/anonymous-work/_data","Destination":"/home/runner/_work","Driver":"local","RW":true,"Propagation":""},
        {"Type":"volume","Name":"anonymous-tools","Source":"/var/lib/docker/volumes/anonymous-tools/_data","Destination":"/opt/hostedtoolcache","Driver":"local","RW":true,"Propagation":""},
        {"Type":"volume","Name":"anonymous-docker","Source":"/var/lib/docker/volumes/anonymous-docker/_data","Destination":"/var/lib/docker","Driver":"local","RW":true,"Propagation":""},
        {"Type":"bind","Source":state_dir,"Destination":"/velnor/scaleset","RW":true,"Propagation":"rprivate"}
    ]);
    let groups = if role == "runner" {
        serde_json::json!([dind::DIND_SOCKET_GID])
    } else {
        serde_json::Value::Null
    };
    let entrypoint = if role == "dind" {
        serde_json::json!([dind::DIND_ENTRYPOINT])
    } else {
        serde_json::Value::Null
    };
    let command = if role == "dind" {
        serde_json::json!(dind::daemon_command())
    } else {
        serde_json::json!([runner::RUNNER_START_COMMAND])
    };
    [
        serde_json::json!(id),
        mounts,
        serde_json::json!(role == "dind"),
        serde_json::json!({}),
        serde_json::json!(false),
        groups,
        entrypoint,
        command,
        serde_json::json!(if role == "dind" { "" } else { "runner" }),
        serde_json::json!(labels),
        serde_json::Value::Null,
    ]
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join("\t")
}

#[test]
fn recorded_profile_matches_compiled_pins() {
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    assert_eq!(fixtures.manifest().files.len(), 1);
    let profile: serde_json::Value = fixtures.parse("worker_profile.json").unwrap();
    assert_eq!(profile["profile"], "homogeneous");

    let runner = &profile["runner"];
    assert_eq!(runner["repository"], RUNNER_REPOSITORY);
    assert_eq!(runner["version"], RUNNER_VERSION);
    assert_eq!(runner["index_digest"], RUNNER_INDEX_DIGEST);
    assert_eq!(runner["digest_amd64"], RUNNER_DIGEST_AMD64);
    assert_eq!(runner["digest_arm64"], RUNNER_DIGEST_ARM64);
    // Every recorded digest parses as a pin (no tags in the fixture).
    for digest in ["index_digest", "digest_amd64", "digest_arm64"] {
        let reference = format!(
            "{}@{}",
            runner["repository"].as_str().unwrap(),
            runner[digest].as_str().unwrap()
        );
        assert!(PinnedImage::parse(&reference).is_ok(), "{reference}");
    }

    let dind = &profile["dind"];
    assert_eq!(dind["repository"], DIND_REPOSITORY);
    assert_eq!(dind["version"], DIND_VERSION);
    assert_eq!(dind["index_digest"], DIND_INDEX_DIGEST);
    assert_eq!(dind["digest_amd64"], DIND_DIGEST_AMD64);
    assert_eq!(dind["digest_arm64"], DIND_DIGEST_ARM64);

    assert_eq!(profile["socket"], "/velnor/scaleset/dind.sock");
    assert_eq!(profile["jit_env"], "ACTIONS_RUNNER_INPUT_JITCONFIG");
}

#[test]
fn full_lifecycle_holds_one_permit_until_confirmed_cleanup() {
    let root = temp_dir("lifecycle");
    let ledger_path = root.join("permit-ledger.db");
    let mut ledger = PermitLedger::open(&ledger_path).unwrap();
    ledger.set_max_jobs(1).unwrap();
    ledger.begin_epoch().unwrap();
    ledger.reconcile(&[]).unwrap();

    let allocator = ScaleSetAllocator::open(&ledger_path);
    let holder = permit_holder(7, 4242);
    let guard = allocator
        .acquire(&holder)
        .unwrap()
        .expect("N=1 grants once");
    // Full while held: a second acquisition is refused, not queued.
    assert!(allocator
        .acquire(&permit_holder(7, 4243))
        .unwrap()
        .is_none());

    let identity = WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007"));
    let mut worker = ScaleSetWorker::new(identity.clone(), "op-4242");
    worker.bind_request(4242);
    worker.bind_permit(&holder);
    let mut sink = VecEdgeSink::default();
    for state in [
        ScaleSetWorkerState::Eligible,
        ScaleSetWorkerState::Reserved,
        ScaleSetWorkerState::AcquireIntent,
        ScaleSetWorkerState::Acquired,
        ScaleSetWorkerState::ProvisionIntent,
    ] {
        worker.transition(&mut sink, state).unwrap();
    }

    let dind_ref = format!("{DIND_REPOSITORY}@{DIND_INDEX_DIGEST}");
    let runner_ref = format!("{RUNNER_REPOSITORY}@{RUNNER_INDEX_DIGEST}");
    let state_dir = root.join("worker");
    let plan = ProvisionPlan {
        identity: identity.clone(),
        profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
        state_dir: state_dir.clone(),
        jit_config: "jit-blob".to_string(),
        ready_attempts: 2,
    };
    let mut script = ScriptRunner::scripted(vec![
        // Tool-content hook × 2.
        ScriptRunner::ok("pulled\n"),
        ScriptRunner::ok(&format!("[\"{dind_ref}\"]\n")),
        ScriptRunner::ok("null\n"),
        ScriptRunner::ok("sha256:beef\n"),
        ScriptRunner::ok("pulled\n"),
        ScriptRunner::ok(&format!("[\"{runner_ref}\"]\n")),
        ScriptRunner::ok(
            r#"{"org.opencontainers.image.source":"https://github.com/actions/runner"}"#,
        ),
        ScriptRunner::ok("sha256:feed\n"),
        // Network + DinD + ready + runner + connected.
        ScriptRunner::fail(
            1,
            "Error: No such network: velnor-scaleset-net-s7-velnor-set-0007-2ad92676",
        ),
        ScriptRunner::ok("netid\n"),
        ScriptRunner::fail(
            1,
            "Error: No such container: velnor-scaleset-volume-holder-s7-velnor-set-0007-2ad92676",
        ),
        ScriptRunner::fail(
            1,
            "Error: No such container: velnor-scaleset-dind-s7-velnor-set-0007-2ad92676",
        ),
        ScriptRunner::fail(
            1,
            "Error: No such container: velnor-scaleset-runner-s7-velnor-set-0007-2ad92676",
        ),
        ScriptRunner::ok("holderid\n"),
        ScriptRunner::ok(&holder_projection(&identity, "holderid")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-work")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-tools")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-docker")),
        ScriptRunner::ok(&holder_isolation_projection(&identity, "holderid")),
        ScriptRunner::fail(
            1,
            "Error: No such container: velnor-scaleset-dind-s7-velnor-set-0007-2ad92676",
        ),
        ScriptRunner::ok("dindid\n"),
        ScriptRunner::ok(&container_isolation_projection(
            &identity, "dindid", "dind", &state_dir,
        )),
        ScriptRunner::ok("velnor-scaleset-dind-s7-velnor-set-0007-2ad92676\n"),
        ScriptRunner::ok("28.5.2\n"),
        ScriptRunner::fail(
            1,
            "Error: No such container: velnor-scaleset-runner-s7-velnor-set-0007-2ad92676",
        ),
        ScriptRunner::ok("runnerid\n"),
        ScriptRunner::ok(&container_isolation_projection(
            &identity, "runnerid", "runner", &state_dir,
        )),
        ScriptRunner::ok("velnor-scaleset-runner-s7-velnor-set-0007-2ad92676\n"),
        ScriptRunner::ok("running\n"),
        ScriptRunner::ok("Connected to GitHub\n"),
        // Supervision tick: healthy.
        ScriptRunner::ok("running\n"),
        ScriptRunner::ok("running\n"),
        ScriptRunner::ok("Connected to GitHub\n"),
        // Supervision tick: runner died mid-job → fail (no restart).
        ScriptRunner::ok("running\n"),
        ScriptRunner::ok("exited\n"),
        // Owned cleanup: preflight, stop, export ×4, preflight, then remove.
        ScriptRunner::ok(&owned_container_projection(
            &identity,
            "runner",
            "runner-id",
        )),
        ScriptRunner::ok(&owned_container_projection(&identity, "dind", "dind-id")),
        ScriptRunner::ok(&owned_network_projection(&identity, "network-id")),
        ScriptRunner::ok(&holder_projection(&identity, "holder-id")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-work")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-tools")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-docker")),
        ScriptRunner::ok(&holder_isolation_projection(&identity, "holder-id")),
        ScriptRunner::ok("runner\n"),
        ScriptRunner::ok("RUNNER-LOGS\n"),
        ScriptRunner::ok("DIND-LOGS\n"),
        ScriptRunner::ok("[{}]\n"),
        ScriptRunner::ok("[{}]\n"),
        ScriptRunner::ok(&owned_container_projection(
            &identity,
            "runner",
            "runner-id",
        )),
        ScriptRunner::ok(&owned_container_projection(&identity, "dind", "dind-id")),
        ScriptRunner::ok(&owned_network_projection(&identity, "network-id")),
        ScriptRunner::ok(&holder_projection(&identity, "holder-id")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-work")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-tools")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-docker")),
        ScriptRunner::ok(&holder_isolation_projection(&identity, "holder-id")),
        ScriptRunner::ok("runner\n"),
        ScriptRunner::ok("dind\n"),
        ScriptRunner::ok("dind\n"),
        ScriptRunner::ok("holder\n"),
        ScriptRunner::ok("net\n"),
    ]);
    let outcome = provision_worker(&mut script, &CompleteTestHook, &plan, &|_| {}, &mut || {
        Ok(())
    })
    .unwrap();
    assert_eq!(outcome.dind_attestation.content_version, DIND_VERSION);
    assert_eq!(outcome.runner_attestation.content_version, RUNNER_VERSION);
    worker.record_versions(
        &outcome.runner_attestation.content_version,
        &outcome.dind_attestation.content_version,
    );
    worker
        .transition(&mut sink, ScaleSetWorkerState::DindReady)
        .unwrap();
    worker
        .transition(&mut sink, ScaleSetWorkerState::RunnerConnected)
        .unwrap();
    worker
        .transition(&mut sink, ScaleSetWorkerState::Running)
        .unwrap();
    guard.transition_running();

    let mut supervision = Supervision::new(identity.clone(), &state_dir);
    assert_eq!(
        supervision.tick(&mut script, worker.state()).unwrap(),
        SupervisionOutcome::Healthy
    );
    let failed = supervision.tick(&mut script, worker.state()).unwrap();
    assert!(
        matches!(failed, SupervisionOutcome::WorkerFailed { .. }),
        "{failed:?}"
    );
    // No restart ran for the dead runner: no `start` after the failure.
    let verbs: Vec<String> = script.seen.iter().map(|argv| argv.join(" ")).collect();
    assert_eq!(verbs.iter().filter(|v| v.starts_with("start ")).count(), 2);

    worker
        .transition(&mut sink, ScaleSetWorkerState::Terminal)
        .unwrap();
    worker
        .transition(&mut sink, ScaleSetWorkerState::DiagnosticExport)
        .unwrap();
    let report = supervision.cleanup(&mut script).unwrap();
    assert!(report.confirmed(), "{report:?}");
    assert_eq!(
        std::fs::read_to_string(&report.export.runner_log).unwrap(),
        "RUNNER-LOGS\n"
    );
    worker
        .transition(&mut sink, ScaleSetWorkerState::OwnedCleanup)
        .unwrap();
    worker
        .transition(&mut sink, ScaleSetWorkerState::PermitReleased)
        .unwrap();
    guard.release();
    // The permit is gone: the state dir (raw job logs) is deleted with it.
    supervision.release_state().unwrap();
    assert!(!state_dir.exists());

    // The ledger is empty again and the freed N grants immediately.
    assert_eq!(allocator.occupied().unwrap(), 0);
    assert!(allocator
        .acquire(&permit_holder(7, 4243))
        .unwrap()
        .is_some());
    assert_eq!(sink.edges().len(), 12);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn cleanup_failure_retains_permit_uncertain() {
    let root = temp_dir("retain");
    let ledger_path = root.join("permit-ledger.db");
    let mut ledger = PermitLedger::open(&ledger_path).unwrap();
    ledger.set_max_jobs(1).unwrap();
    ledger.begin_epoch().unwrap();
    ledger.reconcile(&[]).unwrap();

    let allocator = ScaleSetAllocator::open(&ledger_path);
    let holder = permit_holder(7, 4242);
    let guard = allocator.acquire(&holder).unwrap().expect("grants");

    let identity = WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007"));
    let state_dir = root.join("worker");
    std::fs::create_dir_all(&state_dir).unwrap();
    let supervision = Supervision::new(identity.clone(), &state_dir);
    let mut script = ScriptRunner::scripted(vec![
        ScriptRunner::ok(&owned_container_projection(
            &identity,
            "runner",
            "runner-id",
        )),
        ScriptRunner::ok(&owned_container_projection(&identity, "dind", "dind-id")),
        ScriptRunner::ok(&owned_network_projection(&identity, "network-id")),
        ScriptRunner::ok(&holder_projection(&identity, "holder-id")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-work")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-tools")),
        ScriptRunner::ok(&holder_volume_projection(&identity, "anonymous-docker")),
        ScriptRunner::ok(&holder_isolation_projection(&identity, "holder-id")),
        ScriptRunner::ok("runner\n"),
        ScriptRunner::ok("RUNNER-LOGS\n"),
        ScriptRunner::ok("DIND-LOGS\n"),
        ScriptRunner::ok("[{}]\n"),
        ScriptRunner::ok("[{}]\n"),
        ScriptRunner::ok("runner\n"),
        ScriptRunner::ok("dind\n"),
        ScriptRunner::fail(1, "device or resource busy"), // rm dind fails
        ScriptRunner::ok("net\n"),
    ]);
    let report = supervision.cleanup(&mut script).unwrap();
    assert!(!report.confirmed());
    // Cleanup failed: retain, do not release.
    guard.mark_uncertain_and_disarm();
    assert_eq!(allocator.occupied().unwrap(), 1);
    let ledger = PermitLedger::open(&ledger_path).unwrap();
    assert_eq!(
        ledger.holder_state(&holder).unwrap(),
        Some(PermitState::Uncertain)
    );
    // Still full: the residue occupies its N.
    assert!(allocator
        .acquire(&permit_holder(7, 4243))
        .unwrap()
        .is_none());
    std::fs::remove_dir_all(&root).unwrap();
}

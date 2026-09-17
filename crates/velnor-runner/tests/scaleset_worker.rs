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

use std::collections::VecDeque;

use velnor_control::permit_ledger::{PermitLedger, PermitState};
use velnor_model::ScaleSetWorkerState;
use velnor_runner::scaleset::allocator::ScaleSetAllocator;
use velnor_runner::scaleset::permit_holder;
use velnor_runner::scaleset::worker::{
    provision_worker, DockerToolContentHook, HomogeneousProfile, OwnershipId, PinnedImage,
    ProvisionPlan, ScaleSetWorker, Supervision, SupervisionOutcome, VecEdgeSink, WorkerIdentity,
    WorkerOutput, WorkerRunner, DIND_DIGEST_AMD64, DIND_DIGEST_ARM64, DIND_INDEX_DIGEST,
    DIND_REPOSITORY, DIND_VERSION, RUNNER_DIGEST_AMD64, RUNNER_DIGEST_ARM64, RUNNER_INDEX_DIGEST,
    RUNNER_REPOSITORY, RUNNER_VERSION,
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
        ScriptRunner::ok(""),
        ScriptRunner::ok("netid\n"),
        ScriptRunner::ok(""),
        ScriptRunner::ok("dindid\n"),
        ScriptRunner::ok("velnor-scaleset-dind-s7-velnor-set-0007\n"),
        ScriptRunner::ok("28.5.2\n"),
        ScriptRunner::ok(""),
        ScriptRunner::ok("runnerid\n"),
        ScriptRunner::ok("velnor-scaleset-runner-s7-velnor-set-0007\n"),
        ScriptRunner::ok("true\n"),
        ScriptRunner::ok("Connected to GitHub\n"),
        // Supervision tick: healthy.
        ScriptRunner::ok("true\n"),
        ScriptRunner::ok("true\n"),
        ScriptRunner::ok("Connected to GitHub\n"),
        // Supervision tick: runner died mid-job → fail (no restart).
        ScriptRunner::ok("true\n"),
        ScriptRunner::ok("false\n"),
        // Owned cleanup: stop, export ×4, rm ×2, network, volumes ×2.
        ScriptRunner::ok("runner\n"),
        ScriptRunner::ok("RUNNER-LOGS\n"),
        ScriptRunner::ok("DIND-LOGS\n"),
        ScriptRunner::ok("{}\n"),
        ScriptRunner::ok("{}\n"),
        ScriptRunner::ok("runner\n"),
        ScriptRunner::ok("dind\n"),
        ScriptRunner::ok("dind\n"),
        ScriptRunner::ok("net\n"),
        ScriptRunner::ok("work\n"),
        ScriptRunner::ok("dindata\n"),
    ]);
    let outcome = provision_worker(&mut script, &DockerToolContentHook, &plan, &|_| {}).unwrap();
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
    let supervision = Supervision::new(identity, &state_dir);
    let mut script = ScriptRunner::scripted(vec![
        ScriptRunner::ok("runner\n"),
        ScriptRunner::ok("RUNNER-LOGS\n"),
        ScriptRunner::ok("DIND-LOGS\n"),
        ScriptRunner::ok("{}\n"),
        ScriptRunner::ok("{}\n"),
        ScriptRunner::ok("runner\n"),
        ScriptRunner::ok("dind\n"),
        ScriptRunner::fail(1, "device or resource busy"), // rm dind fails
        ScriptRunner::ok("net\n"),
        ScriptRunner::ok("work\n"),
        ScriptRunner::ok("dindata\n"),
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

//! Homogeneous scale-set worker lane.
//!
//! One worker = one acquired GitHub job = one official runner container
//! paired with its own private DinD daemon. Every worker provisions from
//! the same [`HomogeneousProfile`](runner::HomogeneousProfile), so any
//! worker can serve any acquired job.
//!
//! Lifecycle ([`ScaleSetWorkerState`], mirrored from `velnor-model`):
//!
//! ```text
//! observed → eligible → reserved → acquire_intent → acquired ─┐
//!       │         │          │           │                     │
//!       │         │          │           └→ uncertain ─────────┤
//!       │         │          │                                 ▼
//!       │         │          │                         provision_intent
//!       │         │          │                                 │
//!       └─────────┴──────────┴→ terminal → diagnostic_export → owned_cleanup
//!                                                              │
//!                                        dind_ready ←───────────┤ (provision path)
//!                                            │                 │
//!                                     runner_connected ────────┤
//!                                            │                 │
//!                                         running ─────────────┘
//!                                            │
//!                                            ▼
//!                                  ... → permit_released
//! ```
//!
//! Every edge is validated by [`legal_edge`] and recorded through an
//! [`EdgeSink`] (the journal `ScaleSetWorkerEdge` event once the journal
//! extension lands; the sink keeps the machine independent of the store).
//! Supervision ([`supervise`]) reconciles observed Docker state against
//! the recorded state at every boundary before advancing.

pub mod dind;
pub mod ownership;
pub mod runner;
pub mod supervise;

pub use dind::{
    DindProvision, DindSpec, NetworkProvision, VolumeHolderProvision, BUILDKIT_CACHE_DIR,
    DIND_READY_POLL_INTERVAL, DIND_READY_TIMEOUT, DIND_SOCKET, STATE_MOUNT, WORK_DIR,
};
pub use ownership::{OwnershipId, WorkerIdentity};
pub use runner::{
    DockerToolContentHook, HomogeneousProfile, ImagePlatform, InvalidPinnedImage, PinnedImage,
    RunnerConnection, RunnerProvision, RunnerSpec, ToolContentAttestation, ToolContentExpectation,
    ToolContentHook, DIND_DIGEST_AMD64, DIND_DIGEST_ARM64, DIND_INDEX_DIGEST, DIND_REPOSITORY,
    DIND_VERSION, RUNNER_DIGEST_AMD64, RUNNER_DIGEST_ARM64, RUNNER_INDEX_DIGEST, RUNNER_REPOSITORY,
    RUNNER_VERSION, RUNNER_WORK_DIR, TOOL_CACHE_DIR,
};
pub use supervise::{CleanupReport, DiagnosticExport, Supervision, SupervisionOutcome};

use velnor_model::ScaleSetWorkerState;

/// Marker for an explicitly absent Docker object during restart attestation.
///
/// Absence is reported separately from malformed, foreign, or transport-failed
/// inspection, all of which fail closed before any Docker mutation.
#[derive(Debug)]
pub(crate) struct RestartObjectMissing;

impl std::fmt::Display for RestartObjectMissing {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("restart-attested Docker object is absent")
    }
}

impl std::error::Error for RestartObjectMissing {}

/// Result of inspect-only restart attestation for one worker pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "the caller must route Absent through crash recovery; it is not a healthy pair"]
pub(crate) enum RestartWorkerPairAttestation {
    /// Volume holder, network, DinD, and official runner all exist and satisfy
    /// their exact ownership, image, topology, command, mount, and lifecycle
    /// invariants.
    Complete,
    /// At least one required Docker object explicitly does not exist.
    ///
    /// This is not adoption success. The caller must hand the worker to its
    /// crash-recovery/uncertain path and must not count it as healthy solely
    /// because the deterministic names were found in the journal.
    Absent,
}

/// Inspect-only restart gate for one worker pair.
///
/// A complete pair must pass all image, label, mode, command, attachment, and
/// lifecycle checks in the worker modules. If Docker explicitly reports one
/// object missing, return [`RestartWorkerPairAttestation::Absent`]; do not
/// treat that outcome as a healthy/adopted pair. Malformed, foreign, and
/// transport-failed inspections remain errors. No mutating Docker call is made
/// here.
pub(crate) fn attest_restart_worker_pair(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    profile: &HomogeneousProfile,
) -> anyhow::Result<RestartWorkerPairAttestation> {
    match dind::attest_volume_holder(runner, identity, profile.runner()) {
        Ok(_) => {}
        Err(error) if error.downcast_ref::<RestartObjectMissing>().is_some() => {
            return Ok(RestartWorkerPairAttestation::Absent)
        }
        Err(error) => return Err(error),
    }
    let network_id = match dind::attest_restart_network(runner, identity) {
        Ok(network_id) => network_id,
        Err(error) if error.downcast_ref::<RestartObjectMissing>().is_some() => {
            return Ok(RestartWorkerPairAttestation::Absent)
        }
        Err(error) => return Err(error),
    };
    match dind::attest_restart_dind(runner, identity, profile.dind(), &network_id) {
        Ok(_) => {}
        Err(error) if error.downcast_ref::<RestartObjectMissing>().is_some() => {
            return Ok(RestartWorkerPairAttestation::Absent)
        }
        Err(error) => return Err(error),
    }
    match runner::attest_restart_runner(runner, identity, profile.runner()) {
        Ok(_) => Ok(RestartWorkerPairAttestation::Complete),
        Err(error) if error.downcast_ref::<RestartObjectMissing>().is_some() => {
            Ok(RestartWorkerPairAttestation::Absent)
        }
        Err(error) => Err(error),
    }
}

/// One finished process: exit code + captured streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// What the worker lane needs from a process runner: spawn-and-capture.
///
/// This is the lane's public seam over the crate's internal command
/// runner, whose surface the lane must not leak into its public API.
/// Every in-crate command runner implements this automatically via the
/// blanket impl below; tests bring scripted doubles.
pub trait WorkerRunner {
    /// Run `program` to completion, capturing both streams.
    fn run(&mut self, program: &str, args: &[String]) -> anyhow::Result<WorkerOutput>;
}

impl<T> WorkerRunner for T
where
    T: crate::executor::CommandRunner + ?Sized,
{
    fn run(&mut self, program: &str, args: &[String]) -> anyhow::Result<WorkerOutput> {
        let output = crate::executor::CommandRunner::run(self, program, args)?;
        Ok(WorkerOutput {
            code: output.code,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// A recorded lifecycle edge: `from → to` for one ownership id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerEdge {
    pub ownership: String,
    pub from: ScaleSetWorkerState,
    pub to: ScaleSetWorkerState,
}

/// Where lifecycle edges are recorded.
///
/// The journal implements this once the `ScaleSetWorkerEdge` event lands
/// (design §2); until then the provision path records edges through the
/// test/in-memory sinks. The machine never skips an edge: an unrecorded
/// transition is a bug, not an optimization.
pub trait EdgeSink {
    /// Record one validated edge. Fails the transition on error: the
    /// worker stays in `from` when the edge cannot be recorded.
    fn record_edge(&mut self, edge: &WorkerEdge) -> anyhow::Result<()>;
}

/// In-memory edge sink (tests + the pre-journal provision path).
#[derive(Debug, Default)]
pub struct VecEdgeSink {
    edges: Vec<WorkerEdge>,
}

impl VecEdgeSink {
    #[must_use]
    pub fn edges(&self) -> &[WorkerEdge] {
        &self.edges
    }
}

impl EdgeSink for VecEdgeSink {
    fn record_edge(&mut self, edge: &WorkerEdge) -> anyhow::Result<()> {
        self.edges.push(edge.clone());
        Ok(())
    }
}

/// Whether `from → to` is a legal lifecycle edge.
///
/// The table mirrors the style of `velnor_model::JobState::transition_target`:
/// a single match is the whole policy. Terminal `permit_released` has no
/// outgoing edges; `terminal` is reachable from every pre-release state
/// (a job can complete, fail, or be cancelled at any point); `uncertain`
/// resolves forward to `acquired` (late `JobAssigned`) or sideways to
/// `terminal` (convergence gave up waiting).
#[must_use]
pub fn legal_edge(from: ScaleSetWorkerState, to: ScaleSetWorkerState) -> bool {
    use ScaleSetWorkerState as S;
    if from == to {
        return false;
    }
    match (from, to) {
        // Happy path.
        (S::Observed, S::Eligible)
        | (S::Eligible, S::Reserved)
        | (S::Reserved, S::AcquireIntent)
        | (S::AcquireIntent, S::Acquired)
        | (S::AcquireIntent, S::Uncertain)
        | (S::Uncertain, S::Acquired)
        | (S::Acquired, S::ProvisionIntent)
        | (S::ProvisionIntent, S::DindReady)
        | (S::DindReady, S::RunnerConnected)
        | (S::RunnerConnected, S::Running)
        | (S::Running, S::Terminal)
        | (S::Terminal, S::DiagnosticExport)
        | (S::DiagnosticExport, S::OwnedCleanup)
        | (S::OwnedCleanup, S::PermitReleased) => true,
        // Terminal is reachable from every pre-release state: completion,
        // failure, and cancellation can land at any point.
        (_, S::Terminal)
            if !matches!(
                from,
                S::Terminal | S::DiagnosticExport | S::OwnedCleanup | S::PermitReleased
            ) =>
        {
            true
        }
        // Uncertain resolves sideways when convergence gives up.
        (S::Uncertain, S::Terminal) => true,
        // Retry: a failed provision intent returns to acquired (the
        // ownership id is stable, so the retry converges on the same names).
        (S::ProvisionIntent, S::Acquired)
        | (S::DindReady, S::ProvisionIntent)
        | (S::RunnerConnected, S::ProvisionIntent) => true,
        _ => false,
    }
}

/// One worker's durable record: identity + lifecycle + bindings.
///
/// The journal `scaleset_workers` row persists this shape; the struct is
/// the in-memory owner of the transition policy.
#[derive(Debug, Clone)]
pub struct ScaleSetWorker {
    identity: WorkerIdentity,
    operation_id: String,
    request_id: Option<i64>,
    state: ScaleSetWorkerState,
    permit_holder: Option<String>,
    runner_version: Option<String>,
    dind_version: Option<String>,
}

impl ScaleSetWorker {
    /// New worker at `observed`, bound to `operation_id` (the provision
    /// idempotency key, stable across retries).
    #[must_use]
    pub fn new(identity: WorkerIdentity, operation_id: &str) -> Self {
        Self {
            identity,
            operation_id: operation_id.to_string(),
            request_id: None,
            state: ScaleSetWorkerState::Observed,
            permit_holder: None,
            runner_version: None,
            dind_version: None,
        }
    }

    #[must_use]
    pub fn identity(&self) -> &WorkerIdentity {
        &self.identity
    }

    #[must_use]
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    #[must_use]
    pub fn request_id(&self) -> Option<i64> {
        self.request_id
    }

    #[must_use]
    pub fn state(&self) -> ScaleSetWorkerState {
        self.state
    }

    #[must_use]
    pub fn permit_holder(&self) -> Option<&str> {
        self.permit_holder.as_deref()
    }

    /// Bind the acquired GitHub request id (once, at `acquired`).
    pub fn bind_request(&mut self, request_id: i64) {
        self.request_id = Some(request_id);
    }

    /// Bind the ledger permit holder (once, at `reserved`).
    pub fn bind_permit(&mut self, holder: &str) {
        self.permit_holder = Some(holder.to_string());
    }

    /// Record the provisioned tool versions (at `provision_intent`, from
    /// the tool-content attestation + pin metadata).
    pub fn record_versions(&mut self, runner_version: &str, dind_version: &str) {
        self.runner_version = Some(runner_version.to_string());
        self.dind_version = Some(dind_version.to_string());
    }

    #[must_use]
    pub fn versions(&self) -> (Option<&str>, Option<&str>) {
        (self.runner_version.as_deref(), self.dind_version.as_deref())
    }

    /// Advance to `to`, recording the edge. Illegal edges and sink
    /// failures both leave the worker in its current state.
    pub fn transition<S: EdgeSink>(
        &mut self,
        sink: &mut S,
        to: ScaleSetWorkerState,
    ) -> anyhow::Result<()> {
        let from = self.state;
        if !legal_edge(from, to) {
            anyhow::bail!(
                "illegal scale-set worker edge {} → {}",
                from.as_str(),
                to.as_str()
            );
        }
        sink.record_edge(&WorkerEdge {
            ownership: self.identity.ownership().as_str(),
            from,
            to,
        })?;
        self.state = to;
        Ok(())
    }
}

/// What to provision: identity + profile + secrets + readiness budget.
///
/// `Debug` never prints the JIT blob: presence-only, mirroring
/// [`ActionsAuth`][crate::scaleset::ActionsAuth].
#[derive(Clone)]
pub struct ProvisionPlan {
    /// Recorded worker identity (stable names + labels).
    pub identity: WorkerIdentity,
    /// The homogeneous image profile.
    pub profile: runner::HomogeneousProfile,
    /// Host state dir for this worker.
    pub state_dir: std::path::PathBuf,
    /// Encoded JIT config blob (`--env-file` into the runner, deleted
    /// after create; never logged).
    pub jit_config: String,
    /// DinD readiness probes before giving up (× [`DIND_READY_POLL_INTERVAL`]).
    pub ready_attempts: u32,
}

impl std::fmt::Debug for ProvisionPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProvisionPlan")
            .field("identity", &self.identity)
            .field("profile", &self.profile)
            .field("state_dir", &self.state_dir)
            .field("jit_config", &"<redacted>")
            .field("ready_attempts", &self.ready_attempts)
            .finish()
    }
}

/// What provisioning produced.
#[derive(Debug, Clone)]
pub struct ProvisionOutcome {
    pub volume_holder: VolumeHolderProvision,
    pub network: NetworkProvision,
    pub dind: DindProvision,
    pub runner: RunnerProvision,
    pub connection: RunnerConnection,
    pub dind_attestation: ToolContentAttestation,
    pub runner_attestation: ToolContentAttestation,
}

/// Provision one worker pair end to end (the listener's provision call).
///
/// Order: verify both images (tool-content hook) → ensure network →
/// ensure the never-started volume holder → ensure DinD → readiness loop →
/// ensure runner → observe connection.
/// Every step is idempotent on the recorded identity, so a retried
/// provision converges instead of duplicating. Any failure aborts with
/// the worker still in `provision_intent`: the caller drives the retry
/// edge (`→ acquired`) or the terminal edge — partial Docker state is
/// adopted or cleaned by the next attempt, never orphaned silently.
///
/// `sleep` is injected (production: `std::thread::sleep`) so tests run
/// the readiness loop without waiting.
pub fn provision_worker(
    runner: &mut dyn WorkerRunner,
    hook: &dyn ToolContentHook,
    plan: &ProvisionPlan,
    sleep: &dyn Fn(std::time::Duration),
    before_runner_start: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<ProvisionOutcome> {
    use anyhow::Context;
    if plan.ready_attempts == 0 {
        anyhow::bail!("provision plan needs at least one DinD readiness attempt");
    }
    let dind_expectation = ToolContentExpectation::dind_for(plan.profile.platform());
    let dind_attestation =
        runner::admit_tool_content(hook, runner, plan.profile.dind(), &dind_expectation)
            .context("admit DinD tool content")?;
    let runner_expectation = ToolContentExpectation::runner_for(plan.profile.platform());
    let runner_attestation =
        runner::admit_tool_content(hook, runner, plan.profile.runner(), &runner_expectation)
            .context("admit runner tool content")?;

    let network = dind::ensure_network(runner, &plan.identity)?;
    let volume_holder = dind::ensure_volume_holder(runner, &plan.identity, plan.profile.runner())
        .context("ensure anonymous-volume holder")?;
    let dind_spec = DindSpec::new(
        plan.identity.clone(),
        plan.profile.dind().clone(),
        &plan.state_dir,
    );
    let dind = dind::ensure_dind(runner, &dind_spec)?;
    let mut ready = false;
    for attempt in 0..plan.ready_attempts {
        if dind::dind_ready(runner, &dind_spec)? {
            ready = true;
            break;
        }
        if attempt + 1 < plan.ready_attempts {
            sleep(DIND_READY_POLL_INTERVAL);
        }
    }
    if !ready {
        anyhow::bail!(
            "DinD container {} never became ready ({} probes)",
            plan.identity.dind_container(),
            plan.ready_attempts
        );
    }
    let runner_spec = RunnerSpec::new(
        plan.identity.clone(),
        plan.profile.runner().clone(),
        &plan.state_dir,
        &plan.jit_config,
    );
    let provisioned = runner::ensure_runner(runner, &runner_spec, before_runner_start)?;
    let connection = runner::runner_connection(runner, &plan.identity)?;
    Ok(ProvisionOutcome {
        volume_holder,
        network,
        dind,
        runner: provisioned,
        connection,
        dind_attestation,
        runner_attestation,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;
    use ScaleSetWorkerState as S;

    fn worker() -> ScaleSetWorker {
        ScaleSetWorker::new(
            WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007")),
            "op-1",
        )
    }

    #[test]
    fn provision_plan_debug_redacts_the_jit_blob() {
        let plan = ProvisionPlan {
            identity: WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007")),
            profile: runner::HomogeneousProfile::for_arch("x86_64").unwrap(),
            state_dir: std::path::PathBuf::from("/tmp/velnor-test-worker-state"),
            jit_config: "live-jit-blob-bytes".to_string(),
            ready_attempts: 3,
        };
        let rendered = format!("{plan:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("live-jit-blob-bytes"), "{rendered}");
    }

    #[test]
    fn happy_path_edges_are_legal() {
        let path = [
            S::Observed,
            S::Eligible,
            S::Reserved,
            S::AcquireIntent,
            S::Acquired,
            S::ProvisionIntent,
            S::DindReady,
            S::RunnerConnected,
            S::Running,
            S::Terminal,
            S::DiagnosticExport,
            S::OwnedCleanup,
            S::PermitReleased,
        ];
        for leg in path.windows(2) {
            assert!(
                legal_edge(leg[0], leg[1]),
                "{} → {}",
                leg[0].as_str(),
                leg[1].as_str()
            );
        }
    }

    #[test]
    fn uncertain_branch_edges_are_legal() {
        assert!(legal_edge(S::AcquireIntent, S::Uncertain));
        assert!(legal_edge(S::Uncertain, S::Acquired));
        assert!(legal_edge(S::Uncertain, S::Terminal));
    }

    #[test]
    fn terminal_is_reachable_from_every_live_state() {
        for from in [
            S::Observed,
            S::Eligible,
            S::Reserved,
            S::AcquireIntent,
            S::Acquired,
            S::ProvisionIntent,
            S::DindReady,
            S::RunnerConnected,
            S::Running,
        ] {
            assert!(legal_edge(from, S::Terminal), "{}", from.as_str());
        }
    }

    #[test]
    fn backward_and_terminal_edges_are_illegal() {
        assert!(!legal_edge(S::Running, S::Acquired));
        assert!(!legal_edge(S::PermitReleased, S::Observed));
        assert!(!legal_edge(S::PermitReleased, S::Terminal));
        assert!(!legal_edge(S::Terminal, S::Running));
        assert!(!legal_edge(S::Observed, S::Running));
        assert!(!legal_edge(S::Running, S::Running));
        assert!(!legal_edge(S::Acquired, S::DindReady));
        assert!(!legal_edge(S::OwnedCleanup, S::Terminal));
    }

    #[test]
    fn transition_records_edges_and_moves() {
        let mut worker = worker();
        let mut sink = VecEdgeSink::default();
        worker.transition(&mut sink, S::Eligible).unwrap();
        worker.transition(&mut sink, S::Reserved).unwrap();
        assert_eq!(worker.state(), S::Reserved);
        assert_eq!(sink.edges().len(), 2);
        assert_eq!(sink.edges()[0].from, S::Observed);
        assert_eq!(sink.edges()[0].to, S::Eligible);
        assert_eq!(sink.edges()[0].ownership, "7/velnor-set-0007");
    }

    #[test]
    fn illegal_transition_changes_nothing() {
        let mut worker = worker();
        let mut sink = VecEdgeSink::default();
        let error = worker.transition(&mut sink, S::Running).unwrap_err();
        assert!(
            error.to_string().contains("illegal scale-set worker edge"),
            "{error}"
        );
        assert_eq!(worker.state(), S::Observed);
        assert!(sink.edges().is_empty());
    }

    #[test]
    fn sink_failure_keeps_current_state() {
        struct FailingSink;
        impl EdgeSink for FailingSink {
            fn record_edge(&mut self, _edge: &WorkerEdge) -> anyhow::Result<()> {
                anyhow::bail!("journal unavailable")
            }
        }
        let mut worker = worker();
        let mut sink = FailingSink;
        assert!(worker.transition(&mut sink, S::Eligible).is_err());
        assert_eq!(worker.state(), S::Observed);
    }

    #[test]
    fn bindings_record_once() {
        let mut worker = worker();
        worker.bind_request(4242);
        worker.bind_permit("scaleset/7/4242");
        worker.record_versions("2.337.0", "28.5.2-dind");
        assert_eq!(worker.request_id(), Some(4242));
        assert_eq!(worker.permit_holder(), Some("scaleset/7/4242"));
        assert_eq!(worker.versions(), (Some("2.337.0"), Some("28.5.2-dind")));
    }

    struct ScriptRunner {
        results: std::collections::VecDeque<WorkerOutput>,
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

    fn restart_pair_identity() -> WorkerIdentity {
        WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007"))
    }

    fn restart_pair_projections(
        identity: &WorkerIdentity,
        profile: &HomogeneousProfile,
    ) -> Vec<WorkerOutput> {
        let mut holder_labels = identity.labels();
        holder_labels.insert(
            ownership::WORKER_ROLE_LABEL.to_string(),
            ownership::ROLE_VOLUME_HOLDER.to_string(),
        );
        let holder_mounts = serde_json::json!([
            {"Type":"volume","Name":"anonymous-work","Destination":dind::WORK_DIR,"Driver":"local","RW":true},
            {"Type":"volume","Name":"anonymous-tools","Destination":dind::TOOL_CACHE_DIR,"Driver":"local","RW":true},
            {"Type":"volume","Name":"anonymous-docker","Destination":dind::DIND_DATA_ROOT,"Driver":"local","RW":true}
        ]);
        let holder = format!(
            "{}\t{}\t{}\tnull\t{}\t{}\t{}\n",
            serde_json::to_string("holder-object-id").unwrap(),
            serde_json::to_string(&profile.runner().reference()).unwrap(),
            serde_json::to_string(&holder_labels).unwrap(),
            serde_json::to_string(&vec![dind::VOLUME_HOLDER_COMMAND]).unwrap(),
            serde_json::to_string("created").unwrap(),
            holder_mounts,
        );
        let network_id = "restart-network-id";
        let network = format!(
            "{}\t{}\t{}\n",
            serde_json::to_string(network_id).unwrap(),
            serde_json::to_string("bridge").unwrap(),
            serde_json::to_string(&identity.labels()).unwrap(),
        );

        let mut dind_labels = identity.labels();
        dind_labels.insert(
            ownership::WORKER_ROLE_LABEL.to_string(),
            ownership::ROLE_DIND.to_string(),
        );
        let mut dind_networks = std::collections::BTreeMap::new();
        dind_networks.insert(
            identity.network(),
            serde_json::json!({"NetworkID": network_id}),
        );
        let dind = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            serde_json::to_string(&profile.dind().reference()).unwrap(),
            serde_json::to_string(&dind_labels).unwrap(),
            serde_json::to_string(&identity.network()).unwrap(),
            serde_json::to_string(&Some(vec![identity.volume_holder_container()])).unwrap(),
            serde_json::to_string(&dind_networks).unwrap(),
            serde_json::to_string("running").unwrap(),
        );

        let mut runner_labels = identity.labels();
        runner_labels.insert(
            ownership::WORKER_ROLE_LABEL.to_string(),
            ownership::ROLE_RUNNER.to_string(),
        );
        let runner = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            serde_json::to_string(&profile.runner().reference()).unwrap(),
            serde_json::to_string(&runner_labels).unwrap(),
            serde_json::to_string(&format!("container:{}", identity.dind_container())).unwrap(),
            serde_json::to_string(&Some(vec![identity.volume_holder_container()])).unwrap(),
            serde_json::to_string(&Some(Vec::<String>::new())).unwrap(),
            serde_json::to_string(&Some(vec![runner::RUNNER_START_COMMAND.to_string()])).unwrap(),
            serde_json::to_string("running").unwrap(),
        );

        vec![
            ScriptRunner::ok(&holder),
            ScriptRunner::ok(&network),
            ScriptRunner::ok(&dind),
            ScriptRunner::ok(&runner),
        ]
    }

    #[test]
    fn restart_pair_attestation_distinguishes_complete_from_absent_network() {
        let identity = restart_pair_identity();
        let profile = HomogeneousProfile::for_arch("x86_64").unwrap();
        let mut complete = ScriptRunner::scripted(restart_pair_projections(&identity, &profile));
        assert_eq!(
            attest_restart_worker_pair(&mut complete, &identity, &profile).unwrap(),
            RestartWorkerPairAttestation::Complete
        );
        assert_eq!(complete.seen.len(), 4);

        let complete_holder = restart_pair_projections(&identity, &profile)
            .into_iter()
            .next()
            .unwrap();
        let mut absent = ScriptRunner::scripted(vec![
            complete_holder,
            ScriptRunner::fail(
                1,
                &format!("Error: No such network: {}", identity.network()),
            ),
        ]);
        assert_eq!(
            attest_restart_worker_pair(&mut absent, &identity, &profile).unwrap(),
            RestartWorkerPairAttestation::Absent
        );
        // Absence stops the attestation gate before it can inspect or mutate
        // the containers. The caller must route this outcome to recovery.
        assert_eq!(absent.seen.len(), 2);
    }

    #[test]
    fn restart_pair_attestation_preserves_transport_fail_closed() {
        let identity = restart_pair_identity();
        let profile = HomogeneousProfile::for_arch("x86_64").unwrap();
        let holder = restart_pair_projections(&identity, &profile)
            .into_iter()
            .next()
            .unwrap();
        let mut runner = ScriptRunner::scripted(vec![
            holder,
            ScriptRunner::fail(1, "permission denied while contacting Docker"),
        ]);
        let error = attest_restart_worker_pair(&mut runner, &identity, &profile).unwrap_err();
        assert!(
            error.chain().any(|cause| cause
                .to_string()
                .contains("permission denied while contacting Docker")),
            "{error:#}"
        );
        assert!(error.downcast_ref::<RestartObjectMissing>().is_none());
        assert_eq!(runner.seen.len(), 2);
        assert_eq!(runner.seen[1].first().map(String::as_str), Some("network"));
        assert_eq!(runner.seen[1].get(1).map(String::as_str), Some("inspect"));
    }

    /// The provision-order test supplies all admission proofs explicitly;
    /// the production Docker hook performs the real runner provenance check.
    struct CompleteTestHook;

    impl ToolContentHook for CompleteTestHook {
        fn verify(
            &self,
            runner: &mut dyn WorkerRunner,
            image: &runner::PinnedImage,
            expected: &runner::ToolContentExpectation,
        ) -> anyhow::Result<runner::ToolContentAttestation> {
            DockerToolContentHook.verify(runner, image, expected)
        }

        fn verify_platform(
            &self,
            _runner: &mut dyn WorkerRunner,
            _image: &runner::PinnedImage,
            _expected: &runner::ImagePlatform,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        fn verify_attestation(
            &self,
            _runner: &mut dyn WorkerRunner,
            _image: &runner::PinnedImage,
            _expected: &runner::ToolContentExpectation,
            _attestation: &runner::ToolContentAttestation,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        fn verify_signature(
            &self,
            _runner: &mut dyn WorkerRunner,
            _image: &runner::PinnedImage,
            _expected: &runner::ToolContentExpectation,
            _attestation: &runner::ToolContentAttestation,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn provision_runs_verify_network_dind_ready_runner_in_order() {
        let dind_ref = format!("{DIND_REPOSITORY}@{DIND_INDEX_DIGEST}");
        let runner_ref = format!("{RUNNER_REPOSITORY}@{RUNNER_INDEX_DIGEST}");
        let state = std::env::temp_dir().join(format!("velnor-provision-{}", std::process::id()));
        let plan = ProvisionPlan {
            identity: WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007")),
            profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
            state_dir: state.clone(),
            jit_config: "jit-blob".to_string(),
            ready_attempts: 3,
        };
        let mut script = ScriptRunner::scripted(vec![
            // DinD tool-content hook (4 calls).
            ScriptRunner::ok("pulled\n"),
            ScriptRunner::ok(&format!("[\"{dind_ref}\"]\n")),
            ScriptRunner::ok("null\n"),
            ScriptRunner::ok("sha256:beef\n"),
            // Runner tool-content hook (4 calls).
            ScriptRunner::ok("pulled\n"),
            ScriptRunner::ok(&format!("[\"{runner_ref}\"]\n")),
            ScriptRunner::ok(
                r#"{"org.opencontainers.image.source":"https://github.com/actions/runner"}"#,
            ),
            ScriptRunner::ok("sha256:feed\n"),
            // Network: missing → create.
            ScriptRunner::fail(
                1,
                "Error: No such network: velnor-scaleset-net-s7-velnor-set-0007-2ad92676",
            ),
            ScriptRunner::ok("netid\n"),
            // Volume holder: missing → create → exact created-state attestation.
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
            restart_pair_projections(&plan.identity, &plan.profile)
                .into_iter()
                .next()
                .unwrap(),
            // DinD: missing → create → start.
            ScriptRunner::fail(
                1,
                "Error: No such container: velnor-scaleset-dind-s7-velnor-set-0007-2ad92676",
            ),
            ScriptRunner::ok("dindid\n"),
            ScriptRunner::ok("velnor-scaleset-dind-s7-velnor-set-0007-2ad92676\n"),
            // Readiness: one miss, then ready.
            ScriptRunner::fail(1, "Cannot connect"),
            ScriptRunner::ok("28.5.2\n"),
            // Runner: missing → create → start.
            ScriptRunner::fail(
                1,
                "Error: No such container: velnor-scaleset-runner-s7-velnor-set-0007-2ad92676",
            ),
            ScriptRunner::ok("runnerid\n"),
            ScriptRunner::ok("velnor-scaleset-runner-s7-velnor-set-0007-2ad92676\n"),
            // Connection: running + marker.
            ScriptRunner::ok("running\n"),
            ScriptRunner::ok("Connected to GitHub\n"),
        ]);
        let sleeps = std::cell::Cell::new(0u32);
        let outcome = provision_worker(
            &mut script,
            &CompleteTestHook,
            &plan,
            &|_| {
                sleeps.set(sleeps.get() + 1);
            },
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(outcome.volume_holder, VolumeHolderProvision::Created);
        assert_eq!(outcome.network, NetworkProvision::Created);
        assert_eq!(outcome.dind, DindProvision::Created);
        assert_eq!(outcome.runner, RunnerProvision::Created);
        assert_eq!(outcome.connection, RunnerConnection::Connected);
        assert_eq!(outcome.dind_attestation.content_version, DIND_VERSION);
        assert_eq!(outcome.runner_attestation.content_version, RUNNER_VERSION);
        // One sleep between the two readiness probes.
        assert_eq!(sleeps.get(), 1);
        // Order proof: first pull precedes network create precedes dind
        // create precedes runner create.
        let verbs: Vec<String> = script.seen.iter().map(|argv| argv.join(" ")).collect();
        let position = |needle: &str| verbs.iter().position(|v| v.contains(needle)).unwrap();
        assert!(position("pull") < position("network create"));
        assert!(position("network create") < position("create --privileged"));
        assert!(position("create --privileged") < position("container:velnor-scaleset-dind"));
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn provision_fails_closed_when_dind_never_ready() {
        let dind_ref = format!("{DIND_REPOSITORY}@{DIND_INDEX_DIGEST}");
        let runner_ref = format!("{RUNNER_REPOSITORY}@{RUNNER_INDEX_DIGEST}");
        let state =
            std::env::temp_dir().join(format!("velnor-provision-noready-{}", std::process::id()));
        let plan = ProvisionPlan {
            identity: WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007")),
            profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
            state_dir: state.clone(),
            jit_config: "jit-blob".to_string(),
            ready_attempts: 2,
        };
        let mut script = ScriptRunner::scripted(vec![
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
            restart_pair_projections(&plan.identity, &plan.profile)
                .into_iter()
                .next()
                .unwrap(),
            ScriptRunner::fail(
                1,
                "Error: No such container: velnor-scaleset-dind-s7-velnor-set-0007-2ad92676",
            ),
            ScriptRunner::ok("dindid\n"),
            ScriptRunner::ok("velnor-scaleset-dind-s7-velnor-set-0007-2ad92676\n"),
            ScriptRunner::fail(1, "Cannot connect"),
            ScriptRunner::fail(1, "Cannot connect"),
        ]);
        let error = provision_worker(&mut script, &CompleteTestHook, &plan, &|_| {}, &mut || {
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("never became ready"), "{error}");
        // The runner was never created: no runner argv ran.
        assert!(
            !script.seen.iter().any(|argv| argv
                .windows(2)
                .any(|pair| pair[0] == "--name" && pair[1].contains("velnor-scaleset-runner"))),
            "{:?}",
            script.seen
        );
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn provision_plan_debug_redacts_jit_blob() {
        let profile = runner::HomogeneousProfile::for_arch("x86_64").unwrap();
        let plan = ProvisionPlan {
            identity: WorkerIdentity::new(OwnershipId::bind(7, "velnor-7-4244")),
            profile,
            state_dir: std::path::PathBuf::from("/tmp/velnor-plan-redact"),
            jit_config: "live-jit-config-blob".to_owned(),
            ready_attempts: 2,
        };
        let rendered = format!("{plan:?}");
        assert!(
            !rendered.contains("live-jit-config-blob"),
            "ProvisionPlan Debug leaked JIT: {rendered}"
        );
        assert!(rendered.contains("velnor-7-4244"));
    }

    #[test]
    fn internal_command_runners_serve_the_worker_seam() {
        use crate::executor::{CommandResult, CommandRunner};
        struct EchoRunner;
        impl CommandRunner for EchoRunner {
            fn run(&mut self, program: &str, args: &[String]) -> anyhow::Result<CommandResult> {
                Ok(CommandResult {
                    code: 0,
                    stdout: format!("{program} {}", args.join(" ")),
                    stderr: String::new(),
                })
            }
        }
        // The blanket impl makes any internal runner a WorkerRunner.
        let mut echo = EchoRunner;
        let output = WorkerRunner::run(&mut echo, "docker", &["version".to_string()]).unwrap();
        assert_eq!(output.stdout, "docker version");
    }
}

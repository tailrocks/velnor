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
//! [`EdgeSink`]. The scale-set lane persists production edges in its worker
//! registry; the sink keeps transition policy independent from persistence.
//! Supervision ([`supervise`]) reconciles observed Docker state against
//! recorded state at every boundary before advancing.
//!
//! Provisioning is scoped to the scale-set lifecycle coordinator:
//!
//! ```compile_fail
//! use velnor_runner::scaleset::worker::provision_worker;
//! ```

use std::time::Instant;

pub mod dind;
pub mod ownership;
pub mod runner;
#[cfg(unix)]
mod secure_fs;
pub mod supervise;

pub use dind::{
    DindProvision, DindSpec, NetworkProvision, BUILDKIT_CACHE_DIR, DIND_READY_POLL_INTERVAL,
    DIND_READY_TIMEOUT, DIND_SOCKET, STATE_MOUNT,
};
pub use ownership::{OwnershipId, WorkerIdentity};
pub use runner::{
    DockerToolContentHook, HomogeneousProfile, InvalidPinnedImage, PinnedImage, RunnerConnection,
    RunnerProvision, RunnerSpec, ToolContentAttestation, ToolContentExpectation, ToolContentHook,
    DIND_DIGEST_AMD64, DIND_DIGEST_ARM64, DIND_INDEX_DIGEST, DIND_REPOSITORY, DIND_VERSION,
    RUNNER_DIGEST_AMD64, RUNNER_DIGEST_ARM64, RUNNER_INDEX_DIGEST, RUNNER_REPOSITORY,
    RUNNER_VERSION,
};
pub use supervise::{DiagnosticExport, Supervision, SupervisionOutcome};

use std::path::{Path, PathBuf};

use velnor_model::ScaleSetWorkerState;

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

    /// Run one operation with an operation-scoped timeout. Scripted worker
    /// runners inherit the unbounded behavior; production runners override
    /// this through the command runner's bounded API.
    fn run_timeout(
        &mut self,
        program: &str,
        args: &[String],
        timeout: std::time::Duration,
    ) -> anyhow::Result<WorkerOutput> {
        let _ = timeout;
        self.run(program, args)
    }
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

    fn run_timeout(
        &mut self,
        program: &str,
        args: &[String],
        timeout: std::time::Duration,
    ) -> anyhow::Result<WorkerOutput> {
        let output = crate::executor::CommandRunner::run_timeout(self, program, args, timeout)?;
        Ok(WorkerOutput {
            code: output.code,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// Resolve and prepare the host-owned root for scale-set worker state.
///
/// The returned absolute path must be persisted in lane configuration so
/// later worker creation and cleanup do not depend on process cwd changes.
pub(crate) fn prepare_state_root(path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(unix)]
    {
        secure_fs::prepare_state_root(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "secure worker-state roots require Unix dirfd support",
        ))
    }
}

/// Create or replace the worker's stable owner-only JIT env file.
///
/// The worker state directory is a writable bind mount inside both DinD
/// and the runner, so host-only secrets must never be staged inside it.
/// The private sibling name lets crash recovery find and remove an env
/// file left behind before Docker returned.
pub(crate) fn create_owner_only_file_next_to(
    state_dir: &Path,
    contents: &[u8],
) -> std::io::Result<PathBuf> {
    #[cfg(unix)]
    {
        let parent_path = state_dir.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "worker state directory has no parent",
            )
        })?;
        let parent = secure_fs::open_absolute_directory(parent_path)?;
        secure_fs::verify_host_parent(&parent)?;
        let directory_name = secure_fs::private_jit_directory_name(state_dir)?;
        let directory = secure_fs::open_or_create_private_directory_at(&parent, &directory_name)?;
        let file_name = std::ffi::OsStr::new("jit.env");
        if let Err(write_error) = secure_fs::write_file_at(&directory, file_name, contents, 0o600) {
            drop(directory);
            return match secure_fs::remove_tree_at(&parent, &directory_name) {
                Ok(()) => Err(write_error),
                Err(cleanup_error) => Err(std::io::Error::other(format!(
                    "host-only JIT write failed: {write_error}; cleanup failed: {cleanup_error}"
                ))),
            };
        }
        Ok(parent_path.join(directory_name).join(file_name))
    }
    #[cfg(not(unix))]
    {
        let _ = (state_dir, contents);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "secure host-owned worker files require descriptor-relative filesystem support",
        ))
    }
}

/// Remove the deterministic private JIT directory, including `jit.env` and
/// any temporary file left by a process crash during atomic creation.
pub(crate) fn remove_owner_only_file_next_to(state_dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let parent_path = state_dir.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "worker state directory has no parent",
            )
        })?;
        let parent = secure_fs::open_absolute_directory(parent_path)?;
        secure_fs::verify_host_parent(&parent)?;
        let directory_name = secure_fs::private_jit_directory_name(state_dir)?;
        match secure_fs::open_private_directory_at(&parent, &directory_name) {
            Ok(directory) => drop(directory),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return secure_fs::sync_directory(&parent);
            }
            Err(error) => return Err(error),
        }
        secure_fs::remove_tree_at(&parent, &directory_name)
    }
    #[cfg(not(unix))]
    {
        let _ = state_dir;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "secure host-owned worker files require descriptor-relative filesystem support",
        ))
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
/// The scale-set lane uses its worker registry; isolated transition tests
/// use in-memory sinks. The machine never skips an edge: an unrecorded
/// transition is a bug, not an optimization.
pub trait EdgeSink {
    /// Record one validated edge. Fails the transition on error: the
    /// worker stays in `from` when the edge cannot be recorded.
    fn record_edge(&mut self, edge: &WorkerEdge) -> anyhow::Result<()>;
}

/// In-memory edge sink for isolated transition tests.
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
/// The worker registry persists this shape; the struct is the in-memory
/// owner of the transition policy.
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
    pub network: NetworkProvision,
    pub dind: DindProvision,
    pub runner: RunnerProvision,
    pub connection: RunnerConnection,
    pub dind_attestation: ToolContentAttestation,
    pub runner_attestation: ToolContentAttestation,
}

/// Provision one worker pair end to end while the scale-set lane holds the
/// worker lifecycle lock and records each Docker boundary durably.
///
/// Order: verify both images (tool-content hook) → ensure network →
/// ensure DinD → readiness loop → ensure runner → observe connection.
/// Every step is idempotent on the recorded identity, so a retried
/// provision converges instead of duplicating. Any failure aborts with
/// the worker still in `provision_intent`: the caller drives the retry
/// edge (`→ acquired`) or the terminal edge — partial Docker state is
/// adopted or cleaned by the next attempt, never orphaned silently.
///
/// `sleep` is injected (production: `std::thread::sleep`) so tests run
/// the readiness loop without waiting.
pub(in crate::scaleset) fn provision_worker(
    lifecycle: &mut crate::scaleset::lane::WorkerLifecycleCapability<'_>,
    runner: &mut dyn WorkerRunner,
    hook: &dyn ToolContentHook,
    plan: &ProvisionPlan,
    sleep: &dyn Fn(std::time::Duration),
) -> anyhow::Result<ProvisionOutcome> {
    let ownership_id = plan.identity.ownership().as_str();
    lifecycle.validate_provision_owner(&ownership_id)?;
    let mut lifecycle_event = |event| lifecycle.record_docker_lifecycle_event(&ownership_id, event);
    provision_worker_inner(runner, hook, plan, sleep, &mut lifecycle_event)
}

fn provision_worker_inner(
    runner: &mut dyn WorkerRunner,
    hook: &dyn ToolContentHook,
    plan: &ProvisionPlan,
    sleep: &dyn Fn(std::time::Duration),
    lifecycle_event: &mut dyn FnMut(runner::DockerLifecycleEvent) -> anyhow::Result<()>,
) -> anyhow::Result<ProvisionOutcome> {
    use anyhow::Context;
    if plan.ready_attempts == 0 {
        anyhow::bail!("provision plan needs at least one DinD readiness attempt");
    }
    let dind_attestation = hook
        .verify(runner, plan.profile.dind(), &ToolContentExpectation::dind())
        .context("verify DinD tool content")?;
    let runner_attestation = hook
        .verify(
            runner,
            plan.profile.runner(),
            &ToolContentExpectation::runner(),
        )
        .context("verify runner tool content")?;

    let network = dind::ensure_network(runner, &plan.identity, lifecycle_event)?;
    let dind_spec = DindSpec::new(
        plan.identity.clone(),
        plan.profile.dind().clone(),
        &plan.state_dir,
    );
    let dind = dind::ensure_dind(runner, &dind_spec, lifecycle_event)?;
    let dind_id = dind::inspect_owned_container(runner, &plan.identity, ownership::ROLE_DIND)?
        .context("DinD disappeared or lost ownership after provisioning")?;
    let readiness_deadline = Instant::now() + DIND_READY_TIMEOUT;
    let ready = probe_dind_until_ready(
        runner,
        &dind_spec,
        &dind_id,
        plan.ready_attempts,
        sleep,
        || readiness_deadline.saturating_duration_since(Instant::now()),
    );
    if !ready {
        anyhow::bail!(
            "DinD container {} never became ready ({} probes)",
            plan.identity.dind_container(),
            plan.ready_attempts
        );
    }
    let network_id = dind::inspect_owned_network(runner, &plan.identity)?
        .context("worker network disappeared after DinD became ready")?;
    dind::validate_network_spec(runner, &plan.identity, &network_id)?;
    if !dind::validate_dind_runtime(runner, &dind_spec, &dind_id, &network_id)? {
        anyhow::bail!(
            "DinD container {} disappeared after readiness",
            plan.identity.dind_container()
        );
    }
    let runner_spec = RunnerSpec::new(
        plan.identity.clone(),
        plan.profile.runner().clone(),
        &plan.state_dir,
        &plan.jit_config,
    );
    let provisioned = runner::ensure_runner(runner, &runner_spec, &dind_id, lifecycle_event)?;
    let runner_id = dind::inspect_owned_container(runner, &plan.identity, ownership::ROLE_RUNNER)?
        .context("runner disappeared or lost ownership after provisioning")?;
    let connection = runner::runner_connection_by_id(runner, &runner_id)?;
    Ok(ProvisionOutcome {
        network,
        dind,
        runner: provisioned,
        connection,
        dind_attestation,
        runner_attestation,
    })
}

fn probe_dind_until_ready(
    runner: &mut dyn WorkerRunner,
    spec: &DindSpec,
    dind_id: &str,
    ready_attempts: u32,
    sleep: &dyn Fn(std::time::Duration),
    mut remaining: impl FnMut() -> std::time::Duration,
) -> bool {
    for attempt in 0..ready_attempts {
        let Some(operation_timeout) = dind::readiness_probe_timeout(remaining()) else {
            break;
        };
        if dind::dind_ready_with_timeout(runner, spec, dind_id, operation_timeout).unwrap_or(false)
        {
            return true;
        }
        if attempt + 1 < ready_attempts {
            let remaining = remaining();
            if remaining.is_zero() {
                break;
            }
            sleep(DIND_READY_POLL_INTERVAL.min(remaining));
        }
    }
    false
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
                anyhow::bail!("worker registry unavailable")
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
        timeouts: Vec<std::time::Duration>,
    }

    impl ScriptRunner {
        fn scripted(results: Vec<WorkerOutput>) -> Self {
            Self {
                results: results.into(),
                seen: Vec::new(),
                timeouts: Vec::new(),
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

    fn owned_container_output(identity: &WorkerIdentity, id: &str, role: &str) -> String {
        let mut labels = identity.labels();
        labels.insert(ownership::WORKER_ROLE_LABEL.to_string(), role.to_string());
        format!("{id}\n{}", serde_json::to_string(&labels).unwrap())
    }

    fn owned_network_output(identity: &WorkerIdentity, id: &str) -> String {
        format!(
            "{id}\n{}",
            serde_json::to_string(&identity.labels()).unwrap()
        )
    }

    fn owned_network_snapshot(identity: &WorkerIdentity, id: &str) -> String {
        serde_json::json!({
            "Id": id,
            "Name": identity.network(),
            "Labels": identity.labels(),
            "Driver": "bridge",
            "Scope": "local",
            "Internal": false,
            "Attachable": false,
            "Ingress": false,
            "ConfigOnly": false,
            "EnableIPv6": false,
            "Options": null,
            "IPAM": {
                "Driver": "default",
                "Options": null,
                "Config": [{
                    "Subnet": "172.30.0.0/16",
                    "IPRange": "",
                    "Gateway": "172.30.0.1"
                }]
            }
        })
        .to_string()
    }

    fn owned_volume_output(identity: &WorkerIdentity, kind: &str, name: &str) -> String {
        let mut labels = identity.labels();
        labels.insert("velnor.scaleset.volume".to_string(), kind.to_string());
        labels.insert("velnor.scaleset.volume-name".to_string(), name.to_string());
        format!("{name}\n{}", serde_json::to_string(&labels).unwrap())
    }

    fn owned_volume_snapshot(identity: &WorkerIdentity, kind: &str, name: &str) -> String {
        let mut labels = identity.labels();
        labels.insert("velnor.scaleset.volume".to_string(), kind.to_string());
        labels.insert("velnor.scaleset.volume-name".to_string(), name.to_string());
        serde_json::json!({
            "Name": name,
            "Driver": "local",
            "Options": null,
            "Labels": labels
        })
        .to_string()
    }

    fn dind_runtime_outputs(plan: &ProvisionPlan, id: &str, network_id: &str) -> Vec<WorkerOutput> {
        std::fs::create_dir_all(plan.state_dir.join("buildkit-cache")).unwrap();
        let state_dir = std::fs::canonicalize(&plan.state_dir).unwrap();
        let mut labels = plan.identity.labels();
        labels.insert(
            ownership::WORKER_ROLE_LABEL.to_string(),
            ownership::ROLE_DIND.to_string(),
        );
        let container = serde_json::json!({
            "Id": id,
            "Image": "sha256:beef",
            "Name": format!("/{}", plan.identity.dind_container()),
            "Config": {
                "Image": plan.profile.dind().reference(),
                "Labels": labels,
                "Env": ["DOCKER_TLS_CERTDIR="],
                "Cmd": [format!("-H unix://{}", dind::DIND_SOCKET)],
                "Entrypoint": null,
                "User": "",
                "WorkingDir": "",
                "ExposedPorts": null,
                "Volumes": null,
                "StopSignal": null,
                "Healthcheck": null,
                "Shell": null
            },
            "HostConfig": {
                "NetworkMode": network_id,
                "Privileged": true,
                "PortBindings": null,
                "CapAdd": null,
                "CapDrop": null,
                "Devices": null,
                "SecurityOpt": null,
                "AutoRemove": false,
                "RestartPolicy": {"Name": "no", "MaximumRetryCount": 0},
                "PublishAllPorts": false,
                "ReadonlyRootfs": false
            },
            "Mounts": [
                {
                    "Type": "volume",
                    "Source": "/var/lib/docker/volumes/dind-data/_data",
                    "Destination": dind::DIND_DATA_ROOT,
                    "Name": plan.identity.dind_data_volume(),
                    "RW": true
                },
                {
                    "Type": "bind",
                    "Source": state_dir,
                    "Destination": dind::STATE_MOUNT,
                    "RW": true
                }
            ],
            "State": {"Running": false},
            "NetworkSettings": {
                "Networks": {
                    (plan.identity.network()): {"NetworkID": network_id}
                }
            }
        });
        let image = serde_json::json!({
            "Id": "sha256:beef",
            "Config": {
                "Labels": null,
                "Env": [],
                "Entrypoint": null,
                "User": "",
                "WorkingDir": "",
                "ExposedPorts": null,
                "Volumes": null,
                "StopSignal": null,
                "Healthcheck": null,
                "Shell": null,
                "Cmd": null
            }
        });
        vec![
            ScriptRunner::ok(&format!("{id}\n")),
            ScriptRunner::ok(&container.to_string()),
            ScriptRunner::ok(&image.to_string()),
        ]
    }

    fn runner_runtime_outputs(
        plan: &ProvisionPlan,
        runner_id: &str,
        dind_id: &str,
    ) -> Vec<WorkerOutput> {
        std::fs::create_dir_all(plan.state_dir.join("buildkit-cache")).unwrap();
        let state_dir = std::fs::canonicalize(&plan.state_dir).unwrap();
        let cache_dir = std::fs::canonicalize(plan.state_dir.join("buildkit-cache")).unwrap();
        let mut labels = plan.identity.labels();
        labels.insert(
            ownership::WORKER_ROLE_LABEL.to_string(),
            ownership::ROLE_RUNNER.to_string(),
        );
        labels.insert(
            runner::RUNNER_SOURCE_LABEL.to_string(),
            runner::RUNNER_SOURCE.to_string(),
        );
        let workspace = plan.identity.workspace_volume();
        let container = serde_json::json!({
            "Id": runner_id,
            "Image": "sha256:feed",
            "Name": format!("/{}", plan.identity.runner_container()),
            "Config": {
                "Image": plan.profile.runner().reference(),
                "Labels": labels,
                "Env": [
                    format!("{}={}", runner::RUNNER_NAME_ENV, plan.identity.ownership().runner_name()),
                    format!("DOCKER_HOST=unix://{}", dind::DIND_SOCKET),
                    format!("RUNNER_WORK_FOLDER={}", runner::RUNNER_WORK_DIR),
                    format!("{}={}", runner::JIT_CONFIG_ENV, plan.jit_config)
                ],
                "Cmd": null,
                "Entrypoint": null,
                "User": "",
                "WorkingDir": "",
                "ExposedPorts": null,
                "Volumes": null,
                "StopSignal": null,
                "Healthcheck": null,
                "Shell": null
            },
            "HostConfig": {
                "NetworkMode": format!("container:{dind_id}"),
                "Privileged": false,
                "PortBindings": null,
                "CapAdd": null,
                "CapDrop": null,
                "Devices": null,
                "SecurityOpt": null,
                "AutoRemove": false,
                "RestartPolicy": {"Name": "no", "MaximumRetryCount": 0},
                "PublishAllPorts": false,
                "ReadonlyRootfs": false
            },
            "Mounts": [
                {
                    "Type": "bind",
                    "Source": state_dir,
                    "Destination": dind::STATE_MOUNT,
                    "RW": true
                },
                {
                    "Type": "volume",
                    "Source": "/var/lib/docker/volumes/workspace/_data",
                    "Destination": runner::RUNNER_WORK_DIR,
                    "Name": workspace,
                    "RW": true
                },
                {
                    "Type": "volume",
                    "Source": "/var/lib/docker/volumes/workspace/_data",
                    "Destination": runner::TOOL_CACHE_DIR,
                    "Name": plan.identity.workspace_volume(),
                    "RW": true
                },
                {
                    "Type": "bind",
                    "Source": cache_dir,
                    "Destination": dind::BUILDKIT_CACHE_DIR,
                    "RW": true
                }
            ],
            "State": {"Running": false},
            "NetworkSettings": {"Networks": {}}
        });
        let image = serde_json::json!({
            "Id": "sha256:feed",
            "Config": {
                "Labels": {runner::RUNNER_SOURCE_LABEL: runner::RUNNER_SOURCE},
                "Env": [],
                "Entrypoint": null,
                "User": "",
                "WorkingDir": "",
                "ExposedPorts": null,
                "Volumes": null,
                "StopSignal": null,
                "Healthcheck": null,
                "Shell": null,
                "Cmd": null
            }
        });
        vec![
            ScriptRunner::ok(&format!("{runner_id}\n")),
            ScriptRunner::ok(&container.to_string()),
            ScriptRunner::ok(&image.to_string()),
        ]
    }

    impl WorkerRunner for ScriptRunner {
        fn run(&mut self, program: &str, args: &[String]) -> anyhow::Result<WorkerOutput> {
            assert_eq!(program, "docker");
            self.seen.push(args.to_vec());
            self.results
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("script exhausted at docker {}", args.join(" ")))
        }

        fn run_timeout(
            &mut self,
            program: &str,
            args: &[String],
            timeout: std::time::Duration,
        ) -> anyhow::Result<WorkerOutput> {
            self.timeouts.push(timeout);
            self.run(program, args)
        }
    }

    #[cfg(unix)]
    #[test]
    fn provision_runs_verify_network_dind_ready_runner_in_order() {
        let dind_ref = format!("{DIND_REPOSITORY}@{DIND_INDEX_DIGEST}");
        let runner_ref = format!("{RUNNER_REPOSITORY}@{RUNNER_INDEX_DIGEST}");
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!("velnor-provision-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let state = root.join("worker");
        let plan = ProvisionPlan {
            identity: WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007")),
            profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
            state_dir: state.clone(),
            jit_config: "jit-blob".to_string(),
            ready_attempts: 3,
        };
        let dind_runtime = dind_runtime_outputs(&plan, "dindid", "networkid");
        let runner_runtime = runner_runtime_outputs(&plan, "runnerid", "dindid");
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
            // Network: one inspect returns ID plus every identity label.
            ScriptRunner::ok(&owned_network_output(&plan.identity, "networkid")),
            ScriptRunner::ok("networkid\n"),
            ScriptRunner::ok(&owned_network_snapshot(&plan.identity, "networkid")),
            // DinD data volume is already owned.
            ScriptRunner::ok(&owned_volume_output(
                &plan.identity,
                "dind-data",
                &plan.identity.dind_data_volume(),
            )),
            ScriptRunner::ok(&format!("{}\n", plan.identity.dind_data_volume())),
            ScriptRunner::ok(&owned_volume_snapshot(
                &plan.identity,
                "dind-data",
                &plan.identity.dind_data_volume(),
            )),
            ScriptRunner::ok(&owned_network_output(&plan.identity, "networkid")),
            ScriptRunner::ok("networkid\n"),
            ScriptRunner::ok(&owned_network_snapshot(&plan.identity, "networkid")),
            // DinD: missing → exact network-ID create → inspect by ID → start.
            ScriptRunner::fail(
                1,
                "Error: No such container: velnor-scaleset-dind-s7-velnor-set-0007-2ad92676",
            ),
            ScriptRunner::ok("dindid\n"),
            ScriptRunner::ok(&owned_container_output(
                &plan.identity,
                "dindid",
                ownership::ROLE_DIND,
            )),
            ScriptRunner::ok(&dind_runtime[0].stdout),
            ScriptRunner::ok(&dind_runtime[1].stdout),
            ScriptRunner::ok(&dind_runtime[2].stdout),
            ScriptRunner::ok("dind started\n"),
            // Provision result is resolved again by exact name before the first probe.
            ScriptRunner::ok(&owned_container_output(
                &plan.identity,
                "dindid",
                ownership::ROLE_DIND,
            )),
            // Readiness: probe the immutable ID; one miss, then ready.
            ScriptRunner::fail(1, "Cannot connect"),
            ScriptRunner::ok("28.5.2\n"),
            ScriptRunner::ok(&owned_network_output(&plan.identity, "networkid")),
            ScriptRunner::ok("networkid\n"),
            ScriptRunner::ok(&owned_network_snapshot(&plan.identity, "networkid")),
            ScriptRunner::ok(&dind_runtime[0].stdout),
            ScriptRunner::ok(&dind_runtime[1].stdout),
            ScriptRunner::ok(&dind_runtime[2].stdout),
            // Runner workspace is owned; runner is missing → create by DinD ID → start.
            ScriptRunner::ok(&owned_volume_output(
                &plan.identity,
                "workspace",
                &plan.identity.workspace_volume(),
            )),
            ScriptRunner::ok(&format!("{}\n", plan.identity.workspace_volume())),
            ScriptRunner::ok(&owned_volume_snapshot(
                &plan.identity,
                "workspace",
                &plan.identity.workspace_volume(),
            )),
            ScriptRunner::fail(
                1,
                "Error: No such container: velnor-scaleset-runner-s7-velnor-set-0007-2ad92676",
            ),
            ScriptRunner::ok("runnerid\n"),
            ScriptRunner::ok(&owned_container_output(
                &plan.identity,
                "runnerid",
                ownership::ROLE_RUNNER,
            )),
            ScriptRunner::ok(&runner_runtime[0].stdout),
            ScriptRunner::ok(&runner_runtime[1].stdout),
            ScriptRunner::ok(&runner_runtime[2].stdout),
            ScriptRunner::ok("runner started\n"),
            // Resolve runner ownership before its ID-bound state/log observation.
            // Connection: running + marker.
            ScriptRunner::ok(&owned_container_output(
                &plan.identity,
                "runnerid",
                ownership::ROLE_RUNNER,
            )),
            ScriptRunner::ok("true\n"),
            ScriptRunner::ok("Connected to GitHub\nListening for Jobs\n"),
        ]);
        let sleeps = std::cell::Cell::new(0u32);
        let outcome = provision_worker_inner(
            &mut script,
            &DockerToolContentHook,
            &plan,
            &|_| {
                sleeps.set(sleeps.get() + 1);
            },
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(outcome.network, NetworkProvision::Adopted);
        assert_eq!(outcome.dind, DindProvision::Created);
        assert_eq!(outcome.runner, RunnerProvision::Created);
        assert_eq!(outcome.connection, RunnerConnection::Connected);
        assert_eq!(outcome.dind_attestation.content_version, DIND_VERSION);
        assert_eq!(outcome.runner_attestation.content_version, RUNNER_VERSION);
        // One sleep between the two readiness probes.
        assert_eq!(sleeps.get(), 1);
        // Order proof: first pull precedes network ownership verification,
        // which precedes dind creation and runner creation.
        let verbs: Vec<String> = script.seen.iter().map(|argv| argv.join(" ")).collect();
        let position = |needle: &str| {
            verbs
                .iter()
                .position(|v| v.contains(needle))
                .unwrap_or_else(|| panic!("missing {needle:?} in {verbs:?}"))
        };
        assert!(position("pull") < position("network inspect"));
        assert!(position("{{json .Labels}}") < position("create --privileged"));
        assert!(position("create --privileged") < position("container:dindid"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn provision_fails_closed_when_dind_never_ready() {
        let dind_ref = format!("{DIND_REPOSITORY}@{DIND_INDEX_DIGEST}");
        let runner_ref = format!("{RUNNER_REPOSITORY}@{RUNNER_INDEX_DIGEST}");
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!("velnor-provision-noready-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let state = root.join("worker");
        let plan = ProvisionPlan {
            identity: WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007")),
            profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
            state_dir: state.clone(),
            jit_config: "jit-blob".to_string(),
            ready_attempts: 2,
        };
        let dind_runtime = dind_runtime_outputs(&plan, "dindid", "networkid");
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
            ScriptRunner::ok(&owned_network_output(&plan.identity, "networkid")),
            ScriptRunner::ok("networkid\n"),
            ScriptRunner::ok(&owned_network_snapshot(&plan.identity, "networkid")),
            ScriptRunner::ok(&owned_volume_output(
                &plan.identity,
                "dind-data",
                &plan.identity.dind_data_volume(),
            )),
            ScriptRunner::ok(&format!("{}\n", plan.identity.dind_data_volume())),
            ScriptRunner::ok(&owned_volume_snapshot(
                &plan.identity,
                "dind-data",
                &plan.identity.dind_data_volume(),
            )),
            ScriptRunner::ok(&owned_network_output(&plan.identity, "networkid")),
            ScriptRunner::ok("networkid\n"),
            ScriptRunner::ok(&owned_network_snapshot(&plan.identity, "networkid")),
            ScriptRunner::fail(
                1,
                "Error: No such container: velnor-scaleset-dind-s7-velnor-set-0007-2ad92676",
            ),
            ScriptRunner::ok("dindid\n"),
            ScriptRunner::ok(&owned_container_output(
                &plan.identity,
                "dindid",
                ownership::ROLE_DIND,
            )),
            ScriptRunner::ok(&dind_runtime[0].stdout),
            ScriptRunner::ok(&dind_runtime[1].stdout),
            ScriptRunner::ok(&dind_runtime[2].stdout),
            ScriptRunner::ok("dind started\n"),
            ScriptRunner::ok(&owned_container_output(
                &plan.identity,
                "dindid",
                ownership::ROLE_DIND,
            )),
            ScriptRunner::fail(1, "Cannot connect"),
            ScriptRunner::fail(1, "Cannot connect"),
        ]);
        let error = provision_worker_inner(
            &mut script,
            &DockerToolContentHook,
            &plan,
            &|_| {},
            &mut |_| Ok(()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("never became ready"), "{error}");
        // The runner was never created: no runner argv ran.
        assert!(
            !script.seen.iter().any(|argv| argv
                .iter()
                .any(|arg| arg.contains("velnor-scaleset-runner"))),
            "{:?}",
            script.seen
        );
        std::fs::remove_dir_all(&root).unwrap();
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
    fn readiness_probes_stop_at_monotonic_deadline_and_use_remaining_timeout() {
        let profile = runner::HomogeneousProfile::for_arch("x86_64").unwrap();
        let spec = DindSpec::new(
            WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007")),
            profile.dind().clone(),
            Path::new("/tmp/worker-readiness-deadline"),
        );
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, "dockerd is still starting"),
            ScriptRunner::fail(1, "dockerd is still starting"),
        ]);
        let mut remaining = [
            std::time::Duration::from_secs(8),
            std::time::Duration::from_secs(4),
            std::time::Duration::from_secs(2),
            std::time::Duration::ZERO,
        ]
        .into_iter();
        let sleeps = std::cell::RefCell::new(Vec::new());
        let ready = probe_dind_until_ready(
            &mut runner,
            &spec,
            "immutable-dind-id",
            4,
            &|duration| sleeps.borrow_mut().push(duration),
            || remaining.next().unwrap_or_default(),
        );

        assert!(!ready);
        assert_eq!(
            runner.timeouts,
            [
                dind::DIND_READY_OPERATION_TIMEOUT,
                std::time::Duration::from_secs(2),
            ]
        );
        assert_eq!(
            runner.seen.len(),
            2,
            "expired total deadline must stop probes"
        );
        assert_eq!(*sleeps.borrow(), [std::time::Duration::from_secs(4)]);
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

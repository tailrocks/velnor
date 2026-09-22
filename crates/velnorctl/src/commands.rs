//! Native `velnorctl` command shapes for the migration surface.
//!
//! These types own parsing only. Shared services remain in control/model
//! crates, and command handlers must call those services rather than inspect
//! daemon files directly.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

/// Resource nouns owned by the query service.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum GetResource {
    Hosts,
    Instances,
    Slots,
    Runners,
    Jobs,
    Runs,
    Queue,
    Events,
    Reservations,
    Leases,
}

impl GetResource {
    /// Wire noun used by the `/v1/<resource>` query route.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hosts => "hosts",
            Self::Instances => "instances",
            Self::Slots => "slots",
            Self::Runners => "runners",
            Self::Jobs => "jobs",
            Self::Runs => "runs",
            Self::Queue => "queue",
            Self::Events => "events",
            Self::Reservations => "reservations",
            Self::Leases => "leases",
        }
    }
}

/// Query one resource collection.
#[derive(Debug, Args)]
pub struct GetArgs {
    #[command(subcommand)]
    pub resource: GetResourceCommand,
}

/// Subcommands under `get` (kept as subcommands for noun-shaped help).
#[derive(Debug, Subcommand)]
pub enum GetResourceCommand {
    Hosts(ResourceQueryArgs),
    Instances(ResourceQueryArgs),
    Slots(ResourceQueryArgs),
    Runners(ResourceQueryArgs),
    Jobs(ResourceQueryArgs),
    Runs(ResourceQueryArgs),
    Queue(ResourceQueryArgs),
    Events(ResourceQueryArgs),
    Reservations(ResourceQueryArgs),
    Leases(ResourceQueryArgs),
}

// The filters (`--selector`, `--field-selector`, `--since`) are global flags
// on `crate::GlobalArgs` and are deliberately not restated here. A second
// declaration with the same id is a clap definition error: the global value
// is propagated into the subcommand matches under the same id and the local
// accessor then downcasts it to the wrong type at runtime (`velnorctl get
// jobs --since 1h` panicked with "Mismatch between definition and access of
// `since`"). `crate::tests::no_local_argument_shadows_a_global_argument`
// refuses any such shadowing at test time. (This is a plain comment: clap
// renders a tuple-variant Args doc comment as the subcommand `about`.)
/// Shared pagination for collection reads.
#[derive(Debug, Args)]
pub struct ResourceQueryArgs {
    #[arg(long)]
    pub page_token: Option<String>,
    #[arg(long)]
    pub limit: Option<u32>,
}

/// Describe one canonical resource identity.
#[derive(Debug, Args)]
pub struct DescribeArgs {
    pub resource: String,
}

/// Read one active/completed log source.
#[derive(Debug, Args)]
pub struct LogsArgs {
    pub resource: String,
    #[arg(long)]
    pub source: Option<String>,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub step: Option<u32>,
    #[arg(long)]
    pub failed: bool,
    #[arg(long)]
    pub tail: Option<u32>,
}

/// Read the daemon-shared performance telemetry stream.
#[derive(Debug, Args)]
pub struct TelemetryArgs {
    /// Resume after an opaque telemetry cursor.
    #[arg(long)]
    pub after: Option<String>,
    /// Maximum records returned.
    #[arg(long)]
    pub limit: Option<u32>,
}

/// Query ordered events.
#[derive(Debug, Args)]
pub struct EventsArgs {
    #[command(flatten)]
    pub query: ResourceQueryArgs,
}

/// Live metric target.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TopTarget {
    Host,
    Instances,
    Slots,
    Jobs,
    Storage,
}

/// Query bounded live metrics.
#[derive(Debug, Args)]
pub struct TopArgs {
    pub target: TopTarget,
}

/// Wait for one condition.
#[derive(Debug, Args)]
pub struct WaitArgs {
    pub resource: String,
    #[arg(long = "for")]
    pub condition: String,
}

/// Reconciliation targets, all plan-first.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReconcileTarget {
    Runners,
    Jobs,
    Docker,
    Storage,
}

/// Execute or preview one exact reconciliation plan.
#[derive(Debug, Args)]
pub struct ReconcileArgs {
    #[command(subcommand)]
    pub target: ReconcileCommand,
}

/// Reconciliation subcommand payload.
#[derive(Debug, Subcommand)]
pub enum ReconcileCommand {
    Runners(ReconcileOptions),
    Jobs(ReconcileOptions),
    Docker(ReconcileOptions),
    Storage(ReconcileOptions),
}

/// Common reconciliation plan/execution flags.
#[derive(Debug, Args)]
pub struct ReconcileOptions {
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub plan_id: Option<String>,
    #[arg(long)]
    pub reason: Option<String>,
}

/// A typed lifecycle mutation request.
#[derive(Debug, Args)]
pub struct LifecycleArgs {
    pub target: String,
    #[arg(long)]
    pub reason: String,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long)]
    pub expected_version: Option<u64>,
}

/// Scale an instance to a desired number of stable slots.
#[derive(Debug, Args)]
pub struct ScaleArgs {
    pub target: String,
    #[arg(long)]
    pub slots: u32,
    #[arg(long)]
    pub reason: String,
    #[arg(long)]
    pub idempotency_key: String,
}

/// Workflow-run command family.
#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(subcommand)]
    pub command: RunCommand,
}

/// Workflow-run operations.
#[derive(Debug, Subcommand)]
pub enum RunCommand {
    List(ResourceQueryArgs),
    View(RunIdArgs),
    Watch(RunIdArgs),
    Cancel(RunIdArgs),
    Rerun(RunIdArgs),
    Logs(RunIdArgs),
    Download(RunIdArgs),
    Dispatch(DispatchArgs),
    Open(RunIdArgs),
}

/// One numeric workflow run identity.
#[derive(Debug, Args)]
pub struct RunIdArgs {
    pub run_id: u64,
}

/// Dispatch one workflow and use the exact response run id. The repository
/// is the global `--repo OWNER/NAME` selector.
#[derive(Debug, Args)]
pub struct DispatchArgs {
    pub workflow: String,
    #[arg(long, default_value = "main")]
    pub reference: String,
}

/// Storage command family.
#[derive(Debug, Args)]
pub struct StorageArgs {
    #[command(subcommand)]
    pub command: StorageCommand,
}

/// Storage operations.
#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    Status,
    Paths,
    Du,
    Gc(StorageGcArgs),
    History,
    Reservations,
    Leases,
    ExplainPressure,
}

/// Storage GC operation. Execution requires an exact reviewed plan.
#[derive(Debug, Args)]
pub struct StorageGcArgs {
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub plan_id: Option<String>,
    #[arg(long)]
    pub reason: Option<String>,
}

/// Config service command family.
#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    View,
    Validate,
    Diff,
    Sources,
}

/// Context persistence command family.
#[derive(Debug, Args)]
pub struct ContextArgs {
    #[command(subcommand)]
    pub command: ContextCommand,
}

#[derive(Debug, Subcommand)]
pub enum ContextCommand {
    List,
    Current,
    Use(ContextNameArgs),
    Set(ContextSetArgs),
    Delete(ContextNameArgs),
}

#[derive(Debug, Args)]
pub struct ContextNameArgs {
    pub name: String,
}

#[derive(Debug, Args)]
pub struct ContextSetArgs {
    pub name: String,
    #[arg(long)]
    pub endpoint: String,
}

/// Auth command family.
#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    Status,
    Check,
}

/// Instance command family.
#[derive(Debug, Args)]
pub struct InstanceArgs {
    #[command(subcommand)]
    pub command: InstanceCommand,
}

#[derive(Debug, Subcommand)]
pub enum InstanceCommand {
    Init(InstanceNameArgs),
    Install(InstanceNameArgs),
    Apply(InstanceNameArgs),
    Delete(InstanceNameArgs),
}

#[derive(Debug, Args)]
pub struct InstanceNameArgs {
    pub name: String,
}

/// Capability command family.
#[derive(Debug, Args)]
pub struct CapabilityArgs {
    #[command(subcommand)]
    pub command: CapabilityCommand,
}

#[derive(Debug, Subcommand)]
pub enum CapabilityCommand {
    List,
    Explain(FeatureArgs),
    Check(JobDumpArgs),
    Export,
}

#[derive(Debug, Args)]
pub struct FeatureArgs {
    pub feature_or_action: String,
}

#[derive(Debug, Args)]
pub struct JobDumpArgs {
    #[arg(long)]
    pub job_dump: PathBuf,
}

/// Native adapter command family.
#[derive(Debug, Args)]
pub struct AdapterArgs {
    #[command(subcommand)]
    pub command: AdapterCommand,
}

#[derive(Debug, Subcommand)]
pub enum AdapterCommand {
    List,
    Describe(FeatureArgs),
    Check(FeatureArgs),
}

/// Static workflow compatibility check.
#[derive(Debug, Args)]
pub struct WorkflowArgs {
    #[command(subcommand)]
    pub command: WorkflowCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkflowCommand {
    Check(WorkflowCheckArgs),
}

/// Static check inputs. The repository is the global `--repo OWNER/NAME`
/// selector.
#[derive(Debug, Args)]
pub struct WorkflowCheckArgs {
    #[arg(long)]
    pub reference: String,
    #[arg(long)]
    pub workflow: String,
}

/// Diagnostics command family.
#[derive(Debug, Args)]
pub struct DiagnosticsArgs {
    #[command(subcommand)]
    pub command: DiagnosticsCommand,
}

#[derive(Debug, Subcommand)]
pub enum DiagnosticsCommand {
    Bundle(DiagnosticsBundleArgs),
}

#[derive(Debug, Args)]
pub struct DiagnosticsBundleArgs {
    #[arg(long)]
    pub archive: PathBuf,
}

/// Local Docker endpoint and Velnor capability report.
#[derive(Debug, Args)]
pub struct DockerArgs {
    /// Report action. The default keeps `velnorctl docker` convenient while
    /// making the documented `velnorctl docker report` spelling explicit.
    #[arg(value_enum, default_value = "report")]
    pub action: DockerAction,

    /// Container image used only when `--check-bind-mount` is requested.
    #[arg(long, default_value = "alpine:3.20")]
    pub image: String,

    /// Run a bounded host-to-container bind-mount visibility probe.
    #[arg(long)]
    pub check_bind_mount: bool,

    /// Local work directory used by the bind-mount probe.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Path to the work directory as seen by the Docker daemon.
    #[arg(long)]
    pub docker_host_work_dir: Option<PathBuf>,
}

/// Docker report actions. `status` is intentionally the same read-only
/// report, so operators can use the noun that matches their incident.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DockerAction {
    Report,
    Status,
}

/// On-demand host operations.
#[derive(Debug, Args)]
pub struct HostArgs {
    #[command(subcommand)]
    pub command: HostCommand,
}

/// On-demand host verbs.
#[derive(Debug, Subcommand)]
pub enum HostCommand {
    /// Start foreground, repository-scoped, Docker-backed capacity.
    Start(HostStartArgs),
    /// Build the local job image from this checkout (no GHCR/Sentry).
    BootstrapImage(HostBootstrapImageArgs),
    /// Show the selected host, Docker endpoint, and execution platform.
    Status,
    /// Explain how to drain a foreground host session.
    Drain,
    /// Explain how to stop a foreground host session.
    Stop,
}

/// Source-bootstrap `velnor/job-ubuntu:26.04` on the current Docker daemon.
#[derive(Debug, Args)]
pub struct HostBootstrapImageArgs {
    /// Tag written by the source build. Defaults to velnor/job-ubuntu:26.04.
    #[arg(long)]
    pub docker_image: Option<String>,
    /// Rebuild release-binaries/$TARGETARCH/velnor-workflow even if it exists.
    #[arg(long)]
    pub rebuild_workflow: bool,
}

/// Explicit host execution topology exposed by host and daemon commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HostMode {
    #[value(name = "native-only")]
    NativeOnly,
    #[value(name = "scale-set-only")]
    ScaleSetOnly,
    #[value(name = "both")]
    Both,
}

impl HostMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeOnly => "native-only",
            Self::ScaleSetOnly => "scale-set-only",
            Self::Both => "both",
        }
    }

    #[must_use]
    pub const fn native_enabled(self) -> bool {
        matches!(self, Self::NativeOnly | Self::Both)
    }
}

impl From<HostMode> for velnor_runner::args::HostMode {
    fn from(mode: HostMode) -> Self {
        match mode {
            HostMode::NativeOnly => Self::NativeOnly,
            HostMode::ScaleSetOnly => Self::ScaleSetOnly,
            HostMode::Both => Self::Both,
        }
    }
}

/// Start one repository-scoped recovery host. The repository comes from the
/// global `--repo OWNER/NAME` selector (default tailrocks/velnor) or `--url`.
#[derive(Debug, Args)]
pub struct HostStartArgs {
    /// Full repository URL. Must be owner/name, never an org pool.
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,
    /// Local runner/instance name.
    #[arg(long)]
    pub name: Option<String>,
    /// Bounded slot count.
    #[arg(long, default_value_t = 1)]
    pub slots: usize,
    /// Host execution topology. Defaults to native-only.
    #[arg(
        long,
        value_enum,
        env = "VELNOR_HOST_MODE",
        default_value = "native-only"
    )]
    pub mode: HostMode,
    /// PR number to document targeting semantics for. GitHub stays the scheduler.
    #[arg(long, value_name = "NUMBER")]
    pub pr: Option<u64>,
    #[arg(long)]
    pub work_dir: Option<PathBuf>,
    #[arg(long)]
    pub config_dir: Option<PathBuf>,
    /// Path the Docker daemon uses for --work-dir when it differs from the host.
    #[arg(long)]
    pub docker_host_work_dir: Option<PathBuf>,
    /// Job image. Defaults to velnor/job-ubuntu:26.04. Must already exist locally.
    #[arg(long)]
    pub docker_image: Option<String>,
    /// Host-wide maximum concurrent jobs. Required for Scale Set modes.
    #[arg(long, env = "VELNOR_MAX_JOBS")]
    pub max_jobs: Option<u32>,
    /// Explicit host-wide permit ledger path shared by every enabled engine.
    #[arg(long, env = "VELNOR_PERMIT_LEDGER")]
    pub permit_ledger: Option<PathBuf>,
    /// Scale Set lane configuration. Required for Scale Set modes.
    #[arg(long, env = "VELNOR_SCALE_SET_CONFIG")]
    pub scale_set_config: Option<PathBuf>,
}

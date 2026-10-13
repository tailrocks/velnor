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

/// Shared read filters.
#[derive(Debug, Args)]
pub struct ResourceQueryArgs {
    #[arg(long)]
    pub selector: Option<String>,
    #[arg(long)]
    pub field_selector: Option<String>,
    #[arg(long)]
    pub page_token: Option<String>,
    #[arg(long)]
    pub limit: Option<u32>,
    #[arg(long)]
    pub since: Option<String>,
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

/// Dispatch one workflow and use the exact response run id.
#[derive(Debug, Args)]
pub struct DispatchArgs {
    pub workflow: String,
    #[arg(long)]
    pub repo: String,
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

#[derive(Debug, Args)]
pub struct WorkflowCheckArgs {
    #[arg(long)]
    pub repo: String,
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

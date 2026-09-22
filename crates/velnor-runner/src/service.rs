//! Service-only entrypoint surface of the `velnor-runner` daemon binary.
//!
//! The operator-facing Velnor CLI lives exclusively in the `velnorctl`
//! command center; what remains here is exactly the machine-invoked plumbing
//! packaged consumers execute: the node-local guardian/controller/slot/job
//! processes (`ExecStart=/usr/bin/velnor-runner daemon` still launches the
//! controller that spawns one OS process per slot), the pre-start coherence hook
//! (`release verify-installed`), Debian maintainer-script identity exports
//! (`release export`, `capabilities export`), and the release workflow's
//! metadata job. The [`packaged invoker guard`](#) in `crates/velnorctl/tests`
//! proves the packaged files never reference a verb dropped from either
//! binary. Plan 079 records the long-term home of each verb.

use std::{num::NonZeroU32, path::PathBuf};

use clap::{Args, Parser, Subcommand};

/// Parsed service entrypoint of the `velnor-runner` binary.
#[derive(Debug, Parser)]
#[command(
    name = "velnor-runner",
    version = env!("CARGO_PKG_VERSION"),
    about = "Velnor daemon service entrypoint",
    long_about = "Velnor daemon service entrypoint.\n\n\
        Machine-invoked service plumbing only: the daemon loop plus the \
        release/capability hooks executed by systemd units, Debian \
        maintainer scripts, and the release workflow. Every operator-facing \
        command lives in the velnorctl control panel."
)]
pub struct ServiceCli {
    #[command(subcommand)]
    pub command: ServiceCommand,
}

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Run one daemon process that manages one or more internal runner slots.
    Daemon(Box<DaemonArgs>),
    /// Node-local guardian: supervise units and health, never GitHub or Docker.
    Guardian(crate::node::GuardianArgs),
    /// Per-scope controller: permits, registrations, slot process desired state.
    Controller(Box<crate::node::ControllerArgs>),
    /// One ready slot as its own OS process.
    Slot(Box<crate::node::SlotArgs>),
    /// Transient per-job worker process.
    Job(crate::node::JobArgs),
    /// Release-coherence hooks for ExecStartPre, postinst, and release CI.
    Release(ReleaseArgs),
    /// Compiled-manifest export for postinst identity validation.
    Capabilities(CapabilitiesArgs),
}

/// The complete runtime dispatch surface shared with the velnorctl facade.
/// Operator variants are constructed by velnorctl; service variants arrive
/// through [`ServiceCommand`].
#[derive(Debug)]
pub enum Command {
    Cache(crate::args::CacheArgs),
    Capabilities(crate::args::CapabilitiesArgs),
    Configure(crate::args::ConfigureArgs),
    Daemon(Box<DaemonArgs>),
    Preflight(crate::args::PreflightArgs),
    Remove(crate::args::RemoveArgs),
    Status(crate::args::StatusArgs),
    Storage(crate::args::StorageArgs),
    Doctor(crate::args::DoctorArgs),
    Release(ReleaseArgs),
}

impl TryFrom<ServiceCommand> for Command {
    type Error = anyhow::Error;

    /// Node roles dispatch before conversion, so they cannot become a
    /// [`Command`]: the fallible conversion reports that caller error
    /// instead of panicking on four of the seven [`ServiceCommand`] variants.
    fn try_from(command: ServiceCommand) -> Result<Self, Self::Error> {
        match command {
            ServiceCommand::Daemon(args) => Ok(Self::Daemon(args)),
            ServiceCommand::Release(args) => Ok(Self::Release(args)),
            ServiceCommand::Capabilities(args) => Ok(Self::Capabilities(args.into())),
            ServiceCommand::Guardian(_)
            | ServiceCommand::Controller(_)
            | ServiceCommand::Slot(_)
            | ServiceCommand::Job(_) => Err(anyhow::anyhow!(
                "node roles dispatch before Command conversion"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Daemon arguments (clap-owned here; the single definition point)
// ---------------------------------------------------------------------------

/// Base configuration directory default helper lives with the handler; the
/// struct below is the single clap definition of every daemon flag.
#[derive(Debug, Clone, Args)]
pub struct DaemonArgs {
    /// Operational store path shared by the daemon's durable lifecycle state.
    #[arg(long, env = "VELNOR_STATE_DB")]
    pub state_db: Option<PathBuf>,
    /// Base configuration directory. For --slots > 1, each slot reads config from <config-dir>/slots/slot-N.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    /// Repository, organization, or enterprise URL accepted by GitHub JIT runner configuration. If provided, daemon configures internal slots before polling.
    #[arg(long)]
    pub url: Option<String>,

    /// GitHub personal access token used to create JIT runner configurations.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,

    /// Base runner display name. For --slots > 1, Velnor appends -slot-N.
    #[arg(long)]
    pub name: Option<String>,

    /// Comma-separated labels for each JIT runner. Example: velnor,hetzner-sentry-ci.
    #[arg(long, value_delimiter = ',')]
    pub labels: Vec<String>,

    /// Add labels needed by the current target repositories' x64 Linux jobs.
    #[arg(long)]
    pub target_mvp_labels: bool,

    /// Add the current target repositories' ARM Linux label.
    #[arg(long)]
    pub target_mvp_arm_label: bool,

    /// Replace existing slot configs during daemon startup JIT configuration.
    #[arg(long)]
    pub replace: bool,

    /// Optional runner group id. Organization/enterprise scopes require
    /// `--pool-name`/`VELNOR_POOL_NAME`; no scope may silently use group 1.
    #[arg(long, env = "VELNOR_POOL_ID")]
    pub pool_id: Option<i64>,

    /// Resolve this organization or enterprise runner group name through GitHub.
    #[arg(long, env = "VELNOR_POOL_NAME")]
    pub pool_name: Option<String>,

    /// Explicit desired routing policy JSON for organization/enterprise scopes.
    /// Missing or invalid policy keeps routing fail-closed.
    #[arg(long, env = "VELNOR_ROUTING_POLICY_FILE")]
    pub routing_policy_file: Option<PathBuf>,

    /// Validate daemon slot JIT payloads without calling GitHub.
    #[arg(long = "dry-run-jit-config")]
    pub dry_run_registration: bool,

    /// Number of internal GitHub runner slots managed by this daemon.
    #[arg(long, default_value_t = 1)]
    pub slots: usize,

    /// Recycle an idle slot after this many seconds even when it still looks
    /// healthy, so every slot gets a periodically fresh JIT registration.
    /// 0 disables the bound. Defaults to 14400 (4 hours).
    #[arg(long)]
    pub max_idle_slot_age_seconds: Option<u64>,

    /// Exit each internal slot after one job. Useful for bounded live proof runs.
    #[arg(long)]
    pub once: bool,

    /// Fail each slot if no job is acquired within this many seconds. Default is no idle timeout.
    #[arg(long)]
    pub idle_timeout_seconds: Option<u64>,

    /// Mark received jobs as succeeded without executing user steps.
    #[arg(long)]
    pub complete_noop: bool,

    /// Execute supported run steps in Docker, then finish and acknowledge the job. This is the default unless --complete-noop or --dry-run-jobs is set.
    #[arg(long)]
    pub execute_scripts: bool,

    /// Poll and inspect jobs without acknowledging or executing them.
    #[arg(long)]
    pub dry_run_jobs: bool,

    /// Write sanitized AgentJobRequestMessage JSON snapshots to this file or directory. For --slots > 1, Velnor writes under slot-N children.
    #[arg(long)]
    pub dump_job_message: Option<PathBuf>,

    /// Docker image for executable jobs.
    #[arg(long, default_value = "velnor/job-ubuntu:26.04")]
    pub docker_image: String,

    /// Host execution topology. Defaults to native-only.
    #[arg(
        long,
        env = "VELNOR_HOST_MODE",
        default_value_t = crate::args::HostMode::NativeOnly
    )]
    pub mode: crate::args::HostMode,

    /// Host-wide maximum concurrent jobs. Native and Scale Set lanes share
    /// one permit ledger capped at this N; job containers run unbounded and
    /// only this number is tuned for throughput. Unset (or 0) falls back to
    /// --slots, which is correct only for single-daemon hosts.
    #[arg(long, env = "VELNOR_MAX_JOBS")]
    pub max_jobs: Option<u32>,

    /// Host-wide permit ledger database. Defaults to `permit-ledger.db` next
    /// to the operational state db. Every daemon on the host must resolve to
    /// the same file.
    #[arg(long, env = "VELNOR_PERMIT_LEDGER")]
    pub permit_ledger: Option<PathBuf>,

    /// Scale-set lane config file (TOML). When set, the daemon supervises
    /// the Scale Set adapter next to its native slots on the shared
    /// host-wide permit ledger. Unset disables the lane entirely.
    #[arg(long, env = "VELNOR_SCALE_SET_CONFIG")]
    pub scale_set_config: Option<PathBuf>,

    /// Pool trust boundary. Declared once in [`crate::trust_scope`] and
    /// flattened here so this binary and `velnorctl` cannot disagree about a
    /// security gate.
    #[command(flatten)]
    pub trust: crate::trust_scope::TrustScopeArg,

    /// Filesystem bytes never available to new jobs.
    #[arg(
        long,
        env = "VELNOR_EMERGENCY_RESERVE_BYTES",
        default_value_t = crate::capacity::DEFAULT_EMERGENCY_RESERVE_BYTES
    )]
    pub emergency_reserve_bytes: u64,

    /// Conservative disk reservation for every advertised slot.
    #[arg(
        long,
        env = "VELNOR_JOB_PEAK_BYTES",
        default_value_t = crate::capacity::DEFAULT_JOB_PEAK_BYTES
    )]
    pub job_peak_bytes: u64,

    /// Override Docker image used to run JavaScript actions. By default Velnor uses the action's declared Node runtime image.
    #[arg(long, default_value = "")]
    pub node_action_image: String,

    /// Base host work directory for Docker job state. For --slots > 1, each slot uses a slot-N child.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Path where the Docker daemon host sees --work-dir. For --slots > 1, each slot uses a slot-N child.
    #[arg(long)]
    pub docker_host_work_dir: Option<PathBuf>,

    /// Skip selected execution-backend preflight before polling GitHub for executable jobs.
    #[arg(long)]
    pub skip_preflight: bool,

    /// Require /var/run/docker.sock before polling GitHub for executable jobs.
    #[arg(long)]
    pub require_docker_socket: bool,
}

/// Internal single-worker mode; C075 folds it into `daemon --once`. Not a
/// parsed service verb — kept for the library API and characterization tests.
#[derive(Debug, Clone)]
pub struct RunArgs {
    /// Validated number of slots provisioned by the owning daemon.
    pub slot_count: NonZeroU32,
    pub state_db: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub pat: Option<String>,
    pub max_idle_slot_age_seconds: Option<u64>,
    pub once: bool,
    pub idle_timeout_seconds: Option<u64>,
    pub complete_noop: bool,
    pub execute_scripts: bool,
    pub dry_run_jobs: bool,
    pub dump_job_message: Option<PathBuf>,
    pub docker_image: String,
    /// Host-wide permit ledger database. `None` resolves to the default path
    /// next to the operational state db.
    pub permit_ledger: Option<PathBuf>,
    pub trust_scope: String,
    pub emergency_reserve_bytes: u64,
    pub job_peak_bytes: u64,
    pub node_action_image: String,
    pub work_dir: Option<PathBuf>,
    pub docker_host_work_dir: Option<PathBuf>,
    pub skip_preflight: bool,
    pub require_docker_socket: bool,
}

// ---------------------------------------------------------------------------
// Release-coherence hooks
// ---------------------------------------------------------------------------

#[derive(Debug, Args)]
pub struct ReleaseArgs {
    #[command(subcommand)]
    pub command: ReleaseCommand,
}

#[derive(Debug, Subcommand)]
pub enum ReleaseCommand {
    /// Emit a coherent record from this (release-build) binary: a stable release
    /// record, or a deb's own package record with `--binary`. Refuses to run
    /// from a development build.
    Emit(ReleaseEmitArgs),
    /// Re-assemble and re-verify a record from downloaded artifacts.
    Assemble(ReleaseAssembleArgs),
    /// Verify a record against its independent checksum and internal coherence.
    VerifyRecord(ReleaseVerifyRecordArgs),
    /// Validate the installed binary/package/manifest against the active record.
    /// Run by both `.service` units before ExecStart.
    VerifyInstalled(ReleaseVerifyInstalledArgs),
    /// Atomically activate a record, demoting the current active to rollback.
    Activate(ReleaseActivateArgs),
    /// Restore the previous coherent record.
    Rollback(ReleaseRollbackArgs),
    /// Print this binary's embedded build identity (or a deployed identity file).
    Export(ReleaseExportArgs),
}

#[derive(Debug, Args)]
pub struct ReleaseEmitArgs {
    /// Path to the assembled release or package record JSON to validate + persist.
    #[arg(long)]
    pub record: PathBuf,
    /// Release store root the immutable record is written under.
    #[arg(long, default_value = crate::args::ACTIVE_RELEASE_DIR)]
    pub out_dir: PathBuf,
    /// Write canonical record bytes + a `.sha256` sidecar here instead of
    /// storing the record in the release store (the packaging lanes' staging path).
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Runner binary a package record is staged against; emission refuses a
    /// record whose architecture entry does not name exactly these bytes.
    #[arg(long)]
    pub binary: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReleaseAssembleArgs {
    /// Candidate record JSON.
    #[arg(long)]
    pub record: PathBuf,
    /// Required directory of downloaded artifacts (per-arch binary/Debian
    /// sidecars and Debian payloads) to cross-check the record's digests against.
    #[arg(long)]
    pub artifacts: PathBuf,
    /// Write the re-verified canonical record + checksum here.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReleaseVerifyRecordArgs {
    /// Release record JSON.
    #[arg(long)]
    pub record: PathBuf,
    /// Independent checksum file (`<sha256>  <name>` format).
    #[arg(long)]
    pub checksum: Option<PathBuf>,
    /// Independent checksum as a bare 64-hex string.
    #[arg(long)]
    pub sha256: Option<String>,
    /// APT publication record; claim files are required with this option.
    #[arg(long)]
    pub publication: Option<PathBuf>,
    /// Expected preverified APT metadata claims JSON; not a raw-byte verifier.
    #[arg(long)]
    pub expected_apt_metadata: Option<PathBuf>,
    /// Served preverified APT metadata claims JSON; not a raw-byte verifier.
    #[arg(long)]
    pub served_apt_metadata: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReleaseVerifyInstalledArgs {
    /// Active release record.
    #[arg(long, default_value = crate::args::ACTIVE_RECORD_PATH)]
    pub record: PathBuf,
    /// Active deployed-identity pointer.
    #[arg(long, default_value = crate::args::ACTIVE_DEPLOYED_PATH)]
    pub deployed: PathBuf,
    /// Installed daemon binary to hash.
    #[arg(long, default_value = crate::args::INSTALLED_BINARY_PATH)]
    pub binary: PathBuf,
    /// Host architecture override (amd64|arm64); defaults to the running arch.
    #[arg(long)]
    pub arch: Option<String>,
}

#[derive(Debug, Args)]
pub struct ReleaseActivateArgs {
    /// Release store root.
    #[arg(long, default_value = crate::args::ACTIVE_RELEASE_DIR)]
    pub dir: PathBuf,
    /// Record to activate.
    #[arg(long)]
    pub record: PathBuf,
}

#[derive(Debug, Args)]
pub struct ReleaseRollbackArgs {
    /// Release store root.
    #[arg(long, default_value = crate::args::ACTIVE_RELEASE_DIR)]
    pub dir: PathBuf,
}

#[derive(Debug, Args)]
pub struct ReleaseExportArgs {
    /// Optional deployed-identity file to pretty-print instead of the embedded
    /// build identity.
    #[arg(long)]
    pub deployed: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Capability manifest hooks
// ---------------------------------------------------------------------------

#[derive(Debug, Args)]
pub struct CapabilitiesArgs {
    #[command(subcommand)]
    pub command: CapabilitiesCommand,
}

#[derive(Debug, Subcommand)]
pub enum CapabilitiesCommand {
    /// Export the compiled manifest as JSON (postinst identity validation).
    Export,
}

impl From<CapabilitiesArgs> for crate::args::CapabilitiesArgs {
    fn from(args: CapabilitiesArgs) -> Self {
        Self {
            command: match args.command {
                CapabilitiesCommand::Export => crate::args::CapabilitiesCommand::Export,
            },
        }
    }
}

impl From<DaemonArgs> for crate::args::DaemonArgs {
    fn from(a: DaemonArgs) -> Self {
        Self {
            state_db: a.state_db,
            config_dir: a.config_dir,
            url: a.url,
            pat: a.pat,
            name: a.name,
            labels: a.labels,
            target_mvp_labels: a.target_mvp_labels,
            target_mvp_arm_label: a.target_mvp_arm_label,
            replace: a.replace,
            pool_id: a.pool_id,
            pool_name: a.pool_name,
            pool_id_pre_resolved: false,
            routing_policy_file: a.routing_policy_file,
            dry_run_registration: a.dry_run_registration,
            slots: a.slots,
            max_idle_slot_age_seconds: a.max_idle_slot_age_seconds,
            once: a.once,
            idle_timeout_seconds: a.idle_timeout_seconds,
            complete_noop: a.complete_noop,
            execute_scripts: a.execute_scripts,
            dry_run_jobs: a.dry_run_jobs,
            dump_job_message: a.dump_job_message,
            docker_image: a.docker_image,
            mode: a.mode,
            max_jobs: a.max_jobs,
            permit_ledger: a.permit_ledger,
            scale_set_config: a.scale_set_config,
            // The one resolution point of the pool trust boundary: the ceiling
            // every job on this pool runs under. Admission narrows it by the
            // job's trust class, and everything downstream — the capability
            // gates and every trust-scoped store path — reads the job's
            // admitted scope, never this flag directly.
            trust_scope: a.trust.resolve().into_string(),
            emergency_reserve_bytes: a.emergency_reserve_bytes,
            job_peak_bytes: a.job_peak_bytes,
            node_action_image: a.node_action_image,
            work_dir: a.work_dir,
            docker_host_work_dir: a.docker_host_work_dir,
            skip_preflight: a.skip_preflight,
            require_docker_socket: a.require_docker_socket,
        }
    }
}

impl From<ReleaseArgs> for crate::args::ReleaseArgs {
    fn from(a: ReleaseArgs) -> Self {
        Self {
            command: match a.command {
                ReleaseCommand::Emit(x) => crate::args::ReleaseCommand::Emit(x.into()),
                ReleaseCommand::Assemble(x) => crate::args::ReleaseCommand::Assemble(x.into()),
                ReleaseCommand::VerifyRecord(x) => {
                    crate::args::ReleaseCommand::VerifyRecord(x.into())
                }
                ReleaseCommand::VerifyInstalled(x) => {
                    crate::args::ReleaseCommand::VerifyInstalled(x.into())
                }
                ReleaseCommand::Activate(x) => crate::args::ReleaseCommand::Activate(x.into()),
                ReleaseCommand::Rollback(x) => crate::args::ReleaseCommand::Rollback(x.into()),
                ReleaseCommand::Export(x) => crate::args::ReleaseCommand::Export(x.into()),
            },
        }
    }
}

macro_rules! fwd_release {
    ($f:ty,$t:ty { $($k:ident),+ }) => { impl From<$f> for $t { fn from(a: $f) -> Self { Self { $($k: a.$k),+ } } } };
}
fwd_release!(
    ReleaseEmitArgs,
    crate::args::ReleaseEmitArgs {
        record,
        out_dir,
        out,
        binary
    }
);
fwd_release!(
    ReleaseAssembleArgs,
    crate::args::ReleaseAssembleArgs {
        record,
        artifacts,
        out
    }
);
fwd_release!(
    ReleaseVerifyRecordArgs,
    crate::args::ReleaseVerifyRecordArgs {
        record,
        checksum,
        sha256,
        publication,
        expected_apt_metadata,
        served_apt_metadata
    }
);
fwd_release!(
    ReleaseVerifyInstalledArgs,
    crate::args::ReleaseVerifyInstalledArgs {
        record,
        deployed,
        binary,
        arch
    }
);
fwd_release!(
    ReleaseActivateArgs,
    crate::args::ReleaseActivateArgs { dir, record }
);
fwd_release!(
    ReleaseRollbackArgs,
    crate::args::ReleaseRollbackArgs { dir }
);
fwd_release!(
    ReleaseExportArgs,
    crate::args::ReleaseExportArgs { deployed }
);

impl From<RunArgs> for crate::args::RunArgs {
    fn from(a: RunArgs) -> Self {
        Self {
            slot_count: a.slot_count,
            // The service CLI is standalone `run`; only the daemon owns slots.
            slot_index: None,
            state_db: a.state_db,
            config_dir: a.config_dir,
            pat: a.pat,
            max_idle_slot_age_seconds: a.max_idle_slot_age_seconds,
            once: a.once,
            idle_timeout_seconds: a.idle_timeout_seconds,
            complete_noop: a.complete_noop,
            execute_scripts: a.execute_scripts,
            dry_run_jobs: a.dry_run_jobs,
            dump_job_message: a.dump_job_message,
            docker_image: a.docker_image,
            permit_ledger: a.permit_ledger,
            trust_scope: crate::trust_scope::resolve(&a.trust_scope).into_string(),
            emergency_reserve_bytes: a.emergency_reserve_bytes,
            job_peak_bytes: a.job_peak_bytes,
            node_action_image: a.node_action_image,
            work_dir: a.work_dir,
            docker_host_work_dir: a.docker_host_work_dir,
            skip_preflight: a.skip_preflight,
            require_docker_socket: a.require_docker_socket,
        }
    }
}

/// Binary used to spawn node-local guardian/controller/slot/job children.
///
/// Packaged hosts execute `velnor-runner` directly. When the control plane is
/// launched through `velnorctl` (for example `velnorctl host start`), slot and
/// job workers must still exec the service binary beside it.
pub fn node_service_executable() -> std::io::Result<std::path::PathBuf> {
    if let Ok(path) = std::env::var("VELNOR_SERVICE_BINARY")
        && !path.is_empty()
    {
        return Ok(std::path::PathBuf::from(path));
    }
    let current = std::env::current_exe()?;
    let file_name = current
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if (file_name == "velnorctl" || file_name.starts_with("velnorctl-"))
        && let Some(dir) = current.parent()
    {
        let candidate = dir.join(if cfg!(windows) {
            "velnor-runner.exe"
        } else {
            "velnor-runner"
        });
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Ok(current)
}

/// Service entry point: unconditional strict-capability admission, manifest
/// integrity check, service-surface CLI parsing, telemetry initialization,
/// and dispatch. Behavior matches the historical daemon bootstrap.
pub async fn execute() -> anyhow::Result<()> {
    crate::scaffold::enforce_admission()?;
    let cli = ServiceCli::parse();
    match cli.command {
        ServiceCommand::Guardian(args) => {
            crate::scaffold::init_telemetry(None);
            crate::node::run_guardian(args).await
        }
        ServiceCommand::Controller(args) => {
            crate::scaffold::init_telemetry(None);
            crate::node::run_controller(*args).await
        }
        ServiceCommand::Slot(args) => {
            crate::scaffold::init_telemetry(None);
            crate::node::run_slot(*args).await
        }
        ServiceCommand::Job(args) => {
            crate::scaffold::init_telemetry(None);
            crate::node::run_job(args).await
        }
        other => {
            let command = Command::try_from(other)?;
            let telemetry_dir = match &command {
                Command::Daemon(args) => {
                    crate::runner::daemon_config_dir(&((**args).clone().into()))
                        .ok()
                        .map(|dir| dir.join("logs"))
                }
                _ => None,
            };
            crate::scaffold::init_telemetry(telemetry_dir.as_deref());
            dispatch_service(command).await
        }
    }
}

async fn dispatch_service(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Daemon(args) => crate::runner::daemon((*args).into()).await,
        Command::Release(args) => crate::release::run(args.into()),
        Command::Cache(args) => crate::scaffold::dispatch(crate::args::Command::Cache(args)).await,
        Command::Capabilities(args) => {
            crate::scaffold::dispatch(crate::args::Command::Capabilities(args)).await
        }
        Command::Configure(args) => {
            crate::scaffold::dispatch(crate::args::Command::Configure(args)).await
        }
        Command::Preflight(args) => {
            crate::scaffold::dispatch(crate::args::Command::Preflight(args)).await
        }
        Command::Remove(args) => {
            crate::scaffold::dispatch(crate::args::Command::Remove(args)).await
        }
        Command::Status(args) => {
            crate::scaffold::dispatch(crate::args::Command::Status(args)).await
        }
        Command::Storage(args) => {
            crate::scaffold::dispatch(crate::args::Command::Storage(args)).await
        }
        Command::Doctor(args) => {
            crate::scaffold::dispatch(crate::args::Command::Doctor(args)).await
        }
    }
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
    use clap::error::ErrorKind;

    #[test]
    fn service_command_conversion_reports_node_roles_instead_of_panicking() {
        let daemon = ServiceCli::try_parse_from(["velnor-runner", "daemon"])
            .expect("daemon arguments should parse");
        assert!(matches!(
            Command::try_from(daemon.command),
            Ok(Command::Daemon(_))
        ));

        let capabilities = ServiceCli::try_parse_from(["velnor-runner", "capabilities", "export"])
            .expect("capabilities arguments should parse");
        assert!(matches!(
            Command::try_from(capabilities.command),
            Ok(Command::Capabilities(_))
        ));

        for argv in [
            vec![
                "velnor-runner",
                "guardian",
                "--state-dir",
                "/tmp/velnor-state",
            ],
            vec![
                "velnor-runner",
                "controller",
                "--state-dir",
                "/tmp/velnor-state",
            ],
            vec![
                "velnor-runner",
                "slot",
                "--state-dir",
                "/tmp/velnor-state",
                "--scope",
                "scope",
                "--slot-index",
                "0",
                "--generation",
                "1",
            ],
            vec![
                "velnor-runner",
                "job",
                "--state-dir",
                "/tmp/velnor-state",
                "--job-id",
                "job",
            ],
        ] {
            let parsed =
                ServiceCli::try_parse_from(argv).expect("node role arguments should parse");
            let error = Command::try_from(parsed.command).expect_err("node roles must not convert");
            assert!(
                error.to_string().contains("node roles dispatch"),
                "unexpected error: {error:#}"
            );
        }
    }

    #[test]
    fn service_daemon_host_mode_defaults_and_parses_explicitly() {
        let default = ServiceCli::try_parse_from(["velnor-runner", "daemon"])
            .expect("default daemon arguments should parse");
        let ServiceCommand::Daemon(default_args) = default.command else {
            panic!("expected daemon command");
        };
        assert_eq!(default_args.mode, crate::args::HostMode::NativeOnly);

        let explicit =
            ServiceCli::try_parse_from(["velnor-runner", "daemon", "--mode", "scale-set-only"])
                .expect("explicit host mode should parse");
        let ServiceCommand::Daemon(explicit_args) = explicit.command else {
            panic!("expected daemon command");
        };
        assert_eq!(explicit_args.mode, crate::args::HostMode::ScaleSetOnly);
    }

    #[test]
    fn service_slot_requires_generation() {
        let error = ServiceCli::try_parse_from([
            "velnor-runner",
            "slot",
            "--state-dir",
            "/tmp/velnor-state",
            "--scope",
            "scope",
            "--slot-index",
            "0",
        ])
        .expect_err("slot parsing should require --generation");

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
        assert!(error.to_string().contains("--generation"));
    }

    #[test]
    fn service_release_assemble_requires_artifacts() {
        let error = ServiceCli::try_parse_from([
            "velnor-runner",
            "release",
            "assemble",
            "--record",
            "/tmp/record.json",
        ])
        .expect_err("release assembly should require downloaded artifacts");

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
        assert!(error.to_string().contains("--artifacts"));

        let parsed = ServiceCli::try_parse_from([
            "velnor-runner",
            "release",
            "assemble",
            "--record",
            "/tmp/record.json",
            "--artifacts",
            "/tmp/artifacts",
        ])
        .expect("release assembly arguments should parse");
        let ServiceCommand::Release(ReleaseArgs {
            command: ReleaseCommand::Assemble(args),
        }) = parsed.command
        else {
            panic!("expected release assemble command");
        };
        assert_eq!(args.artifacts, PathBuf::from("/tmp/artifacts"));
    }

    #[test]
    fn service_release_verify_record_parses_apt_metadata_paths() {
        let parsed = ServiceCli::try_parse_from([
            "velnor-runner",
            "release",
            "verify-record",
            "--record",
            "/tmp/record.json",
            "--sha256",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "--expected-apt-metadata",
            "/tmp/expected.json",
            "--served-apt-metadata",
            "/tmp/served.json",
        ])
        .expect("release verification arguments should parse");

        let ServiceCommand::Release(ReleaseArgs {
            command: ReleaseCommand::VerifyRecord(args),
        }) = parsed.command
        else {
            panic!("expected release verify-record command");
        };
        assert_eq!(
            args.expected_apt_metadata,
            Some(PathBuf::from("/tmp/expected.json"))
        );
        assert_eq!(
            args.served_apt_metadata,
            Some(PathBuf::from("/tmp/served.json"))
        );
    }
}

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "velnor-runner")]
#[command(about = "Rust GitHub self-hosted runner compatibility agent")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect Velnor's daemon-shared host cache stores.
    Cache(CacheArgs),
    /// Inspect or validate against the compiled strict capability manifest.
    Capabilities(CapabilitiesArgs),
    /// Create and store a GitHub JIT runner configuration.
    Configure(ConfigureArgs),
    /// Run one daemon process that manages one or more internal runner slots.
    Daemon(DaemonArgs),
    /// Validate local Docker prerequisites before polling GitHub for jobs.
    Preflight(PreflightArgs),
    /// Start polling GitHub for jobs.
    Run(RunArgs),
    /// Remove local runner configuration.
    Remove(RemoveArgs),
    /// Print local runner configuration status.
    Status(StatusArgs),
    /// Inspect the canonical Velnor storage layout and catalog.
    Storage(StorageArgs),
    /// Probe GitHub for this daemon's registered runners and fail loudly when
    /// the fleet is gone (run from a systemd timer for alerting).
    Doctor(DoctorArgs),
    /// Plan 010 release-coherence chain: emit/assemble/verify the acyclic release
    /// record and atomically activate or roll back the installed identity.
    Release(ReleaseArgs),
}

/// Default host location of the atomically activated release identity. Both the
/// package scripts and the daemon `.service` units read from here, so the units
/// can invoke `release verify-installed` with no arguments.
pub const ACTIVE_RELEASE_DIR: &str = "/var/lib/velnor/release/active";
const ACTIVE_RECORD_PATH: &str = "/var/lib/velnor/release/active/record.json";
const ACTIVE_DEPLOYED_PATH: &str = "/var/lib/velnor/release/active/deployed.json";
const INSTALLED_BINARY_PATH: &str = "/usr/bin/velnor-runner";

#[derive(Debug, Args)]
pub struct ReleaseArgs {
    #[command(subcommand)]
    pub command: ReleaseCommand,
}

#[derive(Debug, Subcommand)]
pub enum ReleaseCommand {
    /// Emit a coherent release record from this (release-build) binary. Refuses
    /// to run from a development build.
    Emit(ReleaseEmitArgs),
    /// Re-assemble and re-verify a record from downloaded artifacts.
    Assemble(ReleaseAssembleArgs),
    /// Verify a release record against its independent checksum and internal
    /// coherence.
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
    /// Path to the assembled release record JSON to validate + persist.
    #[arg(long)]
    pub record: PathBuf,
    /// Release store root the immutable record is written under.
    #[arg(long, default_value = ACTIVE_RELEASE_DIR)]
    pub out_dir: PathBuf,
}

#[derive(Debug, Args)]
pub struct ReleaseAssembleArgs {
    /// Candidate record JSON.
    #[arg(long)]
    pub record: PathBuf,
    /// Directory of downloaded artifacts (per-arch `*.bin.sha256` sidecars) to
    /// cross-check the record's digests against.
    #[arg(long)]
    pub artifacts: Option<PathBuf>,
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
    /// Optional APT publication record to cross-check binds this source record.
    #[arg(long)]
    pub publication: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReleaseVerifyInstalledArgs {
    /// Active release record.
    #[arg(long, default_value = ACTIVE_RECORD_PATH)]
    pub record: PathBuf,
    /// Active deployed-identity pointer.
    #[arg(long, default_value = ACTIVE_DEPLOYED_PATH)]
    pub deployed: PathBuf,
    /// Installed binary to hash.
    #[arg(long, default_value = INSTALLED_BINARY_PATH)]
    pub binary: PathBuf,
    /// Host architecture override (amd64|arm64); defaults to the running arch.
    #[arg(long)]
    pub arch: Option<String>,
}

#[derive(Debug, Args)]
pub struct ReleaseActivateArgs {
    /// Release store root.
    #[arg(long, default_value = ACTIVE_RELEASE_DIR)]
    pub dir: PathBuf,
    /// Record to activate.
    #[arg(long)]
    pub record: PathBuf,
}

#[derive(Debug, Args)]
pub struct ReleaseRollbackArgs {
    /// Release store root.
    #[arg(long, default_value = ACTIVE_RELEASE_DIR)]
    pub dir: PathBuf,
}

#[derive(Debug, Args)]
pub struct ReleaseExportArgs {
    /// Optional deployed-identity file to pretty-print instead of the embedded
    /// build identity.
    #[arg(long)]
    pub deployed: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct CapabilitiesArgs {
    #[command(subcommand)]
    pub command: CapabilitiesCommand,
}

#[derive(Debug, Subcommand)]
pub enum CapabilitiesCommand {
    /// Validate a sanitized broker job-message JSON dump.
    Check { job_dump: PathBuf },
    /// Export the compiled manifest as JSON.
    Export,
}

#[derive(Debug, Args)]
pub struct StorageArgs {
    /// Store configuration under this directory for legacy/dev-mode resolution.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: StorageCommand,
}

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    /// Print every resolved storage root.
    Paths,
    /// Print bytes by canonical trust scope and class.
    Status,
}

#[derive(Debug, Args)]
pub struct CacheArgs {
    /// Host work directory that contains daemon-shared stores. Defaults under the runner config directory.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Store configuration under this directory when deriving the default work dir.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    #[arg(
        long,
        env = "VELNOR_BUDGET_TARGETS_BYTES",
        default_value_t = 214_748_364_800u64
    )]
    pub budget_targets_bytes: u64,

    #[arg(
        long,
        env = "VELNOR_BUDGET_CACHES_BYTES",
        default_value_t = 53_687_091_200u64
    )]
    pub budget_caches_bytes: u64,

    #[arg(
        long,
        env = "VELNOR_BUDGET_ARTIFACTS_BYTES",
        default_value_t = 21_474_836_480u64
    )]
    pub budget_artifacts_bytes: u64,

    #[arg(
        long,
        env = "VELNOR_BUDGET_CARGO_BYTES",
        default_value_t = 21_474_836_480u64
    )]
    pub budget_cargo_bytes: u64,

    #[arg(
        long,
        env = "VELNOR_BUDGET_MISE_BYTES",
        default_value_t = 21_474_836_480u64
    )]
    pub budget_mise_bytes: u64,

    #[command(subcommand)]
    pub command: CacheCommand,
}

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Report store sizes by store and scope. Read-only.
    Du,
    /// Preview or execute bounded cache eviction.
    Gc(CacheGcArgs),
}

#[derive(Debug, Args)]
pub struct CacheGcArgs {
    /// Print candidates without deleting anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Confirm deletion for a destructive GC run.
    #[arg(long)]
    pub yes: bool,

    /// Permit destructive GC before plan 036 lease wiring is active.
    #[arg(long)]
    pub force_no_lease_check: bool,

    /// Keep this many newest target buckets per trust/repo/workflow/job scope.
    #[arg(long, default_value_t = 3)]
    pub keep_newest_targets: usize,

    /// Consider cache/artifact/cargo/mise entries older than this many days candidates.
    #[arg(long, default_value_t = 30)]
    pub max_age_days: u64,

    /// Optional total byte ceiling for all GC-managed stores.
    #[arg(long)]
    pub max_size_bytes: Option<u64>,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Repository, organization, or enterprise URL the daemon registers against.
    #[arg(long)]
    pub url: String,

    /// Runner base name (slots register as <name>-slot-N).
    #[arg(long, default_value = "velnor")]
    pub name: String,

    /// Expected number of runner slots.
    #[arg(long, default_value_t = 1)]
    pub slots: usize,

    /// GitHub token used to list runners (same credential as the daemon).
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,
}

#[derive(Debug, Args)]
pub struct PreflightArgs {
    /// Host work directory for Docker job state. Defaults to ./.velnor-work.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Path where the Docker daemon host sees --work-dir. Use when DOCKER_HOST points at a remote daemon with the work dir mounted at a different path.
    #[arg(long)]
    pub docker_host_work_dir: Option<PathBuf>,

    /// Docker image used for the bind-mount visibility check.
    #[arg(long, default_value = "velnor/job-ubuntu:26.04")]
    pub docker_image: String,

    /// Require /var/run/docker.sock to exist on the host.
    #[arg(long)]
    pub require_docker_socket: bool,

    /// Require docker buildx to be available on the host.
    #[arg(long, default_value_t = true)]
    pub require_buildx: bool,
}

#[derive(Debug, Clone, Args)]
pub struct ConfigureArgs {
    /// Repository, organization, or enterprise URL accepted by GitHub JIT runner configuration.
    #[arg(long)]
    pub url: String,

    /// GitHub personal access token used to create a JIT runner configuration.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,

    /// Runner display name.
    #[arg(long)]
    pub name: Option<String>,

    /// Comma-separated labels for the JIT runner. Example: velnor,hetzner-sentry-ci.
    #[arg(long, value_delimiter = ',')]
    pub labels: Vec<String>,

    /// Add labels needed by the current target repositories' x64 Linux jobs.
    #[arg(long)]
    pub target_mvp_labels: bool,

    /// Add the current target repositories' ARM Linux label.
    #[arg(long)]
    pub target_mvp_arm_label: bool,

    /// Replace an existing runner with the same name.
    #[arg(long)]
    pub replace: bool,

    /// Runner group id for JIT configuration. Defaults to GitHub's default group id 1.
    #[arg(long)]
    pub pool_id: Option<i64>,

    /// Resolve this organization or enterprise runner group name through GitHub.
    #[arg(long)]
    pub pool_name: Option<String>,

    /// Validate local config and payloads without calling GitHub.
    #[arg(long)]
    pub dry_run: bool,

    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    /// GitHub token used for idle-slot registry health checks (the runner
    /// verifies its own registration stays online). Optional: without it the
    /// registry reconciler is disabled.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,

    /// Recycle an idle slot after this many seconds even when it still looks
    /// healthy, so every slot gets a periodically fresh JIT registration.
    /// 0 disables the bound. Defaults to 14400 (4 hours).
    #[arg(long)]
    pub max_idle_slot_age_seconds: Option<u64>,

    /// Exit after one job.
    #[arg(long)]
    pub once: bool,

    /// Fail if no job is acquired within this many seconds. Default is no idle timeout.
    #[arg(long)]
    pub idle_timeout_seconds: Option<u64>,

    /// Mark a received job as succeeded without executing user steps.
    #[arg(long)]
    pub complete_noop: bool,

    /// Execute supported run steps in Docker, then finish and acknowledge the job. This is the default unless --complete-noop or --dry-run-jobs is set.
    #[arg(long)]
    pub execute_scripts: bool,

    /// Poll and inspect jobs without acknowledging or executing them.
    #[arg(long)]
    pub dry_run_jobs: bool,

    /// Write a sanitized AgentJobRequestMessage JSON snapshot to this file or directory.
    #[arg(long)]
    pub dump_job_message: Option<PathBuf>,

    /// Docker image for --execute-scripts jobs.
    #[arg(long, default_value = "velnor/job-ubuntu:26.04")]
    pub docker_image: String,

    /// Docker --cpus limit appended to every job container. Empty disables the daemon-level CPU cap.
    #[arg(long, env = "VELNOR_JOB_CPUS", default_value = "")]
    pub job_cpus: String,

    /// Docker --memory limit appended to every job container. Empty disables the daemon-level memory cap.
    #[arg(long, env = "VELNOR_JOB_MEMORY", default_value = "")]
    pub job_memory: String,

    /// Trust boundary for this daemon/pool. "trusted" keeps full capabilities; any other value disables shared Docker socket access and rejects user secrets.
    #[arg(long, env = "VELNOR_TRUST_SCOPE", default_value = "trusted")]
    pub trust_scope: String,

    /// Filesystem bytes never available to new jobs.
    #[arg(
        long,
        env = "VELNOR_EMERGENCY_RESERVE_BYTES",
        default_value_t = 10_737_418_240u64
    )]
    pub emergency_reserve_bytes: u64,

    /// Conservative disk reservation for every advertised slot.
    #[arg(
        long,
        env = "VELNOR_JOB_PEAK_BYTES",
        default_value_t = 32_212_254_720u64
    )]
    pub job_peak_bytes: u64,

    /// Override Docker image used to run JavaScript actions. By default Velnor uses the action's declared Node runtime image.
    #[arg(long, default_value = "")]
    pub node_action_image: String,

    /// Host work directory for Docker job state. Defaults under the runner config directory.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Path where the Docker daemon host sees --work-dir. Use when DOCKER_HOST points at a remote daemon with the work dir mounted at a different path.
    #[arg(long)]
    pub docker_host_work_dir: Option<PathBuf>,

    /// Skip Docker preflight before polling GitHub for executable jobs.
    #[arg(long)]
    pub skip_preflight: bool,

    /// Require /var/run/docker.sock before polling GitHub for executable jobs.
    #[arg(long)]
    pub require_docker_socket: bool,
}

#[derive(Debug, Clone, Args)]
pub struct DaemonArgs {
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

    /// Runner group id for JIT configuration. Defaults to GitHub's default group id 1.
    #[arg(long, env = "VELNOR_POOL_ID")]
    pub pool_id: Option<i64>,

    /// Resolve this organization or enterprise runner group name through GitHub.
    #[arg(long, env = "VELNOR_POOL_NAME")]
    pub pool_name: Option<String>,

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

    /// Docker --cpus limit appended to every job container. Empty disables the daemon-level CPU cap.
    #[arg(long, env = "VELNOR_JOB_CPUS", default_value = "")]
    pub job_cpus: String,

    /// Docker --memory limit appended to every job container. Empty disables the daemon-level memory cap.
    #[arg(long, env = "VELNOR_JOB_MEMORY", default_value = "")]
    pub job_memory: String,

    /// Trust boundary for this daemon/pool. "trusted" keeps full capabilities; any other value disables shared Docker socket access and rejects user secrets.
    #[arg(long, env = "VELNOR_TRUST_SCOPE", default_value = "trusted")]
    pub trust_scope: String,

    /// Filesystem bytes never available to new jobs.
    #[arg(
        long,
        env = "VELNOR_EMERGENCY_RESERVE_BYTES",
        default_value_t = 10_737_418_240u64
    )]
    pub emergency_reserve_bytes: u64,

    /// Conservative disk reservation for every advertised slot.
    #[arg(
        long,
        env = "VELNOR_JOB_PEAK_BYTES",
        default_value_t = 32_212_254_720u64
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

    /// Skip Docker preflight before polling GitHub for executable jobs.
    #[arg(long)]
    pub skip_preflight: bool,

    /// Require /var/run/docker.sock before polling GitHub for executable jobs.
    #[arg(long)]
    pub require_docker_socket: bool,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// GitHub personal access token used to delete the exact stored JIT runner id.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub pat: Option<String>,

    /// Only remove local configuration, even if --pat is provided.
    #[arg(long)]
    pub local_only: bool,

    /// Number of daemon slot configs to remove. For --slots > 1, removes <config-dir>/slots/slot-N.
    #[arg(long, default_value_t = 1)]
    pub slots: usize,

    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Store configuration under this directory.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    /// Number of daemon slot configs to inspect. For --slots > 1, reads <config-dir>/slots/slot-N.
    #[arg(long, default_value_t = 1)]
    pub slots: usize,

    /// Validate that local config is ready for current target repository x64 Linux jobs.
    #[arg(long)]
    pub check_target_mvp: bool,
}

/// Legacy capability-bypass environment variables whose corresponding CLI flags
/// were removed. Their mere presence in a production runner's environment is a
/// deployment error and fails startup fast.
const REMOVED_BYPASS_ENV_VARS: &[&str] = &[
    "VELNOR_SKIP_CAPABILITY_VALIDATION",
    "VELNOR_DIAGNOSTIC_NODE_SIDECAR",
];

/// Enforce the strict-capability deployment policy before the CLI dispatches any
/// command. Production admission is unconditional and cannot be bypassed, so a
/// removed bypass variable — or any `VELNOR_CAPABILITY_VALIDATION` value other
/// than `strict` — fails startup. The received value is never echoed.
pub fn enforce_strict_capability_env() -> anyhow::Result<()> {
    enforce_strict_capability_env_from(|name| std::env::var(name).ok())
}

fn enforce_strict_capability_env_from(
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    for name in REMOVED_BYPASS_ENV_VARS {
        if lookup(name).is_some() {
            anyhow::bail!(
                "{name} is set, but capability-bypass switches were removed. \
                 Unset it: production admission is always strict."
            );
        }
    }
    if let Some(value) = lookup("VELNOR_CAPABILITY_VALIDATION") {
        if value != "strict" {
            anyhow::bail!(
                "VELNOR_CAPABILITY_VALIDATION must be 'strict' (received a non-strict value); \
                 strict is the only supported capability-validation mode."
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn strict_env_default_absent_is_accepted() {
        enforce_strict_capability_env_from(lookup(&[])).unwrap();
    }

    #[test]
    fn strict_env_explicit_strict_is_accepted() {
        enforce_strict_capability_env_from(lookup(&[("VELNOR_CAPABILITY_VALIDATION", "strict")]))
            .unwrap();
    }

    #[test]
    fn strict_env_rejects_removed_skip_flag_presence() {
        for value in ["1", "0", "true", "false", ""] {
            let error = enforce_strict_capability_env_from(lookup(&[(
                "VELNOR_SKIP_CAPABILITY_VALIDATION",
                value,
            )]))
            .unwrap_err();
            assert!(error
                .to_string()
                .contains("VELNOR_SKIP_CAPABILITY_VALIDATION"));
        }
    }

    #[test]
    fn strict_env_rejects_removed_diagnostic_sidecar_presence() {
        let error =
            enforce_strict_capability_env_from(lookup(&[("VELNOR_DIAGNOSTIC_NODE_SIDECAR", "1")]))
                .unwrap_err();
        assert!(error.to_string().contains("VELNOR_DIAGNOSTIC_NODE_SIDECAR"));
    }

    #[test]
    fn strict_env_rejects_non_strict_capability_validation_values() {
        for value in [
            "legacy",
            "false",
            "off",
            "skip",
            "permissive",
            "0",
            "STRICT",
        ] {
            let error = enforce_strict_capability_env_from(lookup(&[(
                "VELNOR_CAPABILITY_VALIDATION",
                value,
            )]))
            .unwrap_err();
            assert!(
                error.to_string().contains("strict"),
                "value {value} should be rejected"
            );
        }
    }

    #[test]
    fn removed_capability_bypass_flags_are_not_parseable() {
        use clap::Parser;
        for flag in ["--skip-capability-validation", "--diagnostic-node-sidecar"] {
            assert!(
                Cli::try_parse_from(["velnor-runner", "run", flag]).is_err(),
                "flag {flag} must not be accepted"
            );
            assert!(
                Cli::try_parse_from(["velnor-runner", "daemon", flag]).is_err(),
                "flag {flag} must not be accepted on daemon"
            );
        }
    }

    #[test]
    fn help_text_exposes_no_capability_bypass_path() {
        use clap::CommandFactory;
        let mut command = Cli::command();
        let mut help = Vec::new();
        command.write_long_help(&mut help).unwrap();
        let rendered = String::from_utf8(help).unwrap();
        assert!(!rendered.contains("skip-capability"));
        assert!(!rendered.contains("diagnostic-node-sidecar"));
        // Per-subcommand help must also be clean.
        for name in ["run", "daemon"] {
            let mut sub = Cli::command();
            let sub = sub.find_subcommand_mut(name).unwrap();
            let mut sub_help = Vec::new();
            sub.write_long_help(&mut sub_help).unwrap();
            let rendered = String::from_utf8(sub_help).unwrap();
            assert!(
                !rendered.contains("skip-capability"),
                "{name} help exposes skip-capability"
            );
            assert!(
                !rendered.contains("diagnostic-node-sidecar"),
                "{name} help exposes diagnostic-node-sidecar"
            );
        }
    }
}

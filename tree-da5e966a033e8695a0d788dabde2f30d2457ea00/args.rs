//! Plain argument and command types for the runner runtime handlers.
//!
//! Fully migrated out of the former `cli.rs`: every operator-facing clap
//! surface lives in the `velnorctl` command center (Plan 064 dependency law —
//! domain crates never depend on `clap`). These types carry parsed values
//! only; `velnorctl` converts its CLI-facing enums explicitly at the
//! boundary. Plan 079 deletes this crate after the runtime modules move.

use std::path::PathBuf;

/// Default host location of the atomically activated release identity. Both the
/// package scripts and the daemon `.service` units read from here, so the units
/// can invoke `release verify-installed` with no arguments.
pub const ACTIVE_RELEASE_DIR: &str = "/var/lib/velnor/release";
pub const ACTIVE_RECORD_PATH: &str = "/var/lib/velnor/release/active/record.json";
pub const ACTIVE_DEPLOYED_PATH: &str = "/var/lib/velnor/release/active/deployed.json";
/// The single installed product binary owned by the Debian package.
pub const INSTALLED_BINARY_PATH: &str = "/usr/bin/velnor-runner";

/// Runtime dispatch surface. `velnorctl` builds this enum from its CLI tree;
/// the exhaustive match in [`crate::scaffold::dispatch`] is the registry.
#[derive(Debug)]
pub enum Command {
    Cache(CacheArgs),
    Capabilities(CapabilitiesArgs),
    Configure(ConfigureArgs),
    Daemon(Box<DaemonArgs>),
    Preflight(PreflightArgs),
    Remove(RemoveArgs),
    Status(StatusArgs),
    Storage(StorageArgs),
    Doctor(DoctorArgs),
    Release(ReleaseArgs),
}

#[derive(Debug)]
pub struct ReleaseArgs {
    pub command: ReleaseCommand,
}

#[derive(Debug)]
pub enum ReleaseCommand {
    Emit(ReleaseEmitArgs),
    Assemble(ReleaseAssembleArgs),
    VerifyRecord(ReleaseVerifyRecordArgs),
    VerifyInstalled(ReleaseVerifyInstalledArgs),
    Activate(ReleaseActivateArgs),
    Rollback(ReleaseRollbackArgs),
    Export(ReleaseExportArgs),
}

#[derive(Debug)]
pub struct ReleaseEmitArgs {
    pub record: PathBuf,
    pub out_dir: PathBuf,
}

#[derive(Debug)]
pub struct ReleaseAssembleArgs {
    pub record: PathBuf,
    pub artifacts: Option<PathBuf>,
    pub out: Option<PathBuf>,
}

#[derive(Debug)]
pub struct ReleaseVerifyRecordArgs {
    pub record: PathBuf,
    pub checksum: Option<PathBuf>,
    pub sha256: Option<String>,
    pub publication: Option<PathBuf>,
    pub expected_apt_metadata: Option<PathBuf>,
    pub served_apt_metadata: Option<PathBuf>,
}

#[derive(Debug)]
pub struct ReleaseVerifyInstalledArgs {
    pub record: PathBuf,
    pub deployed: PathBuf,
    pub binary: PathBuf,
    pub arch: Option<String>,
}

#[derive(Debug)]
pub struct ReleaseActivateArgs {
    pub dir: PathBuf,
    pub record: PathBuf,
}

#[derive(Debug)]
pub struct ReleaseRollbackArgs {
    pub dir: PathBuf,
}

#[derive(Debug)]
pub struct ReleaseExportArgs {
    pub deployed: Option<PathBuf>,
}

#[derive(Debug)]
pub struct CapabilitiesArgs {
    pub command: CapabilitiesCommand,
}

#[derive(Debug)]
pub enum CapabilitiesCommand {
    Check { job_dump: PathBuf },
    Export,
}

#[derive(Debug)]
pub struct StorageArgs {
    pub config_dir: Option<PathBuf>,
    pub command: StorageCommand,
}

#[derive(Debug)]
pub enum StorageCommand {
    Paths,
    Status,
}

#[derive(Debug)]
pub struct CacheArgs {
    pub work_dir: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub budget_targets_bytes: u64,
    pub budget_caches_bytes: u64,
    pub budget_artifacts_bytes: u64,
    pub budget_cargo_bytes: u64,
    pub budget_mise_bytes: u64,
    pub command: CacheCommand,
}

#[derive(Debug)]
pub enum CacheCommand {
    Du,
    Gc(CacheGcArgs),
}

#[derive(Debug)]
pub struct CacheGcArgs {
    pub dry_run: bool,
    pub yes: bool,
    pub force_no_lease_check: bool,
    pub keep_newest_targets: usize,
    pub max_age_days: u64,
    pub max_size_bytes: Option<u64>,
}

#[derive(Debug)]
pub struct DoctorArgs {
    pub url: String,
    pub name: String,
    pub slots: usize,
    pub pat: Option<String>,
}

#[derive(Debug)]
pub struct PreflightArgs {
    pub work_dir: Option<PathBuf>,
    pub docker_host_work_dir: Option<PathBuf>,
    pub docker_image: String,
    pub require_docker_socket: bool,
    pub require_buildx: bool,
    pub execution_backend: Option<velnor_model::ExecutionBackendKind>,
    pub config_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ConfigureArgs {
    pub url: String,
    pub pat: Option<String>,
    pub name: Option<String>,
    pub labels: Vec<String>,
    pub target_mvp_labels: bool,
    pub target_mvp_arm_label: bool,
    pub replace: bool,
    pub pool_id: Option<i64>,
    pub pool_name: Option<String>,
    /// Internal daemon marker: the name/id pair was resolved and validated
    /// once before configuring all slots.
    pub pool_id_pre_resolved: bool,
    pub dry_run: bool,
    pub config_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct RunArgs {
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
    pub job_cpus: String,
    pub job_memory: String,
    pub trust_scope: String,
    pub emergency_reserve_bytes: u64,
    pub job_peak_bytes: u64,
    pub node_action_image: String,
    pub work_dir: Option<PathBuf>,
    pub docker_host_work_dir: Option<PathBuf>,
    pub skip_preflight: bool,
    pub require_docker_socket: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DaemonArgs {
    pub config_dir: Option<PathBuf>,
    pub url: Option<String>,
    #[serde(default, skip_serializing)]
    pub pat: Option<String>,
    pub name: Option<String>,
    pub labels: Vec<String>,
    pub target_mvp_labels: bool,
    pub target_mvp_arm_label: bool,
    pub replace: bool,
    pub pool_id: Option<i64>,
    pub pool_name: Option<String>,
    /// Internal daemon marker for a validated name/id pair.
    #[serde(default)]
    pub pool_id_pre_resolved: bool,
    #[serde(default)]
    pub routing_policy_file: Option<PathBuf>,
    pub dry_run_registration: bool,
    pub slots: usize,
    pub max_idle_slot_age_seconds: Option<u64>,
    pub once: bool,
    pub idle_timeout_seconds: Option<u64>,
    pub complete_noop: bool,
    pub execute_scripts: bool,
    pub dry_run_jobs: bool,
    pub dump_job_message: Option<PathBuf>,
    pub docker_image: String,
    pub job_cpus: String,
    pub job_memory: String,
    pub trust_scope: String,
    pub emergency_reserve_bytes: u64,
    pub job_peak_bytes: u64,
    pub node_action_image: String,
    pub work_dir: Option<PathBuf>,
    pub docker_host_work_dir: Option<PathBuf>,
    pub skip_preflight: bool,
    pub require_docker_socket: bool,
}

#[derive(Debug)]
pub struct RemoveArgs {
    pub pat: Option<String>,
    pub local_only: bool,
    pub slots: usize,
    pub config_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub struct StatusArgs {
    pub config_dir: Option<PathBuf>,
    pub slots: usize,
    pub check_target_mvp: bool,
}

/// Legacy capability-bypass environment variables whose corresponding CLI flags
/// were removed. Their mere presence in a production runner's environment is a
/// deployment error and fails startup fast.
const REMOVED_BYPASS_ENV_VARS: &[&str] = &[
    "VELNOR_SKIP_CAPABILITY_VALIDATION",
    "VELNOR_DIAGNOSTIC_NODE_SIDECAR",
];

/// Enforce the strict-capability deployment policy before any command runs.
/// Production admission is unconditional and cannot be bypassed, so a removed
/// bypass variable — or any `VELNOR_CAPABILITY_VALIDATION` value other than
/// `strict` — fails startup. The received value is never echoed.
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
    if let Some(value) = lookup("VELNOR_CAPABILITY_VALIDATION")
        && value != "strict"
    {
        anyhow::bail!(
            "VELNOR_CAPABILITY_VALIDATION must be 'strict' (received a non-strict value); \
                 strict is the only supported capability-validation mode."
        );
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
    fn installed_binary_path_is_the_daemon_binary() {
        assert_eq!(INSTALLED_BINARY_PATH, "/usr/bin/velnor-runner");
    }
}

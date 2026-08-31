//! Slot/job execution config on disk. Never persists a GitHub token.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::args::DaemonArgs;
use anyhow::bail;

pub const EXEC_FILE: &str = "daemon-exec.json";

/// Write daemon execution config with `pat` cleared and mode 0600.
pub fn write_exec_config(
    dir: &Path,
    args: &DaemonArgs,
    slot_count: usize,
) -> anyhow::Result<std::path::PathBuf> {
    if slot_count == 0 {
        bail!("daemon execution config must declare at least one slot");
    }
    let mut exec = args.clone();
    exec.pat = None;
    exec.slots = slot_count;
    let path = dir.join(EXEC_FILE);
    let tmp = dir.join(".daemon-exec.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&exec)?)?;
    let mut perms = std::fs::metadata(&tmp)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(&tmp, perms)?;
    std::fs::rename(&tmp, &path)?;
    let _ = std::fs::remove_file(dir.join("daemon-args.json"));
    Ok(path)
}

/// Load execution config. A serialized token is a bug; GITHUB_TOKEN comes from
/// the process environment (job unit EnvironmentFile), never this file.
pub fn load_exec_config(dir: &Path) -> anyhow::Result<DaemonArgs> {
    let bytes = std::fs::read(dir.join(EXEC_FILE))?;
    if std::str::from_utf8(&bytes).is_ok_and(|text| text.contains("\"pat\"")) {
        anyhow::bail!("daemon-exec.json must not contain a GitHub token field");
    }
    let mut args: DaemonArgs = serde_json::from_slice(&bytes)?;
    if args.pat.is_some() {
        anyhow::bail!("daemon-exec.json must not contain a GitHub token");
    }
    if args.slots == 0 {
        bail!("daemon-exec.json must declare at least one slot");
    }
    args.pat = std::env::var("GITHUB_TOKEN").ok();
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::DaemonArgs;

    fn dummy_args() -> DaemonArgs {
        serde_json::from_value(serde_json::json!({
            "url": "https://github.com/o/r",
            "name": "velnor",
            "labels": [],
            "target_mvp_labels": false,
            "target_mvp_arm_label": false,
            "replace": false,
            "dry_run_registration": false,
            "slots": 2,
            "once": false,
            "complete_noop": false,
            "execute_scripts": false,
            "dry_run_jobs": false,
            "docker_image": "img",
            "job_cpus": "",
            "job_memory": "",
            "trust_scope": "trusted",
            "emergency_reserve_bytes": 0,
            "job_peak_bytes": 0,
            "node_action_image": "img",
            "skip_preflight": false,
            "require_docker_socket": false
        }))
        .unwrap()
    }

    #[test]
    fn exec_config_never_contains_a_token() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-exec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut args = dummy_args();
        args.pat = Some("github_pat_secret".to_owned());
        let path = write_exec_config(&dir, &args, 3).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("pat"), "{text}");
        assert!(!text.contains("github_pat_secret"), "{text}");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let loaded = load_exec_config(&dir).unwrap();
        assert!(loaded.pat.is_none() || std::env::var_os("GITHUB_TOKEN").is_some());
        assert_eq!(loaded.slots, 3);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn zero_slot_exec_config_is_rejected_on_write_and_load() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-exec-zero-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let args = dummy_args();
        assert!(write_exec_config(&dir, &args, 0).is_err());
        assert!(!dir.exists());

        std::fs::create_dir_all(&dir).unwrap();
        let mut invalid = args;
        invalid.slots = 0;
        invalid.pat = None;
        std::fs::write(dir.join(EXEC_FILE), serde_json::to_vec(&invalid).unwrap()).unwrap();
        let error = load_exec_config(&dir).unwrap_err();
        assert!(error.to_string().contains("at least one slot"));
        std::fs::remove_dir_all(dir).ok();
    }
}

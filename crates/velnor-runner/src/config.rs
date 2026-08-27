use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "runner.json";

#[cfg(unix)]
fn replace_atomically(dir: &Path, path: &Path, bytes: &[u8], temp_path: &Path) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temp_path)
        .with_context(|| format!("create temporary config {}", temp_path.display()))?;
    let write_result = (|| -> Result<()> {
        file.write_all(bytes)
            .with_context(|| format!("write {}", temp_path.display()))?;
        fs::set_permissions(temp_path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("fsync {}", temp_path.display()))?;
        fs::rename(temp_path, path)
            .with_context(|| format!("atomically replace {} with new config", path.display()))?;
        fs::File::open(dir)
            .with_context(|| format!("open config directory {}", dir.display()))?
            .sync_all()
            .with_context(|| format!("fsync config directory {}", dir.display()))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(temp_path);
    }
    write_result
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerSettings {
    pub github_url: String,
    pub server_url: Option<String>,
    pub server_url_v2: Option<String>,
    pub pool_id: Option<i64>,
    pub pool_name: Option<String>,
    pub agent_id: Option<i64>,
    pub agent_name: String,
    pub labels: Vec<String>,
    pub use_v2_flow: bool,
    pub ephemeral: bool,
    pub disable_update: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCredentials {
    pub scheme: CredentialScheme,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialScheme {
    #[serde(rename = "OAuth")]
    OAuth,
    #[serde(rename = "OAuthAccessToken")]
    OAuthAccessToken,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRunnerConfig {
    pub settings: RunnerSettings,
    pub credentials: Option<StoredCredentials>,
}

/// Inputs for the shipped path resolver. Tests drive this type so they do
/// not mutate process environment.
#[derive(Debug, Clone, Default)]
pub struct ResolveConfigDir {
    pub explicit: Option<PathBuf>,
    pub velnor_config_dir: Option<std::ffi::OsString>,
    pub state_directory: Option<std::ffi::OsString>,
    pub velnor_storage_root: Option<std::ffi::OsString>,
    pub xdg_state_home: Option<std::ffi::OsString>,
    pub home: Option<std::ffi::OsString>,
}

pub fn config_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    resolve_config_dir(ResolveConfigDir {
        explicit,
        velnor_config_dir: env::var_os("VELNOR_CONFIG_DIR"),
        state_directory: env::var_os("STATE_DIRECTORY"),
        velnor_storage_root: env::var_os("VELNOR_STORAGE_ROOT"),
        xdg_state_home: env::var_os("XDG_STATE_HOME"),
        home: env::var_os("HOME"),
    })
}

/// Packaged systemd daemons use `STATE_DIRECTORY` (`/var/lib/velnor*`) or
/// `VELNOR_STORAGE_ROOT=/var` → `/var/lib/velnor/runner`. Interactive CLI
/// uses XDG state (`$XDG_STATE_HOME/velnor/runner`, default
/// `$HOME/.local/state/velnor/runner`; macOS Application Support). Never
/// `$HOME/.velnor`.
pub fn resolve_config_dir(input: ResolveConfigDir) -> Result<PathBuf> {
    if let Some(path) = input.explicit {
        return Ok(path);
    }
    if let Some(path) = nonempty_path(input.velnor_config_dir) {
        return Ok(path);
    }
    // systemd.exec StateDirectory= → STATE_DIRECTORY (colon-separated).
    if let Some(state) = nonempty_path(input.state_directory) {
        let first = state
            .to_str()
            .unwrap_or_default()
            .split(':')
            .next()
            .filter(|part| !part.is_empty());
        if let Some(first) = first {
            return Ok(PathBuf::from(first).join("runner"));
        }
        return Ok(state.join("runner"));
    }
    if let Some(prefix) = nonempty_path(input.velnor_storage_root) {
        return Ok(prefix.join("lib").join("velnor").join("runner"));
    }
    Ok(user_state_dir(input.xdg_state_home, input.home)?
        .join("velnor")
        .join("runner"))
}

pub(crate) fn user_state_dir(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    if let Some(xdg) = nonempty_path(xdg_state_home) {
        return Ok(xdg);
    }
    let home = home.context("HOME is not set; pass --config-dir")?;
    if cfg!(target_os = "macos") {
        return Ok(PathBuf::from(home).join("Library/Application Support"));
    }
    Ok(PathBuf::from(home).join(".local").join("state"))
}

pub(crate) fn user_cache_dir(
    xdg_cache_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    if let Some(xdg) = nonempty_path(xdg_cache_home) {
        return Ok(xdg);
    }
    let home = home.context("HOME is not set; pass --config-dir")?;
    if cfg!(target_os = "macos") {
        return Ok(PathBuf::from(home).join("Library/Caches"));
    }
    Ok(PathBuf::from(home).join(".cache"))
}

pub(crate) fn user_runtime_dir(xdg_runtime_dir: Option<std::ffi::OsString>) -> PathBuf {
    nonempty_path(xdg_runtime_dir).unwrap_or_else(|| std::env::temp_dir().join("velnor-run"))
}

fn nonempty_path(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

pub fn load(dir: &Path) -> Result<StoredRunnerConfig> {
    let path = dir.join(SETTINGS_FILE);
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let config =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    Ok(config)
}

pub fn save(dir: &Path, config: &StoredRunnerConfig) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 0700 {}", dir.display()))?;
    }
    let path = dir.join(SETTINGS_FILE);
    let bytes = serde_json::to_vec_pretty(config)?;
    #[cfg(unix)]
    {
        let temp_path = dir.join(format!(".{SETTINGS_FILE}.{}.tmp", uuid::Uuid::new_v4()));
        replace_atomically(dir, &path, &bytes, &temp_path)?;
    }
    #[cfg(not(unix))]
    fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn remove(dir: &Path) -> Result<bool> {
    let path = dir.join(SETTINGS_FILE);
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    Ok(true)
}

/// Atomically move a prepared successor identity into the live config path.
/// Both directories belong to one daemon slot, so the rename preserves the
/// configless-gap invariant between JIT generations.
#[cfg(any())]
pub fn promote_prepared(dir: &Path) -> Result<bool> {
    let prepared = dir.join(PREPARED_SETTINGS_DIR);
    let source = prepared.join(SETTINGS_FILE);
    if !source.exists() {
        return Ok(false);
    }

    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 0700 {}", dir.display()))?;
    }
    let target = dir.join(SETTINGS_FILE);
    fs::rename(&source, &target).with_context(|| {
        format!(
            "promote prepared runner config {} to {}",
            source.display(),
            target.display()
        )
    })?;
    let _ = fs::remove_dir(&prepared);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn saved_config_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let temp = std::env::temp_dir().join(format!(
            "velnor-config-perms-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&temp).unwrap();
        let path = temp.join(SETTINGS_FILE);
        fs::write(&path, "{}").unwrap();
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        save(
            &temp,
            &StoredRunnerConfig {
                settings: RunnerSettings {
                    github_url: "https://github.com/owner/repo".to_string(),
                    server_url: Some("https://pipelines.actions.githubusercontent.com".to_string()),
                    server_url_v2: Some("https://broker.actions.githubusercontent.com".to_string()),
                    pool_id: Some(1),
                    pool_name: Some("Default".to_string()),
                    agent_id: Some(42),
                    agent_name: "velnor-test".to_string(),
                    labels: vec!["self-hosted".to_string(), "Linux".to_string()],
                    use_v2_flow: true,
                    ephemeral: true,
                    disable_update: true,
                },
                credentials: Some(StoredCredentials {
                    scheme: CredentialScheme::OAuth,
                    data: serde_json::json!({"clientId": "client-id"}),
                }),
            },
        )
        .unwrap();

        assert_eq!(
            fs::metadata(&temp).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn failed_staging_does_not_truncate_existing_config() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let temp = std::env::temp_dir().join(format!(
            "velnor-config-atomic-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&temp).unwrap();
        let existing = br#"{"existing":true}"#;
        fs::write(temp.join(SETTINGS_FILE), existing).unwrap();
        let blocked_temp = temp.join("blocked-staging-path");
        fs::create_dir(&blocked_temp).unwrap();

        let error = replace_atomically(
            &temp,
            &temp.join(SETTINGS_FILE),
            br#"{"replacement":true}"#,
            &blocked_temp,
        )
        .unwrap_err();

        assert!(error.to_string().contains("create temporary config"));
        assert_eq!(fs::read(temp.join(SETTINGS_FILE)).unwrap(), existing);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn packaged_storage_root_is_var_lib_not_home_dot_velnor() {
        let dir = resolve_config_dir(ResolveConfigDir {
            velnor_storage_root: Some("/var".into()),
            home: Some("/root".into()),
            ..ResolveConfigDir::default()
        })
        .unwrap();
        assert_eq!(dir, PathBuf::from("/var/lib/velnor/runner"));
        assert_ne!(dir, PathBuf::from("/root/.velnor/runner"));
        assert_ne!(dir, PathBuf::from("/root/.velnor/runner/_work"));
    }

    #[test]
    fn systemd_state_directory_wins_over_home() {
        let dir = resolve_config_dir(ResolveConfigDir {
            state_directory: Some("/var/lib/velnor-tailrocks".into()),
            velnor_storage_root: Some("/var".into()),
            home: Some("/root".into()),
            ..ResolveConfigDir::default()
        })
        .unwrap();
        assert_eq!(dir, PathBuf::from("/var/lib/velnor-tailrocks/runner"));
        assert!(!dir.starts_with("/root/.velnor"));
    }

    #[test]
    fn user_cli_path_uses_xdg_state_home() {
        let dir = resolve_config_dir(ResolveConfigDir {
            xdg_state_home: Some("/home/alice/.local/state".into()),
            home: Some("/home/alice".into()),
            ..ResolveConfigDir::default()
        })
        .unwrap();
        assert_eq!(dir, PathBuf::from("/home/alice/.local/state/velnor/runner"));
        assert!(!dir.as_os_str().to_string_lossy().contains("/.velnor/"));
    }

    fn shipped_state_directory(unit: &str) -> &str {
        unit.lines()
            .find_map(|line| line.strip_prefix("StateDirectory="))
            .unwrap()
    }

    /// systemd.exec maps relative `StateDirectory=name` to `/var/lib/name`.
    fn systemd_state_directory_env(state_directory: &str, instance: Option<&str>) -> String {
        let name = match instance {
            Some(instance) => state_directory.replace("%i", instance),
            None => state_directory.to_string(),
        };
        format!("/var/lib/{name}")
    }

    fn doctor_unit_env(state_directory: &str, instance: Option<&str>) -> ResolveConfigDir {
        ResolveConfigDir {
            state_directory: Some(systemd_state_directory_env(state_directory, instance).into()),
            velnor_storage_root: Some("/var".into()),
            home: Some("/root".into()),
            ..ResolveConfigDir::default()
        }
    }

    #[test]
    fn doctor_instance_unit_shares_daemon_state_directory() {
        let doctor = include_str!("../debian/velnor-doctor@.service");
        let daemon = include_str!("../debian/velnor-daemon@.service");
        assert_eq!(shipped_state_directory(doctor), "velnor-%i");
        assert_eq!(
            shipped_state_directory(doctor),
            shipped_state_directory(daemon)
        );

        let doctor_dir = resolve_config_dir(doctor_unit_env(
            shipped_state_directory(doctor),
            Some("tailrocks"),
        ))
        .unwrap();
        let daemon_dir = resolve_config_dir(doctor_unit_env(
            shipped_state_directory(daemon),
            Some("tailrocks"),
        ))
        .unwrap();
        assert_eq!(doctor_dir, daemon_dir);
        assert_eq!(
            doctor_dir,
            PathBuf::from("/var/lib/velnor-tailrocks/runner")
        );
        assert_ne!(doctor_dir, PathBuf::from("/var/lib/velnor/runner"));
        assert!(!doctor_dir.starts_with("/root/.velnor"));
    }

    #[test]
    fn doctor_base_unit_shares_daemon_state_directory() {
        let doctor = include_str!("../debian/velnor-doctor.service");
        let daemon = include_str!("../debian/velnor-daemon.service");
        assert_eq!(shipped_state_directory(doctor), "velnor");
        assert_eq!(
            shipped_state_directory(doctor),
            shipped_state_directory(daemon)
        );

        let doctor_dir =
            resolve_config_dir(doctor_unit_env(shipped_state_directory(doctor), None)).unwrap();
        assert_eq!(doctor_dir, PathBuf::from("/var/lib/velnor/runner"));
        assert!(!doctor_dir.starts_with("/root/.velnor"));
    }

    #[test]
    fn packaged_env_explicitly_selects_native_github_transport() {
        let env = include_str!("../debian/velnor.env");
        let assignments: Vec<_> = env
            .lines()
            .filter(|line| line.starts_with("VELNOR_GITHUB_HTTP_TRANSPORT="))
            .collect();
        assert_eq!(assignments, ["VELNOR_GITHUB_HTTP_TRANSPORT=native"]);
    }
}

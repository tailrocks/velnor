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

pub fn config_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }

    if let Ok(path) = env::var("VELNOR_CONFIG_DIR") {
        return Ok(PathBuf::from(path));
    }

    let home = env::var("HOME").context("HOME is not set; pass --config-dir")?;
    Ok(PathBuf::from(home).join(".velnor").join("runner"))
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
}

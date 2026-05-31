use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "runner.json";

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
    OAuth,
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
    let path = dir.join(SETTINGS_FILE);
    let bytes = serde_json::to_vec_pretty(config)?;
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

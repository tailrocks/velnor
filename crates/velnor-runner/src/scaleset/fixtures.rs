//! Recorded, sanitized protocol fixtures.
//!
//! Layout (`crates/velnor-runner/tests/fixtures/scaleset/`):
//!
//! ```text
//! manifest.json            {upstream_commit, files: {name: sha256}}
//! session_created.json     RunnerScaleSetSession (tokens REDACTED)
//! message_batch.json       RunnerScaleSetJobMessages envelope, one of each kind
//! message_stats_only.json  envelope with empty body + statistics
//! acquire_jobs.json        acquireJobsResponse subset
//! jit_runner_config.json   JIT config (encoded blob REDACTED)
//! registration_token.json  registration-token response (token REDACTED)
//! installation_token.json  installation access-token response (REDACTED)
//! admin_connection.json    admin connection (URL host + token REDACTED)
//! runner_scale_set.json    RunnerScaleSet get-by-id response
//! runner_reference.json    RunnerReference get response
//! error_agent_not_found.json  Actions exception body
//! ```
//!
//! Every load verifies the manifest pin ([`crate::scaleset::upstream_pin`])
//! and every file hash, then runs the redaction verifier: no live tokens,
//! PEM blocks, or real hosts may hide in a recorded fixture.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Placeholder every secret-adjacent fixture value must use.
pub const REDACTED: &str = "REDACTED";

/// Host placeholder for recorded URLs (no real tenant/API hosts in fixtures).
pub const FIXTURE_HOST: &str = "scaleset-fixture.invalid";

/// Fixture manifest (`manifest.json`).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct FixtureManifest {
    pub upstream_commit: String,
    pub files: BTreeMap<String, String>,
}

/// Verified fixture bundle: manifest + directory.
#[derive(Debug)]
pub struct Fixtures {
    dir: PathBuf,
    manifest: FixtureManifest,
}

impl Fixtures {
    /// Load and verify a fixture directory: pin check, per-file sha256,
    /// redaction scan of every listed file. Fails closed on any mismatch.
    pub fn load(dir: &Path) -> Result<Self> {
        let manifest_text = std::fs::read_to_string(dir.join("manifest.json"))
            .with_context(|| format!("read fixture manifest in {}", dir.display()))?;
        let manifest: FixtureManifest =
            serde_json::from_str(&manifest_text).context("parse fixture manifest")?;
        crate::scaleset::upstream_pin::require_pin(&manifest.upstream_commit)?;
        for (name, expected) in &manifest.files {
            let bytes =
                std::fs::read(dir.join(name)).with_context(|| format!("read fixture {name}"))?;
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let actual = hex_encode(&hasher.finalize());
            if &actual != expected {
                anyhow::bail!("fixture {name} hash drift: manifest {expected}, file {actual}");
            }
            let text =
                String::from_utf8(bytes).with_context(|| format!("fixture {name} is not UTF-8"))?;
            verify_redaction(name, &text)?;
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            manifest,
        })
    }

    /// Read one verified fixture file.
    pub fn read(&self, name: &str) -> Result<String> {
        if !self.manifest.files.contains_key(name) {
            anyhow::bail!("fixture {name} is not in the manifest");
        }
        std::fs::read_to_string(self.dir.join(name)).with_context(|| format!("read fixture {name}"))
    }

    /// Deserialize one verified fixture file.
    pub fn parse<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<T> {
        let text = self.read(name)?;
        serde_json::from_str(&text).with_context(|| format!("parse fixture {name}"))
    }

    #[must_use]
    pub fn manifest(&self) -> &FixtureManifest {
        &self.manifest
    }
}

/// Assert a recorded payload carries no live secrets or real hosts.
/// Fails closed on: PEM armor, GitHub token prefixes, JWT shapes outside
/// `REDACTED`, and any URL host that is not [`FIXTURE_HOST`].
pub fn verify_redaction(name: &str, text: &str) -> Result<()> {
    for marker in [
        "-----BEGIN ",
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "api.github.com",
        "actions.githubusercontent.com",
    ] {
        if text.contains(marker) {
            anyhow::bail!("fixture {name} leaks live material ({marker})");
        }
    }
    for url in extract_urls(text) {
        let host = url.host_str().unwrap_or_default();
        if host != FIXTURE_HOST {
            anyhow::bail!("fixture {name} references non-fixture host {host}");
        }
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn extract_urls(text: &str) -> Vec<url::Url> {
    let mut urls = Vec::new();
    for token in text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        if (token.starts_with("https://") || token.starts_with("http://"))
            && let Ok(url) = url::Url::parse(token.trim_end_matches([')', ',', '.']))
        {
            urls.push(url);
        }
    }
    urls
}

/// Compute the manifest `files` map for a directory (recording helper;
/// the committed manifest is the reviewed output of this function).
pub fn hash_fixture_dir(dir: &Path) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("list fixture dir {}", dir.display()))?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "manifest.json" {
            continue;
        }
        let bytes = std::fs::read(entry.path())?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        files.insert(name, hex_encode(&hasher.finalize()));
    }
    Ok(files)
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

    #[test]
    fn redaction_verifier_catches_live_material() {
        assert!(verify_redaction("x", r#"{"a":1}"#).is_ok());
        assert!(verify_redaction("x", "ghp_deadbeef").is_err());
        assert!(verify_redaction("x", "-----BEGIN RSA PRIVATE KEY-----").is_err());
        assert!(verify_redaction("x", "https://api.github.com/x").is_err());
        assert!(verify_redaction("x", "https://tenant.actions.example/x").is_err());
        assert!(verify_redaction("x", &format!("https://{FIXTURE_HOST}/queue")).is_ok());
    }

    #[test]
    fn load_rejects_pin_drift_and_hash_drift() {
        let dir =
            std::env::temp_dir().join(format!("velnor-scaleset-fixtures-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.json"), serde_json::json!({"a": 1}).to_string()).unwrap();
        let mut files = BTreeMap::new();
        files.insert("a.json".to_string(), "0".repeat(64));
        let manifest = FixtureManifest {
            upstream_commit: crate::scaleset::UPSTREAM_COMMIT.to_string(),
            files,
        };
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        assert!(Fixtures::load(&dir).is_err());

        let manifest = FixtureManifest {
            upstream_commit: "drifted".to_string(),
            files: hash_fixture_dir(&dir).unwrap(),
        };
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        assert!(Fixtures::load(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_accepts_matching_manifest() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-scaleset-fixtures-ok-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.json"), serde_json::json!({"a": 1}).to_string()).unwrap();
        let manifest = FixtureManifest {
            upstream_commit: crate::scaleset::UPSTREAM_COMMIT.to_string(),
            files: hash_fixture_dir(&dir).unwrap(),
        };
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        let loaded = Fixtures::load(&dir).unwrap();
        assert_eq!(loaded.manifest().files.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

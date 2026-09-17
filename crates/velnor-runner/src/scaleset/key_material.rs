//! Key material loading for the Scale Set adapter: 0600 files or env.
//!
//! App private keys and PATs enter the daemon from a root-only file or an
//! explicitly named environment variable. This module is the one place that
//! reads them, and it never logs, formats, or returns them in an error:
//! every `Debug` impl and every error names the *source* (path or variable
//! name) while the value stays in memory on its way into the credential
//! provider.
//!
//! File rule: a key file must grant nothing to group/other (mode `0600` or
//! stricter — `0400` is fine). A readable-by-others key file fails the
//! daemon before any GitHub call, naming the path only.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Where one secret comes from. `Debug` prints the source, never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// Read from this file (must be `0600` or stricter on unix).
    File(PathBuf),
    /// Read from this environment variable.
    Env(String),
}

impl KeySource {
    /// Human-readable source description for errors and status lines.
    /// Never contains secret content.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::File(path) => format!("file {}", path.display()),
            Self::Env(name) => format!("env {name}"),
        }
    }
}

/// GitHub App credential configuration (key material by reference only).
/// `Debug` prints identifiers and the key source, never the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppKeyConfig {
    pub client_id: String,
    pub installation_id: i64,
    pub key: KeySource,
}

/// Scale-set credential configuration (secrets by reference only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaleSetAuthConfig {
    App(AppKeyConfig),
    Pat(KeySource),
}

/// Load GitHub App credentials: validates identifiers, reads the private
/// key from its source, and returns the validated auth struct the
/// [`ScaleSetClient::new_with_app`][crate::scaleset::ScaleSetClient::new_with_app]
/// constructor takes.
pub fn load_app_auth(config: &AppKeyConfig) -> Result<crate::scaleset::GitHubAppAuth> {
    if config.client_id.is_empty() {
        anyhow::bail!("scale-set App client ID is required");
    }
    if config.installation_id == 0 {
        anyhow::bail!("scale-set App installation ID is required");
    }
    let private_key_pem = read_secret(&config.key, "App private key")?;
    let auth = crate::scaleset::GitHubAppAuth {
        client_id: config.client_id.clone(),
        installation_id: config.installation_id,
        private_key_pem,
    };
    auth.validate().context("invalid App credentials")?;
    Ok(auth)
}

/// Load a personal access token from its source for
/// [`ScaleSetClient::new_with_pat`][crate::scaleset::ScaleSetClient::new_with_pat].
pub fn load_pat(source: &KeySource) -> Result<String> {
    let token = read_secret(source, "personal access token")?;
    if looks_like_placeholder(&token) {
        anyhow::bail!(
            "scale-set PAT from {} is an unexpanded placeholder; refusing to authenticate with it",
            source.describe()
        );
    }
    Ok(token)
}

/// Read one secret, enforcing the file-permission rule. Errors name the
/// source and the failure only — never the value.
fn read_secret(source: &KeySource, what: &str) -> Result<String> {
    match source {
        KeySource::Env(name) => {
            let value = std::env::var(name)
                .with_context(|| format!("scale-set {what} env var {name} is not set"))?;
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                anyhow::bail!("scale-set {what} env var {name} is empty");
            }
            Ok(trimmed)
        }
        KeySource::File(path) => {
            check_key_file_permissions(path, what)?;
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("read scale-set {what} from {}", path.display()))?;
            let trimmed = raw.trim().to_owned();
            if trimmed.is_empty() {
                anyhow::bail!("scale-set {what} from {} is empty", path.display());
            }
            Ok(trimmed)
        }
    }
}

/// Reject key files readable by group/other. The value is never inspected
/// here — only the permission bits.
fn check_key_file_permissions(path: &Path, what: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .with_context(|| format!("stat scale-set {what} file {}", path.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "scale-set {what} file {} has mode {:04o}: group/other access denied, use 0600 or stricter",
                path.display(),
                mode & 0o7777,
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, what);
    }
    Ok(())
}

/// systemd `EnvironmentFile` does not expand variables: a literal
/// `${...}` placeholder can never authenticate.
fn looks_like_placeholder(value: &str) -> bool {
    value.contains("${")
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

    fn temp_key(name: &str, contents: &str, mode: u32) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "velnor-keymat-{name}-{}-{}.pem",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        path
    }

    #[test]
    fn file_0600_loads_and_trims() {
        let path = temp_key("ok", "s3cret\n", 0o600);
        let secret = read_secret(&KeySource::File(path.clone()), "token").unwrap();
        assert_eq!(secret, "s3cret");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn file_0400_loads() {
        let path = temp_key("ro", "s3cret", 0o400);
        let secret = read_secret(&KeySource::File(path.clone()), "token").unwrap();
        assert_eq!(secret, "s3cret");
        std::fs::remove_file(&path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn file_0644_is_rejected_naming_path_only() {
        let path = temp_key("wide", "s3cret", 0o644);
        let error = read_secret(&KeySource::File(path.clone()), "token").unwrap_err();
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(&path.display().to_string()),
            "names the path: {rendered}"
        );
        assert!(
            !rendered.contains("s3cret"),
            "never leaks the value: {rendered}"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn file_0600_plus_group_is_rejected() {
        let path = temp_key("grp", "s3cret", 0o640);
        assert!(
            read_secret(&KeySource::File(path.clone()), "token").is_err(),
            "group-readable key must fail closed"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn empty_file_is_rejected() {
        let path = temp_key("empty", "  \n", 0o600);
        assert!(
            read_secret(&KeySource::File(path.clone()), "token").is_err(),
            "empty key must fail closed"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_file_names_path_not_value() {
        let missing = std::env::temp_dir().join(format!(
            "velnor-keymat-absent-{}-{}.pem",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let error = read_secret(&KeySource::File(missing.clone()), "token").unwrap_err();
        assert!(format!("{error:#}").contains(&missing.display().to_string()));
    }

    #[test]
    fn env_loads_and_empty_fails() {
        let name = format!(
            "VELNOR_TEST_KEYMAT_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        assert!(read_secret(&KeySource::Env(name.clone()), "token").is_err());
        unsafe { std::env::set_var(&name, "  env-secret  ") };
        assert_eq!(
            read_secret(&KeySource::Env(name.clone()), "token").unwrap(),
            "env-secret"
        );
        unsafe { std::env::remove_var(&name) };
    }

    #[test]
    fn pat_placeholder_is_rejected() {
        let name = format!(
            "VELNOR_TEST_KEYMAT_PH_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        unsafe { std::env::set_var(&name, "${GITHUB_TOKEN}") };
        let error = load_pat(&KeySource::Env(name.clone())).unwrap_err();
        assert!(format!("{error:#}").contains(&name));
        unsafe { std::env::remove_var(&name) };
    }

    #[test]
    fn app_auth_validates_identifiers_without_touching_key() {
        let empty_client = AppKeyConfig {
            client_id: String::new(),
            installation_id: 7,
            key: KeySource::Env("VELNOR_TEST_KEYMAT_NEVER_SET".into()),
        };
        assert!(load_app_auth(&empty_client).is_err());
        let zero_install = AppKeyConfig {
            client_id: "Iv1.abc".into(),
            installation_id: 0,
            key: KeySource::Env("VELNOR_TEST_KEYMAT_NEVER_SET".into()),
        };
        assert!(load_app_auth(&zero_install).is_err());
    }

    #[test]
    fn debug_impls_never_carry_key_material() {
        let app = AppKeyConfig {
            client_id: "Iv1.client".into(),
            installation_id: 42,
            key: KeySource::Env("VELNOR_TEST_KEYMAT_NEVER_SET".into()),
        };
        let rendered = format!("{app:?}");
        assert!(rendered.contains("Iv1.client"));
        assert!(rendered.contains("VELNOR_TEST_KEYMAT_NEVER_SET"));
        let auth = crate::scaleset::GitHubAppAuth {
            client_id: "Iv1.client".into(),
            installation_id: 42,
            private_key_pem: "super-secret-pem-body".into(),
        };
        let rendered = format!("{auth:?}");
        assert!(rendered.contains("Iv1.client"));
        assert!(
            !rendered.contains("super-secret-pem-body"),
            "App auth Debug leaked key: {rendered}"
        );
        let pat = crate::scaleset::ActionsAuth::pat("ghp-live-token".into());
        let rendered = format!("{pat:?}");
        assert!(
            !rendered.contains("ghp-live-token"),
            "ActionsAuth Debug leaked PAT: {rendered}"
        );
    }
}

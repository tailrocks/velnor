//! Strict, fail-closed mise policy.
//!
//! Plan 008 makes the native mise adapter mandatory-locked. This module holds
//! the *pure* policy helpers that decide, before any project command runs,
//! whether a mise invocation is admissible:
//!
//! * the requested mise binary version is an exact `YYYY.M.D` date-version (or
//!   omitted, which resolves to the fleet pin — never a live "latest" lookup);
//! * `install_args` name only tool keys that are actually committed in the
//!   adjacent lock (no flags, `@version`, URLs, paths, or shell syntax);
//! * the effective config resolved from the action working directory has a
//!   correctly-named adjacent lock, both are git-tracked regular files inside
//!   the checkout, clean against index/HEAD, and the config enables
//!   `[settings] lockfile = true`.
//!
//! Everything here is deterministic and unit-tested. Git access is expressed
//! through the [`GitOracle`] trait so the policy is exercised without a real
//! repository; [`GitCli`] is the production implementation.
//!
//! TOML is parsed with the `toml` crate — never with ad-hoc regexes.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Fleet-pinned mise binary version.
///
/// **008-R5 decision.** mise-action's `version` input selects the *mise binary*
/// itself; omitting it means "install latest stable", which is a live network
/// lookup. That contradicts the fail-closed `MISE_LOCKED=1` contract this plan
/// enforces. So an omitted action version resolves to this committed constant —
/// offline and reproducible — instead of reaching the network. The value MUST
/// match the `MISE_VERSION` baked by `docker/job-ubuntu.Dockerfile` and the
/// exact version admitted by `manifest.rs`. There is no field in `mise.lock`
/// for the mise binary itself (the lock pins *tools*), so this constant is the
/// authoritative committed record of the fleet pin.
pub const FLEET_PINNED_MISE_VERSION: &str = "2026.7.7";

/// Config file names mise recognizes, each paired with its adjacent lock name.
/// Ordered by mise's own precedence (nearest, most specific first).
const CONFIG_LOCK_PAIRS: &[(&str, &str)] = &[
    ("mise.toml", "mise.lock"),
    (".mise.toml", ".mise.lock"),
    ("mise/config.toml", "mise/config.lock"),
    (".mise/config.toml", ".mise/config.lock"),
    (".config/mise.toml", ".config/mise.lock"),
    (".config/mise/config.toml", ".config/mise/config.lock"),
];

/// Is `value` an admissible mise binary version selector?
///
/// Accepts an empty string (omitted → fleet-pinned latest, see
/// [`resolve_effective_mise_version`]) or an exact `YYYY.M.D` date-version.
/// Rejects a leading `v`, surrounding/embedded whitespace, path separators,
/// range/selector operators, `latest`, and template expressions.
pub fn is_valid_mise_version(value: &str) -> bool {
    if value.is_empty() {
        // Omission alone means "latest stable" (resolved offline to the pin).
        return true;
    }
    // No surrounding or embedded whitespace, and nothing that is not a plain
    // dotted numeric date. This rejects `v2026.7.7`, ` 2026.7.7`, `latest`,
    // `~2026`, `>=2026.7.7`, `2026.*`, `2026.7.x`, paths, and `{{ ... }}`.
    if value.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    let bounds = [(4usize, 2000u32, 9999u32), (2, 1, 12), (2, 1, 31)];
    for (part, (max_len, lo, hi)) in parts.iter().zip(bounds) {
        if part.is_empty() || part.len() > max_len {
            return false;
        }
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        let Ok(n) = part.parse::<u32>() else {
            return false;
        };
        if n < lo || n > hi {
            return false;
        }
    }
    true
}

/// Resolve the exact mise binary version to install and persist.
///
/// An explicit valid version is returned verbatim; an omitted (empty) version
/// resolves to [`FLEET_PINNED_MISE_VERSION`] (008-R5). Any other value, or an
/// empty pin, fails closed rather than reaching the network.
pub fn resolve_effective_mise_version(requested: &str) -> Result<String> {
    let requested = requested.trim();
    if requested.is_empty() {
        if FLEET_PINNED_MISE_VERSION.is_empty() || !is_valid_mise_version(FLEET_PINNED_MISE_VERSION)
        {
            bail!(
                "mise version omitted and no fleet-pinned version is recorded; refusing a live \
                 'latest' lookup under MISE_LOCKED"
            );
        }
        return Ok(FLEET_PINNED_MISE_VERSION.to_string());
    }
    if !is_valid_mise_version(requested) {
        bail!(
            "mise version {requested:?} is not an exact YYYY.M.D date-version \
             (reject leading 'v', ranges, 'latest', paths, whitespace, expressions)"
        );
    }
    Ok(requested.to_string())
}

/// Is `token` a syntactically valid single mise tool key (not a flag,
/// `@version`, URL, path, or shell fragment)?
///
/// Backend-qualified keys such as `aqua:owner/repo`, `cargo:rust-script`, and
/// `core:rust` are allowed; membership in the committed lock is checked
/// separately by [`validate_install_args_against_lock`].
pub fn is_valid_install_arg_token(token: &str) -> bool {
    if token.is_empty() || token.len() > 200 {
        return false;
    }
    // Flags, version pins, URLs, and template/shell syntax are never tool keys.
    if token.starts_with('-') || token.contains('@') || token.contains("://") {
        return false;
    }
    // Filesystem paths (absolute, relative, home, traversal, Windows).
    if token.starts_with('/')
        || token.starts_with('.')
        || token.starts_with('~')
        || token.contains('\\')
        || token.contains("..")
    {
        return false;
    }
    if token.starts_with(':') {
        return false;
    }
    // Whitelist the character set. Anything else (whitespace already split off,
    // plus `$`, backticks, quotes, `;`, `&`, `|`, `*`, `?`, parens, braces …)
    // is rejected as a shell metacharacter.
    if !token
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/' | b'+'))
    {
        return false;
    }
    // Must carry at least one alphanumeric character.
    token.bytes().any(|b| b.is_ascii_alphanumeric())
}

/// Validate the *shape* of `install_args`: whitespace-separated, each token a
/// valid tool key. Empty/blank is accepted ("install all locked tools"). This
/// is the static check wired into the capability manifest; it does not read the
/// lock.
pub fn is_valid_install_args_shape(install_args: &str) -> bool {
    install_args
        .split_whitespace()
        .all(is_valid_install_arg_token)
}

/// Does the config TOML enable `[settings] lockfile = true`?
pub fn settings_lockfile_enabled(config_toml: &str) -> Result<bool> {
    let table: toml::Table = config_toml
        .parse()
        .context("parse mise config TOML for [settings] lockfile")?;
    Ok(table
        .get("settings")
        .and_then(|settings| settings.as_table())
        .and_then(|settings| settings.get("lockfile"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false))
}

/// Collect every tool key committed in a `mise.lock` (`[[tools.<key>]]`).
pub fn lock_tool_keys(lock_toml: &str) -> Result<BTreeSet<String>> {
    let table: toml::Table = lock_toml
        .parse()
        .context("parse mise.lock TOML for committed tool keys")?;
    let keys = table
        .get("tools")
        .and_then(toml::Value::as_table)
        .map(|tools| tools.keys().cloned().collect())
        .unwrap_or_default();
    Ok(keys)
}

/// Every `install_args` token must be a valid tool key *and* committed in
/// `lock_toml`. Blank args are accepted ("install all locked tools").
pub fn validate_install_args_against_lock(install_args: &str, lock_toml: &str) -> Result<()> {
    let install_args = install_args.trim();
    if install_args.is_empty() {
        return Ok(());
    }
    let committed = lock_tool_keys(lock_toml)?;
    for token in install_args.split_whitespace() {
        if !is_valid_install_arg_token(token) {
            bail!(
                "mise install_args token {token:?} is not a bare tool key \
                 (no flags, @version, URLs, paths, or shell syntax)"
            );
        }
        if !committed.contains(token) {
            bail!("mise install_args token {token:?} is not present in the committed mise.lock");
        }
    }
    Ok(())
}

/// The config + lock a locked mise install will use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLockedConfig {
    /// Absolute path to the effective mise config file.
    pub config: PathBuf,
    /// Absolute path to its adjacent lock file.
    pub lock: PathBuf,
}

/// Git queries the policy needs, abstracted so it can be faked in tests.
pub trait GitOracle {
    /// Is `path` tracked by the git repository rooted at `repo_root`?
    fn is_tracked(&self, repo_root: &Path, path: &Path) -> Result<bool>;
    /// Is `path` clean (no uncommitted/staged change) in `repo_root`?
    fn is_clean(&self, repo_root: &Path, path: &Path) -> Result<bool>;
}

/// Production [`GitOracle`] shelling out to the `git` binary.
pub struct GitCli;

impl GitOracle for GitCli {
    fn is_tracked(&self, repo_root: &Path, path: &Path) -> Result<bool> {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["ls-files", "--error-unmatch", "--"])
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .context("run git ls-files")?;
        Ok(status.success())
    }

    fn is_clean(&self, repo_root: &Path, path: &Path) -> Result<bool> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["status", "--porcelain", "--"])
            .arg(path)
            .output()
            .context("run git status")?;
        if !output.status.success() {
            bail!("git status failed for {}", path.display());
        }
        Ok(output.stdout.is_empty())
    }
}

/// Locate and fully validate the locked mise config for `working_directory`.
///
/// Walks from `working_directory` up to `checkout_root`, returning the first
/// recognized config. Requires: an adjacent correctly-named lock; both are
/// regular (non-symlink) files inside the checkout; both are git-tracked and
/// clean against index/HEAD; and the config enables `[settings] lockfile`.
/// Any failure is fail-closed.
pub fn resolve_locked_mise_config(
    checkout_root: &Path,
    working_directory: &Path,
    git: &impl GitOracle,
) -> Result<ResolvedLockedConfig> {
    let checkout_root = checkout_root
        .canonicalize()
        .with_context(|| format!("canonicalize checkout root {}", checkout_root.display()))?;
    let working_directory = working_directory.canonicalize().with_context(|| {
        format!(
            "canonicalize mise working directory {}",
            working_directory.display()
        )
    })?;
    if !working_directory.starts_with(&checkout_root) {
        bail!(
            "mise working directory {} is outside the checkout {}",
            working_directory.display(),
            checkout_root.display()
        );
    }

    let mut dir = working_directory.as_path();
    loop {
        for (config_name, lock_name) in CONFIG_LOCK_PAIRS {
            let config = dir.join(config_name);
            if !is_regular_file(&config) {
                continue;
            }
            let lock = dir.join(lock_name);
            if !is_regular_file(&lock) {
                bail!(
                    "mise config {} has no adjacent committed lock {}",
                    config.display(),
                    lock.display()
                );
            }
            let config = normalize_inside(&config, &checkout_root)?;
            let lock = normalize_inside(&lock, &checkout_root)?;
            for file in [&config, &lock] {
                if !git.is_tracked(&checkout_root, file)? {
                    bail!("mise policy: {} is not tracked by git", file.display());
                }
                if !git.is_clean(&checkout_root, file)? {
                    bail!(
                        "mise policy: {} is dirty against index/HEAD",
                        file.display()
                    );
                }
            }
            let config_src = std::fs::read_to_string(&config)
                .with_context(|| format!("read mise config {}", config.display()))?;
            if !settings_lockfile_enabled(&config_src)? {
                bail!(
                    "mise config {} does not set [settings] lockfile = true",
                    config.display()
                );
            }
            return Ok(ResolvedLockedConfig { config, lock });
        }
        if dir == checkout_root {
            break;
        }
        match dir.parent() {
            Some(parent) if parent.starts_with(&checkout_root) => dir = parent,
            _ => break,
        }
    }
    bail!(
        "no locked mise config found between {} and {}",
        working_directory.display(),
        checkout_root.display()
    )
}

/// Whole-invocation gate: resolve the locked config, then validate
/// `install_args` against its committed lock. Returns the resolved config on
/// success so the caller can pass the exact config/lock paths downstream.
pub fn enforce_locked_mise_policy(
    checkout_root: &Path,
    working_directory: &Path,
    install_args: &str,
    git: &impl GitOracle,
) -> Result<ResolvedLockedConfig> {
    let resolved = resolve_locked_mise_config(checkout_root, working_directory, git)?;
    let lock_src = std::fs::read_to_string(&resolved.lock)
        .with_context(|| format!("read mise.lock {}", resolved.lock.display()))?;
    validate_install_args_against_lock(install_args, &lock_src)?;
    Ok(resolved)
}

/// Regular file that is not a symlink or directory.
fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_file())
        .unwrap_or(false)
}

/// Canonicalize `path` and confirm it is inside `checkout_root`.
fn normalize_inside(path: &Path, checkout_root: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", path.display()))?;
    if !canonical.starts_with(checkout_root) {
        bail!(
            "{} resolves outside the checkout {}",
            canonical.display(),
            checkout_root.display()
        );
    }
    // Defense in depth: no traversal component survived canonicalization.
    if canonical
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        bail!("{} contains a parent-dir traversal", canonical.display());
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    // ── version validation ────────────────────────────────────────────────

    #[test]
    fn exact_date_versions_are_valid_and_omission_is_valid() {
        assert!(is_valid_mise_version("2026.7.7"));
        assert!(is_valid_mise_version("2026.07.07"));
        assert!(is_valid_mise_version("2026.12.31"));
        assert!(is_valid_mise_version("")); // omitted → latest stable
    }

    #[test]
    fn malformed_versions_are_rejected() {
        for bad in [
            "v2026.7.7",     // leading v
            " 2026.7.7",     // whitespace
            "2026.7.7 ",     // trailing whitespace
            "2026 .7.7",     // embedded whitespace
            "latest",        // selector
            "~2026.7.7",     // range
            ">=2026.7.7",    // range
            "2026.*",        // wildcard
            "2026.7.x",      // wildcard
            "2026.7",        // too few parts
            "2026.7.7.1",    // too many parts
            "2026.13.7",     // month out of range
            "2026.7.32",     // day out of range
            "1999.7.7",      // year out of range
            "../2026.7.7",   // path
            "{{ version }}", // expression
            "2026.7.7-rc1",  // suffix
        ] {
            assert!(!is_valid_mise_version(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn resolve_uses_fleet_pin_on_omission_and_rejects_garbage() {
        assert_eq!(
            resolve_effective_mise_version("").unwrap(),
            FLEET_PINNED_MISE_VERSION
        );
        assert_eq!(
            resolve_effective_mise_version("2026.7.7").unwrap(),
            "2026.7.7"
        );
        assert!(resolve_effective_mise_version("latest").is_err());
        assert!(resolve_effective_mise_version("v2026.7.7").is_err());
    }

    #[test]
    fn fleet_pin_is_itself_a_valid_exact_version() {
        assert!(is_valid_mise_version(FLEET_PINNED_MISE_VERSION));
    }

    // ── install_args shape ────────────────────────────────────────────────

    #[test]
    fn tool_keys_are_valid_including_backend_qualified() {
        for good in [
            "just",
            "cargo-nextest",
            "aqua:casey/just",
            "cargo:rust-script",
            "core:rust",
            "protoc",
        ] {
            assert!(is_valid_install_arg_token(good), "should accept {good:?}");
        }
        assert!(is_valid_install_args_shape("just protoc aqua:casey/just"));
        assert!(is_valid_install_args_shape("")); // install all locked tools
    }

    #[test]
    fn flags_versions_urls_paths_and_shell_are_rejected() {
        for bad in [
            "--yes",
            "-v",
            "node@20",
            "https://mise.run",
            "http://x/y",
            "/etc/passwd",
            "./local",
            "../escape",
            "~/tool",
            "a;b",
            "$(id)",
            "`id`",
            "a|b",
            "a&b",
            "a b", // contains space → not a single token
            "tool*",
        ] {
            assert!(!is_valid_install_arg_token(bad), "should reject {bad:?}");
        }
        assert!(!is_valid_install_args_shape("just --yes"));
        assert!(!is_valid_install_args_shape("node@20"));
    }

    // ── settings + lock parsing ───────────────────────────────────────────

    #[test]
    fn lockfile_setting_is_parsed_not_regexed() {
        assert!(settings_lockfile_enabled("[settings]\nlockfile = true\n").unwrap());
        assert!(!settings_lockfile_enabled("[settings]\nlockfile = false\n").unwrap());
        assert!(!settings_lockfile_enabled("[tools]\njust = \"1.0\"\n").unwrap());
        // A commented-out setting must not read as enabled (regex would miss this).
        assert!(!settings_lockfile_enabled("[settings]\n# lockfile = true\n").unwrap());
    }

    #[test]
    fn lock_tool_keys_collected_from_array_of_tables() {
        let lock = r#"
[[tools.mold]]
version = "2.41.0"
backend = "aqua:rui314/mold"

[tools.mold."platforms.linux-x64"]
checksum = "sha256:abc"

[[tools."aqua:casey/just"]]
version = "1.57.0"
backend = "aqua:casey/just"

[[tools.rust]]
version = "1.97.1"
backend = "core:rust"

[tools.rust.options]
components = "clippy,rustfmt"
"#;
        let keys = lock_tool_keys(lock).unwrap();
        assert!(keys.contains("mold"));
        assert!(keys.contains("aqua:casey/just"));
        assert!(keys.contains("rust"));
        assert_eq!(keys.len(), 3);
    }

    #[test]
    fn install_args_membership_is_enforced() {
        let lock = r#"
[[tools.just]]
version = "1.57.0"
backend = "aqua:casey/just"

[[tools.protoc]]
version = "35.1"
backend = "aqua:protocolbuffers/protobuf/protoc"
"#;
        assert!(validate_install_args_against_lock("just protoc", lock).is_ok());
        assert!(validate_install_args_against_lock("", lock).is_ok());
        assert!(validate_install_args_against_lock("just gh", lock).is_err());
        assert!(validate_install_args_against_lock("--yes", lock).is_err());
    }

    // ── locked config resolution (fake git oracle) ────────────────────────

    struct FakeGit {
        tracked: bool,
        clean: bool,
    }
    impl GitOracle for FakeGit {
        fn is_tracked(&self, _root: &Path, _path: &Path) -> Result<bool> {
            Ok(self.tracked)
        }
        fn is_clean(&self, _root: &Path, _path: &Path) -> Result<bool> {
            Ok(self.clean)
        }
    }

    // canonicalize() on macOS resolves /var → /private/var etc.; keep every
    // temp dir under one canonical root so `starts_with` is exact.
    static TMP_LOCK: Mutex<()> = Mutex::new(());

    fn scratch(name: &str) -> PathBuf {
        let _guard = TMP_LOCK.lock().unwrap();
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "velnor-mise-policy-{}-{}",
            name,
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn resolves_tracked_clean_locked_config() {
        let root = scratch("ok");
        write(&root.join("mise.toml"), "[settings]\nlockfile = true\n");
        write(&root.join("mise.lock"), "# lock\n");
        let git = FakeGit {
            tracked: true,
            clean: true,
        };
        let resolved = resolve_locked_mise_config(&root, &root, &git).unwrap();
        assert_eq!(
            resolved.config,
            root.canonicalize().unwrap().join("mise.toml")
        );
        assert_eq!(
            resolved.lock,
            root.canonicalize().unwrap().join("mise.lock")
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_lock_fails_closed() {
        let root = scratch("nolock");
        write(&root.join("mise.toml"), "[settings]\nlockfile = true\n");
        let git = FakeGit {
            tracked: true,
            clean: true,
        };
        assert!(resolve_locked_mise_config(&root, &root, &git).is_err());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn untracked_lock_fails_closed() {
        let root = scratch("untracked");
        write(&root.join("mise.toml"), "[settings]\nlockfile = true\n");
        write(&root.join("mise.lock"), "# lock\n");
        let git = FakeGit {
            tracked: false,
            clean: true,
        };
        assert!(resolve_locked_mise_config(&root, &root, &git).is_err());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dirty_lock_fails_closed() {
        let root = scratch("dirty");
        write(&root.join("mise.toml"), "[settings]\nlockfile = true\n");
        write(&root.join("mise.lock"), "# lock\n");
        let git = FakeGit {
            tracked: true,
            clean: false,
        };
        assert!(resolve_locked_mise_config(&root, &root, &git).is_err());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn lockfile_disabled_config_fails_closed() {
        let root = scratch("nolockfile");
        write(&root.join("mise.toml"), "[settings]\nlockfile = false\n");
        write(&root.join("mise.lock"), "# lock\n");
        let git = FakeGit {
            tracked: true,
            clean: true,
        };
        assert!(resolve_locked_mise_config(&root, &root, &git).is_err());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn config_is_located_by_walking_up_from_working_directory() {
        let root = scratch("walkup");
        write(&root.join("mise.toml"), "[settings]\nlockfile = true\n");
        write(&root.join("mise.lock"), "# lock\n");
        let workdir = root.join("crates/sub");
        fs::create_dir_all(&workdir).unwrap();
        let git = FakeGit {
            tracked: true,
            clean: true,
        };
        let resolved = resolve_locked_mise_config(&root, &workdir, &git).unwrap();
        assert_eq!(
            resolved.config,
            root.canonicalize().unwrap().join("mise.toml")
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn symlinked_lock_is_rejected() {
        let root = scratch("symlink");
        write(&root.join("mise.toml"), "[settings]\nlockfile = true\n");
        write(&root.join("real.lock"), "# lock\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("real.lock"), root.join("mise.lock")).unwrap();
        let git = FakeGit {
            tracked: true,
            clean: true,
        };
        #[cfg(unix)]
        assert!(resolve_locked_mise_config(&root, &root, &git).is_err());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn enforce_ties_resolution_and_install_args_membership() {
        let root = scratch("enforce");
        write(&root.join("mise.toml"), "[settings]\nlockfile = true\n");
        write(
            &root.join("mise.lock"),
            "[[tools.just]]\nversion = \"1.57.0\"\nbackend = \"aqua:casey/just\"\n",
        );
        let git = FakeGit {
            tracked: true,
            clean: true,
        };
        assert!(enforce_locked_mise_policy(&root, &root, "just", &git).is_ok());
        assert!(enforce_locked_mise_policy(&root, &root, "gh", &git).is_err());
        fs::remove_dir_all(&root).ok();
    }
}

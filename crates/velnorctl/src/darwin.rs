//! Darwin/Homebrew installed-service boundary.
//!
//! A Homebrew install is a launchd service, not an interactive `velnorctl`
//! process.  This module resolves the package-owned metadata and service
//! envelope once, then lets the CLI replay the exact paths and Docker binding
//! that the service is required to use.  It deliberately does not invoke the
//! Docker heuristic resolver for an installed operation.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;
use velnor_runner::{
    daemon_instance::DaemonInstance,
    docker::{DockerEndpoint, DockerEndpointSource},
};

/// Homebrew's launchd service label.
pub const SERVICE_LABEL: &str = "com.tailrocks.velnor.runner";

const INSTALL_SCHEMA: &str = "velnor.homebrew-install/v3";
const IDENTITY_SCHEMA: &str = "velnor.homebrew-install-identity/v2";
const PRODUCT_ID: &str = "velnor";

const PATH_ENV_KEYS: &[&str] = &[
    "VELNOR_CONFIG_DIR",
    "VELNOR_ENV_FILE",
    "VELNOR_STORAGE_ROOT",
    "VELNOR_STATE_DB",
    "VELNOR_PERMIT_LEDGER",
    "VELNOR_MODE_STATE",
    "VELNOR_WORK_DIR",
    "VELNOR_LOG_DIR",
];

const REPLAY_ENV_KEYS: &[&str] = &[
    "VELNOR_CAPABILITY_VALIDATION",
    "VELNOR_CONFIG_DIR",
    "VELNOR_ENV_FILE",
    "VELNOR_LOG_DIR",
    "VELNOR_MODE_STATE",
    "VELNOR_NAME",
    "VELNOR_PERMIT_LEDGER",
    "VELNOR_STATE_DB",
    "VELNOR_STORAGE_ROOT",
    "VELNOR_TRUST_SCOPE",
    "VELNOR_URL",
    "VELNOR_WORK_DIR",
    "VELNOR_SLOTS",
    "VELNOR_DOCKER_HOST",
    "VELNOR_DOCKER_CONTEXT",
    "VELNOR_DOCKER_DAEMON_ID",
];

const DOCKER_ENV_KEYS: &[&str] = &[
    "VELNOR_DOCKER_HOST",
    "DOCKER_HOST",
    "VELNOR_DOCKER_CONTEXT",
    "DOCKER_CONTEXT",
    "VELNOR_DOCKER_DAEMON_ID",
];

/// Which boundary failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The package metadata/service shape is not trustworthy.
    Metadata,
    /// The installed package has no complete explicit Docker binding.
    DockerConfiguration,
    /// The CLI process names a different Docker endpoint.
    EndpointDrift,
    /// The CLI process names a different Docker daemon identity.
    DaemonIdentityDrift,
    /// The CLI process names a different package-owned path.
    PathDrift,
}

/// Typed resolver failure, kept independent of CLI exit formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Package-owned paths from the installed manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DarwinServicePaths {
    pub formula_prefix: PathBuf,
    pub manifest: PathBuf,
    pub identity: PathBuf,
    pub launchd_plist: PathBuf,
    pub launcher: PathBuf,
    pub config_dir: PathBuf,
    pub env_file: PathBuf,
    pub storage_root: PathBuf,
    pub state_db: PathBuf,
    pub permit_ledger: PathBuf,
    pub mode_state: PathBuf,
    pub work_dir: PathBuf,
    pub log_dir: PathBuf,
}

/// Explicit Docker endpoint plus the identity the package expects behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerBinding {
    pub endpoint: DockerEndpoint,
    pub daemon_id: String,
}

/// One installed Homebrew service as launchd sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DarwinInstalledService {
    pub paths: DarwinServicePaths,
    pub service_label: String,
    pub name: String,
    pub url: Option<String>,
    pub slots: Option<usize>,
    /// Non-secret package/service environment. Credentials are never copied.
    pub environment: BTreeMap<String, String>,
    pub docker: Option<DockerBinding>,
}

impl DarwinInstalledService {
    /// Adapt the Darwin service to the existing packaged-daemon selection
    /// model.  The adapter is deliberately path-explicit: Homebrew passes
    /// `--config-dir`, `--work-dir`, and state paths to the runner.
    #[must_use]
    pub fn into_daemon_instance(self) -> DaemonInstance {
        let run_root = self.paths.storage_root.join("run/velnor");
        let socket_dir = run_root.join(&self.name);
        DaemonInstance {
            instance: "velnor".to_owned(),
            unit: self.service_label,
            env_file: self.paths.env_file,
            name: self.name,
            url: self.url,
            slots: self.slots,
            storage_root: self.paths.storage_root.clone(),
            run_root: run_root.clone(),
            lib_root: self.paths.storage_root.join("lib/velnor"),
            cache_root: self.paths.storage_root.join("cache/velnor/v1"),
            log_root: self.paths.log_dir,
            state_directory: self.paths.storage_root.clone(),
            config_dir: self.paths.config_dir.clone(),
            daemon_dir: self.paths.config_dir,
            work_dir: self.paths.work_dir,
            trust_scope: self
                .environment
                .get("VELNOR_TRUST_SCOPE")
                .cloned()
                .unwrap_or_else(|| "untrusted".to_owned()),
            socket_dir,
            environment: self.environment,
        }
    }
}

/// Discover the installed Homebrew service on macOS.
///
/// Non-Darwin builds return no installed service.  The function is still
/// available there so fixture tests can prove the resolver without changing
/// Linux's `/etc/velnor` path.
pub fn discover() -> Result<Option<DarwinInstalledService>, Error> {
    #[cfg(target_os = "macos")]
    {
        for formula_prefix in formula_prefix_candidates() {
            let manifest = formula_prefix.join("share/velnor/manifest.json");
            if !manifest.is_file() {
                continue;
            }
            return resolve_from_formula_prefix(&formula_prefix).map(Some);
        }
    }
    Ok(None)
}

/// Resolve a staged Homebrew formula prefix.  This is public for packaging
/// and integration fixtures; production discovery calls it after finding the
/// package-owned manifest.
pub fn resolve_from_formula_prefix(formula_prefix: &Path) -> Result<DarwinInstalledService, Error> {
    let manifest_path = formula_prefix.join("share/velnor/manifest.json");
    let identity_path = formula_prefix.join("share/velnor/identity.json");
    let manifest = read_json(&manifest_path)?;
    let identity = read_json(&identity_path)?;
    require_string(&manifest, "schema", INSTALL_SCHEMA)?;
    require_string(&manifest, "product_id", PRODUCT_ID)?;
    require_string(&manifest, "service_label", SERVICE_LABEL)?;
    require_string(&identity, "schema", IDENTITY_SCHEMA)?;
    require_string(&identity, "product_id", PRODUCT_ID)?;

    let manifest_paths = manifest.get("paths").ok_or_else(|| {
        Error::new(
            ErrorKind::Metadata,
            format!("{} is missing paths", manifest_path.display()),
        )
    })?;
    let config_dir = required_path(manifest_paths, "config_dir")?;
    let env_file = required_path(manifest_paths, "env_file")?;
    let storage_root = required_path(manifest_paths, "storage_root")?;
    let state_db = required_path(manifest_paths, "state_db")?;
    let permit_ledger = required_path(manifest_paths, "permit_ledger")?;
    let work_dir = required_path(manifest_paths, "work_dir")?;
    let log_dir = required_path(manifest_paths, "log_dir")?;
    let mode_state = required_path_value(&manifest, "mode_state_path")?;
    let paths = DarwinServicePaths {
        formula_prefix: formula_prefix.to_path_buf(),
        manifest: manifest_path.clone(),
        identity: identity_path,
        launchd_plist: formula_prefix
            .join("share/velnor/launchd")
            .join(format!("{SERVICE_LABEL}.plist")),
        launcher: formula_prefix.join("libexec/velnor-runner-launch"),
        config_dir,
        env_file,
        storage_root,
        state_db,
        permit_ledger,
        mode_state,
        work_dir,
        log_dir,
    };
    for path in [&paths.env_file, &paths.launchd_plist, &paths.launcher] {
        if !path.is_file() {
            return Err(Error::new(
                ErrorKind::Metadata,
                format!("installed service file is missing: {}", path.display()),
            ));
        }
    }

    let plist_text = fs::read_to_string(&paths.launchd_plist).map_err(|error| {
        Error::new(
            ErrorKind::Metadata,
            format!("read {}: {error}", paths.launchd_plist.display()),
        )
    })?;
    validate_plist(&plist_text, &paths)?;

    let mut environment = parse_environment_file(&paths.env_file)?;
    for key in REPLAY_ENV_KEYS
        .iter()
        .copied()
        .chain(["VELNOR_PATH", "VELNOR_TRUST_SCOPE"])
    {
        if let Some(value) = plist_string(&plist_text, key) {
            environment.insert(key.to_owned(), value);
        }
    }
    for (key, value) in package_docker_values(&manifest) {
        if let Some(existing) = environment.get(&key)
            && existing.trim() != value
        {
            return Err(Error::new(
                ErrorKind::Metadata,
                format!("package Docker field {key} disagrees with service environment"),
            ));
        }
        environment.insert(key, value);
    }
    validate_manifest_environment(&environment, &paths, &manifest)?;

    let docker = docker_binding_from_environment(&environment)?;
    let name = environment
        .get("VELNOR_NAME")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or("velnor-macos")
        .to_owned();
    let url = environment
        .get("VELNOR_URL")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let slots = environment
        .get("VELNOR_SLOTS")
        .and_then(|value| value.trim().parse::<usize>().ok());
    let environment = replay_environment(&environment);

    Ok(DarwinInstalledService {
        paths,
        service_label: SERVICE_LABEL.to_owned(),
        name,
        url,
        slots,
        environment,
        docker,
    })
}

/// Re-enter one installed service's package-owned process environment.
///
/// The guard rejects an already-set conflicting endpoint, context, daemon
/// identity, or package path before changing anything.  It restores the
/// caller's environment on drop, which keeps test/process reuse bounded.
pub fn enter(instance: &DaemonInstance, require_docker: bool) -> Result<EnvironmentGuard, Error> {
    if instance.unit != SERVICE_LABEL {
        return Ok(EnvironmentGuard::noop());
    }
    let binding = docker_binding_from_environment(&instance.environment)?;
    let ambient = ambient_environment();
    validate_path_drift(&instance.environment, &ambient)?;
    match (&binding, require_docker) {
        (Some(binding), _) => validate_docker_drift(binding, &ambient)?,
        (None, true) => {
            return Err(Error::new(
                ErrorKind::DockerConfiguration,
                "installed Homebrew service has no explicit VELNOR_DOCKER_HOST and VELNOR_DOCKER_DAEMON_ID; package metadata must persist the resolved Docker endpoint and daemon identity",
            ));
        }
        (None, false) => {}
    }

    let mut guard = EnvironmentGuard::default();
    for key in REPLAY_ENV_KEYS {
        if let Some(value) = instance.environment.get(*key) {
            guard.set(key, value);
        }
    }
    if let Some(binding) = binding {
        guard.set("VELNOR_DOCKER_HOST", &binding.endpoint.host);
        guard.set("DOCKER_HOST", &binding.endpoint.host);
        if let Some(context) = &binding.endpoint.context {
            guard.set("VELNOR_DOCKER_CONTEXT", context);
        } else {
            guard.remove("VELNOR_DOCKER_CONTEXT");
        }
        guard.remove("DOCKER_CONTEXT");
    }
    Ok(guard)
}

/// A process-environment transaction for one installed operation.
#[derive(Debug, Default)]
pub struct EnvironmentGuard {
    previous: Vec<(String, Option<OsString>)>,
}

impl EnvironmentGuard {
    /// No-op guard for development/Linux selections.
    #[must_use]
    pub fn noop() -> Self {
        Self::default()
    }

    fn set(&mut self, key: &str, value: &str) {
        self.remember(key);
        // SAFETY: CLI command setup owns this process environment; the guard
        // restores every touched value before the command returns.
        unsafe { env::set_var(key, value) };
    }

    fn remove(&mut self, key: &str) {
        self.remember(key);
        // SAFETY: paired with `set` and restored by `Drop`.
        unsafe { env::remove_var(key) };
    }

    fn remember(&mut self, key: &str) {
        if self.previous.iter().any(|(known, _)| known == key) {
            return;
        }
        self.previous.push((key.to_owned(), env::var_os(key)));
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            // SAFETY: restore the exact value captured by this guard.
            unsafe {
                match value {
                    Some(value) => env::set_var(key, value),
                    None => env::remove_var(key),
                }
            }
        }
    }
}

fn docker_binding_from_environment(
    environment: &BTreeMap<String, String>,
) -> Result<Option<DockerBinding>, Error> {
    let host = nonempty(environment.get("VELNOR_DOCKER_HOST"));
    let context = nonempty(environment.get("VELNOR_DOCKER_CONTEXT"));
    let daemon_id = nonempty(environment.get("VELNOR_DOCKER_DAEMON_ID"));
    if host.is_none() && context.is_none() && daemon_id.is_none() {
        return Ok(None);
    }
    let host = host.ok_or_else(|| {
        Error::new(
            ErrorKind::DockerConfiguration,
            "installed Homebrew service configures Docker context/identity but no explicit VELNOR_DOCKER_HOST",
        )
    })?;
    let daemon_id = daemon_id.ok_or_else(|| {
        Error::new(
            ErrorKind::DockerConfiguration,
            "installed Homebrew service configures a Docker endpoint without VELNOR_DOCKER_DAEMON_ID",
        )
    })?;
    let endpoint = endpoint_from_host(host)?;
    Ok(Some(DockerBinding {
        endpoint: DockerEndpoint {
            host: endpoint,
            socket: PathBuf::from(host.strip_prefix("unix://").unwrap_or(host)),
            source: DockerEndpointSource::Explicit,
            context: context.map(str::to_owned),
        },
        daemon_id: daemon_id.to_owned(),
    }))
}

fn endpoint_from_host(host: &str) -> Result<String, Error> {
    let host = host.trim();
    let socket = host.strip_prefix("unix://").unwrap_or(host).trim();
    if !socket.starts_with('/') || socket.contains('\0') {
        return Err(Error::new(
            ErrorKind::DockerConfiguration,
            format!("installed Docker endpoint {host:?} is not an absolute Unix socket"),
        ));
    }
    Ok(format!("unix://{socket}"))
}

fn ambient_environment() -> BTreeMap<String, String> {
    DOCKER_ENV_KEYS
        .iter()
        .filter_map(|key| {
            let value = env::var(key).ok()?;
            (!value.trim().is_empty()).then_some(((*key).to_owned(), value))
        })
        .chain(PATH_ENV_KEYS.iter().filter_map(|key| {
            let value = env::var(key).ok()?;
            (!value.trim().is_empty()).then_some(((*key).to_owned(), value))
        }))
        .collect()
}

fn validate_path_drift(
    expected: &BTreeMap<String, String>,
    ambient: &BTreeMap<String, String>,
) -> Result<(), Error> {
    for key in PATH_ENV_KEYS.iter().copied().chain(["VELNOR_NAME"]) {
        if let (Some(want), Some(have)) = (expected.get(key), ambient.get(key))
            && want != have
        {
            return Err(Error::new(
                ErrorKind::PathDrift,
                format!("installed service path/identity drift for {key}: expected {want:?}, found {have:?}"),
            ));
        }
    }
    Ok(())
}

fn validate_docker_drift(
    expected: &DockerBinding,
    ambient: &BTreeMap<String, String>,
) -> Result<(), Error> {
    for key in ["VELNOR_DOCKER_HOST", "DOCKER_HOST"] {
        if let Some(value) = ambient.get(key) {
            let actual = endpoint_from_host(value).map_err(|error| {
                Error::new(
                    ErrorKind::EndpointDrift,
                    format!("ambient {key} is invalid for the installed service: {error}"),
                )
            })?;
            if actual != expected.endpoint.host {
                return Err(Error::new(
                    ErrorKind::EndpointDrift,
                    format!(
                        "installed Docker endpoint drift for {key}: expected {}, found {actual}",
                        expected.endpoint.host
                    ),
                ));
            }
        }
    }
    for key in ["VELNOR_DOCKER_CONTEXT", "DOCKER_CONTEXT"] {
        if let Some(value) = ambient.get(key)
            && nonempty(Some(value)) != expected.endpoint.context.as_deref()
        {
            return Err(Error::new(
                ErrorKind::EndpointDrift,
                format!(
                    "installed Docker context drift for {key}: expected {:?}, found {value:?}",
                    expected.endpoint.context
                ),
            ));
        }
    }
    if let Some(value) = ambient.get("VELNOR_DOCKER_DAEMON_ID")
        && value.trim() != expected.daemon_id
    {
        return Err(Error::new(
            ErrorKind::DaemonIdentityDrift,
            format!(
                "installed Docker daemon identity drift: expected {:?}, found {value:?}",
                expected.daemon_id
            ),
        ));
    }
    Ok(())
}

fn validate_manifest_environment(
    environment: &BTreeMap<String, String>,
    paths: &DarwinServicePaths,
    manifest: &Value,
) -> Result<(), Error> {
    let expected = [
        ("VELNOR_CONFIG_DIR", &paths.config_dir),
        ("VELNOR_ENV_FILE", &paths.env_file),
        ("VELNOR_STORAGE_ROOT", &paths.storage_root),
        ("VELNOR_STATE_DB", &paths.state_db),
        ("VELNOR_PERMIT_LEDGER", &paths.permit_ledger),
        ("VELNOR_MODE_STATE", &paths.mode_state),
        ("VELNOR_WORK_DIR", &paths.work_dir),
        ("VELNOR_LOG_DIR", &paths.log_dir),
    ];
    for (key, path) in expected {
        let want = path.to_string_lossy();
        let Some(have) = environment.get(key) else {
            return Err(Error::new(
                ErrorKind::Metadata,
                format!("launchd service is missing package-owned {key}"),
            ));
        };
        if have != want.as_ref() {
            return Err(Error::new(
                ErrorKind::PathDrift,
                format!("package-owned {key} disagrees with manifest: expected {want:?}, found {have:?}"),
            ));
        }
    }
    if let Some(manifest_mode_state) = manifest.get("mode_state_path").and_then(Value::as_str)
        && manifest_mode_state != paths.mode_state.to_string_lossy()
    {
        return Err(Error::new(
            ErrorKind::Metadata,
            "manifest mode_state_path is not the resolved package path",
        ));
    }
    Ok(())
}

fn replay_environment(environment: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    REPLAY_ENV_KEYS
        .iter()
        .filter_map(|key| {
            environment
                .get(*key)
                .map(|value| ((*key).to_owned(), value.trim().to_owned()))
        })
        .collect()
}

fn package_docker_values(manifest: &Value) -> Vec<(String, String)> {
    let mut values = Vec::new();
    let docker = manifest.get("docker");
    for (key, names) in [
        ("VELNOR_DOCKER_HOST", ["endpoint", "host"] as [&str; 2]),
        ("VELNOR_DOCKER_CONTEXT", ["context", "context_name"]),
        ("VELNOR_DOCKER_DAEMON_ID", ["daemon_id", "identity"]),
    ] {
        let value = docker
            .and_then(|value| names.iter().find_map(|name| value.get(*name)))
            .or_else(|| {
                manifest.get(match key {
                    "VELNOR_DOCKER_HOST" => "docker_endpoint",
                    "VELNOR_DOCKER_CONTEXT" => "docker_context",
                    _ => "docker_daemon_id",
                })
            })
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(value) = value {
            values.push((key.to_owned(), value.to_owned()));
        }
    }
    values
}

fn nonempty(value: Option<&String>) -> Option<&str> {
    value
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn read_json(path: &Path) -> Result<Value, Error> {
    let text = fs::read_to_string(path).map_err(|error| {
        Error::new(
            ErrorKind::Metadata,
            format!("read {}: {error}", path.display()),
        )
    })?;
    serde_json::from_str(&text).map_err(|error| {
        Error::new(
            ErrorKind::Metadata,
            format!("parse {}: {error}", path.display()),
        )
    })
}

fn require_string(value: &Value, key: &str, expected: &str) -> Result<(), Error> {
    let actual = value.get(key).and_then(Value::as_str).ok_or_else(|| {
        Error::new(
            ErrorKind::Metadata,
            format!("metadata is missing string {key}"),
        )
    })?;
    if actual != expected {
        return Err(Error::new(
            ErrorKind::Metadata,
            format!("metadata {key} must be {expected:?}, found {actual:?}"),
        ));
    }
    Ok(())
}

fn required_path(value: &Value, key: &str) -> Result<PathBuf, Error> {
    let path = value
        .get(key)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Metadata,
                format!("metadata is missing path {key}"),
            )
        })?;
    if !path.is_absolute() {
        return Err(Error::new(
            ErrorKind::Metadata,
            format!("metadata path {key} is not absolute: {}", path.display()),
        ));
    }
    Ok(path)
}

fn required_path_value(value: &Value, key: &str) -> Result<PathBuf, Error> {
    required_path(value, key)
}

fn parse_environment_file(path: &Path) -> Result<BTreeMap<String, String>, Error> {
    let text = fs::read_to_string(path).map_err(|error| {
        Error::new(
            ErrorKind::Metadata,
            format!("read {}: {error}", path.display()),
        )
    })?;
    let mut result = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or_else(|| {
            Error::new(
                ErrorKind::Metadata,
                format!("invalid environment line in {}: {line:?}", path.display()),
            )
        })?;
        let key = key.trim();
        if key.is_empty() {
            return Err(Error::new(
                ErrorKind::Metadata,
                format!("empty environment key in {}", path.display()),
            ));
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
            })
            .unwrap_or(value);
        result.insert(key.to_owned(), value.to_owned());
    }
    Ok(result)
}

fn plist_string(text: &str, key: &str) -> Option<String> {
    let key_tag = format!("<key>{key}</key>");
    let rest = text.split_once(&key_tag)?.1;
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")? + start;
    Some(rest[start..end].to_owned())
}

fn validate_plist(text: &str, paths: &DarwinServicePaths) -> Result<(), Error> {
    if plist_string(text, "Label").as_deref() != Some(SERVICE_LABEL)
        || !text.contains(&format!("<string>{}</string>", paths.launcher.display()))
    {
        return Err(Error::new(
            ErrorKind::Metadata,
            format!("launchd plist does not bind service {SERVICE_LABEL} to the package launcher"),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn formula_prefix_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(prefix) = env::var_os("HOMEBREW_PREFIX").filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(prefix).join("opt/velnorctl"));
    }
    if let Ok(executable) = env::current_exe()
        && let Some(bin) = executable.parent()
        && let Some(prefix) = bin.parent()
    {
        candidates.push(prefix.to_path_buf());
        candidates.push(prefix.join("opt/velnorctl"));
    }
    if let Ok(output) = std::process::Command::new("brew")
        .args(["--prefix", "velnorctl"])
        .output()
        && output.status.success()
        && let Ok(prefix) = String::from_utf8(output.stdout)
    {
        candidates.push(PathBuf::from(prefix.trim()));
    }
    let mut unique = Vec::new();
    for candidate in candidates {
        if !candidate.as_os_str().is_empty() && !unique.contains(&candidate) {
            unique.push(candidate);
        }
    }
    unique
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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "velnorctl-darwin-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn fixture(root: &Path, docker: bool) -> DarwinServicePaths {
        let prefix = root.join("opt/velnorctl");
        let config = root.join("etc/velnor");
        let storage = root.join("var/velnor");
        let work = storage.join("work");
        let log = root.join("var/log/velnor");
        let env_file = config.join("velnor.env");
        let paths = DarwinServicePaths {
            formula_prefix: prefix.clone(),
            manifest: prefix.join("share/velnor/manifest.json"),
            identity: prefix.join("share/velnor/identity.json"),
            launchd_plist: prefix
                .join("share/velnor/launchd")
                .join(format!("{SERVICE_LABEL}.plist")),
            launcher: prefix.join("libexec/velnor-runner-launch"),
            config_dir: config.clone(),
            env_file: env_file.clone(),
            storage_root: storage.clone(),
            state_db: storage.join("state.db"),
            permit_ledger: storage.join("permit-ledger.db"),
            mode_state: storage.join("mode-state.json"),
            work_dir: work,
            log_dir: log,
        };
        fs::create_dir_all(paths.launchd_plist.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.launcher.parent().unwrap()).unwrap();
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(&paths.launcher, "#!/bin/sh\n").unwrap();
        let docker_env = if docker {
            "VELNOR_DOCKER_HOST=unix:///tmp/orbstack/docker.sock\nVELNOR_DOCKER_CONTEXT=orbstack\nVELNOR_DOCKER_DAEMON_ID=engine-a\n"
        } else {
            ""
        };
        fs::write(
            &paths.env_file,
            format!("VELNOR_NAME=velnor-macos\nVELNOR_SLOTS=2\n{docker_env}"),
        )
        .unwrap();
        let plist = format!(
            "<key>Label</key><string>{SERVICE_LABEL}</string><key>ProgramArguments</key><array><string>{}</string></array>{}
             <key>VELNOR_CONFIG_DIR</key><string>{}</string>
             <key>VELNOR_ENV_FILE</key><string>{}</string>
             <key>VELNOR_LOG_DIR</key><string>{}</string>
             <key>VELNOR_MODE_STATE</key><string>{}</string>
             <key>VELNOR_PERMIT_LEDGER</key><string>{}</string>
             <key>VELNOR_STATE_DB</key><string>{}</string>
             <key>VELNOR_STORAGE_ROOT</key><string>{}</string>
             <key>VELNOR_TRUST_SCOPE</key><string>untrusted</string>
             <key>VELNOR_WORK_DIR</key><string>{}</string>
             ",
            paths.launcher.display(),
            if docker {
                format!(
                    "<key>VELNOR_DOCKER_HOST</key><string>unix:///tmp/orbstack/docker.sock</string><key>VELNOR_DOCKER_CONTEXT</key><string>orbstack</string><key>VELNOR_DOCKER_DAEMON_ID</key><string>engine-a</string>"
                )
            } else {
                String::new()
            },
            paths.config_dir.display(),
            paths.env_file.display(),
            paths.log_dir.display(),
            paths.mode_state.display(),
            paths.permit_ledger.display(),
            paths.state_db.display(),
            paths.storage_root.display(),
            paths.work_dir.display(),
        );
        fs::write(&paths.launchd_plist, plist).unwrap();
        fs::write(
            &paths.manifest,
            serde_json::json!({
                "schema": INSTALL_SCHEMA,
                "product_id": PRODUCT_ID,
                "service_label": SERVICE_LABEL,
                "mode_state_path": paths.mode_state,
                "paths": {
                    "config_dir": paths.config_dir,
                    "env_file": paths.env_file,
                    "storage_root": paths.storage_root,
                    "state_db": paths.state_db,
                    "permit_ledger": paths.permit_ledger,
                    "work_dir": paths.work_dir,
                    "log_dir": paths.log_dir,
                }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            &paths.identity,
            serde_json::json!({"schema": IDENTITY_SCHEMA, "product_id": PRODUCT_ID}).to_string(),
        )
        .unwrap();
        paths
    }

    #[test]
    fn resolver_replays_homebrew_paths_and_service_identity() {
        let root = temp_root("paths");
        let paths = fixture(&root, true);
        let service = resolve_from_formula_prefix(&paths.formula_prefix).unwrap();
        assert_eq!(service.name, "velnor-macos");
        assert_eq!(service.slots, Some(2));
        assert_eq!(service.paths.config_dir, paths.config_dir);
        assert_eq!(service.paths.work_dir, paths.work_dir);
        assert_eq!(service.docker.as_ref().unwrap().daemon_id, "engine-a");
        let instance = service.into_daemon_instance();
        assert_eq!(instance.unit, SERVICE_LABEL);
        assert_eq!(instance.daemon_dir, paths.config_dir);
        assert_eq!(
            instance.control_socket(),
            paths
                .storage_root
                .join("run/velnor/velnor-macos/control.sock")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installed_operation_rejects_missing_explicit_docker_binding() {
        let root = temp_root("missing-docker");
        let paths = fixture(&root, false);
        let instance = resolve_from_formula_prefix(&paths.formula_prefix)
            .unwrap()
            .into_daemon_instance();
        let error = enter(&instance, true).unwrap_err();
        assert_eq!(error.kind, ErrorKind::DockerConfiguration);
        assert!(error.message.contains("VELNOR_DOCKER_HOST"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn endpoint_drift_is_rejected_before_environment_replay() {
        let root = temp_root("drift");
        let paths = fixture(&root, true);
        let service = resolve_from_formula_prefix(&paths.formula_prefix).unwrap();
        let binding = service.docker.unwrap();
        let mut ambient = BTreeMap::new();
        ambient.insert(
            "DOCKER_HOST".to_owned(),
            "unix:///tmp/docker-desktop/docker.sock".to_owned(),
        );
        let error = validate_docker_drift(&binding, &ambient).unwrap_err();
        assert_eq!(error.kind, ErrorKind::EndpointDrift);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_identity_drift_is_distinct_from_endpoint_drift() {
        let root = temp_root("identity-drift");
        let paths = fixture(&root, true);
        let service = resolve_from_formula_prefix(&paths.formula_prefix).unwrap();
        let binding = service.docker.unwrap();
        let mut ambient = BTreeMap::new();
        ambient.insert("VELNOR_DOCKER_DAEMON_ID".to_owned(), "engine-b".to_owned());
        let error = validate_docker_drift(&binding, &ambient).unwrap_err();
        assert_eq!(error.kind, ErrorKind::DaemonIdentityDrift);
        fs::remove_dir_all(root).unwrap();
    }
}

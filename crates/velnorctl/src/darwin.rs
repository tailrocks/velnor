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
    ffi::{CStr, OsString},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use sha2::{Digest, Sha256};
use velnor_runner::{
    daemon_instance::DaemonInstance,
    docker::{DockerEndpoint, DockerEndpointSource},
};

/// Homebrew's launchd service label.
pub const SERVICE_LABEL: &str = "com.tailrocks.velnor.runner";

const INSTALL_SCHEMA: &str = "velnor.homebrew-install/v3";
const IDENTITY_SCHEMA: &str = "velnor.homebrew-install-identity/v2";
const DOCKER_BINDING_SCHEMA: &str = "velnor.docker-binding/v1";
const PRODUCT_ID: &str = "velnor";
const DOCKER_BINDING_FILE: &str = "docker-binding.json";
const PRODUCT_BINARIES: [&str; 3] = ["velnorctl", "velnor-runner", "velnor-workflow"];

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
    "VELNOR_DOCKER_BINDING_FILE",
    "VELNOR_PATH",
    "DOCKER_CONFIG",
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
    pub docker_binding: PathBuf,
}

/// Explicit Docker endpoint plus the identity the package expects behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerBinding {
    pub endpoint: DockerEndpoint,
    pub daemon_id: String,
}

impl DockerBinding {
    pub(crate) fn verify_context(&self, value: &Value) -> Result<(), Error> {
        let name = value.get("Name").and_then(Value::as_str);
        let host = value
            .pointer("/Endpoints/docker/Host")
            .and_then(Value::as_str)
            .and_then(|value| endpoint_from_host(value).ok());
        if name != self.endpoint.context.as_deref() || host.as_deref() != Some(&self.endpoint.host)
        {
            return Err(Error::new(
                ErrorKind::EndpointDrift,
                "installed Docker context no longer identifies the pinned endpoint",
            ));
        }
        Ok(())
    }

    pub(crate) fn verify_daemon(&self, value: &Value) -> Result<(), Error> {
        if value.get("ID").and_then(Value::as_str) != Some(&self.daemon_id) {
            return Err(Error::new(
                ErrorKind::DaemonIdentityDrift,
                "installed Docker endpoint no longer identifies the pinned daemon ID",
            ));
        }
        if value.get("OSType").and_then(Value::as_str) != Some("linux") {
            return Err(Error::new(
                ErrorKind::DockerConfiguration,
                "installed Docker daemon must execute Linux containers",
            ));
        }
        Ok(())
    }
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
        let run_root = velnor_client::socket_root_for_storage_root(Some(&self.paths.storage_root));
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
/// Non-Darwin builds return no installed service and retain Linux's own
/// packaged instance discovery.
pub fn discover() -> Result<Option<DarwinInstalledService>, Error> {
    #[cfg(target_os = "macos")]
    {
        for formula_prefix in formula_prefix_candidates() {
            let manifest = formula_prefix.join("share/velnor/manifest.json");
            match fs::symlink_metadata(&manifest) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err(Error::new(
                        ErrorKind::Metadata,
                        "cannot inspect installed Homebrew manifest",
                    ))
                }
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
    let formula_prefix = canonicalize_existing(formula_prefix, "formula prefix")?;
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
        formula_prefix: formula_prefix.clone(),
        manifest: manifest_path.clone(),
        identity: identity_path,
        launchd_plist: formula_prefix
            .join("share/velnor/launchd")
            .join(format!("{SERVICE_LABEL}.plist")),
        launcher: formula_prefix.join("libexec/velnor-runner-launch"),
        config_dir,
        env_file,
        storage_root: storage_root.clone(),
        state_db,
        permit_ledger,
        mode_state,
        work_dir,
        log_dir,
        docker_binding: storage_root.join(DOCKER_BINDING_FILE),
    };
    for path in [&paths.env_file, &paths.launchd_plist, &paths.launcher] {
        if !path.is_file() {
            return Err(Error::new(
                ErrorKind::Metadata,
                format!("installed service file is missing: {}", path.display()),
            ));
        }
    }
    use std::os::unix::fs::PermissionsExt;
    if !fs::metadata(&paths.launcher)
        .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    {
        return Err(Error::new(
            ErrorKind::Metadata,
            "package launcher is not executable",
        ));
    }

    let plist = read_plist(&paths.launchd_plist)?;
    let service_environment = validate_plist(&plist, &paths)?;

    verify_install_identity(&manifest, &identity, &paths, &manifest_path)?;

    let mut environment = parse_environment_file(&paths.env_file)?;
    // The launcher reasserts package-owned paths and policy *after* loading
    // the operator env file. Operator Docker/name settings otherwise win.
    for (key, value) in service_environment {
        if PATH_ENV_KEYS.contains(&key.as_str())
            || [
                "VELNOR_PATH",
                "VELNOR_TRUST_SCOPE",
                "VELNOR_CAPABILITY_VALIDATION",
            ]
            .contains(&key.as_str())
        {
            environment.insert(key, value);
        } else {
            environment.entry(key).or_insert(value);
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
    environment.insert(
        "VELNOR_DOCKER_BINDING_FILE".to_owned(),
        paths.docker_binding.to_string_lossy().into_owned(),
    );
    if let Some(persisted) = read_docker_binding(&paths.docker_binding)? {
        if let Some(configured) = docker_binding_from_environment(&environment)? {
            ensure_same_docker_binding(&persisted, &configured)?;
        }
        environment.extend(docker_binding_environment(&persisted));
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
        .map(|value| {
            value
                .parse::<usize>()
                .ok()
                .filter(|slots| *slots > 0)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Metadata,
                        "VELNOR_SLOTS must be a positive integer",
                    )
                })
        })
        .transpose()?;
    // Preserve launcher defaults in the typed environment too; otherwise
    // ambient VELNOR_NAME drift would escape validation when the file omits it.
    environment.insert("VELNOR_NAME".to_owned(), name.clone());
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

/// Immutable installed-operation inputs. Never mutate the caller's process
/// environment: CLI dispatch already runs inside a multithreaded runtime.
#[derive(Debug, Clone)]
pub struct InstalledOperation {
    pub instance: DaemonInstance,
    pub docker: Option<DockerBinding>,
    search_path: OsString,
    home: PathBuf,
    docker_config: PathBuf,
}

pub fn operation(
    instance: &DaemonInstance,
    require_docker: bool,
) -> Result<InstalledOperation, Error> {
    let binding = docker_binding_from_environment(&instance.environment)?;
    let binding_path = instance
        .environment
        .get("VELNOR_DOCKER_BINDING_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| instance.storage_root.join(DOCKER_BINDING_FILE));
    validate_absolute_path(&binding_path, "Docker binding file")?;
    let ambient = ambient_environment();
    validate_path_drift(&instance.environment, &ambient)?;
    match (&binding, require_docker) {
        (Some(binding), true) => {
            let persisted = read_docker_binding(&binding_path)?.ok_or_else(|| {
                Error::new(
                    ErrorKind::DockerConfiguration,
                    "installed Docker binding has not been durably recorded; start the package service once to bind its selected daemon",
                )
            })?;
            ensure_same_docker_binding(&persisted, binding)?;
            validate_docker_drift(binding, &ambient)?;
        }
        (Some(binding), false) => validate_docker_drift(binding, &ambient)?,
        (None, true) => {
            return Err(Error::new(
                ErrorKind::DockerConfiguration,
                "installed Homebrew service has no explicit VELNOR_DOCKER_HOST and VELNOR_DOCKER_DAEMON_ID; package metadata must persist the resolved Docker endpoint and daemon identity",
            ));
        }
        (None, false) => {}
    }

    let search_path = instance.environment.get("VELNOR_PATH").ok_or_else(|| {
        Error::new(
            ErrorKind::Metadata,
            "launchd service has no package-owned VELNOR_PATH",
        )
    })?;
    validate_search_path(search_path)?;
    let home = account_home()?;
    let docker_config = instance
        .environment
        .get("DOCKER_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".docker"));
    validate_absolute_path(&docker_config, "DOCKER_CONFIG")?;
    if let Some(config) = env::var_os("DOCKER_CONFIG").filter(|value| !value.is_empty())
        && Path::new(&config) != docker_config
    {
        return Err(Error::new(
            ErrorKind::EndpointDrift,
            "ambient DOCKER_CONFIG differs from the installed service's context store",
        ));
    }
    Ok(InstalledOperation {
        instance: instance.clone(),
        docker: binding,
        search_path: OsString::from(search_path),
        home,
        docker_config,
    })
}

impl InstalledOperation {
    pub(crate) fn validate_path_override(
        &self,
        option: &str,
        actual: Option<&Path>,
        expected: &Path,
    ) -> Result<(), Error> {
        if actual.is_some_and(|path| path != expected) {
            return Err(Error::new(
                ErrorKind::PathDrift,
                format!("--{option} disagrees with the selected installed service"),
            ));
        }
        Ok(())
    }

    /// Resolve only against launchd's absolute search path. Shell PATH,
    /// Docker TLS/context overrides and management credentials are not copied.
    pub(crate) fn command(&self, program: &str) -> Result<Command, Error> {
        use std::os::unix::fs::PermissionsExt;
        let executable = env::split_paths(&self.search_path)
            .map(|path| path.join(program))
            .find(|path| {
                fs::metadata(path).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Metadata,
                    format!("{program} is unavailable on the package-owned launchd PATH"),
                )
            })?;
        let mut command = Command::new(executable);
        command
            .env_clear()
            .env("PATH", &self.search_path)
            .env("HOME", &self.home)
            .env("DOCKER_CONFIG", &self.docker_config);
        Ok(command)
    }
}

fn account_home() -> Result<PathBuf, Error> {
    use std::os::unix::ffi::OsStrExt;
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 65536];
    // SAFETY: getpwuid_r fills caller-owned storage; pointers are consumed
    // only after success, before that storage is dropped. No global libc
    // passwd buffer or interactive HOME value participates.
    let status = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return Err(Error::new(
            ErrorKind::Metadata,
            "cannot resolve service user's home directory",
        ));
    }
    // SAFETY: successful getpwuid_r returns an initialized, NUL-terminated pw_dir.
    let directory = unsafe { (*result).pw_dir };
    if directory.is_null() {
        return Err(Error::new(
            ErrorKind::Metadata,
            "service user has no home directory",
        ));
    }
    // SAFETY: the non-null directory belongs to the still-live passwd buffer.
    let bytes = unsafe { CStr::from_ptr(directory) }.to_bytes();
    let home = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
    validate_absolute_path(&home, "service home")?;
    Ok(home)
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
    if let Some(context) = context
        && (context.starts_with('-')
            || !context
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)))
    {
        return Err(Error::new(
            ErrorKind::DockerConfiguration,
            "installed Docker context is invalid",
        ));
    }
    for (key, expected) in [("DOCKER_HOST", Some(host)), ("DOCKER_CONTEXT", context)] {
        if let Some(value) = nonempty(environment.get(key))
            && Some(value) != expected
        {
            return Err(Error::new(
                ErrorKind::DockerConfiguration,
                format!("installed {key} disagrees with the pinned Velnor Docker binding"),
            ));
        }
    }
    Ok(Some(DockerBinding {
        endpoint: DockerEndpoint {
            host: endpoint,
            socket: PathBuf::from(host.strip_prefix("unix://").unwrap_or(host).trim()),
            source: DockerEndpointSource::Explicit,
            context: context.map(str::to_owned),
        },
        daemon_id: daemon_id.to_owned(),
    }))
}

fn docker_binding_environment(binding: &DockerBinding) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::from([
        (
            "VELNOR_DOCKER_HOST".to_owned(),
            binding.endpoint.host.clone(),
        ),
        (
            "VELNOR_DOCKER_DAEMON_ID".to_owned(),
            binding.daemon_id.clone(),
        ),
    ]);
    if let Some(context) = &binding.endpoint.context {
        environment.insert("VELNOR_DOCKER_CONTEXT".to_owned(), context.clone());
    }
    environment
}

fn ensure_same_docker_binding(
    expected: &DockerBinding,
    actual: &DockerBinding,
) -> Result<(), Error> {
    if expected.endpoint.host != actual.endpoint.host
        || expected.endpoint.context != actual.endpoint.context
    {
        return Err(Error::new(
            ErrorKind::EndpointDrift,
            "installed Docker endpoint differs from the durable package binding",
        ));
    }
    if expected.daemon_id != actual.daemon_id {
        return Err(Error::new(
            ErrorKind::DaemonIdentityDrift,
            "installed Docker daemon identity differs from the durable package binding",
        ));
    }
    Ok(())
}

fn read_docker_binding(path: &Path) -> Result<Option<DockerBinding>, Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(Error::new(
                    ErrorKind::DockerConfiguration,
                    "durable Docker binding is not a regular file",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(Error::new(
                        ErrorKind::DockerConfiguration,
                        "durable Docker binding is accessible by another user",
                    ));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(Error::new(
                ErrorKind::DockerConfiguration,
                "cannot inspect durable Docker binding",
            ));
        }
    }
    let value = read_json(path)?;
    require_string(&value, "schema", DOCKER_BINDING_SCHEMA)?;
    let mut environment = BTreeMap::new();
    for (key, json_key) in [
        ("VELNOR_DOCKER_HOST", "host"),
        ("VELNOR_DOCKER_CONTEXT", "context"),
        ("VELNOR_DOCKER_DAEMON_ID", "daemon_id"),
    ] {
        if let Some(value) = value.get(json_key).and_then(Value::as_str) {
            environment.insert(key.to_owned(), value.to_owned());
        }
    }
    docker_binding_from_environment(&environment)?
        .ok_or_else(|| {
            Error::new(
                ErrorKind::DockerConfiguration,
                "durable Docker binding is incomplete",
            )
        })
        .map(Some)
}

fn endpoint_from_host(host: &str) -> Result<String, Error> {
    let host = host.trim();
    let socket = host.strip_prefix("unix://").unwrap_or(host).trim();
    if validate_absolute_path(Path::new(socket), "Docker socket").is_err() {
        return Err(Error::new(
            ErrorKind::DockerConfiguration,
            "installed Docker endpoint is not an absolute normalized Unix socket",
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
        .chain(
            PATH_ENV_KEYS
                .iter()
                .copied()
                .chain(["VELNOR_NAME"])
                .filter_map(|key| {
                    let value = env::var(key).ok()?;
                    (!value.trim().is_empty()).then_some((key.to_owned(), value))
                }),
        )
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
    let binding_path = paths.docker_binding.to_string_lossy();
    if let Some(have) = environment.get("VELNOR_DOCKER_BINDING_FILE")
        && have != binding_path.as_ref()
    {
        return Err(Error::new(
            ErrorKind::PathDrift,
            "package-owned Docker binding path disagrees with the installed storage root",
        ));
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

fn verify_install_identity(
    manifest: &Value,
    identity: &Value,
    paths: &DarwinServicePaths,
    manifest_path: &Path,
) -> Result<(), Error> {
    let manifest_version = required_metadata_string(manifest, "version")?;
    let identity_version = required_metadata_string(identity, "version")?;
    if manifest_version != identity_version || !valid_product_version(manifest_version) {
        return Err(Error::new(
            ErrorKind::Metadata,
            "installed product version is missing or inconsistent",
        ));
    }

    let manifest_source = required_metadata_string(manifest, "source_commit")?;
    let identity_source = required_metadata_string(identity, "source_commit")?;
    if manifest_source != identity_source || !valid_source_commit(manifest_source) {
        return Err(Error::new(
            ErrorKind::Metadata,
            "installed product source commit is missing or inconsistent",
        ));
    }
    let release_id = required_metadata_string(manifest, "release_id")?;
    if required_metadata_string(identity, "release_id")? != release_id
        || release_id != format!("macos-source-{}", &manifest_source[..12])
    {
        return Err(Error::new(
            ErrorKind::Metadata,
            "installed product release identity is missing or inconsistent",
        ));
    }
    let archive_sha = required_metadata_string(manifest, "source_archive_sha256")?;
    if !valid_sha256_hex(archive_sha) {
        return Err(Error::new(
            ErrorKind::Metadata,
            "installed product source archive digest is malformed",
        ));
    }
    let packaging_commit = required_metadata_string(manifest, "packaging_commit")?;
    if !valid_source_commit(packaging_commit) {
        return Err(Error::new(
            ErrorKind::Metadata,
            "installed product packaging commit is malformed",
        ));
    }

    let recorded_manifest_sha = required_metadata_string(identity, "manifest_sha256")?;
    if !valid_sha256_hex(recorded_manifest_sha) {
        return Err(Error::new(
            ErrorKind::Metadata,
            "installed manifest digest is malformed",
        ));
    }
    let actual_manifest_sha = sha256_file(manifest_path)?;
    if recorded_manifest_sha != actual_manifest_sha {
        return Err(Error::new(
            ErrorKind::Metadata,
            "installed manifest digest does not match identity metadata",
        ));
    }

    let manifest_binaries = manifest
        .get("binaries")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Metadata,
                "installed manifest has no binary digests",
            )
        })?;
    let identity_binaries = identity
        .get("binary_sha256")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Metadata,
                "installed identity has no binary digests",
            )
        })?;
    if manifest_binaries.len() != PRODUCT_BINARIES.len()
        || identity_binaries.len() != PRODUCT_BINARIES.len()
        || !manifest_binaries
            .keys()
            .all(|name| PRODUCT_BINARIES.contains(&name.as_str()))
        || !identity_binaries
            .keys()
            .all(|name| PRODUCT_BINARIES.contains(&name.as_str()))
    {
        return Err(Error::new(
            ErrorKind::Metadata,
            "installed product binary set is incomplete or contains unknown files",
        ));
    }
    for binary in PRODUCT_BINARIES {
        let expected = manifest_binaries
            .get(binary)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Metadata,
                    format!("installed manifest has no digest for {binary}"),
                )
            })?;
        let identity_expected = identity_binaries
            .get(binary)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Metadata,
                    format!("installed identity has no digest for {binary}"),
                )
            })?;
        if expected != identity_expected || !valid_sha256_hex(expected) {
            return Err(Error::new(
                ErrorKind::Metadata,
                format!("installed digest metadata is inconsistent for {binary}"),
            ));
        }
        let executable = paths.formula_prefix.join("bin").join(binary);
        let metadata = fs::metadata(&executable).map_err(|_| {
            Error::new(
                ErrorKind::Metadata,
                format!("installed product binary is missing: {binary}"),
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return Err(Error::new(
                    ErrorKind::Metadata,
                    format!("installed product binary is not executable: {binary}"),
                ));
            }
        }
        if !metadata.is_file() || sha256_file(&executable)? != expected {
            return Err(Error::new(
                ErrorKind::Metadata,
                format!("installed product binary digest mismatch: {binary}"),
            ));
        }
    }
    Ok(())
}

fn required_metadata_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Metadata,
                format!("installed metadata is missing string {key}"),
            )
        })
}

fn valid_product_version(value: &str) -> bool {
    let mut components = value.split('.');
    matches!(
        (components.next(), components.next(), components.next(), components.next()),
        (Some(major), Some(minor), Some(patch), None)
            if !major.is_empty()
                && !minor.is_empty()
                && !patch.is_empty()
                && [major, minor, patch]
                    .into_iter()
                    .all(|component| component.bytes().all(|byte| byte.is_ascii_digit()))
    )
}

fn valid_source_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn sha256_file(path: &Path) -> Result<String, Error> {
    let mut file = fs::File::open(path).map_err(|_| {
        Error::new(
            ErrorKind::Metadata,
            format!("cannot read installed product file: {}", path.display()),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|_| {
            Error::new(
                ErrorKind::Metadata,
                format!("cannot hash installed product file: {}", path.display()),
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn replay_environment(environment: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    REPLAY_ENV_KEYS
        .iter()
        .filter_map(|key| {
            environment
                .get(*key)
                .map(|value| ((*key).to_owned(), value.to_owned()))
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
    validate_absolute_path(&path, key)?;
    Ok(path)
}

fn validate_absolute_path(path: &Path, key: &str) -> Result<(), Error> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().contains(&0)
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(Error::new(
            ErrorKind::Metadata,
            format!("{key} must be an absolute path without parent traversal or NUL"),
        ));
    }
    Ok(())
}

fn validate_search_path(path: &str) -> Result<(), Error> {
    for directory in env::split_paths(path) {
        validate_absolute_path(&directory, "launchd PATH component")?;
    }
    Ok(())
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
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let (key, value) = line.split_once('=').ok_or_else(|| {
            Error::new(
                ErrorKind::Metadata,
                format!("invalid environment assignment at line {}", index + 1),
            )
        })?;
        let key = key.trim();
        if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return Err(Error::new(
                ErrorKind::Metadata,
                format!("invalid environment key at line {}", index + 1),
            ));
        }
        // Credentials are never parsed, retained, debugged or replayed.
        if !REPLAY_ENV_KEYS.contains(&key) && !["DOCKER_HOST", "DOCKER_CONTEXT"].contains(&key) {
            continue;
        }
        let value = literal_environment_value(value).ok_or_else(|| {
            Error::new(
                ErrorKind::Metadata,
                format!(
                    "service setting {key} at line {} must be a literal assignment",
                    index + 1
                ),
            )
        })?;
        result.insert(key.to_owned(), value.to_owned());
    }
    Ok(result)
}

fn literal_environment_value(value: &str) -> Option<&str> {
    let value = value.trim();
    if let Some(single) = value.strip_prefix('\'') {
        return single
            .strip_suffix('\'')
            .filter(|value| !value.contains('\''));
    }
    if let Some(double) = value.strip_prefix('"') {
        return double
            .strip_suffix('"')
            .filter(|value| !value.contains(['"', '$', '`', '\\']));
    }
    (!value
        .chars()
        .any(|c| c.is_whitespace() || "\"'$`\\;|&<>()#".contains(c)))
    .then_some(value)
}

fn read_plist(path: &Path) -> Result<Value, Error> {
    // Darwin's parser owns XML escaping, types and dictionary nesting. Read
    // to stdout only; never rewrite a packaged plist, source an env file or
    // consult the caller's PATH for this parser.
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-", "--"])
            .arg(path)
            .output()
            .map_err(|_| {
                Error::new(
                    ErrorKind::Metadata,
                    "cannot read launchd plist with /usr/bin/plutil",
                )
            })?;
        if !output.status.success() {
            return Err(Error::new(
                ErrorKind::Metadata,
                "invalid installed launchd property list",
            ));
        }
        serde_json::from_slice(&output.stdout).map_err(|_| {
            Error::new(
                ErrorKind::Metadata,
                "invalid launchd property list structure",
            )
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(Error::new(
            ErrorKind::Metadata,
            "launchd property lists require macOS",
        ))
    }
}

fn validate_plist(
    plist: &Value,
    paths: &DarwinServicePaths,
) -> Result<BTreeMap<String, String>, Error> {
    let program_arguments = plist
        .get("ProgramArguments")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Metadata,
                "launchd ProgramArguments must be an array",
            )
        })?;
    if program_arguments.len() != 1 {
        return Err(Error::new(
            ErrorKind::Metadata,
            "launchd service must have exactly one program argument",
        ));
    }
    let launcher = program_arguments[0].as_str().ok_or_else(|| {
        Error::new(
            ErrorKind::Metadata,
            "launchd launcher path must be a string",
        )
    })?;
    let launcher = Path::new(launcher);
    validate_absolute_path(launcher, "launchd launcher")?;
    if canonicalize_existing(launcher, "launchd launcher")?
        != canonicalize_existing(&paths.launcher, "package launcher")?
        || plist.get("Label").and_then(Value::as_str) != Some(SERVICE_LABEL)
        || plist.get("Program").is_some()
        || plist.get("RootDirectory").is_some()
        || plist.get("UserName").is_some()
        || plist.get("GroupName").is_some()
    {
        return Err(Error::new(
            ErrorKind::Metadata,
            "launchd plist must execute only the absolute package launcher as the service user",
        ));
    }
    for (key, expected) in [
        ("WorkingDirectory", paths.storage_root.clone()),
        ("StandardOutPath", paths.log_dir.join("runner.log")),
        ("StandardErrorPath", paths.log_dir.join("runner.err.log")),
    ] {
        if required_path(plist, key)? != expected {
            return Err(Error::new(
                ErrorKind::PathDrift,
                format!("launchd {key} disagrees with package-owned path"),
            ));
        }
    }
    let environment = plist
        .get("EnvironmentVariables")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Metadata,
                "launchd EnvironmentVariables must be a dictionary",
            )
        })?;
    let mut replay = BTreeMap::new();
    for key in REPLAY_ENV_KEYS.iter().copied().chain(["PATH"]) {
        if let Some(value) = environment.get(key) {
            let value = value.as_str().ok_or_else(|| {
                Error::new(
                    ErrorKind::Metadata,
                    format!("launchd {key} must be a string"),
                )
            })?;
            replay.insert(key.to_owned(), value.to_owned());
        }
    }
    let path = replay.get("VELNOR_PATH").ok_or_else(|| {
        Error::new(
            ErrorKind::Metadata,
            "launchd service is missing VELNOR_PATH",
        )
    })?;
    validate_search_path(path)?;
    if replay.get("PATH") != Some(path) {
        return Err(Error::new(
            ErrorKind::PathDrift,
            "launchd PATH disagrees with VELNOR_PATH",
        ));
    }
    Ok(replay)
}

fn canonicalize_existing(path: &Path, key: &str) -> Result<PathBuf, Error> {
    validate_absolute_path(path, key)?;
    fs::canonicalize(path).map_err(|_| {
        Error::new(
            ErrorKind::Metadata,
            format!("{key} does not resolve to an installed path"),
        )
    })
}

#[cfg(target_os = "macos")]
fn formula_prefix_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(executable) = env::current_exe()
        && let Some(bin) = executable.parent()
        && let Some(prefix) = bin.parent()
    {
        candidates.push(prefix.to_path_buf());
        candidates.push(prefix.join("opt/velnorctl"));
    }
    if let Some(prefix) = env::var_os("HOMEBREW_PREFIX").filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(prefix).join("opt/velnorctl"));
    }
    candidates.extend([
        PathBuf::from("/opt/homebrew/opt/velnorctl"),
        PathBuf::from("/usr/local/opt/velnorctl"),
    ]);
    let mut unique = Vec::new();
    for candidate in candidates {
        if !candidate.as_os_str().is_empty() && !unique.contains(&candidate) {
            unique.push(candidate);
        }
    }
    unique
}

#[cfg(all(test, target_os = "macos"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
pub(crate) mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    pub(crate) fn temp_root(label: &str) -> PathBuf {
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

    pub(crate) fn fixture(root: &Path, docker: bool) -> DarwinServicePaths {
        use std::os::unix::fs::PermissionsExt;
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
            docker_binding: storage.join(DOCKER_BINDING_FILE),
        };
        fs::create_dir_all(paths.launchd_plist.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.launcher.parent().unwrap()).unwrap();
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(&paths.launcher, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&paths.launcher, fs::Permissions::from_mode(0o755)).unwrap();
        let bin_dir = prefix.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let binary_digests = PRODUCT_BINARIES
            .into_iter()
            .map(|binary| {
                let path = bin_dir.join(binary);
                fs::write(&path, format!("fixture binary {binary}\n")).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
                (binary, sha256_file(&path).unwrap())
            })
            .collect::<BTreeMap<_, _>>();
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
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>Label</key><string>{SERVICE_LABEL}</string><key>ProgramArguments</key><array><string>{}</string></array>
             <key>WorkingDirectory</key><string>{}</string>
             <key>StandardOutPath</key><string>{}/runner.log</string>
             <key>StandardErrorPath</key><string>{}/runner.err.log</string>
             <key>EnvironmentVariables</key><dict>
             <key>PATH</key><string>/usr/bin:/bin</string>
             <key>VELNOR_PATH</key><string>/usr/bin:/bin</string>{}
             <key>VELNOR_CONFIG_DIR</key><string>{}</string>
             <key>VELNOR_ENV_FILE</key><string>{}</string>
             <key>VELNOR_LOG_DIR</key><string>{}</string>
             <key>VELNOR_MODE_STATE</key><string>{}</string>
             <key>VELNOR_PERMIT_LEDGER</key><string>{}</string>
             <key>VELNOR_STATE_DB</key><string>{}</string>
             <key>VELNOR_STORAGE_ROOT</key><string>{}</string>
             <key>VELNOR_TRUST_SCOPE</key><string>untrusted</string>
             <key>VELNOR_WORK_DIR</key><string>{}</string>
             </dict></dict></plist>",
            paths.launcher.display(),
            paths.storage_root.display(),
            paths.log_dir.display(),
            paths.log_dir.display(),
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
        let manifest = serde_json::json!({
            "schema": INSTALL_SCHEMA,
            "product_id": PRODUCT_ID,
            "channel": "stable",
            "version": "0.1.277",
            "release_id": "macos-source-111111111111",
            "source_commit": "1111111111111111111111111111111111111111",
            "source_archive_sha256": "2222222222222222222222222222222222222222222222222222222222222222",
            "packaging_commit": "3333333333333333333333333333333333333333",
            "binaries": binary_digests,
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
        });
        let manifest_text = manifest.to_string();
        fs::write(&paths.manifest, &manifest_text).unwrap();
        let manifest_sha = sha256_file(&paths.manifest).unwrap();
        fs::write(
            &paths.identity,
            serde_json::json!({
                "schema": IDENTITY_SCHEMA,
                "product_id": PRODUCT_ID,
                "version": "0.1.277",
                "release_id": "macos-source-111111111111",
                "source_commit": "1111111111111111111111111111111111111111",
                "manifest_sha256": manifest_sha,
                "binary_sha256": manifest["binaries"].clone(),
            })
            .to_string(),
        )
        .unwrap();
        if docker {
            fs::create_dir_all(&storage).unwrap();
            fs::write(
                &paths.docker_binding,
                serde_json::json!({
                    "schema": DOCKER_BINDING_SCHEMA,
                    "host": "unix:///tmp/orbstack/docker.sock",
                    "context": "orbstack",
                    "daemon_id": "engine-a",
                })
                .to_string(),
            )
            .unwrap();
            fs::set_permissions(&paths.docker_binding, fs::Permissions::from_mode(0o600)).unwrap();
        }
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
        let error = operation(&instance, true).unwrap_err();
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

    #[test]
    fn unrelated_launcher_string_cannot_authorize_another_program() {
        let root = temp_root("plist-executable");
        let paths = fixture(&root, true);
        let plist = fs::read_to_string(&paths.launchd_plist).unwrap().replace(
            &format!("<array><string>{}</string></array>", paths.launcher.display()),
            &format!(
                "<array><string>/tmp/other-program</string></array><key>Comment</key><string>{}</string>",
                paths.launcher.display()
            ),
        );
        fs::write(&paths.launchd_plist, plist).unwrap();
        assert!(resolve_from_formula_prefix(&paths.formula_prefix).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_environment_diagnostic_never_echoes_a_secret() {
        let root = temp_root("env-secret");
        let path = root.join("velnor.env");
        fs::write(&path, "secret-marker-without-assignment\n").unwrap();
        let error = parse_environment_file(&path).unwrap_err();
        assert!(!error.message.contains("secret-marker"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plist_nesting_and_executable_override_are_fail_closed() {
        let root = temp_root("plist-nesting");
        let paths = fixture(&root, true);
        let plist = read_plist(&paths.launchd_plist).unwrap();
        for bad in [
            {
                let mut value = plist.clone();
                value["Program"] = Value::from("/tmp/evil");
                value
            },
            {
                let mut value = plist.clone();
                value["ProgramArguments"] = serde_json::json!([paths.launcher, "unexpected"]);
                value
            },
            {
                let mut value = plist.clone();
                value["WorkingDirectory"] = Value::from("relative");
                value
            },
            {
                let mut value = plist.clone();
                value["StandardOutPath"] = Value::from("/tmp/other.log");
                value
            },
            {
                let mut value = plist.clone();
                value["EnvironmentVariables"] = Value::Null;
                value
            },
        ] {
            assert!(validate_plist(&bad, &paths).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn launchd_search_path_cannot_use_interactive_relative_entries() {
        for path in [
            "",
            ":/bin",
            "/bin:",
            "/bin::/usr/bin",
            "./bin:/bin",
            "/bin/../other",
        ] {
            assert!(validate_search_path(path).is_err(), "accepted {path:?}");
        }
        assert!(validate_search_path("/Applications/Tools With Spaces/bin:/usr/bin:/bin").is_ok());
    }

    #[test]
    fn operation_preserves_parent_environment_and_uses_service_paths() {
        let root = temp_root("immutable-operation");
        let paths = fixture(&root, true);
        let instance = resolve_from_formula_prefix(&paths.formula_prefix)
            .unwrap()
            .into_daemon_instance();
        let before = env::vars_os().collect::<BTreeMap<_, _>>();
        let operation = operation(&instance, true).unwrap();
        let command = operation.command("true").unwrap();
        assert_eq!(Path::new(command.get_program()), Path::new("/usr/bin/true"));
        let child_env = command.get_envs().collect::<BTreeMap<_, _>>();
        assert_eq!(
            child_env
                .get(std::ffi::OsStr::new("PATH"))
                .copied()
                .flatten(),
            Some(std::ffi::OsStr::new("/usr/bin:/bin"))
        );
        assert!(!child_env.contains_key(std::ffi::OsStr::new("GITHUB_TOKEN")));
        assert!(!child_env.contains_key(std::ffi::OsStr::new("DOCKER_CONTEXT")));
        assert_eq!(env::vars_os().collect::<BTreeMap<_, _>>(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_daemon_and_context_identity_must_match_both_parts_of_binding() {
        let environment = BTreeMap::from([
            (
                "VELNOR_DOCKER_HOST".into(),
                "unix:///tmp/engine.sock".into(),
            ),
            ("VELNOR_DOCKER_CONTEXT".into(), "local-engine".into()),
            ("VELNOR_DOCKER_DAEMON_ID".into(), "engine-a".into()),
        ]);
        let binding = docker_binding_from_environment(&environment)
            .unwrap()
            .unwrap();
        assert!(binding
            .verify_daemon(&serde_json::json!({"ID":"engine-a", "OSType":"linux"}))
            .is_ok());
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"ID":"engine-b", "OSType":"linux"}),
            serde_json::json!({"ID":"engine-a", "OSType":"windows"}),
        ] {
            assert!(binding.verify_daemon(&bad).is_err());
        }
        let good = serde_json::json!({"Name":"local-engine", "Endpoints":{"docker":{"Host":"unix:///tmp/engine.sock"}}});
        assert!(binding.verify_context(&good).is_ok());
        for bad in [
            {
                let mut value = good.clone();
                value["Name"] = Value::from("other");
                value
            },
            {
                let mut value = good.clone();
                value["Endpoints"]["docker"]["Host"] = Value::from("unix:///tmp/replacement.sock");
                value
            },
            {
                let mut value = good.clone();
                value["Endpoints"]["docker"]["Host"] = Value::from("tcp://localhost:2375");
                value
            },
        ] {
            assert!(binding.verify_context(&bad).is_err());
        }
    }

    #[test]
    fn environment_parser_reads_literals_without_shell_execution_or_secrets() {
        let root = temp_root("literal-env");
        let path = root.join("velnor.env");
        fs::write(&path, "export VELNOR_NAME='runner one'\nVELNOR_WORK_DIR=\"/tmp/work space\"\nGITHUB_TOKEN=$(secret-command)\n").unwrap();
        let parsed = parse_environment_file(&path).unwrap();
        assert_eq!(parsed["VELNOR_NAME"], "runner one");
        assert_eq!(parsed["VELNOR_WORK_DIR"], "/tmp/work space");
        assert!(!parsed.contains_key("GITHUB_TOKEN"));
        for value in [
            "$HOME/work",
            "\"$(evil)\"",
            "'/tmp'junk",
            "/tmp;evil",
            "\"unterminated",
        ] {
            assert!(
                literal_environment_value(value).is_none(),
                "accepted {value}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}

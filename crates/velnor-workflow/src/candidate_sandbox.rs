//! OS isolation for binaries supplied by an audited tree.
//!
//! A changed working directory or source copy does not stop malicious code
//! from opening an absolute path into the validator checkout. Candidate
//! products therefore run in a pinned Docker image whose only host input is
//! the candidate binary and, for render calls, an immutable source snapshot.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions, Permissions};
use std::io::{Cursor, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tar::{EntryType, Header as TarHeader};

#[cfg(unix)]
use std::env;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt as _;
#[cfg(unix)]
use std::os::unix::fs::{
    FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(unix)]
use std::sync::Arc;

const IMAGE: &str =
    "ubuntu@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3";
const CONTAINER_TIMEOUT: Duration = Duration::from_secs(180);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const LOG_LIMIT: usize = 1024 * 1024;
const OUTPUT_ARCHIVE_LIMIT: usize = 256 * 1024 * 1024;
const DOCKER_STDERR_LIMIT: usize = LOG_LIMIT;
const OUTPUT_TMPFS_SIZE: &str = "128m";
const LOG_TMPFS_SIZE: &str = "4m";
const CAPTURE_ARCHIVE_LIMIT: usize = 256 * 1024 * 1024;
const ARCHIVE_BLOCK_SIZE: usize = 512;
const ARCHIVE_MEMBER_LIMIT: usize = 16_384;
const ARCHIVE_METADATA_EXTENSION_LIMIT: usize = 64 * 1024;
const ARCHIVE_METADATA_TOTAL_LIMIT: usize = 64 * 1024 * 1024;
const ARCHIVE_PATH_LIMIT: usize = 4 * 1024;
const ARCHIVE_PATH_COMPONENT_LIMIT: usize = 128;
const OUTPUT_CONTENT_LIMIT: usize = 128 * 1024 * 1024;
const CANDIDATE_WRAPPER: &str = "ulimit -f 262144 || exit 125; /usr/bin/setpriv --reuid=65534 --regid=65534 --clear-groups --inh-caps=-all --ambient-caps=-all --bounding-set=-all -- /candidate \"$@\" > /tmp/velnor-candidate.stdout 2> /tmp/velnor-candidate.stderr; exit $?";

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct DockerEndpoint {
    host: String,
    socket: PathBuf,
}

#[cfg(unix)]
#[derive(Clone)]
struct DockerEnvironment {
    executable: PathBuf,
    path: OsString,
    endpoint: DockerEndpoint,
    private: Arc<PrivateDockerEnvironment>,
}

#[cfg(unix)]
struct PrivateDockerEnvironment {
    root: PathBuf,
    config: PathBuf,
}

#[cfg(unix)]
impl Drop for PrivateDockerEnvironment {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
impl DockerEnvironment {
    fn new() -> Result<Self, String> {
        let endpoint = resolve_docker_endpoint()?;
        let (executable, path) = find_docker_executable()?;
        let root = env::temp_dir().join(format!(
            "velnor-candidate-docker-env-{}",
            crate::unique_suffix()
        ));
        fs::create_dir(&root).map_err(|error| {
            format!(
                "create private candidate Docker home {}: {error}",
                root.display()
            )
        })?;
        if let Err(error) = fs::set_permissions(&root, Permissions::from_mode(0o700)) {
            let _ = fs::remove_dir(&root);
            return Err(format!(
                "protect private candidate Docker home {}: {error}",
                root.display()
            ));
        }
        let config = root.join("config");
        if let Err(error) = fs::create_dir(&config) {
            let _ = fs::remove_dir(&root);
            return Err(format!(
                "create private candidate Docker config {}: {error}",
                config.display()
            ));
        }
        if let Err(error) = fs::set_permissions(&config, Permissions::from_mode(0o700)) {
            let _ = fs::remove_dir_all(&root);
            return Err(format!(
                "protect private candidate Docker config {}: {error}",
                config.display()
            ));
        }
        Ok(Self {
            executable,
            path,
            endpoint,
            private: Arc::new(PrivateDockerEnvironment { root, config }),
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .env_clear()
            .env("PATH", &self.path)
            .env("HOME", &self.private.root)
            .env("DOCKER_CONFIG", &self.private.config)
            .env("DOCKER_HOST", &self.endpoint.host);
        command
    }
}

#[cfg(unix)]
fn find_docker_executable() -> Result<(PathBuf, OsString), String> {
    let mut directories = Vec::new();
    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            if directory.is_absolute() {
                directories.push(directory);
            }
        }
    }
    for directory in [
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ] {
        if !directories.iter().any(|candidate| candidate == &directory) {
            directories.push(directory);
        }
    }

    let mut trusted_directories = Vec::new();
    for directory in directories {
        let Ok(directory) = fs::canonicalize(directory) else {
            continue;
        };
        let Ok(directory_metadata) = fs::metadata(&directory) else {
            continue;
        };
        if !directory_metadata.is_dir() || directory_metadata.permissions().mode() & 0o022 != 0 {
            continue;
        }
        let candidate = directory.join("docker");
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o111 == 0
            || metadata.permissions().mode() & 0o022 != 0
        {
            continue;
        }
        // Execute through the `docker` leaf name: resolving the leaf rewrites
        // argv0 and breaks multi-call binaries that dispatch on it (OrbStack's
        // `docker` is a symlink to its `docker-tools` dispatcher, which
        // rejects any other argv0). The directory above is already
        // canonicalized and all metadata checks follow links, so the trust
        // checks still describe the executed file.
        let executable = candidate;
        if !trusted_directories.iter().any(|path| path == &directory) {
            trusted_directories.push(directory.clone());
        }
        // Keep the Docker executable's directory first. Docker plugins are
        // not used by the sandbox, but this makes the child PATH explicit and
        // deterministic if the CLI performs an internal helper lookup.
        trusted_directories.retain(|path| path != executable.parent().unwrap_or(&directory));
        trusted_directories.insert(0, executable.parent().unwrap_or(&directory).to_owned());
        let path = env::join_paths(trusted_directories)
            .map_err(|error| format!("construct trusted Docker PATH: {error}"))?;
        return Ok((executable, path));
    }
    Err("no executable Docker CLI found in absolute PATH directories".to_owned())
}

#[cfg(unix)]
fn resolve_docker_endpoint() -> Result<DockerEndpoint, String> {
    // The shipped workflow generator intentionally has no production
    // dependency on velnor-runner. Keep this precedence, context metadata
    // shape, and platform default order aligned with runner/docker/engine.rs;
    // this boundary adds the sandbox-only requirement that the selected path
    // already be an existing Unix socket.
    let home = env::var_os("HOME").map(PathBuf::from);
    let config_dir = env::var_os("DOCKER_CONFIG")
        .map(PathBuf::from)
        .or_else(|| home.as_deref().map(|path| path.join(".docker")));
    let explicit_host = env_value("VELNOR_DOCKER_HOST")?;
    let docker_host = env_value("DOCKER_HOST")?;
    let context = match env_value("VELNOR_DOCKER_CONTEXT")? {
        Some(context) => Some(context),
        None => env_value("DOCKER_CONTEXT")?,
    };
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    resolve_docker_endpoint_from(
        explicit_host.as_deref(),
        docker_host.as_deref(),
        context.as_deref(),
        config_dir.as_deref(),
        home.as_deref(),
        runtime_dir.as_deref(),
    )
}

#[cfg(unix)]
fn env_value(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) => Ok(nonempty(Some(&value)).map(str::to_owned)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(format!(
            "Docker environment variable {name} is not valid UTF-8"
        )),
    }
}

#[cfg(unix)]
fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(unix)]
fn resolve_docker_endpoint_from(
    explicit_host: Option<&str>,
    docker_host: Option<&str>,
    context: Option<&str>,
    config_dir: Option<&Path>,
    home: Option<&Path>,
    runtime_dir: Option<&Path>,
) -> Result<DockerEndpoint, String> {
    if let Some(host) = nonempty(explicit_host) {
        return endpoint_from_host(host, "VELNOR_DOCKER_HOST");
    }
    if let Some(context) = nonempty(context) {
        return resolve_context_endpoint(config_dir, home, runtime_dir, context);
    }
    if let Some(host) = nonempty(docker_host) {
        return endpoint_from_host(host, "DOCKER_HOST");
    }
    if let Some(context) = configured_context(config_dir)? {
        return resolve_context_endpoint(config_dir, home, runtime_dir, &context);
    }
    default_endpoint(home, runtime_dir)
}

#[cfg(unix)]
fn endpoint_from_host(host: &str, source: &str) -> Result<DockerEndpoint, String> {
    let host = host.trim();
    let socket = if let Some(path) = host.strip_prefix("unix://") {
        PathBuf::from(path)
    } else if host.starts_with('/') {
        PathBuf::from(host)
    } else {
        return Err(format!(
            "refusing remote Docker endpoint {host:?} from {source}; TCP, SSH, and named-pipe endpoints are not supported by candidate isolation"
        ));
    };
    if !socket.is_absolute() {
        return Err(format!(
            "refusing Docker endpoint {host:?} from {source}; it must name an absolute Unix socket path"
        ));
    }
    if socket.as_os_str().is_empty() || socket.to_string_lossy().contains('\0') {
        return Err(format!(
            "Docker endpoint from {source} has an invalid socket path"
        ));
    }
    require_local_socket(&socket, source)?;
    Ok(DockerEndpoint {
        host: normalized_socket_host(&socket, source)?,
        socket,
    })
}

#[cfg(unix)]
fn require_local_socket(path: &Path, source: &str) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "Docker endpoint from {source} must name an existing local Unix socket {}: {error}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_socket() {
        return Err(format!(
            "Docker endpoint from {source} is not a Unix socket: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn configured_context(config_dir: Option<&Path>) -> Result<Option<String>, String> {
    let Some(config_dir) = config_dir else {
        return Ok(None);
    };
    let path = config_dir.join("config.json");
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path)
        .map_err(|error| format!("read Docker config {}: {error}", path.display()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse Docker config {}: {error}", path.display()))?;
    Ok(value
        .get("currentContext")
        .and_then(serde_json::Value::as_str)
        .and_then(|context| nonempty(Some(context)).map(str::to_owned)))
}

#[cfg(unix)]
fn resolve_context_endpoint(
    config_dir: Option<&Path>,
    home: Option<&Path>,
    runtime_dir: Option<&Path>,
    context: &str,
) -> Result<DockerEndpoint, String> {
    let context = context.trim();
    if context.is_empty() {
        return Err("Docker context is empty; refusing an unresolved endpoint".to_owned());
    }
    if context == "default" {
        return default_endpoint_with_context(home, runtime_dir, Some(context));
    }
    let Some(config_dir) = config_dir else {
        return Err(format!(
            "Docker context {context:?} has no readable Docker config directory; refusing an unresolved endpoint"
        ));
    };
    let metadata_root = config_dir.join("contexts/meta");
    if !metadata_root.is_dir() {
        return Err(format!(
            "Docker context {context:?} has no metadata under {}; refusing an unresolved endpoint",
            metadata_root.display()
        ));
    }
    let direct = metadata_root.join(context).join("meta.json");
    if direct.is_file() {
        return context_endpoint_from_metadata(&direct, context);
    }
    let entries = fs::read_dir(&metadata_root).map_err(|error| {
        format!(
            "read Docker context metadata {}: {error}",
            metadata_root.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "read Docker context metadata {}: {error}",
                metadata_root.display()
            )
        })?;
        let metadata = entry.path().join("meta.json");
        if !metadata.is_file() {
            continue;
        }
        let bytes = fs::read(&metadata).map_err(|error| {
            format!(
                "read Docker context metadata {}: {error}",
                metadata.display()
            )
        })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "parse Docker context metadata {}: {error}",
                metadata.display()
            )
        })?;
        if json_string(&value, "Name") == Some(context) {
            return context_endpoint_from_value(&value, context, &metadata);
        }
    }
    Err(format!(
        "Docker context {context:?} was not found in {}; refusing an unresolved endpoint",
        metadata_root.display()
    ))
}

#[cfg(unix)]
fn context_endpoint_from_metadata(path: &Path, context: &str) -> Result<DockerEndpoint, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("read Docker context {}: {error}", path.display()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse Docker context {}: {error}", path.display()))?;
    context_endpoint_from_value(&value, context, path)
}

#[cfg(unix)]
fn context_endpoint_from_value(
    value: &serde_json::Value,
    context: &str,
    path: &Path,
) -> Result<DockerEndpoint, String> {
    let host = value
        .get("Endpoints")
        .and_then(|endpoints| endpoints.get("docker"))
        .and_then(|docker| docker.get("Host"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            format!(
                "Docker context {context:?} at {} has no docker Host endpoint",
                path.display()
            )
        })?;
    endpoint_from_host(host, "Docker context")
}

#[cfg(unix)]
fn json_string<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value
        .as_object()?
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .and_then(|(_, value)| value.as_str())
}

#[cfg(unix)]
fn default_endpoint(
    home: Option<&Path>,
    runtime_dir: Option<&Path>,
) -> Result<DockerEndpoint, String> {
    default_endpoint_with_context(home, runtime_dir, None)
}

#[cfg(unix)]
fn default_endpoint_with_context(
    home: Option<&Path>,
    runtime_dir: Option<&Path>,
    _context: Option<&str>,
) -> Result<DockerEndpoint, String> {
    endpoint_from_candidates(
        default_socket_candidates(home, runtime_dir),
        "portable local default",
    )
}

#[cfg(unix)]
fn endpoint_from_candidates(
    candidates: impl IntoIterator<Item = PathBuf>,
    source: &str,
) -> Result<DockerEndpoint, String> {
    let mut errors = Vec::new();
    for candidate in candidates {
        match require_local_socket(&candidate, source) {
            Ok(()) => {
                return Ok(DockerEndpoint {
                    host: normalized_socket_host(&candidate, source)?,
                    socket: candidate,
                });
            }
            Err(error) => errors.push(error),
        }
    }
    Err(format!(
        "no existing local Docker Unix socket found in the platform default order ({})",
        errors.join("; ")
    ))
}

#[cfg(unix)]
fn normalized_socket_host(path: &Path, source: &str) -> Result<String, String> {
    let path = path.to_str().ok_or_else(|| {
        format!(
            "Docker endpoint from {source} has a non-UTF-8 socket path: {}",
            path.display()
        )
    })?;
    Ok(format!("unix://{path}"))
}

#[cfg(unix)]
fn default_socket_candidates(
    home: Option<&Path>,
    #[cfg(not(target_os = "macos"))] runtime_dir: Option<&Path>,
    #[cfg(target_os = "macos")] _runtime_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home {
            candidates.push(home.join(".orbstack/run/docker.sock"));
            candidates.push(home.join(".docker/run/docker.sock"));
        }
        candidates.push(PathBuf::from("/var/run/docker.sock"));
        candidates.push(PathBuf::from("/run/docker.sock"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        candidates.push(PathBuf::from("/var/run/docker.sock"));
        candidates.push(PathBuf::from("/run/docker.sock"));
        if let Some(runtime_dir) = runtime_dir {
            candidates.push(runtime_dir.join("docker.sock"));
        }
        if let Some(home) = home {
            candidates.push(home.join(".docker/run/docker.sock"));
        }
    }
    candidates
}

pub(crate) struct Output {
    pub(crate) status: i32,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// Run an untrusted policy binary without mounting the validator checkout,
/// home directory, Docker socket, or runner credentials into its container.
/// `source` is mounted read-only at `/workspace`; `output` receives a
/// validated copy of the container's size-capped `/output` tmpfs.
pub(crate) fn run(
    binary: &Path,
    source: Option<&Path>,
    output: Option<&Path>,
    args: &[OsString],
) -> Result<Output, String> {
    #[cfg(not(unix))]
    {
        let _ = (binary, source, output, args);
        Err("candidate execution requires a Unix Docker host".to_owned())
    }

    #[cfg(unix)]
    run_unix(binary, source, output, args)
}

#[cfg(unix)]
fn run_unix(
    binary: &Path,
    source: Option<&Path>,
    output: Option<&Path>,
    args: &[OsString],
) -> Result<Output, String> {
    let (binary, source, output) = validate_candidate_paths(binary, source, output)?;
    let docker = DockerEnvironment::new()?;
    let binary_mount = docker_mount_source(&binary)?;
    let source_mount = source.as_deref().map(docker_mount_source).transpose()?;
    let name = format!("velnor-candidate-{}", crate::unique_suffix());
    let volumes = SandboxVolumes::new(&docker, output.is_some())?;
    let cleanup = ContainerCleanup {
        name: name.clone(),
        volumes: volumes.clone(),
        docker: docker.clone(),
    };

    start_sandbox(
        &docker,
        &name,
        &volumes,
        &binary_mount,
        source_mount.as_deref(),
        source.is_some(),
    )?;
    let result = execute_candidate(
        &docker,
        &name,
        &volumes,
        source.is_some(),
        output.as_deref(),
        args,
    );
    drop(cleanup);
    result
}

#[cfg(unix)]
fn validate_candidate_paths(
    binary: &Path,
    source: Option<&Path>,
    output: Option<&Path>,
) -> Result<(PathBuf, Option<PathBuf>, Option<PathBuf>), String> {
    if source.is_some() != output.is_some() {
        return Err("candidate render needs both source and output paths".to_owned());
    }
    let binary = fs::canonicalize(binary)
        .map_err(|error| format!("candidate binary {}: {error}", binary.display()))?;
    if !binary.is_file() {
        return Err(format!(
            "candidate binary {} is not a file",
            binary.display()
        ));
    }
    let source = source
        .map(fs::canonicalize)
        .transpose()
        .map_err(|error| format!("candidate source snapshot: {error}"))?;
    if let Some(source) = &source
        && !source.is_dir()
    {
        return Err(format!(
            "candidate source snapshot {} is not a directory",
            source.display()
        ));
    }
    let mut output = output.map(Path::to_path_buf);
    if let Some(path) = &output {
        fs::create_dir_all(path)
            .map_err(|error| format!("candidate render output {}: {error}", path.display()))?;
        let canonical = fs::canonicalize(path)
            .map_err(|error| format!("candidate render output {}: {error}", path.display()))?;
        if fs::read_dir(&canonical)
            .map_err(|error| format!("candidate render output {}: {error}", path.display()))?
            .next()
            .is_some()
        {
            return Err(format!(
                "candidate render output {} is not empty",
                path.display()
            ));
        }
        output = Some(canonical);
    }
    Ok((binary, source, output))
}

#[cfg(unix)]
#[derive(Clone)]
struct SandboxVolumes {
    logs: String,
    output: Option<String>,
}

#[cfg(unix)]
impl SandboxVolumes {
    fn new(docker: &DockerEnvironment, has_output: bool) -> Result<Self, String> {
        let logs = create_tmpfs_volume(
            docker,
            &format!("velnor-candidate-logs-{}", crate::unique_suffix()),
            LOG_TMPFS_SIZE,
            4096,
        )?;
        let output = if has_output {
            match create_tmpfs_volume(
                docker,
                &format!("velnor-candidate-output-{}", crate::unique_suffix()),
                OUTPUT_TMPFS_SIZE,
                16384,
            ) {
                Ok(volume) => Some(volume),
                Err(error) => {
                    remove_volume_bounded(docker, &logs, CLEANUP_TIMEOUT);
                    return Err(error);
                }
            }
        } else {
            None
        };
        Ok(Self { logs, output })
    }
}

#[cfg(unix)]
fn create_tmpfs_volume(
    docker: &DockerEnvironment,
    name: &str,
    size: &str,
    inodes: usize,
) -> Result<String, String> {
    let options = format!("size={size},nr_inodes={inodes},mode=1777,nosuid,nodev,noexec");
    let mut command = docker.command();
    let created = match command_output_until(
        command
            .args([
                "volume",
                "create",
                "--driver",
                "local",
                "--opt",
                "type=tmpfs",
                "--opt",
                "device=tmpfs",
                "--opt",
            ])
            .arg(format!("o={options}"))
            .arg(name),
        Instant::now() + CONTAINER_TIMEOUT,
        LOG_LIMIT,
        DOCKER_STDERR_LIMIT,
        "create candidate sandbox tmpfs",
        None,
    ) {
        Ok(created) => created,
        Err(error) => {
            // The daemon may have completed the create request after the CLI
            // timed out or lost its response. Remove by the generated name on
            // every failure path so interrupted setup cannot leak tmpfs.
            remove_volume_bounded(docker, name, CLEANUP_TIMEOUT);
            return Err(error);
        }
    };
    if !created.status.success() {
        remove_volume_bounded(docker, name, CLEANUP_TIMEOUT);
        return Err(format!(
            "create candidate sandbox tmpfs failed: {}",
            String::from_utf8_lossy(&created.stderr).trim()
        ));
    }
    if String::from_utf8_lossy(&created.stdout).trim() != name {
        remove_volume_bounded(docker, name, CLEANUP_TIMEOUT);
        return Err("Docker returned an unexpected candidate tmpfs name".to_owned());
    }
    Ok(name.to_owned())
}

#[cfg(unix)]
fn remove_volume_bounded(docker: &DockerEnvironment, name: &str, timeout: Duration) {
    let mut command = docker.command();
    command.args(["volume", "rm", name]);
    discard_command_until(&mut command, Instant::now() + timeout);
}

#[cfg(unix)]
fn start_sandbox(
    docker: &DockerEnvironment,
    name: &str,
    volumes: &SandboxVolumes,
    binary_mount: &str,
    source_mount: Option<&str>,
    has_source: bool,
) -> Result<(), String> {
    // The wrapper remains uid 0 so the candidate cannot use same-uid
    // `/proc/<parent>/fd` access to bypass its bounded tmpfs logs and write
    // directly to the Docker CLI pipes. `setpriv` drops the candidate to an
    // unprivileged uid with no capabilities before executing its bytes.
    let mut start = docker.command();
    start
        .args(["run", "--pull=missing", "--detach", "--name"])
        .arg(name)
        .args([
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            // Setup/cleanup require these; setpriv removes them from the
            // candidate's bounding, inherited, permitted, and ambient sets.
            "--cap-add=SETUID",
            "--cap-add=SETGID",
            "--cap-add=SETPCAP",
            "--security-opt=no-new-privileges:true",
            "--pids-limit=128",
            "--memory=512m",
            "--memory-swap=512m",
            "--cpus=1",
            "--ulimit",
            "fsize=134217728:134217728",
            "--ulimit",
            "nofile=1024:1024",
            "--env=PATH=/usr/bin:/bin",
            "--env=HOME=/tmp",
            "--mount",
        ])
        .arg(format!("type=volume,src={},dst=/tmp", volumes.logs))
        .arg("--mount")
        .arg(format!(
            "type=bind,src={binary_mount},dst=/candidate,readonly"
        ));
    if let Some(output) = &volumes.output {
        start
            .arg("--mount")
            .arg(format!("type=volume,src={output},dst=/output"));
    }
    if let Some(source_mount) = source_mount {
        start.arg("--mount").arg(format!(
            "type=bind,src={source_mount},dst=/workspace,readonly"
        ));
    }
    start
        .arg("--workdir")
        .arg(if has_source { "/workspace" } else { "/" })
        .args([IMAGE, "/bin/sleep", "infinity"]);
    let started = command_output_until(
        &mut start,
        Instant::now() + CONTAINER_TIMEOUT,
        LOG_LIMIT,
        DOCKER_STDERR_LIMIT,
        "start candidate OS sandbox",
        Some((docker, name)),
    )?;
    if !started.status.success() {
        let detail = String::from_utf8_lossy(&started.stderr);
        return Err(format!(
            "start pinned candidate sandbox failed: {}",
            detail.trim()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn execute_candidate(
    docker: &DockerEnvironment,
    name: &str,
    volumes: &SandboxVolumes,
    has_source: bool,
    output: Option<&Path>,
    args: &[OsString],
) -> Result<Output, String> {
    let deadline = Instant::now() + CONTAINER_TIMEOUT;
    let log_setup = [
        OsString::from("-c"),
        OsString::from("umask 000; : > /tmp/velnor-candidate.stdout; : > /tmp/velnor-candidate.stderr; chmod 0666 /tmp/velnor-candidate.stdout /tmp/velnor-candidate.stderr"),
    ];
    let setup = docker_exec_until(
        docker,
        DockerExecOptions {
            name,
            user: "0:0",
            workdir: None,
            executable: "/bin/sh",
            args: &log_setup,
            deadline,
            stdout_limit: LOG_LIMIT,
        },
    )?;
    if !setup.status.success() {
        return Err(format!(
            "prepare candidate sandbox logs failed: {}",
            String::from_utf8_lossy(&setup.stderr).trim()
        ));
    }
    let mut candidate_args = vec![
        OsString::from("-c"),
        OsString::from(CANDIDATE_WRAPPER),
        OsString::from("candidate-wrapper"),
    ];
    candidate_args.extend_from_slice(args);
    let candidate = docker_exec_until(
        docker,
        DockerExecOptions {
            name,
            user: "0:0",
            workdir: has_source.then_some("/workspace"),
            executable: "/bin/sh",
            args: &candidate_args,
            deadline,
            stdout_limit: LOG_LIMIT,
        },
    )?;
    // A finite process scan is not a quiescence proof: a descendant can fork
    // after the last scan and mutate the output while the archive is being
    // collected. Freeze the entire container before reading any candidate
    // owned path. Docker's freezer covers every process and namespace in the
    // container, so no late writer can run between the proof and the copy.
    pause_container(docker, name, deadline)?;
    let capture = CaptureDirectory::new()?;
    archive_frozen_volumes(docker, volumes, &capture, output.is_some(), deadline)?;
    let stdout_archive = read_capture_archive(
        &capture.root.join("stdout.tar"),
        CAPTURE_ARCHIVE_LIMIT,
        "candidate stdout archive",
    )?;
    let stderr_archive = read_capture_archive(
        &capture.root.join("stderr.tar"),
        CAPTURE_ARCHIVE_LIMIT,
        "candidate stderr archive",
    )?;
    let stdout = single_file_archive(&stdout_archive, "velnor-candidate.stdout", LOG_LIMIT)?;
    let stderr = single_file_archive(&stderr_archive, "velnor-candidate.stderr", LOG_LIMIT)?;
    if let Some(output) = output {
        let archive = read_capture_archive(
            &capture.root.join("output.tar"),
            CAPTURE_ARCHIVE_LIMIT,
            "candidate render archive",
        )?;
        extract_render_archive(&archive, output)?;
    }
    Ok(Output {
        status: candidate.status.code().unwrap_or(128),
        stdout,
        stderr,
    })
}

#[cfg(unix)]
fn pause_container(
    docker: &DockerEnvironment,
    name: &str,
    deadline: Instant,
) -> Result<(), String> {
    let mut command = docker.command();
    let paused = command_output_until(
        command.args(["pause", name]),
        deadline,
        LOG_LIMIT,
        DOCKER_STDERR_LIMIT,
        "pause candidate sandbox",
        Some((docker, name)),
    )?;
    if paused.status.success() {
        Ok(())
    } else {
        Err(format!(
            "cannot freeze candidate sandbox: {}",
            String::from_utf8_lossy(&paused.stderr).trim()
        ))
    }
}

#[cfg(unix)]
struct CaptureDirectory {
    root: PathBuf,
}

#[cfg(unix)]
impl CaptureDirectory {
    fn new() -> Result<Self, String> {
        let root = std::env::temp_dir().join(format!(
            "velnor-candidate-capture-{}",
            crate::unique_suffix()
        ));
        fs::create_dir(&root).map_err(|error| {
            format!(
                "create private candidate capture directory {}: {error}",
                root.display()
            )
        })?;
        fs::set_permissions(&root, Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "protect private candidate capture directory {}: {error}",
                root.display()
            )
        })?;
        Ok(Self { root })
    }
}

#[cfg(unix)]
impl Drop for CaptureDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
fn archive_frozen_volumes(
    docker: &DockerEnvironment,
    volumes: &SandboxVolumes,
    capture: &CaptureDirectory,
    has_output: bool,
    deadline: Instant,
) -> Result<(), String> {
    let sidecar_name = format!("velnor-candidate-capture-{}", crate::unique_suffix());
    let sidecar_cleanup = ContainerNameCleanup {
        docker: docker.clone(),
        name: sidecar_name.clone(),
    };
    let capture_mount = docker_mount_source(&capture.root)?;
    let capture_metadata = fs::metadata(&capture.root).map_err(|error| {
        format!(
            "inspect private candidate capture directory {}: {error}",
            capture.root.display()
        )
    })?;
    let capture_user = format!("{}:{}", capture_metadata.uid(), capture_metadata.gid());
    let archive_limit_blocks = CAPTURE_ARCHIVE_LIMIT / 512;
    let script = format!(
        r#"
set -eu
ulimit -f {archive_limit_blocks}
/bin/tar -C /tmp -cf /capture/stdout.tar velnor-candidate.stdout
/bin/tar -C /tmp -cf /capture/stderr.tar velnor-candidate.stderr
if [ "$1" = render ]; then
  /bin/tar -C /output -cf /capture/output.tar .
fi
"#
    );
    let mut command = docker.command();
    command
        .args(["run", "--pull=missing", "--rm", "--name"])
        .arg(&sidecar_name)
        .args([
            "--user",
            &capture_user,
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges:true",
            "--pids-limit=16",
            "--memory=128m",
            "--cpus=1",
            "--ulimit",
        ])
        .arg(format!(
            "fsize={CAPTURE_ARCHIVE_LIMIT}:{CAPTURE_ARCHIVE_LIMIT}"
        ))
        .arg("--mount")
        .arg(format!(
            "type=volume,src={},dst=/tmp,readonly",
            volumes.logs
        ))
        .args(["--mount"])
        .arg(format!("type=bind,src={capture_mount},dst=/capture"));
    if let Some(output) = &volumes.output {
        command
            .args(["--mount"])
            .arg(format!("type=volume,src={output},dst=/output,readonly"));
    }
    command
        .args([IMAGE, "/bin/sh", "-eu", "-c", &script, "capture"])
        .arg(if has_output { "render" } else { "probe" });
    let copied = command_output_until(
        &mut command,
        deadline,
        LOG_LIMIT,
        DOCKER_STDERR_LIMIT,
        "archive frozen candidate sandbox",
        Some((docker, &sidecar_name)),
    )?;
    drop(sidecar_cleanup);
    if copied.status.success() {
        Ok(())
    } else {
        Err(format!(
            "archive frozen candidate sandbox failed: {}",
            String::from_utf8_lossy(&copied.stderr).trim()
        ))
    }
}

#[cfg(unix)]
fn read_capture_archive(path: &Path, limit: usize, label: &str) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("{label}: cannot inspect {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "{label}: Docker returned a non-regular file at {}",
            path.display()
        ));
    }
    let size = usize::try_from(metadata.len())
        .map_err(|_| format!("{label}: captured file is too large"))?;
    if size > limit {
        return Err(format!("{label} exceeds {limit} bytes"));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("{label}: cannot open {}: {error}", path.display()))?;
    let mut bytes = Vec::with_capacity(size);
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("{label}: cannot read {}: {error}", path.display()))?;
    if bytes.len() > limit {
        return Err(format!("{label} exceeds {limit} bytes"));
    }
    Ok(bytes)
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct DockerExecOptions<'a> {
    name: &'a str,
    user: &'a str,
    workdir: Option<&'a str>,
    executable: &'a str,
    args: &'a [OsString],
    deadline: Instant,
    stdout_limit: usize,
}

#[cfg(unix)]
fn docker_exec_until(
    docker: &DockerEnvironment,
    options: DockerExecOptions<'_>,
) -> Result<std::process::Output, String> {
    let mut command = docker.command();
    command.args(["exec", "--user", options.user]);
    if let Some(workdir) = options.workdir {
        command.args(["--workdir", workdir]);
    }
    let mut child = command
        .arg(options.name)
        .arg(options.executable)
        .args(options.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("start candidate sandbox command: {error}"))?;
    bounded_child_output(
        &mut child,
        options.deadline,
        options.stdout_limit,
        DOCKER_STDERR_LIMIT,
        Some((docker, options.name)),
    )
}

#[cfg(unix)]
fn command_output_until(
    command: &mut Command,
    deadline: Instant,
    stdout_limit: usize,
    stderr_limit: usize,
    label: &str,
    cleanup_container: Option<(&DockerEnvironment, &str)>,
) -> Result<std::process::Output, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("{label}: {error}"))
        .and_then(|mut child| {
            bounded_child_output(
                &mut child,
                deadline,
                stdout_limit,
                stderr_limit,
                cleanup_container,
            )
        })
}

#[cfg(unix)]
fn bounded_child_output(
    child: &mut std::process::Child,
    deadline: Instant,
    stdout_limit: usize,
    stderr_limit: usize,
    cleanup_container: Option<(&DockerEnvironment, &str)>,
) -> Result<std::process::Output, String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "candidate sandbox stdout pipe is unavailable".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "candidate sandbox stderr pipe is unavailable".to_owned())?;
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(stdout_limit.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr
            .take(stderr_limit.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });

    let status = loop {
        let polled = child.try_wait();
        let status = match polled {
            Ok(status) => status,
            Err(error) => {
                terminate_child(child);
                return Err(format!("poll candidate sandbox command: {error}"));
            }
        };
        match status {
            Some(status) => break status,
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            None => {
                terminate_child(child);
                if let Some((docker, name)) = cleanup_container {
                    kill_container_bounded(docker, name, CLEANUP_TIMEOUT);
                }
                return Err(format!(
                    "candidate sandbox command exceeded {} seconds and was killed",
                    CONTAINER_TIMEOUT.as_secs()
                ));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "candidate sandbox stdout reader failed".to_owned())?
        .map_err(|error| format!("read candidate sandbox stdout: {error}"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "candidate sandbox stderr reader failed".to_owned())?
        .map_err(|error| format!("read candidate sandbox stderr: {error}"))?;
    if stdout.len() > stdout_limit {
        return Err(format!(
            "candidate sandbox command stdout exceeds {stdout_limit} bytes"
        ));
    }
    if stderr.len() > stderr_limit {
        return Err(format!(
            "candidate sandbox command stderr exceeds {stderr_limit} bytes"
        ));
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(unix)]
fn terminate_child(child: &mut std::process::Child) {
    let child_pid = rustix::process::Pid::from_child(child);
    let process_group = rustix::process::getpgid(Some(child_pid))
        .ok()
        .filter(|group| *group == child_pid)
        .filter(|group| rustix::process::getpgid(None).ok() != Some(*group));
    if let Some(process_group) = process_group {
        let _ = rustix::process::kill_process_group(process_group, rustix::process::Signal::KILL);
    } else {
        let _ = child.kill();
    }
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => thread::sleep(Duration::from_millis(25)),
        }
    }
    if let Some(process_group) = process_group {
        let _ = rustix::process::kill_process_group(process_group, rustix::process::Signal::KILL);
    } else {
        let _ = child.kill();
    }
}

#[cfg(unix)]
fn kill_container_bounded(docker: &DockerEnvironment, name: &str, timeout: Duration) {
    let mut command = docker.command();
    command.args(["kill", name]);
    discard_command_until(&mut command, Instant::now() + timeout);
}

#[cfg(unix)]
fn discard_command_until(command: &mut Command, deadline: Instant) {
    let Ok(mut child) = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    else {
        return;
    };
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Ok(None) | Err(_) => {
                terminate_child(&mut child);
                return;
            }
        }
    }
}

#[cfg(unix)]
fn docker_mount_source(path: &Path) -> Result<String, String> {
    let value = path
        .to_str()
        .ok_or_else(|| format!("Docker bind path {} is not UTF-8", path.display()))?;
    // Docker's `--mount` is comma-delimited and does not provide an
    // unambiguous representation for every host pathname. Reject ambiguous
    // syntax before the Docker CLI can reinterpret a path as mount options.
    if value.contains(',') || value.contains('\\') {
        return Err(format!(
            "Docker bind path {} contains unsupported mount punctuation",
            path.display()
        ));
    }
    Ok(value.to_owned())
}

#[cfg(unix)]
struct ContainerNameCleanup {
    docker: DockerEnvironment,
    name: String,
}

#[cfg(unix)]
impl Drop for ContainerNameCleanup {
    fn drop(&mut self) {
        remove_container_bounded(&self.docker, &self.name, CLEANUP_TIMEOUT);
    }
}

#[cfg(unix)]
fn remove_container_bounded(docker: &DockerEnvironment, name: &str, timeout: Duration) {
    let mut command = docker.command();
    command.args(["rm", "--force", name]);
    discard_command_until(&mut command, Instant::now() + timeout);
}

#[cfg(unix)]
struct ContainerCleanup {
    name: String,
    volumes: SandboxVolumes,
    docker: DockerEnvironment,
}

#[cfg(unix)]
impl Drop for ContainerCleanup {
    fn drop(&mut self) {
        remove_container_bounded(&self.docker, &self.name, CLEANUP_TIMEOUT);
        if let Some(output) = &self.volumes.output {
            remove_volume_bounded(&self.docker, output, CLEANUP_TIMEOUT);
        }
        remove_volume_bounded(&self.docker, &self.volumes.logs, CLEANUP_TIMEOUT);
    }
}

#[cfg(unix)]
fn safe_archive_path(path: &Path) -> Result<Option<PathBuf>, String> {
    if path.as_os_str().as_bytes().len() > ARCHIVE_PATH_LIMIT {
        return Err(format!(
            "candidate archive path exceeds {ARCHIVE_PATH_LIMIT} bytes"
        ));
    }
    if path.as_os_str().as_bytes().contains(&0) {
        return Err("candidate archive path contains a NUL byte".to_owned());
    }
    let mut safe = PathBuf::new();
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| "candidate archive path depth overflow".to_owned())?;
                if depth > ARCHIVE_PATH_COMPONENT_LIMIT {
                    return Err(format!(
                        "candidate archive path exceeds {ARCHIVE_PATH_COMPONENT_LIMIT} components"
                    ));
                }
                safe.push(part);
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "Docker returned an unsafe archive path {}",
                    path.display()
                ));
            }
        }
    }
    if safe.as_os_str().is_empty() {
        Ok(None)
    } else {
        Ok(Some(safe))
    }
}

#[cfg(unix)]
fn validate_symlink_target(path: &Path, target: &Path) -> Result<(), String> {
    if target.as_os_str().is_empty() {
        return Err(format!(
            "candidate output symlink {} has an empty target",
            path.display()
        ));
    }
    if target.as_os_str().as_bytes().len() > ARCHIVE_PATH_LIMIT {
        return Err(format!(
            "candidate output symlink {} target exceeds {ARCHIVE_PATH_LIMIT} bytes",
            path.display()
        ));
    }
    if target.as_os_str().as_bytes().contains(&0) {
        return Err(format!(
            "candidate output symlink {} target contains a NUL byte",
            path.display()
        ));
    }
    let mut depth = 0usize;
    if let Some(parent) = path.parent() {
        for component in parent.components() {
            if let Component::Normal(_) = component {
                depth = depth.checked_add(1).ok_or_else(|| {
                    format!(
                        "candidate output symlink {} path depth overflow",
                        path.display()
                    )
                })?;
            }
        }
    }
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(_) => {
                depth = depth.checked_add(1).ok_or_else(|| {
                    format!(
                        "candidate output symlink {} target depth overflow",
                        path.display()
                    )
                })?;
            }
            Component::ParentDir => {
                if depth == 0 {
                    return Err(format!(
                        "candidate output symlink {} escapes its output root",
                        path.display()
                    ));
                }
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "candidate output symlink {} has an absolute target",
                    path.display()
                ));
            }
        }
        if depth > ARCHIVE_PATH_COMPONENT_LIMIT {
            return Err(format!(
                "candidate output symlink {} exceeds {ARCHIVE_PATH_COMPONENT_LIMIT} components",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
#[derive(Default)]
struct PendingArchiveMetadata {
    path: Option<Vec<u8>>,
    linkpath: Option<Vec<u8>>,
    size: Option<u64>,
    gnu_path: Option<Vec<u8>>,
    gnu_linkpath: Option<Vec<u8>>,
    pax: bool,
}

#[cfg(unix)]
#[derive(Default)]
struct ArchivePreflightState {
    members: usize,
    metadata_bytes: usize,
    content_bytes: usize,
    pending: PendingArchiveMetadata,
}

#[cfg(unix)]
fn preflight_archive(bytes: &[u8], content_limit: usize) -> Result<(), String> {
    if bytes.len() > OUTPUT_ARCHIVE_LIMIT {
        return Err(format!(
            "candidate archive exceeds {OUTPUT_ARCHIVE_LIMIT} bytes"
        ));
    }
    if !bytes.len().is_multiple_of(ARCHIVE_BLOCK_SIZE) {
        return Err("candidate archive is not block aligned".to_owned());
    }

    let mut offset = 0usize;
    let mut state = ArchivePreflightState::default();

    while offset < bytes.len() {
        let header_end = archive_header_end(offset, bytes.len())?;
        let block = &bytes[offset..header_end];
        if block.iter().all(|byte| *byte == 0) {
            validate_archive_terminator(bytes, header_end, &state.pending)?;
            return Ok(());
        }

        offset = preflight_archive_entry(bytes, offset, header_end, content_limit, &mut state)?;
    }

    Err("candidate archive is missing its terminator".to_owned())
}

#[cfg(unix)]
fn archive_header_end(offset: usize, archive_len: usize) -> Result<usize, String> {
    let header_end = offset
        .checked_add(ARCHIVE_BLOCK_SIZE)
        .ok_or_else(|| "candidate archive header offset overflow".to_owned())?;
    if header_end > archive_len {
        return Err("candidate archive ends in a partial header".to_owned());
    }
    Ok(header_end)
}

#[cfg(unix)]
fn validate_archive_terminator(
    bytes: &[u8],
    header_end: usize,
    pending: &PendingArchiveMetadata,
) -> Result<(), String> {
    let second_end = header_end
        .checked_add(ARCHIVE_BLOCK_SIZE)
        .ok_or_else(|| "candidate archive terminator offset overflow".to_owned())?;
    if second_end > bytes.len() || bytes[header_end..second_end].iter().any(|byte| *byte != 0) {
        return Err("candidate archive has an incomplete terminator".to_owned());
    }
    if bytes[second_end..].iter().any(|byte| *byte != 0) {
        return Err("candidate archive contains data after its terminator".to_owned());
    }
    if pending.pax || pending.gnu_path.is_some() || pending.gnu_linkpath.is_some() {
        return Err("candidate archive metadata has no following entry".to_owned());
    }
    Ok(())
}

#[cfg(unix)]
fn preflight_archive_entry(
    bytes: &[u8],
    offset: usize,
    header_end: usize,
    content_limit: usize,
    state: &mut ArchivePreflightState,
) -> Result<usize, String> {
    state.members = state
        .members
        .checked_add(1)
        .ok_or_else(|| "candidate archive member count overflow".to_owned())?;
    if state.members > ARCHIVE_MEMBER_LIMIT {
        return Err(format!(
            "candidate archive exceeds {ARCHIVE_MEMBER_LIMIT} members"
        ));
    }

    let block = &bytes[offset..header_end];
    validate_archive_header(block, offset)?;
    let mut header = TarHeader::new_old();
    header.as_mut_bytes().copy_from_slice(block);
    let raw_size = header
        .entry_size()
        .map_err(|error| format!("candidate archive has invalid entry size: {error}"))?;
    let entry_type = header.entry_type();
    let raw_data_start = header_end;
    let recognized_extension = header.as_gnu().is_some() || header.as_ustar().is_some();

    if entry_type == EntryType::XGlobalHeader {
        return Err("candidate archive contains unsupported global PAX metadata".to_owned());
    }
    if entry_type == EntryType::GNUSparse {
        return Err("candidate archive contains unsupported sparse data".to_owned());
    }

    match entry_type {
        EntryType::XHeader => {
            if !recognized_extension {
                return Err("candidate archive has an unrecognized PAX header encoding".to_owned());
            }
            return preflight_pax_header(bytes, raw_data_start, raw_size, state);
        }
        EntryType::GNULongName | EntryType::GNULongLink => {
            if !recognized_extension {
                return Err("candidate archive has an unrecognized GNU header encoding".to_owned());
            }
            return preflight_gnu_header(bytes, raw_data_start, raw_size, entry_type, state);
        }
        EntryType::Regular | EntryType::Directory | EntryType::Symlink => {}
        _ => {
            return Err(format!(
                "candidate archive contains unsupported entry type {entry_type:?}"
            ));
        }
    }

    let header_path = header.path_bytes();
    let path = state
        .pending
        .gnu_path
        .as_deref()
        .or(state.pending.path.as_deref())
        .unwrap_or(header_path.as_ref());
    validate_archive_path_bytes(path, "path")?;
    if entry_type == EntryType::Symlink {
        let header_linkpath = header.link_name_bytes();
        let linkpath = state
            .pending
            .gnu_linkpath
            .as_deref()
            .or(state.pending.linkpath.as_deref())
            .or(header_linkpath.as_deref())
            .ok_or_else(|| "candidate archive symlink has no target".to_owned())?;
        validate_archive_path_bytes(linkpath, "link target")?;
    }

    let effective_size = state.pending.size.take().unwrap_or(raw_size);
    if effective_size > content_limit as u64 {
        return Err(format!(
            "candidate archive entry exceeds {content_limit} bytes"
        ));
    }
    let effective_end = archive_data_end(raw_data_start, effective_size, bytes.len())?;
    if entry_type == EntryType::Regular {
        account_archive_content(&mut state.content_bytes, effective_size, content_limit)?;
    }
    state.pending = PendingArchiveMetadata::default();
    Ok(effective_end)
}

#[cfg(unix)]
fn preflight_pax_header(
    bytes: &[u8],
    raw_data_start: usize,
    raw_size: u64,
    state: &mut ArchivePreflightState,
) -> Result<usize, String> {
    let raw_data_end = archive_data_end(raw_data_start, raw_size, bytes.len())?;
    let raw_payload_end =
        raw_data_start
            .checked_add(usize::try_from(raw_size).map_err(|_| {
                "candidate archive PAX payload exceeds platform capacity".to_owned()
            })?)
            .ok_or_else(|| "candidate archive payload offset overflow".to_owned())?;
    let payload = bytes
        .get(raw_data_start..raw_payload_end)
        .ok_or_else(|| "candidate archive PAX payload is truncated".to_owned())?;
    account_archive_metadata(&mut state.metadata_bytes, payload.len())?;
    if state.pending.pax {
        return Err("candidate archive repeats a local PAX header".to_owned());
    }
    state.pending.pax = true;
    parse_pax_metadata(payload, &mut state.pending)?;
    Ok(raw_data_end)
}

#[cfg(unix)]
fn preflight_gnu_header(
    bytes: &[u8],
    raw_data_start: usize,
    raw_size: u64,
    entry_type: EntryType,
    state: &mut ArchivePreflightState,
) -> Result<usize, String> {
    let raw_data_end = archive_data_end(raw_data_start, raw_size, bytes.len())?;
    let raw_payload_end =
        raw_data_start
            .checked_add(usize::try_from(raw_size).map_err(|_| {
                "candidate archive GNU metadata exceeds platform capacity".to_owned()
            })?)
            .ok_or_else(|| "candidate archive payload offset overflow".to_owned())?;
    let payload = bytes
        .get(raw_data_start..raw_payload_end)
        .ok_or_else(|| "candidate archive GNU metadata is truncated".to_owned())?;
    account_archive_metadata(&mut state.metadata_bytes, payload.len())?;
    let value = parse_gnu_metadata(payload)?;
    if entry_type == EntryType::GNULongName {
        if state.pending.gnu_path.is_some() {
            return Err("candidate archive repeats a GNU long-name header".to_owned());
        }
        state.pending.gnu_path = Some(value);
    } else {
        if state.pending.gnu_linkpath.is_some() {
            return Err("candidate archive repeats a GNU long-link header".to_owned());
        }
        state.pending.gnu_linkpath = Some(value);
    }
    Ok(raw_data_end)
}

#[cfg(unix)]
fn account_archive_metadata(total: &mut usize, extension_size: usize) -> Result<(), String> {
    if extension_size > ARCHIVE_METADATA_EXTENSION_LIMIT {
        return Err(format!(
            "candidate archive extension metadata exceeds {ARCHIVE_METADATA_EXTENSION_LIMIT} bytes"
        ));
    }
    *total = total
        .checked_add(extension_size)
        .ok_or_else(|| "candidate archive metadata size overflow".to_owned())?;
    if *total > ARCHIVE_METADATA_TOTAL_LIMIT {
        return Err(format!(
            "candidate archive metadata exceeds {ARCHIVE_METADATA_TOTAL_LIMIT} bytes"
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn account_archive_content(total: &mut usize, size: u64, limit: usize) -> Result<usize, String> {
    let size = usize::try_from(size)
        .map_err(|_| "candidate archive content exceeds platform capacity".to_owned())?;
    *total = total
        .checked_add(size)
        .ok_or_else(|| "candidate archive cumulative content overflow".to_owned())?;
    if *total > limit {
        return Err(format!(
            "candidate archive cumulative content exceeds {limit} bytes"
        ));
    }
    Ok(size)
}

#[cfg(unix)]
fn validate_archive_header(block: &[u8], offset: usize) -> Result<(), String> {
    let mut header = TarHeader::new_old();
    header.as_mut_bytes().copy_from_slice(block);
    let expected = header.cksum().map_err(|error| {
        format!("candidate archive header at {offset} has invalid checksum: {error}")
    })?;
    let actual = block[..148]
        .iter()
        .chain(&block[156..])
        .fold(0u32, |sum, byte| sum.saturating_add(u32::from(*byte)))
        .saturating_add(8 * u32::from(b' '));
    if actual != expected {
        return Err(format!(
            "candidate archive header at {offset} has checksum {expected}, expected {actual}"
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn archive_data_end(start: usize, size: u64, archive_len: usize) -> Result<usize, String> {
    let rounded = size
        .checked_add((ARCHIVE_BLOCK_SIZE - 1) as u64)
        .ok_or_else(|| "candidate archive entry size overflow".to_owned())?
        & !((ARCHIVE_BLOCK_SIZE - 1) as u64);
    let rounded = usize::try_from(rounded)
        .map_err(|_| "candidate archive entry size exceeds platform capacity".to_owned())?;
    let end = start
        .checked_add(rounded)
        .ok_or_else(|| "candidate archive entry end offset overflow".to_owned())?;
    if end > archive_len {
        return Err("candidate archive entry is truncated".to_owned());
    }
    Ok(end)
}

#[cfg(unix)]
fn validate_archive_path_bytes(bytes: &[u8], label: &str) -> Result<(), String> {
    if bytes.len() > ARCHIVE_PATH_LIMIT {
        return Err(format!(
            "candidate archive {label} exceeds {ARCHIVE_PATH_LIMIT} bytes"
        ));
    }
    if bytes.contains(&0) {
        return Err(format!("candidate archive {label} contains a NUL byte"));
    }
    Ok(())
}

#[cfg(unix)]
fn parse_gnu_metadata(payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.len() > ARCHIVE_PATH_LIMIT + 1 {
        return Err(format!(
            "candidate archive GNU metadata exceeds {} bytes",
            ARCHIVE_PATH_LIMIT + 1
        ));
    }
    let end = match payload.iter().position(|byte| *byte == 0) {
        None => payload.len(),
        Some(end) if end + 1 == payload.len() => end,
        Some(_) => {
            return Err(
                "candidate archive GNU metadata has repeated or interior NUL bytes".to_owned(),
            );
        }
    };
    let value = payload[..end].to_vec();
    validate_archive_path_bytes(&value, "GNU metadata")?;
    Ok(value)
}

#[cfg(unix)]
fn parse_pax_metadata(payload: &[u8], pending: &mut PendingArchiveMetadata) -> Result<(), String> {
    let mut offset = 0usize;
    while offset < payload.len() {
        let remaining = &payload[offset..];
        let space = remaining
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or_else(|| "candidate archive PAX record has no length separator".to_owned())?;
        let record_len = usize::try_from(parse_decimal(&remaining[..space], "PAX record length")?)
            .map_err(|_| {
                "candidate archive PAX record length exceeds platform capacity".to_owned()
            })?;
        if record_len < space + 3 || record_len > remaining.len() {
            return Err("candidate archive PAX record length is invalid".to_owned());
        }
        let record = &remaining[..record_len];
        if record[record_len - 1] != b'\n' {
            return Err("candidate archive PAX record is missing its newline".to_owned());
        }
        let body = &record[space + 1..record_len - 1];
        let equals = body
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or_else(|| "candidate archive PAX record has no key".to_owned())?;
        let key = &body[..equals];
        let value = &body[equals + 1..];
        if key.is_empty()
            || key.contains(&0)
            || value.contains(&0)
            || key.contains(&b'\n')
            || value.contains(&b'\n')
        {
            return Err(
                "candidate archive PAX record contains an interior newline or NUL".to_owned(),
            );
        }
        if std::str::from_utf8(key).is_err() || std::str::from_utf8(value).is_err() {
            return Err("candidate archive PAX record is not UTF-8".to_owned());
        }
        if key.starts_with(b"GNU.sparse.") {
            return Err("candidate archive contains unsupported PAX sparse metadata".to_owned());
        }
        match key {
            b"path" => {
                if pending.path.is_some() {
                    return Err("candidate archive repeats a PAX path".to_owned());
                }
                validate_archive_path_bytes(value, "PAX path")?;
                pending.path = Some(value.to_vec());
            }
            b"linkpath" => {
                if pending.linkpath.is_some() {
                    return Err("candidate archive repeats a PAX link target".to_owned());
                }
                validate_archive_path_bytes(value, "PAX link target")?;
                pending.linkpath = Some(value.to_vec());
            }
            b"size" => {
                if pending.size.is_some() {
                    return Err("candidate archive repeats a PAX size".to_owned());
                }
                pending.size = Some(parse_decimal(value, "PAX size")?);
            }
            _ => {}
        }
        offset = offset
            .checked_add(record_len)
            .ok_or_else(|| "candidate archive PAX offset overflow".to_owned())?;
    }
    Ok(())
}

#[cfg(unix)]
fn parse_decimal(bytes: &[u8], label: &str) -> Result<u64, String> {
    if bytes.is_empty() || bytes.iter().any(|byte| !byte.is_ascii_digit()) {
        return Err(format!("candidate archive has invalid {label}"));
    }
    bytes.iter().try_fold(0u64, |value, byte| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(*byte - b'0')))
            .ok_or_else(|| format!("candidate archive {label} overflows"))
    })
}

#[cfg(unix)]
fn single_file_archive(bytes: &[u8], expected: &str, limit: usize) -> Result<Vec<u8>, String> {
    preflight_archive(bytes, limit)?;
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut result = None;
    let entries = archive
        .entries()
        .map_err(|error| format!("read candidate log archive: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read candidate log entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("read candidate log path: {error}"))?
            .into_owned();
        let Some(path) = safe_archive_path(&path)? else {
            continue;
        };
        if path != Path::new(expected) || !entry.header().entry_type().is_file() || result.is_some()
        {
            return Err("Docker returned an unexpected candidate log archive".to_owned());
        }
        let size = entry.size();
        if size > limit as u64 {
            return Err(format!("candidate log exceeds {limit} bytes"));
        }
        let capacity = usize::try_from(size)
            .map_err(|_| format!("candidate log size {size} exceeds platform capacity"))?;
        let mut content = Vec::with_capacity(capacity);
        entry
            .take(
                u64::try_from(capacity)
                    .map_err(|_| format!("candidate log size {size} exceeds platform capacity"))?
                    .checked_add(1)
                    .ok_or_else(|| format!("candidate log size {size} overflows"))?,
            )
            .read_to_end(&mut content)
            .map_err(|error| format!("read candidate log content: {error}"))?;
        if content.len() != capacity {
            return Err(format!(
                "candidate log length {} does not match declared size {capacity}",
                content.len()
            ));
        }
        result = Some(content);
    }
    result.ok_or_else(|| format!("Docker returned no `{expected}` candidate log"))
}

#[cfg(unix)]
enum RenderEntry {
    Directory,
    File { executable: bool, bytes: Vec<u8> },
    Symlink { target: PathBuf },
}

#[cfg(unix)]
fn extract_render_archive(bytes: &[u8], root: &Path) -> Result<(), String> {
    let entries = parse_render_archive(bytes)?;
    validate_render_entries(&entries)?;
    write_render_entries(&entries, root)
}

#[cfg(unix)]
fn parse_render_archive(bytes: &[u8]) -> Result<BTreeMap<PathBuf, RenderEntry>, String> {
    preflight_archive(bytes, OUTPUT_CONTENT_LIMIT)?;
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut entries = BTreeMap::new();
    let mut content_bytes = 0usize;
    let parsed = archive
        .entries()
        .map_err(|error| format!("read candidate render archive: {error}"))?;
    for entry in parsed {
        let entry = entry.map_err(|error| format!("read candidate render entry: {error}"))?;
        let raw_path = entry
            .path()
            .map_err(|error| format!("read candidate render path: {error}"))?
            .into_owned();
        let Some(path) = safe_archive_path(&raw_path)? else {
            continue;
        };
        let _ = safe_archive_path(&path)?;
        let kind = entry.header().entry_type();
        let record = if kind.is_dir() {
            RenderEntry::Directory
        } else if kind.is_file() {
            let mode = entry
                .header()
                .mode()
                .map_err(|error| format!("read candidate render mode: {error}"))?;
            let size = entry.size();
            if size > OUTPUT_CONTENT_LIMIT as u64 {
                return Err(format!(
                    "candidate file {} exceeds the output limit",
                    path.display()
                ));
            }
            let capacity = account_archive_content(&mut content_bytes, size, OUTPUT_CONTENT_LIMIT)
                .map_err(|error| format!("candidate file {}: {error}", path.display()))?;
            let mut contents = Vec::with_capacity(capacity);
            entry
                .take(
                    u64::try_from(capacity)
                        .map_err(|_| {
                            format!(
                                "candidate file {} exceeds platform capacity",
                                path.display()
                            )
                        })?
                        .checked_add(1)
                        .ok_or_else(|| {
                            format!("candidate file {} size overflows", path.display())
                        })?,
                )
                .read_to_end(&mut contents)
                .map_err(|error| format!("read candidate file {}: {error}", path.display()))?;
            if contents.len() != capacity {
                return Err(format!(
                    "candidate file {} length {} does not match declared size {capacity}",
                    path.display(),
                    contents.len()
                ));
            }
            RenderEntry::File {
                executable: mode & 0o111 != 0,
                bytes: contents,
            }
        } else if kind.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|error| format!("read candidate symlink target: {error}"))?
                .ok_or_else(|| "candidate output has a symlink without a target".to_owned())?
                .into_owned();
            validate_symlink_target(&path, &target)?;
            RenderEntry::Symlink { target }
        } else {
            return Err(format!(
                "candidate output contains unsupported entry type at {}",
                path.display()
            ));
        };
        if entries.insert(path.clone(), record).is_some() {
            return Err(format!(
                "candidate output repeats archive path {}",
                path.display()
            ));
        }
    }
    Ok(entries)
}

#[cfg(unix)]
fn validate_render_entries(entries: &BTreeMap<PathBuf, RenderEntry>) -> Result<(), String> {
    for path in entries.keys() {
        let _ = safe_archive_path(path)?;
        let mut parent = path.parent();
        while let Some(directory) = parent {
            if directory.as_os_str().is_empty() {
                break;
            }
            if let Some(record) = entries.get(directory)
                && !matches!(record, RenderEntry::Directory)
            {
                return Err(format!(
                    "candidate output places {} below non-directory {}",
                    path.display(),
                    directory.display()
                ));
            }
            parent = directory.parent();
        }
        if let Some(RenderEntry::Symlink { target }) = entries.get(path) {
            validate_symlink_target(path, target)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_render_entries(
    entries: &BTreeMap<PathBuf, RenderEntry>,
    root: &Path,
) -> Result<(), String> {
    for (path, record) in entries {
        if matches!(record, RenderEntry::Directory) {
            fs::create_dir_all(root.join(path)).map_err(|error| {
                format!(
                    "create candidate output directory {}: {error}",
                    path.display()
                )
            })?;
        }
    }
    for (path, record) in entries {
        if let RenderEntry::File { executable, bytes } = record {
            let destination = root.join(path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "create candidate output parent {}: {error}",
                        parent.display()
                    )
                })?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(|error| format!("create candidate output {}: {error}", path.display()))?;
            file.write_all(bytes)
                .map_err(|error| format!("copy candidate output {}: {error}", path.display()))?;
            file.set_permissions(Permissions::from_mode(if *executable {
                0o755
            } else {
                0o644
            }))
            .map_err(|error| format!("set candidate output mode {}: {error}", path.display()))?;
        }
    }
    for (path, record) in entries {
        if let RenderEntry::Symlink { target } = record {
            let destination = root.join(path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "create candidate symlink parent {}: {error}",
                        parent.display()
                    )
                })?;
            }
            std::os::unix::fs::symlink(target, &destination)
                .map_err(|error| format!("create candidate symlink {}: {error}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #![expect(
        clippy::panic,
        reason = "test setup failures must identify the failing fixture"
    )]

    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::net::UnixListener;

    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn must_fail<T, E>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => panic!("{context}: expected an error"),
            Err(error) => error,
        }
    }

    fn must_some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("{context}: expected a value"),
        }
    }

    fn header(name: &str, kind: EntryType, size: u64) -> [u8; ARCHIVE_BLOCK_SIZE] {
        let mut header = TarHeader::new_ustar();
        header
            .set_path(name)
            .unwrap_or_else(|error| panic!("set test archive path {name}: {error}"));
        header.set_mode(0o644);
        header.set_entry_type(kind);
        header.set_size(size);
        header.set_cksum();
        *header.as_bytes()
    }

    fn header_with_link(
        name: &str,
        kind: EntryType,
        size: u64,
        link: &str,
    ) -> [u8; ARCHIVE_BLOCK_SIZE] {
        let mut tar_header = TarHeader::new_old();
        tar_header
            .as_mut_bytes()
            .copy_from_slice(&header(name, kind, size));
        tar_header
            .set_link_name(link)
            .unwrap_or_else(|error| panic!("set test archive link {link}: {error}"));
        tar_header.set_cksum();
        *tar_header.as_bytes()
    }

    fn append_entry(archive: &mut Vec<u8>, header: &[u8; ARCHIVE_BLOCK_SIZE], payload: &[u8]) {
        archive.extend_from_slice(header);
        archive.extend_from_slice(payload);
        let padded = (payload.len() + ARCHIVE_BLOCK_SIZE - 1) & !(ARCHIVE_BLOCK_SIZE - 1);
        archive.resize(archive.len() + padded - payload.len(), 0);
    }

    fn finish_archive(archive: &mut Vec<u8>) {
        archive.extend_from_slice(&[0; ARCHIVE_BLOCK_SIZE * 2]);
    }

    fn pax_record(key: &str, value: &str) -> Vec<u8> {
        let body_len = key.len() + value.len() + 3;
        let mut length = body_len;
        loop {
            let next = length.to_string().len() + 1 + key.len() + 1 + value.len() + 1;
            if next == length {
                return format!("{length} {key}={value}\n").into_bytes();
            }
            length = next;
        }
    }

    fn append_pax(archive: &mut Vec<u8>, records: &[(&str, &str)]) {
        let payload = records
            .iter()
            .fold(Vec::new(), |mut payload, (key, value)| {
                payload.extend_from_slice(&pax_record(key, value));
                payload
            });
        append_entry(
            archive,
            &header("PaxHeaders/entry", EntryType::XHeader, payload.len() as u64),
            &payload,
        );
    }

    #[test]
    fn archive_preflight_uses_pax_effective_size_for_read_and_cursor() {
        let mut archive = Vec::new();
        append_pax(&mut archive, &[("size", "5")]);
        append_entry(
            &mut archive,
            &header("render.txt", EntryType::Regular, 3),
            b"hello",
        );
        finish_archive(&mut archive);

        let entries = must(
            parse_render_archive(&archive),
            "effective PAX size is valid",
        );
        let RenderEntry::File { bytes, .. } = must_some(
            entries.get(Path::new("render.txt")),
            "effective PAX render entry",
        ) else {
            panic!("expected regular file entry")
        };
        assert_eq!(bytes, b"hello");

        let mut understated = Vec::new();
        append_pax(&mut understated, &[("size", "3")]);
        append_entry(
            &mut understated,
            &header("short.txt", EntryType::Regular, 5),
            b"abc",
        );
        finish_archive(&mut understated);
        let entries = must(
            parse_render_archive(&understated),
            "understated PAX size is valid",
        );
        let RenderEntry::File { bytes, .. } = must_some(
            entries.get(Path::new("short.txt")),
            "understated PAX render entry",
        ) else {
            panic!("expected regular file entry")
        };
        assert_eq!(bytes, b"abc");
    }

    #[test]
    fn archive_preflight_hides_headers_inside_pax_effective_payload() {
        let mut hidden = vec![0; ARCHIVE_BLOCK_SIZE * 2];
        let hidden_header = header_with_link("hidden", EntryType::Symlink, 0, "/outside");
        hidden[ARCHIVE_BLOCK_SIZE..ARCHIVE_BLOCK_SIZE * 2].copy_from_slice(&hidden_header);

        let mut archive = Vec::new();
        append_pax(&mut archive, &[("size", "1024")]);
        append_entry(
            &mut archive,
            &header("visible", EntryType::Regular, 1),
            &hidden,
        );
        finish_archive(&mut archive);

        let entries = must(
            parse_render_archive(&archive),
            "hidden header remains payload",
        );
        assert_eq!(entries.len(), 1);
        assert!(entries.contains_key(Path::new("visible")));
    }

    #[test]
    fn archive_preflight_rejects_truncation_and_strict_pax_records() {
        let mut truncated = Vec::new();
        append_entry(
            &mut truncated,
            &header(
                "truncated",
                EntryType::Regular,
                ARCHIVE_BLOCK_SIZE as u64 * 2,
            ),
            &[b'x'; ARCHIVE_BLOCK_SIZE],
        );
        finish_archive(&mut truncated);
        assert!(must_fail(
            preflight_archive(&truncated, OUTPUT_CONTENT_LIMIT),
            "truncated payload"
        )
        .contains("terminator"));

        let mut interior_newline = Vec::new();
        append_pax(&mut interior_newline, &[("path", "has\nnewline")]);
        append_entry(
            &mut interior_newline,
            &header("entry", EntryType::Regular, 0),
            &[],
        );
        finish_archive(&mut interior_newline);
        assert!(must_fail(
            preflight_archive(&interior_newline, OUTPUT_CONTENT_LIMIT),
            "interior newline"
        )
        .contains("newline"));

        let mut duplicate = Vec::new();
        append_pax(&mut duplicate, &[("size", "1"), ("size", "1")]);
        append_entry(
            &mut duplicate,
            &header("entry", EntryType::Regular, 1),
            b"x",
        );
        finish_archive(&mut duplicate);
        assert!(must_fail(
            preflight_archive(&duplicate, OUTPUT_CONTENT_LIMIT),
            "duplicate PAX size"
        )
        .contains("repeats"));

        let mut trailing = Vec::new();
        append_entry(
            &mut trailing,
            &header("@LongLink", EntryType::GNULongName, 8),
            b"name\0tail",
        );
        finish_archive(&mut trailing);
        assert!(must_fail(
            preflight_archive(&trailing, OUTPUT_CONTENT_LIMIT),
            "GNU trailing data"
        )
        .contains("NUL"));

        let mut repeated_nul = Vec::new();
        append_entry(
            &mut repeated_nul,
            &header("@LongLink", EntryType::GNULongName, 6),
            b"name\0\0",
        );
        finish_archive(&mut repeated_nul);
        assert!(must_fail(
            preflight_archive(&repeated_nul, OUTPUT_CONTENT_LIMIT),
            "repeated GNU terminator"
        )
        .contains("repeated"));

        assert_eq!(
            must(
                parse_gnu_metadata(b"unterminated"),
                "unterminated GNU metadata"
            ),
            b"unterminated"
        );
        assert_eq!(
            must(
                parse_gnu_metadata(b"terminated\0"),
                "terminated GNU metadata"
            ),
            b"terminated"
        );
    }

    #[test]
    fn archive_metadata_budgets_have_per_extension_and_total_caps() {
        let mut total = 0;
        must(
            account_archive_metadata(&mut total, ARCHIVE_METADATA_EXTENSION_LIMIT),
            "per-extension boundary",
        );
        assert!(must_fail(
            account_archive_metadata(&mut total, ARCHIVE_METADATA_EXTENSION_LIMIT + 1),
            "per-extension cap"
        )
        .contains("extension metadata"));

        let mut total = 0;
        for _ in 0..(ARCHIVE_METADATA_TOTAL_LIMIT / ARCHIVE_METADATA_EXTENSION_LIMIT) {
            must(
                account_archive_metadata(&mut total, ARCHIVE_METADATA_EXTENSION_LIMIT),
                "aggregate boundary",
            );
        }
        assert_eq!(total, ARCHIVE_METADATA_TOTAL_LIMIT);
        assert!(
            must_fail(account_archive_metadata(&mut total, 1), "aggregate cap")
                .contains("metadata exceeds")
        );
    }

    #[test]
    fn archive_preflight_aligns_gnu_names_over_pax_names() {
        let long_name = "gnu-name.txt";
        let long_link = "gnu-target.txt";
        let mut archive = Vec::new();
        append_pax(
            &mut archive,
            &[("path", "pax-name.txt"), ("linkpath", "pax-target.txt")],
        );
        let mut gnu_name = long_name.as_bytes().to_vec();
        gnu_name.push(0);
        append_entry(
            &mut archive,
            &header("@LongLink", EntryType::GNULongName, gnu_name.len() as u64),
            &gnu_name,
        );
        let mut gnu_link = long_link.as_bytes().to_vec();
        gnu_link.push(0);
        append_entry(
            &mut archive,
            &header("@LongLink", EntryType::GNULongLink, gnu_link.len() as u64),
            &gnu_link,
        );
        append_entry(
            &mut archive,
            &header_with_link("short", EntryType::Symlink, 0, "short-target"),
            &[],
        );
        finish_archive(&mut archive);

        let entries = must(parse_render_archive(&archive), "GNU metadata combination");
        let RenderEntry::Symlink { target } =
            must_some(entries.get(Path::new(long_name)), "GNU long-name entry")
        else {
            panic!("expected GNU long-name symlink")
        };
        assert_eq!(target, Path::new(long_link));
    }

    #[test]
    fn render_validation_rejects_symlink_escape_children_and_deep_paths() {
        let mut escape = BTreeMap::new();
        escape.insert(
            PathBuf::from("link"),
            RenderEntry::Symlink {
                target: PathBuf::from("../outside"),
            },
        );
        assert!(must_fail(validate_render_entries(&escape), "symlink escape").contains("escapes"));

        let mut child = BTreeMap::new();
        child.insert(
            PathBuf::from("link"),
            RenderEntry::Symlink {
                target: PathBuf::from("inside"),
            },
        );
        child.insert(
            PathBuf::from("link/file"),
            RenderEntry::File {
                executable: false,
                bytes: Vec::new(),
            },
        );
        assert!(
            must_fail(validate_render_entries(&child), "child through symlink")
                .contains("non-directory")
        );

        let deep = (0..=ARCHIVE_PATH_COMPONENT_LIMIT)
            .map(|index| format!("component-{index}"))
            .collect::<PathBuf>();
        assert!(must_fail(safe_archive_path(&deep), "deep archive path").contains("components"));

        let mut safe = BTreeMap::new();
        safe.insert(
            PathBuf::from("dir/link"),
            RenderEntry::Symlink {
                target: PathBuf::from("../target"),
            },
        );
        assert!(validate_render_entries(&safe).is_ok());
    }

    #[test]
    fn archive_preflight_rejects_pax_size_over_limit() {
        let mut archive = Vec::new();
        append_pax(
            &mut archive,
            &[("size", &(OUTPUT_CONTENT_LIMIT as u64 + 1).to_string())],
        );
        append_entry(&mut archive, &header("large", EntryType::Regular, 0), &[]);
        finish_archive(&mut archive);
        let error = must_fail(
            preflight_archive(&archive, OUTPUT_CONTENT_LIMIT),
            "oversize",
        );
        assert!(error.contains("exceeds"), "unexpected error: {error}");
    }

    #[test]
    fn archive_preflight_rejects_oversized_metadata_and_paths() {
        let oversized = "x".repeat(ARCHIVE_METADATA_EXTENSION_LIMIT + 1);
        let mut metadata_archive = Vec::new();
        append_entry(
            &mut metadata_archive,
            &header(
                "PaxHeaders/entry",
                EntryType::XHeader,
                oversized.len() as u64,
            ),
            oversized.as_bytes(),
        );
        finish_archive(&mut metadata_archive);
        assert!(must_fail(
            preflight_archive(&metadata_archive, OUTPUT_CONTENT_LIMIT),
            "oversized PAX metadata"
        )
        .contains("metadata"));

        let long_path = "p".repeat(ARCHIVE_PATH_LIMIT + 1);
        let mut path_archive = Vec::new();
        append_pax(&mut path_archive, &[("path", &long_path)]);
        append_entry(
            &mut path_archive,
            &header("entry", EntryType::Regular, 0),
            &[],
        );
        finish_archive(&mut path_archive);
        assert!(must_fail(
            preflight_archive(&path_archive, OUTPUT_CONTENT_LIMIT),
            "oversized PAX path"
        )
        .contains("path"));

        let long_gnu = vec![b'g'; ARCHIVE_PATH_LIMIT + 2];
        let mut gnu_archive = Vec::new();
        append_entry(
            &mut gnu_archive,
            &header(
                "././@LongLink",
                EntryType::GNULongName,
                long_gnu.len() as u64,
            ),
            &long_gnu,
        );
        finish_archive(&mut gnu_archive);
        assert!(must_fail(
            preflight_archive(&gnu_archive, OUTPUT_CONTENT_LIMIT),
            "oversized GNU metadata"
        )
        .contains("GNU metadata"));
    }

    #[test]
    fn archive_preflight_rejects_member_and_cumulative_limits() {
        let mut members = Vec::new();
        for index in 0..=ARCHIVE_MEMBER_LIMIT {
            append_entry(
                &mut members,
                &header(&format!("entry-{index}"), EntryType::Directory, 0),
                &[],
            );
        }
        finish_archive(&mut members);
        assert!(must_fail(
            preflight_archive(&members, OUTPUT_CONTENT_LIMIT),
            "member limit"
        )
        .contains("members"));

        let mut cumulative = Vec::new();
        append_entry(
            &mut cumulative,
            &header("one", EntryType::Regular, 4),
            b"1111",
        );
        append_entry(
            &mut cumulative,
            &header("two", EntryType::Regular, 4),
            b"2222",
        );
        finish_archive(&mut cumulative);
        assert!(must_fail(
            preflight_archive(&cumulative, 7),
            "cumulative content limit"
        )
        .contains("cumulative"));
    }

    #[test]
    fn archive_preflight_rejects_bad_headers_terminators_and_sparse_forms() {
        let mut checksum = Vec::new();
        let mut checksum_header = header("checksum", EntryType::Regular, 0);
        checksum_header[0] ^= 1;
        append_entry(&mut checksum, &checksum_header, &[]);
        finish_archive(&mut checksum);
        assert!(must_fail(
            preflight_archive(&checksum, OUTPUT_CONTENT_LIMIT),
            "checksum"
        )
        .contains("checksum"));

        let mut terminator = Vec::new();
        append_entry(
            &mut terminator,
            &header("entry", EntryType::Regular, 0),
            &[],
        );
        terminator.extend_from_slice(&[0; ARCHIVE_BLOCK_SIZE]);
        assert!(must_fail(
            preflight_archive(&terminator, OUTPUT_CONTENT_LIMIT),
            "terminator"
        )
        .contains("terminator"));

        let mut sparse = Vec::new();
        append_entry(&mut sparse, &header("sparse", EntryType::GNUSparse, 0), &[]);
        finish_archive(&mut sparse);
        assert!(
            must_fail(preflight_archive(&sparse, OUTPUT_CONTENT_LIMIT), "sparse")
                .contains("sparse")
        );

        let mut global = Vec::new();
        append_entry(
            &mut global,
            &header("global", EntryType::XGlobalHeader, 0),
            &[],
        );
        finish_archive(&mut global);
        assert!(must_fail(
            preflight_archive(&global, OUTPUT_CONTENT_LIMIT),
            "global PAX"
        )
        .contains("global PAX"));
    }

    struct TestSocket {
        root: PathBuf,
        path: PathBuf,
        _listener: UnixListener,
    }

    impl TestSocket {
        fn new(label: &str) -> Self {
            // Bind under `/tmp` with a short stem: `TMPDIR` on macOS plus the
            // unique suffix overflows `SUN_LEN`, so a temp-dir socket path
            // cannot bind there.
            let root = PathBuf::from(format!("/tmp/vcnd-{label}-{}", crate::unique_suffix()));
            fs::create_dir(&root).unwrap_or_else(|error| {
                panic!(
                    "create test Docker socket directory {}: {error}",
                    root.display()
                )
            });
            let path = root.join("docker.sock");
            let listener = UnixListener::bind(&path).unwrap_or_else(|error| {
                panic!("create test Docker socket {}: {error}", path.display())
            });
            Self {
                root,
                path,
                _listener: listener,
            }
        }
    }

    impl Drop for TestSocket {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn platform_default_socket_order_matches_runner_policy() {
        let home = Path::new("/home/velnor-test");
        let runtime = Path::new("/run/user/1000");
        let candidates = default_socket_candidates(Some(home), Some(runtime));
        #[cfg(target_os = "macos")]
        assert_eq!(
            candidates,
            vec![
                home.join(".orbstack/run/docker.sock"),
                home.join(".docker/run/docker.sock"),
                PathBuf::from("/var/run/docker.sock"),
                PathBuf::from("/run/docker.sock"),
            ]
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/var/run/docker.sock"),
                PathBuf::from("/run/docker.sock"),
                runtime.join("docker.sock"),
                home.join(".docker/run/docker.sock"),
            ]
        );
    }

    #[test]
    fn default_endpoint_requires_an_existing_socket_without_fallback() {
        let socket = TestSocket::new("default");
        let missing = socket.root.join("missing.sock");
        let endpoint =
            endpoint_from_candidates([missing, socket.path.clone()], "portable local default")
                .unwrap_or_else(|error| panic!("resolve test Docker socket: {error}"));
        assert_eq!(endpoint.socket, socket.path);
        assert_eq!(endpoint.host, format!("unix://{}", socket.path.display()));
        let error = must_fail(
            endpoint_from_candidates(
                [
                    socket.root.join("missing-a.sock"),
                    socket.root.join("missing-b.sock"),
                ],
                "portable local default",
            ),
            "missing defaults must not fall back",
        );
        assert!(error.contains("no existing local Docker Unix socket"));
    }

    #[test]
    fn accepted_endpoint_is_normalized_to_existing_local_socket() {
        let socket = TestSocket::new("accepted");
        let endpoint =
            endpoint_from_host(&format!("unix://{}", socket.path.display()), "DOCKER_HOST")
                .unwrap_or_else(|error| panic!("resolve test Docker socket: {error}"));
        assert_eq!(endpoint.socket, socket.path);
        assert_eq!(endpoint.host, format!("unix://{}", socket.path.display()));
    }

    #[test]
    fn remote_relative_malformed_and_missing_endpoints_are_rejected() {
        let socket = TestSocket::new("rejected");
        for host in [
            "tcp://docker.example:2376",
            "ssh://docker.example",
            "npipe:////./pipe/docker_engine",
            "relative/docker.sock",
            "unix://relative/docker.sock",
            &format!("unix://{}", socket.root.join("missing.sock").display()),
        ] {
            assert!(
                endpoint_from_host(host, "DOCKER_HOST").is_err(),
                "endpoint must be rejected: {host}"
            );
        }
    }

    #[test]
    fn hostile_context_metadata_is_rejected_without_fallback() {
        let socket = TestSocket::new("context");
        let config = socket.root.join("config");
        let metadata = config.join("contexts/meta/hostile");
        fs::create_dir_all(&metadata)
            .unwrap_or_else(|error| panic!("create test Docker context metadata: {error}"));
        fs::write(
            config.join("config.json"),
            r#"{"currentContext":"hostile"}"#,
        )
        .unwrap_or_else(|error| panic!("write test Docker config: {error}"));
        fs::write(
            metadata.join("meta.json"),
            r#"{"Endpoints":{"docker":{"Host":"tcp://attacker.example:2376"}}}"#,
        )
        .unwrap_or_else(|error| panic!("write test Docker context: {error}"));
        let error = must_fail(
            resolve_docker_endpoint_from(None, None, None, Some(&config), Some(&socket.root), None),
            "remote context must be rejected",
        );
        assert!(error.contains("refusing remote Docker endpoint"));
    }

    #[test]
    fn docker_command_clears_context_tls_proxy_and_credential_environment() {
        let socket = TestSocket::new("environment");
        let root = socket.root.join("private-home");
        let config = root.join("config");
        fs::create_dir_all(&config)
            .unwrap_or_else(|error| panic!("create private test Docker config: {error}"));
        let environment = DockerEnvironment {
            executable: PathBuf::from("/trusted/bin/docker"),
            path: OsString::from("/trusted/bin:/usr/bin:/bin"),
            endpoint: DockerEndpoint {
                host: format!("unix://{}", socket.path.display()),
                socket: socket.path.clone(),
            },
            private: Arc::new(PrivateDockerEnvironment { root, config }),
        };
        let command = environment.command();
        let variables: BTreeMap<OsString, Option<OsString>> = command
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(OsStr::to_os_string)))
            .collect();
        assert_eq!(
            variables.get(OsStr::new("DOCKER_HOST")),
            Some(&Some(OsString::from(format!(
                "unix://{}",
                socket.path.display()
            ))))
        );
        assert!(variables.contains_key(OsStr::new("PATH")));
        assert!(variables.contains_key(OsStr::new("HOME")));
        assert!(variables.contains_key(OsStr::new("DOCKER_CONFIG")));
        for hostile in [
            "DOCKER_CONTEXT",
            "DOCKER_TLS",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
            "DOCKER_CUSTOM_HEADERS",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "SSH_AUTH_SOCK",
            "GITHUB_TOKEN",
        ] {
            assert!(
                !variables.contains_key(OsStr::new(hostile)),
                "hostile Docker environment variable survived scrub: {hostile}"
            );
        }
    }
}

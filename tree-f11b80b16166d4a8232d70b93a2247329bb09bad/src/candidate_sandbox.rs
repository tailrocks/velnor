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

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

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
const CANDIDATE_WRAPPER: &str = "ulimit -f 262144 || exit 125; /usr/bin/setpriv --reuid=65534 --regid=65534 --clear-groups --inh-caps=-all --ambient-caps=-all --bounding-set=-all -- /candidate \"$@\" > /tmp/velnor-candidate.stdout 2> /tmp/velnor-candidate.stderr; exit $?";

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
    let binary_mount = docker_mount_source(&binary)?;
    let source_mount = source.as_deref().map(docker_mount_source).transpose()?;
    let name = format!("velnor-candidate-{}", crate::unique_suffix());
    let volumes = SandboxVolumes::new(output.is_some())?;
    let cleanup = ContainerCleanup {
        name: name.clone(),
        volumes: volumes.clone(),
    };

    start_sandbox(
        &name,
        &volumes,
        &binary_mount,
        source_mount.as_deref(),
        source.is_some(),
    )?;
    let result = execute_candidate(&name, &volumes, source.is_some(), output.as_deref(), args);
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
    fn new(has_output: bool) -> Result<Self, String> {
        let logs = create_tmpfs_volume(
            &format!("velnor-candidate-logs-{}", crate::unique_suffix()),
            LOG_TMPFS_SIZE,
            4096,
        )?;
        let output = if has_output {
            match create_tmpfs_volume(
                &format!("velnor-candidate-output-{}", crate::unique_suffix()),
                OUTPUT_TMPFS_SIZE,
                16384,
            ) {
                Ok(volume) => Some(volume),
                Err(error) => {
                    remove_volume_bounded(&logs, CLEANUP_TIMEOUT);
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
fn create_tmpfs_volume(name: &str, size: &str, inodes: usize) -> Result<String, String> {
    let options = format!("size={size},nr_inodes={inodes},mode=1777,nosuid,nodev,noexec");
    let mut command = Command::new("docker");
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
            remove_volume_bounded(name, CLEANUP_TIMEOUT);
            return Err(error);
        }
    };
    if !created.status.success() {
        remove_volume_bounded(name, CLEANUP_TIMEOUT);
        return Err(format!(
            "create candidate sandbox tmpfs failed: {}",
            String::from_utf8_lossy(&created.stderr).trim()
        ));
    }
    if String::from_utf8_lossy(&created.stdout).trim() != name {
        remove_volume_bounded(name, CLEANUP_TIMEOUT);
        return Err("Docker returned an unexpected candidate tmpfs name".to_owned());
    }
    Ok(name.to_owned())
}

#[cfg(unix)]
fn remove_volume_bounded(name: &str, timeout: Duration) {
    let mut command = Command::new("docker");
    command.args(["volume", "rm", name]);
    discard_command_until(&mut command, Instant::now() + timeout);
}

#[cfg(unix)]
fn start_sandbox(
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
    let mut start = Command::new("docker");
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
        Some(name),
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
        name, "0:0", None, "/bin/sh", &log_setup, deadline, LOG_LIMIT,
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
        name,
        "0:0",
        has_source.then_some("/workspace"),
        "/bin/sh",
        &candidate_args,
        deadline,
        LOG_LIMIT,
    )?;
    // A finite process scan is not a quiescence proof: a descendant can fork
    // after the last scan and mutate the output while the archive is being
    // collected. Freeze the entire container before reading any candidate
    // owned path. Docker's freezer covers every process and namespace in the
    // container, so no late writer can run between the proof and the copy.
    pause_container(name, deadline)?;
    let capture = CaptureDirectory::new()?;
    archive_frozen_volumes(volumes, &capture, output.is_some(), deadline)?;
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
fn pause_container(name: &str, deadline: Instant) -> Result<(), String> {
    let mut command = Command::new("docker");
    let paused = command_output_until(
        command.args(["pause", name]),
        deadline,
        LOG_LIMIT,
        DOCKER_STDERR_LIMIT,
        "pause candidate sandbox",
        Some(name),
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
    volumes: &SandboxVolumes,
    capture: &CaptureDirectory,
    has_output: bool,
    deadline: Instant,
) -> Result<(), String> {
    let sidecar_name = format!("velnor-candidate-capture-{}", crate::unique_suffix());
    let sidecar_cleanup = ContainerNameCleanup(sidecar_name.clone());
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
    let mut command = Command::new("docker");
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
        Some(&sidecar_name),
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
fn docker_exec_until(
    name: &str,
    user: &str,
    workdir: Option<&str>,
    executable: &str,
    args: &[OsString],
    deadline: Instant,
    stdout_limit: usize,
) -> Result<std::process::Output, String> {
    let mut command = Command::new("docker");
    command.args(["exec", "--user", user]);
    if let Some(workdir) = workdir {
        command.args(["--workdir", workdir]);
    }
    let mut child = command
        .arg(name)
        .arg(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("start candidate sandbox command: {error}"))?;
    bounded_child_output(
        &mut child,
        deadline,
        stdout_limit,
        DOCKER_STDERR_LIMIT,
        Some(name),
    )
}

#[cfg(unix)]
fn command_output_until(
    command: &mut Command,
    deadline: Instant,
    stdout_limit: usize,
    stderr_limit: usize,
    label: &str,
    cleanup_container: Option<&str>,
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
    cleanup_container: Option<&str>,
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
                if let Some(name) = cleanup_container {
                    kill_container_bounded(name, CLEANUP_TIMEOUT);
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
fn kill_container_bounded(name: &str, timeout: Duration) {
    let mut command = Command::new("docker");
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
struct ContainerNameCleanup(String);

#[cfg(unix)]
impl Drop for ContainerNameCleanup {
    fn drop(&mut self) {
        remove_container_bounded(&self.0, CLEANUP_TIMEOUT);
    }
}

#[cfg(unix)]
fn remove_container_bounded(name: &str, timeout: Duration) {
    let mut command = Command::new("docker");
    command.args(["rm", "--force", name]);
    discard_command_until(&mut command, Instant::now() + timeout);
}

#[cfg(unix)]
struct ContainerCleanup {
    name: String,
    volumes: SandboxVolumes,
}

#[cfg(unix)]
impl Drop for ContainerCleanup {
    fn drop(&mut self) {
        remove_container_bounded(&self.name, CLEANUP_TIMEOUT);
        if let Some(output) = &self.volumes.output {
            remove_volume_bounded(output, CLEANUP_TIMEOUT);
        }
        remove_volume_bounded(&self.volumes.logs, CLEANUP_TIMEOUT);
    }
}

#[cfg(unix)]
fn safe_archive_path(path: &Path) -> Result<Option<PathBuf>, String> {
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => safe.push(part),
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
fn single_file_archive(bytes: &[u8], expected: &str, limit: usize) -> Result<Vec<u8>, String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut result = None;
    let entries = archive
        .entries()
        .map_err(|error| format!("read candidate log archive: {error}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| format!("read candidate log entry: {error}"))?;
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
        let size = entry
            .header()
            .size()
            .map_err(|error| format!("read candidate log size: {error}"))?;
        if size > limit as u64 {
            return Err(format!("candidate log exceeds {limit} bytes"));
        }
        let capacity = usize::try_from(size)
            .map_err(|_| format!("candidate log size {size} exceeds platform capacity"))?;
        let mut content = Vec::with_capacity(capacity);
        entry
            .read_to_end(&mut content)
            .map_err(|error| format!("read candidate log content: {error}"))?;
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
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut entries = BTreeMap::new();
    let parsed = archive
        .entries()
        .map_err(|error| format!("read candidate render archive: {error}"))?;
    for entry in parsed {
        let mut entry = entry.map_err(|error| format!("read candidate render entry: {error}"))?;
        let raw_path = entry
            .path()
            .map_err(|error| format!("read candidate render path: {error}"))?
            .into_owned();
        let Some(path) = safe_archive_path(&raw_path)? else {
            continue;
        };
        let kind = entry.header().entry_type();
        let record = if kind.is_dir() {
            RenderEntry::Directory
        } else if kind.is_file() {
            let mode = entry
                .header()
                .mode()
                .map_err(|error| format!("read candidate render mode: {error}"))?;
            let size = entry
                .header()
                .size()
                .map_err(|error| format!("read candidate render size: {error}"))?;
            if size > OUTPUT_ARCHIVE_LIMIT as u64 {
                return Err(format!(
                    "candidate file {} exceeds the output limit",
                    path.display()
                ));
            }
            let capacity = usize::try_from(size).map_err(|_| {
                format!(
                    "candidate file {} exceeds platform capacity",
                    path.display()
                )
            })?;
            let mut contents = Vec::with_capacity(capacity);
            entry
                .read_to_end(&mut contents)
                .map_err(|error| format!("read candidate file {}: {error}", path.display()))?;
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

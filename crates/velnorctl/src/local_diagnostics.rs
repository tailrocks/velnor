//! macOS-local diagnostics owned by `velnorctl`.
//!
//! The runner's production Docker contract is Linux-specific: it needs a
//! systemd cgroup-v2 boundary and a daemon-visible work directory. The CLI is
//! still useful on macOS for inspecting that contract and proving the parts
//! OrbStack/Docker can provide. This module therefore reports every observed
//! check, returns a non-zero condition for an incomplete contract, and never
//! turns a macOS Docker VM into a pretending-to-be-Linux runner.

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::Value;
use velnor_model::{ExecutionBackendKind, ExecutionFile, ExitClass};

use crate::{commands, runtime, CommandError, GlobalArgs};

const REPORT_SCHEMA_VERSION: u8 = 1;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const CONTAINER_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_COMMAND_DETAIL_BYTES: usize = 600;
const DEFAULT_WORK_DIR_NAME: &str = ".velnor-work";
const JOB_CGROUP_PARENT: &str = "/velnor-jobs.slice";

/// Run macOS-local preflight without invoking Linux systemd/Firecracker code.
pub fn preflight(globals: &GlobalArgs, args: &runtime::PreflightArgs) -> Result<(), CommandError> {
    let config_dir = default_or_explicit_execution_dir(args.config_dir.as_deref())?;
    let work_dir = args
        .work_dir
        .clone()
        .unwrap_or_else(|| current_work_dir().join(DEFAULT_WORK_DIR_NAME));
    let paths = resolve_paths(Some(&config_dir), Some(&work_dir))?;
    let (backend, config_file) = match load_execution(&config_dir, false) {
        Ok(value) => value,
        Err(error) => {
            let report = PreflightReport {
                schema_version: REPORT_SCHEMA_VERSION,
                backend: None,
                execution_config: config_dir.join("execution.toml"),
                paths,
                docker: None,
                checks: vec![Check::fail(
                    "execution-config",
                    error.message.clone(),
                    Some(format!(
                        "Create {} with [execution] backend = \"docker\" or \"microvm\".",
                        config_dir.join("execution.toml").display()
                    )),
                )],
                ready: false,
                runner_ready: false,
            };
            emit_report(globals, &report)?;
            return Err(error);
        }
    };

    let mut checks = vec![
        Check::pass(
            "execution-config",
            format!("selected backend {backend} from {}", config_file.display()),
        ),
        Check::pass("host-platform", format!("macOS {}", std::env::consts::ARCH)),
    ];

    match backend {
        ExecutionBackendKind::Docker => {
            let target = resolve_docker_target();
            let git = run_process("git", &["--version"], COMMAND_TIMEOUT);
            checks.push(check_command(
                "host-git",
                &git,
                "Git is available",
                "Install Git/Xcode Command Line Tools and rerun `velnorctl preflight`.",
            ));

            let docker = collect_docker_report(
                Some(&args.docker_image),
                true,
                args.require_buildx,
                true,
                &paths.work,
                args.docker_host_work_dir.as_deref(),
            );
            checks.extend(docker.checks.iter().cloned());
            checks.push(check_job_image_tools(
                &target,
                &args.docker_image,
                docker.server_reachable,
            ));
            checks.push(check_container_docker_client(
                &target,
                &args.docker_image,
                docker.socket_path.as_deref(),
                docker.socket_exists,
                args.require_buildx,
            ));

            let ready =
                docker.runner_ready && checks.iter().all(|check| check.status == CheckStatus::Pass);
            let report = PreflightReport {
                schema_version: REPORT_SCHEMA_VERSION,
                backend: Some(backend),
                execution_config: config_file,
                paths,
                docker: Some(docker),
                checks,
                ready,
                runner_ready: ready,
            };
            emit_report(globals, &report)?;
            if ready {
                Ok(())
            } else {
                Err(preflight_error(&report))
            }
        }
        ExecutionBackendKind::MicroVm => {
            checks.push(Check::fail(
                "microvm-platform",
                "Firecracker/KVM execution is not available on a macOS host",
                Some(
                    "Run the microVM backend on Linux with /dev/kvm and its packaged Firecracker artifacts; Docker was not used."
                        .to_owned(),
                ),
            ));
            let report = PreflightReport {
                schema_version: REPORT_SCHEMA_VERSION,
                backend: Some(backend),
                execution_config: config_file,
                paths,
                docker: None,
                checks,
                ready: false,
                runner_ready: false,
            };
            emit_report(globals, &report)?;
            Err(CommandError::new(
                ExitClass::Condition,
                "backend.macos_unsupported",
                "microVM preflight cannot pass on macOS; use a Linux host with KVM and Firecracker",
            ))
        }
    }
}

/// Add a backend report after the existing runner identity status output.
pub fn status(
    _globals: &GlobalArgs,
    args: &runtime::StatusArgs,
    runner_result: Result<(), CommandError>,
) -> Result<(), CommandError> {
    let runner_error = runner_result.err();
    let (backend, config_file) = match load_execution_for_status(args.config_dir.as_deref()) {
        Ok(value) => value,
        Err(error) => return Err(runner_error.unwrap_or(error)),
    };
    let paths = resolve_paths(args.config_dir.as_deref(), None)?;
    println!();
    println!("Backend: {backend}");
    println!("Execution config: {}", config_file.display());
    print_paths_human(&paths);

    let backend_result = match backend {
        ExecutionBackendKind::Docker => {
            let report = collect_docker_report(None, false, true, true, &paths.work, None);
            print_docker_human(&report);
            if report.runner_ready {
                Ok(())
            } else {
                Err(backend_error(&report))
            }
        }
        ExecutionBackendKind::MicroVm => Err(CommandError::new(
            ExitClass::Condition,
            "backend.macos_unsupported",
            "microVM backend is selected, but Firecracker/KVM execution is unavailable on macOS",
        )),
    };
    if let Some(runner_error) = runner_error {
        return Err(match backend_result {
            Ok(()) => runner_error,
            Err(backend_error) => CommandError::new(
                runner_error.class,
                "status.degraded",
                format!(
                    "runner status unavailable: {}; backend diagnostics also failed: {}",
                    runner_error.message, backend_error.message
                ),
            ),
        });
    }
    backend_result
}

/// Report the canonical local paths needed to inspect a runner installation.
pub fn paths(globals: &GlobalArgs, args: &runtime::StorageArgs) -> Result<(), CommandError> {
    let report = resolve_paths(args.config_dir.as_deref(), None)?;
    if globals.output_format().is_machine() {
        emit_json(&report)
    } else {
        print_paths_human(&report);
        Ok(())
    }
}

/// Report the selected Docker endpoint and the Velnor-relevant capabilities.
pub fn docker_report(
    globals: &GlobalArgs,
    args: &commands::DockerArgs,
) -> Result<(), CommandError> {
    let work_dir = args
        .work_dir
        .clone()
        .unwrap_or_else(|| current_work_dir().join(DEFAULT_WORK_DIR_NAME));
    let paths = resolve_paths(None, Some(&work_dir))?;
    let report = collect_docker_report(
        args.check_bind_mount.then_some(&args.image),
        args.check_bind_mount,
        true,
        true,
        &paths.work,
        args.docker_host_work_dir.as_deref(),
    );
    if globals.output_format().is_machine() {
        emit_json(&report)?;
    } else {
        print_docker_human(&report);
    }
    if report.velnor_compatible {
        Ok(())
    } else {
        Err(backend_error(&report))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PreflightReport {
    schema_version: u8,
    backend: Option<ExecutionBackendKind>,
    execution_config: PathBuf,
    paths: PathsReport,
    docker: Option<DockerReport>,
    checks: Vec<Check>,
    ready: bool,
    runner_ready: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PathsReport {
    mode: String,
    cache: PathBuf,
    lib: PathBuf,
    run: PathBuf,
    log: PathBuf,
    config: PathBuf,
    work: PathBuf,
    runner_log: PathBuf,
    artifacts: PathBuf,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DockerReport {
    schema_version: u8,
    host_os: String,
    host_arch: String,
    docker_cli: Option<PathBuf>,
    endpoint_source: String,
    context: Option<String>,
    endpoint: Option<String>,
    endpoint_kind: String,
    provider: String,
    socket_path: Option<PathBuf>,
    socket_exists: bool,
    socket_target: Option<PathBuf>,
    server_reachable: bool,
    server_version: Option<String>,
    server_os: Option<String>,
    server_arch: Option<String>,
    buildx_version: Option<String>,
    cgroup_driver: Option<String>,
    cgroup_version: Option<String>,
    cgroup_mode: String,
    resources: DockerResources,
    endpoint_ready: bool,
    velnor_compatible: bool,
    runner_ready: bool,
    capabilities: DockerCapabilities,
    checks: Vec<Check>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DockerResources {
    cpus: Option<u64>,
    memory_bytes: Option<u64>,
    docker_root_dir: Option<PathBuf>,
    ready: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DockerCapabilities {
    api: bool,
    local_socket: bool,
    buildx: bool,
    bind_mount: Option<bool>,
    systemd_cgroup_v2: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Check {
    name: String,
    status: CheckStatus,
    detail: String,
    remediation: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum CheckStatus {
    Pass,
    Fail,
    Unsupported,
    Skipped,
}

impl Check {
    fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Pass,
            detail: detail.into(),
            remediation: None,
        }
    }

    fn fail(
        name: impl Into<String>,
        detail: impl Into<String>,
        remediation: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Fail,
            detail: detail.into(),
            remediation,
        }
    }

    fn skipped(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Skipped,
            detail: detail.into(),
            remediation: None,
        }
    }

    fn unsupported(
        name: impl Into<String>,
        detail: impl Into<String>,
        remediation: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Unsupported,
            detail: detail.into(),
            remediation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DockerTarget {
    Host {
        endpoint: String,
        source: &'static str,
    },
    Context {
        name: Option<String>,
        source: &'static str,
    },
}

impl DockerTarget {
    fn source(&self) -> &'static str {
        match self {
            Self::Host { source, .. } | Self::Context { source, .. } => source,
        }
    }

    fn cli_args(&self) -> Vec<String> {
        match self {
            Self::Host { endpoint, .. } => vec!["--host".to_owned(), endpoint.clone()],
            Self::Context {
                name: Some(name), ..
            } => vec!["--context".to_owned(), name.clone()],
            Self::Context { name: None, .. } => Vec::new(),
        }
    }
}

fn resolve_docker_target() -> DockerTarget {
    resolve_docker_target_from(
        env_value("VELNOR_DOCKER_HOST"),
        env_value("VELNOR_DOCKER_CONTEXT"),
        env_value("DOCKER_HOST"),
        env_value("DOCKER_CONTEXT"),
    )
}

fn resolve_docker_target_from(
    velnor_host: Option<String>,
    velnor_context: Option<String>,
    docker_host: Option<String>,
    docker_context: Option<String>,
) -> DockerTarget {
    if let Some(endpoint) = velnor_host {
        return DockerTarget::Host {
            endpoint,
            source: "VELNOR_DOCKER_HOST",
        };
    }
    if let Some(name) = velnor_context {
        return DockerTarget::Context {
            name: Some(name),
            source: "VELNOR_DOCKER_CONTEXT",
        };
    }
    if let Some(name) = docker_context {
        return DockerTarget::Context {
            name: Some(name),
            source: "DOCKER_CONTEXT",
        };
    }
    if let Some(endpoint) = docker_host {
        return DockerTarget::Host {
            endpoint,
            source: "DOCKER_HOST",
        };
    }
    DockerTarget::Context {
        name: None,
        source: "docker-default",
    }
}

fn env_value(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[derive(Debug)]
struct ProcessResult {
    status: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
    error: Option<String>,
}

impl ProcessResult {
    fn failed(error: impl Into<String>) -> Self {
        Self {
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            error: Some(error.into()),
        }
    }

    fn succeeded(&self) -> bool {
        self.status == Some(0) && !self.timed_out && self.error.is_none()
    }
}

fn collect_docker_report(
    image: Option<&String>,
    check_bind_mount: bool,
    require_buildx: bool,
    require_socket: bool,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
) -> DockerReport {
    let docker_cli = find_executable("docker");
    let target = resolve_docker_target();
    let mut checks = Vec::new();
    if let Some(path) = &docker_cli {
        checks.push(Check::pass(
            "docker-cli",
            format!("using {}", path.display()),
        ));
    } else {
        checks.push(Check::fail(
            "docker-cli",
            "the `docker` executable is not on PATH",
            Some("Install Docker or OrbStack, then verify `command -v docker`.".to_owned()),
        ));
    }

    let context_result = match &target {
        DockerTarget::Host { .. } => None,
        DockerTarget::Context { .. } => Some(run_docker_process(
            &target,
            &["context", "show"],
            COMMAND_TIMEOUT,
        )),
    };
    let context = context_result.as_ref().and_then(|result| {
        result
            .succeeded()
            .then(|| result.stdout.trim().to_owned())
            .filter(|value| !value.is_empty())
    });
    match (&target, &context_result, &context) {
        (DockerTarget::Host { .. }, None, _) => checks.push(Check::skipped(
            "docker-context",
            format!("direct endpoint selected by {}", target.source()),
        )),
        (_, Some(_), Some(context)) => checks.push(Check::pass(
            "docker-context",
            format!("selected context {context} via {}", target.source()),
        )),
        (_, Some(result), None) => checks.push(Check::fail(
            "docker-context",
            process_detail(result, "Docker context"),
            Some(
                "Run `docker context ls` and select a running local context; for OrbStack, run `orbctl start`."
                    .to_owned(),
            ),
        )),
        _ => checks.push(Check::fail(
            "docker-context",
            "Docker target selection did not yield a usable context result",
            Some(
                "Set only one of VELNOR_DOCKER_HOST or VELNOR_DOCKER_CONTEXT, then rerun the report."
                    .to_owned(),
            ),
        )),
    }

    let context_endpoint = context.as_deref().and_then(|name| {
        let result = run_docker_process(
            &target,
            &["context", "inspect", "--format", "{{json .}}", name],
            COMMAND_TIMEOUT,
        );
        let endpoint = result
            .succeeded()
            .then(|| parse_context_endpoint(&result.stdout))
            .flatten();
        if endpoint.is_none() {
            checks.push(Check::fail(
                "docker-endpoint",
                process_detail(&result, "Docker context endpoint"),
                Some(
                    "Inspect the selected context with `docker context inspect`; Velnor needs a local Unix socket for the Docker backend."
                        .to_owned(),
                ),
            ));
        }
        endpoint
    });
    let endpoint = match &target {
        DockerTarget::Host { endpoint, .. } => {
            checks.push(Check::pass(
                "docker-endpoint",
                format!("using direct endpoint from {}", target.source()),
            ));
            Some(endpoint.clone())
        }
        DockerTarget::Context { .. } => context_endpoint,
    };
    let endpoint_kind = endpoint
        .as_deref()
        .map(endpoint_kind)
        .unwrap_or("unknown")
        .to_owned();
    let socket_path = endpoint
        .as_deref()
        .filter(|_| endpoint_kind == "unix")
        .and_then(unix_socket_path);
    let (socket_exists, socket_target, socket_detail) =
        socket_path.as_deref().map(inspect_socket).unwrap_or((
            false,
            None,
            "no local Unix socket endpoint was resolved".to_owned(),
        ));
    if endpoint_kind == "unix" && socket_exists {
        checks.push(Check::pass("unix-socket", socket_detail));
    } else if endpoint_kind == "remote" {
        checks.push(Check::fail(
            "unix-socket",
            format!(
                "Docker endpoint {} is remote; Velnor job containers need a daemon-visible local socket",
                endpoint.as_deref().unwrap_or("<unknown>")
            ),
            Some(
                "Use a local Docker context/socket, or provide a daemon configuration that explicitly mounts the remote work directory; remote Docker is not silently treated as local."
                    .to_owned(),
            ),
        ));
    } else {
        checks.push(Check::fail(
            "unix-socket",
            socket_detail,
            Some(socket_remediation("unknown")),
        ));
    }

    let version_result = run_docker_process(
        &target,
        &["version", "--format", "{{json .}}"],
        COMMAND_TIMEOUT,
    );
    let version_json = parse_json_output(&version_result);
    let info_result = run_docker_process(
        &target,
        &["info", "--format", "{{json .}}"],
        COMMAND_TIMEOUT,
    );
    let info_json = parse_json_output(&info_result);
    let server_reachable = info_result.succeeded() && info_json.is_some();
    let server_version = version_json
        .as_ref()
        .and_then(|value| json_string(value, &["Server", "Version"]))
        .or_else(|| {
            info_json
                .as_ref()
                .and_then(|value| json_string(value, &["ServerVersion"]))
        });
    let server_os = version_json
        .as_ref()
        .and_then(|value| json_string(value, &["Server", "Os"]))
        .or_else(|| {
            info_json
                .as_ref()
                .and_then(|value| json_string(value, &["OSType"]))
        });
    let server_arch = version_json
        .as_ref()
        .and_then(|value| json_string(value, &["Server", "Arch"]))
        .or_else(|| {
            info_json
                .as_ref()
                .and_then(|value| json_string(value, &["Architecture"]))
        });
    let cgroup_driver = info_json
        .as_ref()
        .and_then(|value| json_string(value, &["CgroupDriver"]));
    let cgroup_version = info_json
        .as_ref()
        .and_then(|value| json_string(value, &["CgroupVersion"]));
    if server_reachable && info_json.is_some() {
        checks.push(Check::pass(
            "docker-api",
            format!(
                "Docker API reachable{}",
                server_version
                    .as_deref()
                    .map_or_else(String::new, |version| format!(" (server {version})"))
            ),
        ));
    } else {
        checks.push(Check::fail(
            "docker-api",
            process_detail(&info_result, "Docker API"),
            Some(docker_api_remediation()),
        ));
    }

    let server_platform_ready = match server_os.as_deref() {
        Some("linux") if server_reachable => {
            checks.push(Check::pass(
                "server-platform",
                format!(
                    "Docker server is Linux{}",
                    server_arch
                        .as_deref()
                        .map_or_else(String::new, |arch| format!("/{arch}"))
                ),
            ));
            true
        }
        Some(os) if server_reachable => {
            checks.push(Check::fail(
                "server-platform",
                format!("Docker server platform {os} is not a Linux job platform"),
                Some(
                    "Use a Docker/OrbStack context backed by a Linux VM for Velnor job containers."
                        .to_owned(),
                ),
            ));
            false
        }
        _ => {
            checks.push(Check::skipped(
                "server-platform",
                "not evaluated because Docker server information is unavailable",
            ));
            false
        }
    };
    let resources = docker_resources(info_json.as_ref(), server_reachable);
    if resources.ready {
        checks.push(Check::pass("resources", format_resource_detail(&resources)));
    } else if server_reachable {
        checks.push(Check::fail(
            "resources",
            format_resource_detail(&resources),
            Some(
                "Start a healthy Docker VM and verify it reports non-zero CPUs and memory with `docker info`."
                    .to_owned(),
            ),
        ));
    } else {
        checks.push(Check::skipped(
            "resources",
            "not evaluated because the Docker API is unavailable",
        ));
    }

    let provider = detect_provider(
        docker_cli.as_deref(),
        endpoint.as_deref(),
        server_os.as_deref(),
        info_json.as_ref(),
    );
    let buildx_result = run_docker_process(&target, &["buildx", "version"], COMMAND_TIMEOUT);
    let buildx_version = buildx_result
        .succeeded()
        .then(|| buildx_result.stdout.trim().to_owned())
        .filter(|value| !value.is_empty());
    if buildx_result.succeeded() {
        checks.push(Check::pass(
            "buildx",
            format!(
                "Docker Buildx available{}",
                buildx_version
                    .as_deref()
                    .map_or_else(String::new, |version| format!(" ({version})"))
            ),
        ));
    } else if require_buildx {
        checks.push(Check::fail(
            "buildx",
            process_detail(&buildx_result, "Docker Buildx"),
            Some("Install/enable Docker Buildx and rerun the report.".to_owned()),
        ));
    } else {
        checks.push(Check::skipped(
            "buildx",
            "Buildx was not required by this diagnostic",
        ));
    }

    let cgroup_mode = cgroup_mode(cgroup_driver.as_deref(), cgroup_version.as_deref());
    let systemd_cgroup_v2 = cgroup_mode == "systemd-v2";
    let cgroup_check = if !server_reachable {
        Check::skipped(
            "linux-cgroup-boundary",
            "not evaluated because the Docker API is unavailable",
        )
    } else if systemd_cgroup_v2 {
        Check::pass(
            "linux-cgroup-boundary",
            cgroup_detail(
                &cgroup_mode,
                cgroup_driver.as_deref(),
                cgroup_version.as_deref(),
            ),
        )
    } else if std::env::consts::OS == "macos" {
        Check::unsupported(
            "linux-cgroup-boundary",
            format!(
                "{}; this optional Linux runner invariant is not a hard macOS Docker endpoint failure",
                cgroup_detail(
                    &cgroup_mode,
                    cgroup_driver.as_deref(),
                    cgroup_version.as_deref(),
                )
            ),
            Some(cgroup_remediation(&provider)),
        )
    } else {
        Check::fail(
            "linux-cgroup-boundary",
            cgroup_detail(
                &cgroup_mode,
                cgroup_driver.as_deref(),
                cgroup_version.as_deref(),
            ),
            Some(cgroup_remediation(&provider)),
        )
    };
    checks.push(cgroup_check);

    let bind_mount = if check_bind_mount {
        match image {
            Some(image) if !image.trim().is_empty() && server_reachable => Some(
                check_bind_mount_probe(&target, image, work_dir, docker_host_work_dir),
            ),
            Some(_) => Some(Check::fail(
                "bind-mount",
                "bind-mount probe needs a non-empty image and a reachable Docker API",
                Some(
                    "Start OrbStack/Docker, then pass an image available to that daemon with `--image`."
                        .to_owned(),
                ),
            )),
            None => Some(Check::fail(
                "bind-mount",
                "bind-mount probe was requested without an image",
                Some("Pass `--image IMAGE` for the bind-mount probe.".to_owned()),
            )),
        }
    } else {
        None
    };
    if let Some(check) = &bind_mount {
        checks.push(check.clone());
    } else {
        checks.push(Check::skipped(
            "bind-mount",
            "not requested; use `--check-bind-mount --image IMAGE`",
        ));
    }

    let endpoint_ready = server_reachable
        && info_json.is_some()
        && (!require_socket || socket_exists)
        && endpoint_kind != "unknown";
    let buildx_ok = buildx_result.succeeded() || !require_buildx;
    let bind_mount_ok = bind_mount
        .as_ref()
        .is_none_or(|check| check.status == CheckStatus::Pass);
    let velnor_compatible =
        endpoint_ready && buildx_ok && resources.ready && server_platform_ready && bind_mount_ok;
    let runner_ready = velnor_compatible && systemd_cgroup_v2;
    DockerReport {
        schema_version: REPORT_SCHEMA_VERSION,
        host_os: std::env::consts::OS.to_owned(),
        host_arch: std::env::consts::ARCH.to_owned(),
        docker_cli,
        endpoint_source: target.source().to_owned(),
        context,
        endpoint,
        endpoint_kind,
        provider,
        socket_path,
        socket_exists,
        socket_target,
        server_reachable,
        server_version,
        server_os,
        server_arch,
        buildx_version,
        cgroup_driver,
        cgroup_version,
        cgroup_mode,
        resources,
        endpoint_ready,
        velnor_compatible,
        runner_ready,
        capabilities: DockerCapabilities {
            api: server_reachable && info_json.is_some(),
            local_socket: socket_exists,
            buildx: buildx_result.succeeded(),
            bind_mount: bind_mount.map(|check| check.status == CheckStatus::Pass),
            systemd_cgroup_v2,
        },
        checks,
    }
}

fn check_job_image_tools(target: &DockerTarget, image: &str, server_reachable: bool) -> Check {
    if !server_reachable {
        return Check::skipped(
            "job-image-tools",
            "not run because the Docker API is unavailable",
        );
    }
    let command = "command -v sh >/dev/null && command -v bash >/dev/null && command -v git >/dev/null && command -v file >/dev/null && command -v sudo >/dev/null && command -v node >/dev/null && command -v rustup >/dev/null && command -v mbx >/dev/null && command -v velnor-workflow >/dev/null && test -x /usr/local/bin/velnor-workflow";
    let result = run_docker_process(
        target,
        &[
            "run",
            "--pull=never",
            "--rm",
            "--name",
            &probe_name("image-tools"),
            image,
            "sh",
            "-c",
            command,
        ],
        CONTAINER_TIMEOUT,
    );
    if result.succeeded() {
        Check::pass(
            "job-image-tools",
            format!("image {image} has the Velnor job toolchain"),
        )
    } else {
        Check::fail(
            "job-image-tools",
            format!("image {image}: {}", process_detail(&result, "job image tools")),
            Some(format!(
                "Pre-pull a Velnor Linux job image containing sh/bash/git/node/rustup/mbx/velnor-workflow, or pass the correct image with `--docker-image {image}`; `--pull=never` means this check never hides a missing image."
            )),
        )
    }
}

fn check_container_docker_client(
    target: &DockerTarget,
    image: &str,
    socket_path: Option<&Path>,
    socket_exists: bool,
    require_buildx: bool,
) -> Check {
    let Some(socket_path) = socket_path else {
        return Check::skipped(
            "job-docker-client",
            "not run because no local Docker socket was resolved",
        );
    };
    if !socket_exists {
        return Check::skipped(
            "job-docker-client",
            "not run because the resolved Docker socket is unavailable",
        );
    }
    let command = if require_buildx {
        "docker version && docker buildx version"
    } else {
        "docker version"
    };
    let mount = format!(
        "type=bind,src={},dst=/var/run/docker.sock,readonly",
        socket_path.display()
    );
    let result = run_docker_process(
        target,
        &[
            "run",
            "--pull=never",
            "--rm",
            "--name",
            &probe_name("docker-client"),
            "--mount",
            &mount,
            image,
            "sh",
            "-c",
            command,
        ],
        CONTAINER_TIMEOUT,
    );
    if result.succeeded() {
        Check::pass(
            "job-docker-client",
            format!("image {image} can reach the mounted Docker socket"),
        )
    } else {
        Check::fail(
            "job-docker-client",
            format!("image {image}: {}", process_detail(&result, "job Docker client")),
            Some(format!(
                "Use a Linux Velnor job image with Docker CLI{} and ensure the daemon socket is mountable; OrbStack's host socket proves the CLI endpoint, not Linux runner cgroup compatibility.",
                if require_buildx { " and Buildx" } else { "" }
            )),
        )
    }
}

fn check_bind_mount_probe(
    target: &DockerTarget,
    image: &str,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
) -> Check {
    let probe_dir = work_dir.join("preflight").join("mount");
    if let Err(error) = fs::create_dir_all(&probe_dir) {
        return Check::fail(
            "bind-mount",
            format!("create local probe directory {}: {error}", probe_dir.display()),
            Some(format!(
                "Choose a writable `--work-dir`; Velnor needs it for job workspaces, logs, and artifacts."
            )),
        );
    }
    let marker_name = format!(".velnorctl-mount-check-{}", std::process::id());
    let marker = probe_dir.join(&marker_name);
    if let Err(error) = fs::write(&marker, b"velnorctl\n") {
        return Check::fail(
            "bind-mount",
            format!("write local probe marker {}: {error}", marker.display()),
            Some(format!(
                "Choose a writable `--work-dir`; Docker cannot see a marker that Velnor cannot create."
            )),
        );
    }
    let source = match docker_visible_path(&marker, work_dir, docker_host_work_dir) {
        Ok(path) => path,
        Err(error) => {
            let _ = fs::remove_file(&marker);
            return Check::fail(
                "bind-mount",
                error,
                Some(
                    "Keep `--docker-host-work-dir` as the path seen by the Docker daemon and make its relative layout match `--work-dir`."
                        .to_owned(),
                ),
            );
        }
    };
    let mount = format!(
        "type=bind,src={},dst=/__velnorctl,readonly",
        source.display()
    );
    let command = format!("test -f /__velnorctl/{marker_name}");
    let result = run_docker_process(
        target,
        &[
            "run",
            "--pull=never",
            "--rm",
            "--name",
            &probe_name("bind-mount"),
            "--mount",
            &mount,
            image,
            "sh",
            "-c",
            &command,
        ],
        CONTAINER_TIMEOUT,
    );
    let _ = fs::remove_file(&marker);
    if result.succeeded() {
        Check::pass(
            "bind-mount",
            format!("Docker daemon can see {} at /__velnorctl", source.display()),
        )
    } else {
        Check::fail(
            "bind-mount",
            format!(
                "Docker daemon could not see {} in image {image}: {}",
                source.display(),
                process_detail(&result, "Docker bind mount")
            ),
            Some(bind_mount_remediation()),
        )
    }
}

fn default_or_explicit_execution_dir(explicit: Option<&Path>) -> Result<PathBuf, CommandError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(path) = env::var_os("VELNOR_CONFIG_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = env::var_os("HOME").ok_or_else(|| {
        CommandError::new(
            ExitClass::Usage,
            "config.home_missing",
            "HOME is not set; pass --config-dir pointing at the directory containing execution.toml",
        )
    })?;
    Ok(PathBuf::from(home).join("Library/Application Support/velnor"))
}

fn load_execution(
    primary_dir: &Path,
    include_parent: bool,
) -> Result<(ExecutionBackendKind, PathBuf), CommandError> {
    let mut candidates = vec![primary_dir.to_path_buf()];
    if include_parent && let Some(parent) = primary_dir.parent() {
        candidates.push(parent.to_path_buf());
    }
    if primary_dir != Path::new("/etc/velnor") {
        candidates.push(PathBuf::from("/etc/velnor"));
    }
    for directory in dedup_paths(candidates) {
        let path = directory.join("execution.toml");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(CommandError::new(
                    ExitClass::Operation,
                    "execution.read_failed",
                    format!("cannot read {}: {error}", path.display()),
                ));
            }
        };
        let file = ExecutionFile::parse_toml(&text).map_err(|error| {
            CommandError::new(
                ExitClass::Usage,
                "execution.invalid",
                format!("cannot parse {}: {error}", path.display()),
            )
        })?;
        return Ok((file.backend(), path));
    }
    Err(CommandError::new(
        ExitClass::Unavailable,
        "execution.config_missing",
        format!(
            "execution.toml was not found under {}; create [execution] backend = \"docker\" or \"microvm\"",
            primary_dir.display()
        ),
    ))
}

fn load_execution_for_status(
    explicit_dir: Option<&Path>,
) -> Result<(ExecutionBackendKind, PathBuf), CommandError> {
    let primary = match explicit_dir {
        Some(path) => path.to_path_buf(),
        None => default_or_explicit_execution_dir(None)?,
    };
    load_execution(&primary, true)
}

fn resolve_paths(
    explicit_config: Option<&Path>,
    explicit_work: Option<&Path>,
) -> Result<PathsReport, CommandError> {
    let storage_root = env::var_os("VELNOR_STORAGE_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let (mode, cache, lib, run, log, config) = if let Some(prefix) = storage_root {
        let run = if prefix == Path::new("/var") {
            PathBuf::from("/run/velnor")
        } else {
            prefix.join("run/velnor")
        };
        (
            "storage-root".to_owned(),
            prefix.join("cache/velnor/v1"),
            prefix.join("lib/velnor"),
            run,
            prefix.join("log/velnor"),
            prefix.join("lib/velnor/runner"),
        )
    } else if let Some(config) = explicit_config {
        (
            "explicit-config".to_owned(),
            config.join("cache"),
            config.to_path_buf(),
            config.join("run"),
            config.join("log"),
            config.to_path_buf(),
        )
    } else {
        let home = env::var_os("HOME").ok_or_else(|| {
            CommandError::new(
                ExitClass::Usage,
                "config.home_missing",
                "HOME is not set; pass --config-dir to resolve local paths",
            )
        })?;
        let home = PathBuf::from(home);
        let state = env::var_os("XDG_STATE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("Library/Application Support"));
        let cache = env::var_os("XDG_CACHE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("Library/Caches"));
        let runtime = env::var_os("XDG_RUNTIME_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| env::temp_dir().join("velnor-run"));
        let lib = state.join("velnor");
        (
            "xdg-user".to_owned(),
            cache.join("velnor"),
            lib.clone(),
            runtime.join("velnor"),
            lib.join("log"),
            lib.join("runner"),
        )
    };
    let work = explicit_work
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config.join("_work"));
    let artifact_root = daemon_shared_root(work.clone()).join("_velnor_artifacts");
    Ok(PathsReport {
        mode,
        cache,
        lib,
        run,
        log,
        config: config.clone(),
        work,
        runner_log: config.join("logs"),
        artifacts: artifact_root,
    })
}

fn current_work_dir() -> PathBuf {
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn daemon_shared_root(root: PathBuf) -> PathBuf {
    let is_slot = root
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("slot-"))
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        });
    if is_slot {
        root.parent().map(Path::to_path_buf).unwrap_or(root)
    } else {
        root
    }
}

fn docker_visible_path(
    path: &Path,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let Some(host_work_dir) = docker_host_work_dir else {
        return Ok(path.to_path_buf());
    };
    let relative = path.strip_prefix(work_dir).map_err(|_| {
        format!(
            "probe path {} is outside work dir {}; cannot map it to Docker host work dir {}",
            path.display(),
            work_dir.display(),
            host_work_dir.display()
        )
    })?;
    Ok(host_work_dir.join(relative))
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn run_process(program: &str, args: &[&str], timeout: Duration) -> ProcessResult {
    let stdout_path = probe_output_path("stdout");
    let stderr_path = probe_output_path("stderr");
    let stdout_file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&stdout_path)
    {
        Ok(file) => file,
        Err(error) => return ProcessResult::failed(format!("create command output file: {error}")),
    };
    let stderr_file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&stderr_path)
    {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            return ProcessResult::failed(format!("create command error file: {error}"));
        }
    };
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return ProcessResult::failed(format!("spawn {program}: {error}"));
        }
    };
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&stdout_path);
                let _ = fs::remove_file(&stderr_path);
                return ProcessResult::failed(format!("wait for {program}: {error}"));
            }
        }
    };
    let stdout = read_probe_output(&stdout_path);
    let stderr = read_probe_output(&stderr_path);
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);
    ProcessResult {
        status,
        stdout,
        stderr,
        timed_out,
        error: None,
    }
}

fn run_docker_process(target: &DockerTarget, args: &[&str], timeout: Duration) -> ProcessResult {
    let mut command_args = target.cli_args();
    command_args.extend(args.iter().map(|arg| (*arg).to_owned()));
    let command_args = command_args.iter().map(String::as_str).collect::<Vec<_>>();
    run_process("docker", &command_args, timeout)
}

fn probe_output_path(kind: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!(".velnorctl-{kind}-{}-{nanos}", std::process::id()))
}

fn probe_name(kind: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("velnorctl-probe-{kind}-{}-{nanos}", std::process::id())
}

fn read_probe_output(path: &Path) -> String {
    let mut text = String::new();
    if let Ok(mut file) = File::open(path) {
        let _ = file.read_to_string(&mut text);
    }
    text
}

fn parse_json_output(result: &ProcessResult) -> Option<Value> {
    result
        .succeeded()
        .then(|| serde_json::from_str(result.stdout.trim()).ok())
        .flatten()
}

fn parse_context_endpoint(stdout: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(stdout.trim()).ok()?;
    let value = value
        .as_array()
        .and_then(|items| items.first())
        .unwrap_or(&value);
    value
        .get("Endpoints")
        .and_then(|value| value.get("docker"))
        .and_then(|value| value.get("Host"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn json_string(value: &Value, path: &[&str]) -> Option<String> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn json_u64(value: &Value, path: &[&str]) -> Option<u64> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_u64)
}

fn cgroup_mode(driver: Option<&str>, version: Option<&str>) -> String {
    match (driver, version) {
        (Some("systemd"), Some("2")) => "systemd-v2".to_owned(),
        (Some("cgroupfs"), Some("2")) => "cgroupfs-v2".to_owned(),
        (Some(driver), Some(version)) => format!("{driver}-v{version}"),
        (Some(driver), None) => format!("{driver}-unknown"),
        (None, Some(version)) => format!("unknown-v{version}"),
        (None, None) => "unknown".to_owned(),
    }
}

fn docker_resources(info: Option<&Value>, server_reachable: bool) -> DockerResources {
    let cpus = info.and_then(|value| json_u64(value, &["NCPU"]));
    let memory_bytes = info.and_then(|value| json_u64(value, &["MemTotal"]));
    let docker_root_dir = info
        .and_then(|value| json_string(value, &["DockerRootDir"]))
        .map(PathBuf::from);
    let ready = server_reachable
        && cpus.is_some_and(|cpus| cpus > 0)
        && memory_bytes.is_some_and(|memory| memory > 0);
    DockerResources {
        cpus,
        memory_bytes,
        docker_root_dir,
        ready,
    }
}

fn format_resource_detail(resources: &DockerResources) -> String {
    format!(
        "Docker resources: cpus={}, memoryBytes={}, root={}, ready={}",
        resources
            .cpus
            .map_or_else(|| "unknown".to_owned(), |cpus| cpus.to_string()),
        resources
            .memory_bytes
            .map_or_else(|| "unknown".to_owned(), |memory| memory.to_string()),
        resources
            .docker_root_dir
            .as_deref()
            .map_or_else(|| "unknown".to_owned(), |root| root.display().to_string()),
        resources.ready,
    )
}

fn endpoint_kind(endpoint: &str) -> &'static str {
    if endpoint.starts_with("unix://") {
        "unix"
    } else if endpoint.starts_with("tcp://")
        || endpoint.starts_with("ssh://")
        || endpoint.starts_with("http://")
        || endpoint.starts_with("https://")
    {
        "remote"
    } else {
        "unknown"
    }
}

fn unix_socket_path(endpoint: &str) -> Option<PathBuf> {
    endpoint
        .strip_prefix("unix://")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn inspect_socket(path: &Path) -> (bool, Option<PathBuf>, String) {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return (
                false,
                None,
                format!("Docker Unix socket {} does not exist", path.display()),
            );
        }
        Err(error) => {
            return (
                false,
                None,
                format!(
                    "cannot inspect Docker Unix socket {}: {error}",
                    path.display()
                ),
            );
        }
    };
    let target = fs::canonicalize(path).ok();
    #[cfg(unix)]
    let is_socket = {
        use std::os::unix::fs::FileTypeExt;
        metadata.file_type().is_socket()
            || target
                .as_deref()
                .and_then(|target| fs::metadata(target).ok())
                .is_some_and(|metadata| metadata.file_type().is_socket())
    };
    #[cfg(not(unix))]
    let is_socket = false;
    if is_socket {
        let target_display = target
            .as_deref()
            .filter(|target| *target != path)
            .map_or_else(String::new, |target| format!(" -> {}", target.display()));
        (
            true,
            target,
            format!(
                "Docker Unix socket {} is available{}",
                path.display(),
                target_display
            ),
        )
    } else {
        (
            false,
            target,
            format!(
                "Docker endpoint {} exists but is not a Unix socket",
                path.display()
            ),
        )
    }
}

fn detect_provider(
    docker_cli: Option<&Path>,
    endpoint: Option<&str>,
    server_os: Option<&str>,
    info: Option<&Value>,
) -> String {
    let operating_system = info
        .and_then(|value| json_string(value, &["OperatingSystem"]))
        .unwrap_or_default();
    if operating_system.contains("OrbStack")
        || docker_cli.is_some_and(|path| path.to_string_lossy().contains(".orbstack"))
        || endpoint.is_some_and(|value| value.contains(".orbstack"))
    {
        "orbstack".to_owned()
    } else if server_os == Some("linux") && cfg!(target_os = "macos") {
        "macos-docker-vm".to_owned()
    } else {
        "docker".to_owned()
    }
}

fn cgroup_detail(mode: &str, driver: Option<&str>, version: Option<&str>) -> String {
    format!(
        "Docker server reports cgroup mode {mode} (driver={}, version={}); Velnor's Linux runner invariant is systemd cgroup-v2 with parent {JOB_CGROUP_PARENT}",
        driver.unwrap_or("unknown"),
        version.unwrap_or("unknown")
    )
}

fn docker_api_remediation() -> String {
    "Start OrbStack/Docker, verify `docker context show`, and rerun; do not bypass an unavailable Docker API."
        .to_owned()
}

fn socket_remediation(provider: &str) -> String {
    if provider == "orbstack" {
        "Run `orbctl start`, select the OrbStack context with `docker context use default`, and verify the resolved Unix socket exists."
            .to_owned()
    } else {
        "Start Docker, select a local context with `docker context use`, and verify its Unix socket exists."
            .to_owned()
    }
}

fn cgroup_remediation(provider: &str) -> String {
    if provider == "orbstack" {
        "OrbStack can satisfy the macOS Docker endpoint diagnostics. If runnerReady is required, use a Linux host-Docker backend that reports systemd cgroup v2 with the Velnor jobs slice; do not bypass this optional Linux invariant."
            .to_owned()
    } else {
        "If runnerReady is required, use a Linux host-Docker backend that reports systemd cgroup v2 with the Velnor jobs slice; macOS Docker VMs can still be used for endpoint diagnostics."
            .to_owned()
    }
}

fn bind_mount_remediation() -> String {
    "For OrbStack/Docker, keep the work directory under a shared host path, grant the Docker VM access to it, and pass `--docker-host-work-dir` when the daemon sees a different path. Verify the image is local and rerun the probe; a failed mount is not ignored."
        .to_owned()
}

fn check_command(name: &str, result: &ProcessResult, success: &str, remediation: &str) -> Check {
    if result.succeeded() {
        Check::pass(name, success)
    } else {
        Check::fail(
            name,
            process_detail(result, name),
            Some(remediation.to_owned()),
        )
    }
}

fn process_detail(result: &ProcessResult, label: &str) -> String {
    if result.timed_out {
        return format!("{label} timed out after {}s", COMMAND_TIMEOUT.as_secs());
    }
    if let Some(error) = &result.error {
        return error.clone();
    }
    let text = if result.stderr.trim().is_empty() {
        result.stdout.trim()
    } else {
        result.stderr.trim()
    };
    if text.is_empty() {
        format!(
            "{label} exited with code {}",
            result
                .status
                .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
        )
    } else {
        truncate_detail(text)
    }
}

fn truncate_detail(text: &str) -> String {
    if text.len() <= MAX_COMMAND_DETAIL_BYTES {
        return text.replace('\n', " ");
    }
    let mut end = MAX_COMMAND_DETAIL_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].replace('\n', " "))
}

fn preflight_error(report: &PreflightReport) -> CommandError {
    if report
        .docker
        .as_ref()
        .is_some_and(|docker| !docker.server_reachable)
    {
        return CommandError::new(
            ExitClass::Unavailable,
            "docker.endpoint_unavailable",
            docker_api_remediation(),
        );
    }
    let detail = report
        .checks
        .iter()
        .filter(|check| matches!(check.status, CheckStatus::Fail | CheckStatus::Unsupported))
        .map(|check| format!("{}: {}", check.name, check.detail))
        .collect::<Vec<_>>()
        .join("; ");
    let has_unsupported = report
        .checks
        .iter()
        .any(|check| check.status == CheckStatus::Unsupported);
    let has_failure = report
        .checks
        .iter()
        .any(|check| check.status == CheckStatus::Fail);
    CommandError::new(
        ExitClass::Condition,
        if has_unsupported && !has_failure {
            "preflight.linux_invariant_unsupported"
        } else {
            "preflight.capability_unavailable"
        },
        if detail.is_empty() {
            "one or more local preflight capabilities are unavailable".to_owned()
        } else {
            format!("Velnor macOS preflight is not ready: {detail}")
        },
    )
}

fn backend_error(report: &DockerReport) -> CommandError {
    if !report.server_reachable {
        return CommandError::new(
            ExitClass::Unavailable,
            "docker.endpoint_unavailable",
            docker_api_remediation(),
        );
    }
    let detail = report
        .checks
        .iter()
        .filter(|check| matches!(check.status, CheckStatus::Fail | CheckStatus::Unsupported))
        .map(|check| format!("{}: {}", check.name, check.detail))
        .collect::<Vec<_>>()
        .join("; ");
    let reason = if report
        .checks
        .iter()
        .any(|check| check.name == "bind-mount" && check.status == CheckStatus::Fail)
    {
        "docker.bind_mount_unavailable"
    } else if report
        .checks
        .iter()
        .any(|check| check.name == "unix-socket" && check.status == CheckStatus::Fail)
    {
        "docker.socket_unavailable"
    } else if report
        .checks
        .iter()
        .any(|check| check.status == CheckStatus::Unsupported)
    {
        "docker.linux_invariant_unsupported"
    } else {
        "docker.capability_unavailable"
    };
    CommandError::new(
        ExitClass::Condition,
        reason,
        if detail.is_empty() {
            cgroup_remediation(&report.provider)
        } else {
            format!("Velnor Docker capability report is not ready: {detail}")
        },
    )
}

fn emit_report<T: Serialize>(globals: &GlobalArgs, report: &T) -> Result<(), CommandError> {
    if globals.output_format().is_machine() {
        emit_json(report)
    } else {
        match serde_json::to_value(report) {
            Ok(value) => print_report_value(&value),
            Err(error) => Err(CommandError::operation(error.to_string())),
        }
    }
}

fn emit_json<T: Serialize>(value: &T) -> Result<(), CommandError> {
    println!(
        "{}",
        serde_json::to_string(value).map_err(|error| CommandError::operation(format!(
            "serialize diagnostic report: {error}"
        )))?
    );
    Ok(())
}

fn print_report_value(value: &Value) -> Result<(), CommandError> {
    if let Some(report) = value.as_object() {
        if let Some(backend) = report.get("backend") {
            println!("Backend: {}", backend.as_str().unwrap_or("unknown"));
        }
        if let Some(config) = report.get("executionConfig") {
            println!("Execution config: {}", config.as_str().unwrap_or("unknown"));
        }
        if let Some(paths) = report.get("paths").and_then(Value::as_object) {
            let paths = PathsReport {
                mode: paths
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                cache: path_value(paths.get("cache"))?,
                lib: path_value(paths.get("lib"))?,
                run: path_value(paths.get("run"))?,
                log: path_value(paths.get("log"))?,
                config: path_value(paths.get("config"))?,
                work: path_value(paths.get("work"))?,
                runner_log: path_value(paths.get("runnerLog"))?,
                artifacts: path_value(paths.get("artifacts"))?,
            };
            print_paths_human(&paths);
        }
        if let Some(docker) = report.get("docker").and_then(Value::as_object) {
            println!(
                "Docker: endpoint={} source={} provider={} compatible={} runnerReady={}",
                docker
                    .get("endpoint")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                docker
                    .get("endpointSource")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                docker
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                docker
                    .get("velnorCompatible")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                docker
                    .get("runnerReady")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            );
            if let Some(resources) = docker.get("resources").and_then(Value::as_object) {
                println!(
                    "Docker resources: cpus={} memoryBytes={} root={} ready={}",
                    resources
                        .get("cpus")
                        .map_or_else(|| "unknown".to_owned(), Value::to_string),
                    resources
                        .get("memoryBytes")
                        .map_or_else(|| "unknown".to_owned(), Value::to_string),
                    resources
                        .get("dockerRootDir")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                    resources
                        .get("ready")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                );
            }
            println!(
                "Cgroup mode: {}",
                docker
                    .get("cgroupMode")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            );
            if let Some(checks) = docker.get("checks").and_then(Value::as_array) {
                print_checks(checks);
            }
        }
        if let Some(checks) = report.get("checks").and_then(Value::as_array) {
            print_checks(checks);
        }
    }
    Ok(())
}

fn path_value(value: Option<&Value>) -> Result<PathBuf, CommandError> {
    value
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| CommandError::operation("diagnostic report contained a non-path value"))
}

fn print_paths_human(paths: &PathsReport) {
    println!("Path\tLocation");
    println!("mode\t{}", paths.mode);
    println!("cache\t{}", paths.cache.display());
    println!("lib\t{}", paths.lib.display());
    println!("run\t{}", paths.run.display());
    println!("log\t{}", paths.log.display());
    println!("config\t{}", paths.config.display());
    println!("work\t{}", paths.work.display());
    println!("runner-log\t{}", paths.runner_log.display());
    println!("artifacts\t{}", paths.artifacts.display());
}

fn print_docker_human(report: &DockerReport) {
    println!("Docker endpoint report (schema {})", report.schema_version);
    println!("Host: {} {}", report.host_os, report.host_arch);
    println!(
        "CLI: {}",
        report
            .docker_cli
            .as_deref()
            .map_or("unavailable".to_owned(), |path| path.display().to_string())
    );
    println!("Endpoint source: {}", report.endpoint_source);
    println!(
        "Context: {}",
        report.context.as_deref().unwrap_or("direct endpoint")
    );
    println!(
        "Endpoint: {}",
        report.endpoint.as_deref().unwrap_or("unknown")
    );
    println!("Provider: {}", report.provider);
    println!(
        "Server: {} {} {}",
        report
            .server_reachable
            .then_some("reachable")
            .unwrap_or("unavailable"),
        report.server_os.as_deref().unwrap_or("unknown"),
        report.server_arch.as_deref().unwrap_or("unknown")
    );
    println!(
        "Cgroup: mode={} driver={} version={} systemd-cgroup-v2={}",
        report.cgroup_mode,
        report.cgroup_driver.as_deref().unwrap_or("unknown"),
        report.cgroup_version.as_deref().unwrap_or("unknown"),
        report.capabilities.systemd_cgroup_v2
    );
    println!("{}", format_resource_detail(&report.resources));
    println!("Velnor-compatible: {}", report.velnor_compatible);
    println!("Runner-ready: {}", report.runner_ready);
    print_checks_from_struct(&report.checks);
}

fn print_checks_from_struct(checks: &[Check]) {
    println!("Check\tStatus\tDetail");
    for check in checks {
        println!(
            "{}\t{}\t{}",
            check.name,
            match check.status {
                CheckStatus::Pass => "pass",
                CheckStatus::Fail => "fail",
                CheckStatus::Unsupported => "unsupported",
                CheckStatus::Skipped => "skipped",
            },
            check.detail
        );
        if let Some(remediation) = &check.remediation {
            println!("remediation\t-\t{remediation}");
        }
    }
}

fn print_checks(checks: &[Value]) {
    println!("Check\tStatus\tDetail");
    for check in checks {
        println!(
            "{}\t{}\t{}",
            check
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            check
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            check
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        );
        if let Some(remediation) = check.get("remediation").and_then(Value::as_str) {
            println!("remediation\t-\t{remediation}");
        }
    }
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for path in paths {
        if !result.iter().any(|candidate| candidate == &path) {
            result.push(path);
        }
    }
    result
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
    fn context_endpoint_parser_accepts_docker_context_shape() {
        let endpoint = parse_context_endpoint(
            r#"[{"Endpoints":{"docker":{"Host":"unix:///var/run/docker.sock"}}}]"#,
        );
        assert_eq!(endpoint.as_deref(), Some("unix:///var/run/docker.sock"));
    }

    #[test]
    fn endpoint_parser_distinguishes_local_unix_from_remote() {
        assert_eq!(endpoint_kind("unix:///var/run/docker.sock"), "unix");
        assert_eq!(endpoint_kind("ssh://docker.example"), "remote");
        assert_eq!(endpoint_kind("tcp://127.0.0.1:2375"), "remote");
        assert_eq!(endpoint_kind("not-an-endpoint"), "unknown");
    }

    #[test]
    fn macos_cgroup_check_never_claims_linux_systemd_boundary() {
        let mode = cgroup_mode(Some("cgroupfs"), Some("2"));
        let detail = cgroup_detail(&mode, Some("cgroupfs"), Some("2"));
        assert_eq!(mode, "cgroupfs-v2");
        assert!(detail.contains("Linux runner invariant"));
        assert!(detail.contains("cgroupfs"));
        assert!(
            !DockerCapabilities {
                api: true,
                local_socket: true,
                buildx: true,
                bind_mount: Some(true),
                systemd_cgroup_v2: false,
            }
            .systemd_cgroup_v2
        );
    }

    #[test]
    fn daemon_shared_artifacts_lift_slot_work_root() {
        let root = PathBuf::from("/tmp/velnor/work/slot-3");
        assert_eq!(daemon_shared_root(root), PathBuf::from("/tmp/velnor/work"));
    }

    #[test]
    fn invalid_host_work_mapping_is_reported() {
        let error = docker_visible_path(
            Path::new("/tmp/other/marker"),
            Path::new("/tmp/work"),
            Some(Path::new("/vm/work")),
        )
        .expect_err("outside work dir must not be silently remapped");
        assert!(error.contains("outside work dir"));
    }

    #[test]
    fn docker_target_precedence_matches_runner_environment_contract() {
        assert_eq!(
            resolve_docker_target_from(
                Some("unix:///velnor.sock".to_owned()),
                Some("velnor".to_owned()),
                Some("tcp://ambient:2375".to_owned()),
                Some("ambient".to_owned()),
            ),
            DockerTarget::Host {
                endpoint: "unix:///velnor.sock".to_owned(),
                source: "VELNOR_DOCKER_HOST",
            }
        );
        assert_eq!(
            resolve_docker_target_from(
                None,
                Some("velnor".to_owned()),
                Some("tcp://ambient:2375".to_owned()),
                Some("ambient".to_owned()),
            ),
            DockerTarget::Context {
                name: Some("velnor".to_owned()),
                source: "VELNOR_DOCKER_CONTEXT",
            }
        );
        assert_eq!(
            resolve_docker_target_from(
                None,
                None,
                Some("tcp://ambient:2375".to_owned()),
                Some("ambient".to_owned()),
            ),
            DockerTarget::Context {
                name: Some("ambient".to_owned()),
                source: "DOCKER_CONTEXT",
            }
        );
        assert_eq!(
            resolve_docker_target_from(None, None, Some("tcp://ambient:2375".to_owned()), None,),
            DockerTarget::Host {
                endpoint: "tcp://ambient:2375".to_owned(),
                source: "DOCKER_HOST",
            }
        );
        assert_eq!(
            resolve_docker_target_from(None, None, None, None),
            DockerTarget::Context {
                name: None,
                source: "docker-default",
            }
        );
    }

    #[test]
    fn docker_report_reads_resources_without_inventing_readiness() {
        let info = serde_json::json!({
            "NCPU": 8,
            "MemTotal": 16_000_000_000_u64,
            "DockerRootDir": "/var/lib/docker"
        });
        let resources = docker_resources(Some(&info), true);
        assert_eq!(resources.cpus, Some(8));
        assert_eq!(resources.memory_bytes, Some(16_000_000_000));
        assert_eq!(
            resources.docker_root_dir,
            Some(PathBuf::from("/var/lib/docker"))
        );
        assert!(resources.ready);
        assert!(!docker_resources(Some(&info), false).ready);
    }
}

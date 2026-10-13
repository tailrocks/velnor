//! On-demand host entry point: start repository-scoped Docker-backed capacity.
//!
//! `velnorctl host start` wraps `configure`+`daemon` with defaults. It never
//! joins an organization runner group. GitHub remains the job scheduler.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use velnor_model::ExitClass;

use crate::commands::{HostBootstrapImageArgs, HostCommand, HostStartArgs};
use crate::runtime::{self, DaemonArgs};
use crate::{CommandError, GlobalArgs};

const DEFAULT_REPO: &str = "tailrocks/velnor";
const HOST_PID_FILE: &str = "host.pid";

pub async fn run(globals: &GlobalArgs, command: HostCommand) -> Result<(), CommandError> {
    match command {
        HostCommand::Start(args) => start(globals, args).await,
        HostCommand::BootstrapImage(args) => bootstrap_image(args),
        HostCommand::Status => status(globals).await,
        HostCommand::Drain => drain(globals).await,
        HostCommand::Stop => stop(globals).await,
    }
}

async fn start(globals: &GlobalArgs, args: HostStartArgs) -> Result<(), CommandError> {
    crate::ensure_native_github_http_transport();
    ensure_dev_canonical_storage()?;
    ensure_dev_service_binary()?;
    let url = resolve_repo_url(&args)?;
    if github_pat().is_none() {
        return Err(CommandError::new(
            ExitClass::Usage,
            "host.token_missing",
            "GITHUB_TOKEN is unset. Export a short-lived registration token \
             with repo administration rights; do not pass the token as a flag.",
        ));
    }

    let name = effective_host_name(globals, &args)?;
    let slots = args.slots.max(1);
    let socket = velnor_client::socket_root();
    let docker = docker_endpoint_display();
    let execution = execution_platform();

    println!("host start");
    if let Ok(root) = env::var("VELNOR_STORAGE_ROOT")
        && !root.is_empty()
    {
        println!("  storage_root     {root}");
    }
    if let Ok(runner) = env::var("VELNOR_SERVICE_BINARY")
        && !runner.is_empty()
    {
        println!("  service_binary   {runner}");
    }
    println!("  github_scope     {url} (repository)");
    println!("  runner_name      {name}");
    println!("  slots            {slots}");
    println!("  docker_endpoint  {docker}");
    println!("  execution        {execution}");
    println!(
        "  host_platform    {}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("  control_socket   unix://{}/{name}", socket.display());
    println!("  pool             none (repository-scoped; will not join velnor-trusted)");
    if let Some(pr) = args.pr {
        print_pr_scheduling(pr);
    }
    println!("  drain/stop       Ctrl-C this process, or `velnorctl host drain` then Ctrl-C");

    let config_dir = resolve_host_config_dir(&args, &name)?;
    let state_db = config_dir.join("state.db");
    ensure_dev_state_db_env(&state_db)?;
    ensure_docker_execution_file(&config_dir, slots)?;
    let docker_image = args
        .docker_image
        .clone()
        .unwrap_or_else(|| "velnor/job-ubuntu:26.04".into());
    ensure_local_job_image(&docker_image)?;
    println!("  config_dir       {}", config_dir.display());
    println!("  job_image        {docker_image}");

    let trust_scope = env::var("VELNOR_TRUST_SCOPE").unwrap_or_else(|_| "untrusted".into());
    let daemon = DaemonArgs {
        state_db: Some(state_db),
        config_dir: Some(config_dir),
        url: Some(url),
        pat: github_pat(),
        name: Some(name.clone()),
        labels: host_start_labels(&trust_scope),
        target_mvp_labels: arch_claims_x64_target_pack(std::env::consts::ARCH),
        target_mvp_arm_label: cfg!(target_arch = "aarch64"),
        replace: false,
        pool_id: None,
        pool_name: None,
        routing_policy_file: None,
        dry_run_registration: false,
        slots,
        max_idle_slot_age_seconds: None,
        once: false,
        idle_timeout_seconds: None,
        complete_noop: false,
        execute_scripts: false,
        dry_run_jobs: false,
        dump_job_message: None,
        docker_image,
        job_cpus: String::new(),
        job_memory: String::new(),
        trust: velnor_runner::trust_scope::TrustScopeArg { trust_scope },
        emergency_reserve_bytes: 10_737_418_240,
        job_peak_bytes: 32_212_254_720,
        node_action_image: String::new(),
        work_dir: args.work_dir,
        docker_host_work_dir: args.docker_host_work_dir,
        skip_preflight: false,
        require_docker_socket: true,
    };
    let instance_dir = socket.join(&name);
    crate::http::prepare_instance_dir(&instance_dir).map_err(|error| {
        CommandError::new(
            ExitClass::Operation,
            "host.socket_dir_unavailable",
            format!("cannot prepare {}: {error}", instance_dir.display()),
        )
    })?;
    let _pid_guard = HostProcessGuard::install(&instance_dir, &name)?;
    println!("  process_pid      {}", std::process::id());

    runtime::run_daemon(daemon).await.map_err(|error| {
        CommandError::new(
            ExitClass::Operation,
            "host.start_failed",
            format!("unable to start on-demand host: {error}"),
        )
    })
}

async fn status(globals: &GlobalArgs) -> Result<(), CommandError> {
    ensure_dev_canonical_storage()?;
    println!(
        "control_socket_root {}",
        velnor_client::socket_root().display()
    );
    println!(
        "package_socket_mode {}",
        velnor_client::is_package_socket_mode()
    );
    println!("docker_endpoint     {}", docker_endpoint_display());
    println!("execution           {}", execution_platform());
    let hosts = discover_hosts(globals.instance.as_deref())?;
    println!("active_hosts        {}", hosts.len());
    for host in hosts {
        println!("host                {}", host.name);
        println!("  process_pid       {}", host.pid);
        println!("  process           {}", host.command);
        println!(
            "  control_socket    {}",
            host.instance_dir.join("control.sock").display()
        );
        let endpoint = velnor_client::UnixEndpoint::from_instance(&host.name).map_err(|error| {
            CommandError::new(ExitClass::Usage, "host.instance_invalid", error.to_string())
        })?;
        let client =
            velnor_client::UnixControlClient::new(endpoint).with_timeout(Duration::from_secs(2));
        match client.info().await {
            Ok(info) => println!(
                "  control_api       {} schema={} mutations={}",
                info.api_version, info.schema_version, info.mutations
            ),
            Err(error) => println!("  control_api       unavailable ({error})"),
        }
        if let Some(health) = read_host_health(&host.name) {
            println!("  health            {health}");
        }
    }
    println!("follow              velnorctl --instance <name> get slots");
    println!("docker_report       velnorctl docker report");
    Ok(())
}

async fn drain(globals: &GlobalArgs) -> Result<(), CommandError> {
    ensure_dev_canonical_storage()?;
    let host = resolve_single_host(globals.instance.as_deref())?;
    signal_host(&host, libc::SIGTERM)?;
    println!(
        "drain requested for {} (pid {}); active jobs finish, waiters and idle slots exit",
        host.name, host.pid
    );
    Ok(())
}

async fn stop(globals: &GlobalArgs) -> Result<(), CommandError> {
    ensure_dev_canonical_storage()?;
    let host = resolve_single_host(globals.instance.as_deref())?;
    signal_host(&host, libc::SIGTERM)?;
    let timeout = globals.timeout.unwrap_or(Duration::from_secs(30));
    if wait_for_exit(host.pid, timeout).await {
        println!("host stopped {} (pid {})", host.name, host.pid);
        return Ok(());
    }
    Err(CommandError::new(
        ExitClass::Timeout,
        "host.stop_timeout",
        format!(
            "host {} is still draining after {}; no force-kill was attempted; retry `velnorctl host stop --instance {}`",
            host.name,
            format_duration(timeout),
            host.name
        ),
    ))
}

fn effective_host_name(globals: &GlobalArgs, args: &HostStartArgs) -> Result<String, CommandError> {
    if let (Some(global), Some(local)) = (globals.instance.as_deref(), args.name.as_deref())
        && global != local
    {
        return Err(CommandError::new(
            ExitClass::Usage,
            "host.instance_conflict",
            "--instance and --name must identify the same host",
        ));
    }
    Ok(globals
        .instance
        .clone()
        .or_else(|| args.name.clone())
        .unwrap_or_else(|| format!("velnor-local-{}", hostname_slug())))
}

#[derive(Debug, Clone)]
struct HostProcess {
    name: String,
    instance_dir: PathBuf,
    pid: u32,
    command: String,
}

struct HostProcessGuard {
    path: PathBuf,
    pid: u32,
}

impl HostProcessGuard {
    fn install(instance_dir: &Path, name: &str) -> Result<Self, CommandError> {
        let path = instance_dir.join(HOST_PID_FILE);
        let pid = std::process::id();
        if let Some(existing) = read_pid_file(&path)
            && process_alive(existing)
        {
            return Err(CommandError::new(
                ExitClass::Conflict,
                "host.already_running",
                format!("host {name} is already running as pid {existing}"),
            ));
        }
        if path.exists() {
            fs::remove_file(&path).map_err(|error| {
                CommandError::new(
                    ExitClass::Operation,
                    "host.pid_unwritable",
                    format!("remove stale {}: {error}", path.display()),
                )
            })?;
        }
        let temporary = instance_dir.join(format!(".{HOST_PID_FILE}.{pid}.tmp"));
        let _ = fs::remove_file(&temporary);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|error| {
            CommandError::new(
                ExitClass::Operation,
                "host.pid_unwritable",
                format!("create {}: {error}", temporary.display()),
            )
        })?;
        writeln!(file, "{pid}").map_err(|error| {
            CommandError::new(
                ExitClass::Operation,
                "host.pid_unwritable",
                format!("write {}: {error}", temporary.display()),
            )
        })?;
        file.sync_all().map_err(|error| {
            CommandError::new(
                ExitClass::Operation,
                "host.pid_unwritable",
                format!("sync {}: {error}", temporary.display()),
            )
        })?;
        fs::rename(&temporary, &path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            CommandError::new(
                ExitClass::Operation,
                "host.pid_unwritable",
                format!("install {}: {error}", path.display()),
            )
        })?;
        Ok(Self { path, pid })
    }
}

impl Drop for HostProcessGuard {
    fn drop(&mut self) {
        if read_pid_file(&self.path) == Some(self.pid) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn discover_hosts(requested: Option<&str>) -> Result<Vec<HostProcess>, CommandError> {
    let root = velnor_client::socket_root();
    if let Some(name) = requested {
        validate_host_name(name)?;
        let instance_dir = root.join(name);
        if !instance_dir.is_dir() {
            return Ok(Vec::new());
        }
        let pid =
            read_pid_file(&instance_dir.join(HOST_PID_FILE)).or_else(|| find_host_process(name));
        return Ok(pid
            .filter(|pid| process_alive(*pid))
            .map(|pid| vec![host_process(name.to_owned(), instance_dir, pid)])
            .unwrap_or_default());
    }
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(CommandError::new(
                ExitClass::Operation,
                "host.socket_root_unreadable",
                format!("read {}: {error}", root.display()),
            ));
        }
    };
    let mut hosts = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            CommandError::new(
                ExitClass::Operation,
                "host.socket_root_unreadable",
                format!("read host entry: {error}"),
            )
        })?;
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let instance_dir = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let pid =
            read_pid_file(&instance_dir.join(HOST_PID_FILE)).or_else(|| find_host_process(&name));
        if let Some(pid) = pid.filter(|pid| process_alive(*pid)) {
            hosts.push(host_process(name, instance_dir, pid));
        }
    }
    hosts.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(hosts)
}

fn resolve_single_host(requested: Option<&str>) -> Result<HostProcess, CommandError> {
    let hosts = discover_hosts(requested)?;
    match hosts.as_slice() {
        [host] => Ok(host.clone()),
        [] => Err(CommandError::new(
            ExitClass::Unavailable,
            "host.not_running",
            requested.map_or_else(
                || {
                    "no on-demand host process is running; pass --instance NAME after starting one"
                        .to_owned()
                },
                |name| format!("on-demand host {name} is not running"),
            ),
        )),
        _ => Err(CommandError::new(
            ExitClass::Usage,
            "host.instance_required",
            "multiple on-demand hosts are running; pass --instance NAME",
        )),
    }
}

fn host_process(name: String, instance_dir: PathBuf, pid: u32) -> HostProcess {
    let command = process_command(pid).unwrap_or_else(|| "unavailable".to_owned());
    HostProcess {
        name,
        instance_dir,
        pid,
        command,
    }
}

fn validate_host_name(name: &str) -> Result<(), CommandError> {
    velnor_client::UnixEndpoint::from_instance(name).map_err(|error| {
        CommandError::new(ExitClass::Usage, "host.instance_invalid", error.to_string())
    })?;
    Ok(())
}

fn read_pid_file(path: &Path) -> Option<u32> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 1)
}

fn process_command(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .chars()
            .take(512)
            .collect()
    })
}

fn find_host_process(name: &str) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .ok()?;
    let name_arg = format!("--name {name}");
    let instance_arg = format!("--instance {name}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains("host start") && line.contains("velnorctl"))
        .filter(|line| line.contains(&name_arg) || line.contains(&instance_arg))
        .find_map(|line| line.split_whitespace().next()?.parse().ok())
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 probes a process without changing its state.
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn signal_host(host: &HostProcess, signal: libc::c_int) -> Result<(), CommandError> {
    if !host.command.contains("host start") || !host.command.contains("velnorctl") {
        return Err(CommandError::new(
            ExitClass::Conflict,
            "host.pid_identity_failed",
            format!("pid {} is not a Velnor host process", host.pid),
        ));
    }
    // SAFETY: the process identity was read from the host-owned pid file or
    // matched against the exact host-start command line immediately above.
    let result = unsafe { libc::kill(host.pid as libc::pid_t, signal) };
    if result != 0 {
        return Err(CommandError::new(
            ExitClass::Operation,
            "host.signal_failed",
            format!(
                "signal pid {}: {}",
                host.pid,
                std::io::Error::last_os_error()
            ),
        ));
    }
    Ok(())
}

async fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while process_alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    true
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn read_host_health(name: &str) -> Option<String> {
    let config = velnor_runner::config_dir(None)
        .ok()?
        .join("hosts")
        .join(name);
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(config.join("health.json")).ok()?).ok()?;
    let object = value.as_object()?;
    let fields = [
        "state",
        "desired_ready_slots",
        "actual_ready_slots",
        "registered_slots",
        "job_processes",
        "routing_valid",
        "github_reachable",
        "execution_backend",
    ];
    let pairs = fields
        .iter()
        .filter_map(|field| object.get(*field).map(|value| format!("{field}={value}")));
    Some(pairs.collect::<Vec<_>>().join(" "))
}

fn resolve_repo_url(args: &HostStartArgs) -> Result<String, CommandError> {
    let raw = match (args.url.as_deref(), args.repo.as_deref()) {
        (Some(url), _) => url.trim().to_owned(),
        (None, Some(repo)) => format!("https://github.com/{}", repo.trim().trim_matches('/')),
        (None, None) => format!("https://github.com/{DEFAULT_REPO}"),
    };
    let Some(path) = github_repo_path(&raw) else {
        return Err(CommandError::new(
            ExitClass::Usage,
            "host.url_invalid",
            format!("host start needs a repository URL (owner/name), got {raw}"),
        ));
    };
    if path.matches('/').count() != 1 {
        return Err(CommandError::new(
            ExitClass::Usage,
            "host.org_scope_refused",
            format!(
                "{raw} is an organization or enterprise URL. Repository-scoped \
                 recovery will not join velnor-trusted or any other org pool. \
                 Pass --repo owner/name."
            ),
        ));
    }
    Ok(raw.trim_end_matches('/').to_owned())
}

/// Whether an on-demand host on `arch` may claim the x64 target pack
/// (`ubuntu-24.04`, `ubuntu-latest`, `hetzner-sentry-ci`). An arm64 host
/// claiming those labels would attract jobs it cannot run natively and
/// would impersonate Sentry's pool identity, so only x86_64 claims them.
fn arch_claims_x64_target_pack(arch: &str) -> bool {
    matches!(arch, "x86_64" | "x64")
}

/// Labels an on-demand host claims. Untrusted hosts must never claim
/// `velnor-host-docker`: gated jobs need the host Docker socket, which
/// only the exact `trusted` scope receives. A trusted recovery host
/// claims the label so it can take those jobs.
fn host_start_labels(trust_scope: &str) -> Vec<String> {
    let mut labels = vec!["self-hosted".into(), "velnor-target-mvp".into()];
    if velnor_runner::runner::github_trust_scope_allows_host_docker(trust_scope) {
        labels.push(velnor_runner::runner::TRUST_GATED_RUNNER_LABEL.into());
    }
    labels
}

fn github_repo_path(url: &str) -> Option<&str> {
    url.strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .map(|path| path.trim_matches('/'))
}

fn ensure_dev_state_db_env(state_db: &Path) -> Result<(), CommandError> {
    if env::var_os("VELNOR_STATE_DB")
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return Ok(());
    }
    if let Some(parent) = state_db.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            CommandError::new(
                ExitClass::Operation,
                "host.state_db_unwritable",
                format!("cannot create {}: {error}", parent.display()),
            )
        })?;
    }
    // SAFETY: `host start` is single-threaded until slot/job children are spawned.
    unsafe { env::set_var("VELNOR_STATE_DB", state_db) };
    Ok(())
}

fn ensure_dev_canonical_storage() -> Result<(), CommandError> {
    if env::var_os("VELNOR_STORAGE_ROOT")
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return Ok(());
    }
    let home = env::var("HOME").map_err(|error| {
        CommandError::new(
            ExitClass::Usage,
            "host.home_missing",
            format!("HOME is unset; export VELNOR_STORAGE_ROOT or set HOME: {error}"),
        )
    })?;
    // macOS Unix sockets are limited to 104 bytes: `~/Library/Application
    // Support/velnor/run/velnor/<name>/control.sock` overflows SUN_LEN, so
    // the on-demand default stays a short dot-directory instead.
    let prefix = if cfg!(target_os = "macos") {
        PathBuf::from(&home).join(".velnor-store")
    } else {
        PathBuf::from(&home).join(".local/state/velnor")
    };
    // SAFETY: `host start` is single-threaded until the daemon child is spawned.
    unsafe { env::set_var("VELNOR_STORAGE_ROOT", &prefix) };
    Ok(())
}

fn ensure_dev_service_binary() -> Result<(), CommandError> {
    if env::var_os("VELNOR_SERVICE_BINARY")
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return Ok(());
    }
    let exe = env::current_exe().map_err(|error| {
        CommandError::new(
            ExitClass::Operation,
            "host.exe_unavailable",
            format!("cannot resolve current executable: {error}"),
        )
    })?;
    let Some(dir) = exe.parent() else {
        return Ok(());
    };
    let runner = dir.join(if cfg!(windows) {
        "velnor-runner.exe"
    } else {
        "velnor-runner"
    });
    if !runner.is_file() {
        return Err(CommandError::new(
            ExitClass::Condition,
            "host.service_binary_missing",
            format!(
                "velnor-runner not found beside {}. Build it with \
                 `cargo build --locked -p velnor-runner`.",
                exe.display()
            ),
        ));
    }
    // SAFETY: `host start` is single-threaded until the daemon child is spawned.
    unsafe { env::set_var("VELNOR_SERVICE_BINARY", &runner) };
    Ok(())
}

fn resolve_host_config_dir(args: &HostStartArgs, name: &str) -> Result<PathBuf, CommandError> {
    if let Some(dir) = args.config_dir.clone() {
        return Ok(dir);
    }
    let root = velnor_runner::config_dir(None).map_err(|error| {
        CommandError::new(
            ExitClass::Usage,
            "host.config_dir_missing",
            format!("pass --config-dir or set HOME: {error}"),
        )
    })?;
    // Per-instance journal. Reusing a shared runner/ directory inherits
    // journal.capacity.invalid when a later start uses fewer slots.
    Ok(root.join("hosts").join(name))
}

fn ensure_docker_execution_file(config_dir: &Path, slots: usize) -> Result<(), CommandError> {
    const EXECUTION_TOML: &str = "[execution]\nbackend = \"docker\"\n";
    write_execution_toml_if_missing(config_dir, EXECUTION_TOML)?;
    if slots > 1 {
        for slot in 1..=slots {
            let slot_dir = config_dir.join("slots").join(format!("slot-{slot}"));
            write_execution_toml_if_missing(&slot_dir, EXECUTION_TOML)?;
        }
    }
    Ok(())
}

fn write_execution_toml_if_missing(dir: &Path, content: &str) -> Result<(), CommandError> {
    std::fs::create_dir_all(dir).map_err(|error| {
        CommandError::new(
            ExitClass::Operation,
            "host.config_dir_unwritable",
            format!("cannot create {}: {error}", dir.display()),
        )
    })?;
    let path = dir.join("execution.toml");
    if path.exists() {
        return Ok(());
    }
    std::fs::write(&path, content).map_err(|error| {
        CommandError::new(
            ExitClass::Operation,
            "host.execution_toml_unwritable",
            format!("cannot write {}: {error}", path.display()),
        )
    })?;
    println!("  wrote            {}", path.display());
    Ok(())
}

fn ensure_local_job_image(image: &str) -> Result<(), CommandError> {
    let output = std::process::Command::new("docker")
        .args(["image", "inspect", image])
        .output()
        .map_err(|error| {
            CommandError::new(
                ExitClass::Condition,
                "host.docker_unavailable",
                format!("docker image inspect failed to start: {error}. Start OrbStack/Docker."),
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(CommandError::new(
        ExitClass::Condition,
        "host.job_image_missing",
        format!(
            "job image {image} is not on this Docker daemon. From the repository root:\n\
             export GITHUB_TOKEN  # mise downloads; never pass as a flag\n\
             velnorctl host bootstrap-image\n\
             That compiles linux velnor-workflow and builds docker/job-ubuntu.Dockerfile locally. GHCR is not required."
        ),
    ))
}

fn bootstrap_image(args: HostBootstrapImageArgs) -> Result<(), CommandError> {
    let root = find_repo_root()?;
    let targetarch = docker_targetarch()?;
    let image = args
        .docker_image
        .clone()
        .unwrap_or_else(|| "velnor/job-ubuntu:26.04".into());

    println!("host bootstrap-image");
    println!("  repo_root        {}", root.display());
    println!("  docker_arch      {targetarch}");
    println!("  job_image        {image}");
    println!("  rust_image       {RUST_BOOTSTRAP_IMAGE}");

    if docker_image_present(&image) && !args.rebuild_workflow {
        println!("  status           {image} already present; nothing to build");
        return Ok(());
    }

    if github_pat().is_none() {
        return Err(CommandError::new(
            ExitClass::Usage,
            "host.token_missing",
            "GITHUB_TOKEN is unset. Export it for the Dockerfile mise_github_token secret; \
             do not pass the token as a flag.",
        ));
    }

    ensure_linux_workflow_binary(&root, &targetarch, args.rebuild_workflow)?;
    build_job_image(&root, &image)?;
    if !docker_image_present(&image) {
        return Err(CommandError::new(
            ExitClass::Operation,
            "host.job_image_build_failed",
            format!("docker build finished but {image} is still missing"),
        ));
    }
    println!("  status           {image} ready");
    Ok(())
}

const RUST_BOOTSTRAP_IMAGE: &str = "rust:1.98.1-bookworm";
const JOB_DOCKERFILE: &str = "docker/job-ubuntu.Dockerfile";

fn find_repo_root() -> Result<PathBuf, CommandError> {
    let cwd = env::current_dir().map_err(|error| {
        CommandError::new(
            ExitClass::Usage,
            "host.repo_root_missing",
            format!("cannot read current directory: {error}"),
        )
    })?;
    let mut dir = cwd.as_path();
    loop {
        if dir.join(JOB_DOCKERFILE).is_file()
            && dir.join("Cargo.toml").is_file()
            && dir.join("crates/velnor-workflow").is_dir()
        {
            return Ok(dir.to_path_buf());
        }
        dir = dir.parent().ok_or_else(|| {
            CommandError::new(
                ExitClass::Usage,
                "host.repo_root_missing",
                format!(
                    "run velnorctl host bootstrap-image from a Velnor checkout containing {JOB_DOCKERFILE}"
                ),
            )
        })?;
    }
}

fn docker_targetarch() -> Result<String, CommandError> {
    let output = Command::new("docker")
        .args(["version", "--format", "{{.Server.Arch}}"])
        .output()
        .map_err(|error| {
            CommandError::new(
                ExitClass::Condition,
                "host.docker_unavailable",
                format!("docker version failed to start: {error}. Start OrbStack/Docker."),
            )
        })?;
    if !output.status.success() {
        return Err(CommandError::new(
            ExitClass::Condition,
            "host.docker_unavailable",
            format!(
                "docker version failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    docker_arch_to_targetarch(raw.trim())
        .map(str::to_owned)
        .ok_or_else(|| {
            CommandError::new(
                ExitClass::Condition,
                "host.unsupported_arch",
                format!(
                    "Docker server architecture {} is not a Velnor job-image TARGETARCH",
                    raw.trim()
                ),
            )
        })
}

fn docker_arch_to_targetarch(arch: &str) -> Option<&'static str> {
    match arch {
        "arm64" | "aarch64" => Some("arm64"),
        "amd64" | "x86_64" => Some("amd64"),
        _ => None,
    }
}

fn docker_image_present(image: &str) -> bool {
    Command::new("docker")
        .args(["image", "inspect", image])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn ensure_linux_workflow_binary(
    root: &Path,
    targetarch: &str,
    rebuild: bool,
) -> Result<(), CommandError> {
    let dest = root
        .join("release-binaries")
        .join(targetarch)
        .join("velnor-workflow");
    if dest.is_file() && dest.metadata().map(|m| m.len() > 0).unwrap_or(false) && !rebuild {
        println!("  workflow         {} (existing)", dest.display());
        return Ok(());
    }

    println!("  workflow         compiling velnor-workflow for linux/{targetarch}");
    let script = format!(
        "set -euo pipefail\n\
         cargo build -p velnor-workflow --release --locked\n\
         mkdir -p release-binaries/{targetarch}\n\
         install -m 0755 .velnor-bootstrap/target/release/velnor-workflow \
           release-binaries/{targetarch}/velnor-workflow\n"
    );
    run_visible(
        root,
        "docker",
        &[
            "run",
            "--rm",
            "--platform",
            &format!("linux/{targetarch}"),
            "-v",
            &format!("{}:/src", root.display()),
            "-w",
            "/src",
            "-e",
            "CARGO_HOME=/src/.velnor-bootstrap/cargo-home",
            "-e",
            "CARGO_TARGET_DIR=/src/.velnor-bootstrap/target",
            "-e",
            "RUSTUP_TOOLCHAIN=1.98.1",
            "-e",
            "PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            RUST_BOOTSTRAP_IMAGE,
            "bash",
            "-c",
            &script,
        ],
        "host.workflow_build_failed",
        "linux velnor-workflow compile",
    )?;
    if !dest.is_file() {
        return Err(CommandError::new(
            ExitClass::Operation,
            "host.workflow_build_failed",
            format!("compile finished but {} is missing", dest.display()),
        ));
    }
    println!("  workflow         {}", dest.display());
    Ok(())
}

fn build_job_image(root: &Path, image: &str) -> Result<(), CommandError> {
    println!("  image            docker build --file {JOB_DOCKERFILE} --tag {image}");
    let status = Command::new("docker")
        .args([
            "build",
            "--file",
            JOB_DOCKERFILE,
            "--tag",
            image,
            "--secret",
            "id=mise_github_token,env=GITHUB_TOKEN",
            ".",
        ])
        .current_dir(root)
        .env("DOCKER_BUILDKIT", "1")
        .stdin(Stdio::null())
        .status()
        .map_err(|error| {
            CommandError::new(
                ExitClass::Condition,
                "host.job_image_build_failed",
                format!("job image source build failed to start: {error}"),
            )
        })?;
    if status.success() {
        return Ok(());
    }
    Err(CommandError::new(
        ExitClass::Operation,
        "host.job_image_build_failed",
        format!("job image source build failed with {status}"),
    ))
}

fn run_visible(
    cwd: &Path,
    program: &str,
    args: &[&str],
    reason: &'static str,
    label: &str,
) -> Result<(), CommandError> {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .status()
        .map_err(|error| {
            CommandError::new(
                ExitClass::Condition,
                reason,
                format!("{label} failed to start ({program}): {error}"),
            )
        })?;
    if status.success() {
        return Ok(());
    }
    Err(CommandError::new(
        ExitClass::Operation,
        reason,
        format!("{label} failed with {status}"),
    ))
}

fn github_pat() -> Option<String> {
    env::var("GITHUB_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
}

fn hostname_slug() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| {
            value
                .chars()
                .flat_map(|ch| ch.to_lowercase())
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .take(24)
                .collect::<String>()
                .trim_matches('-')
                .to_owned()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "host".into())
}

fn docker_endpoint_display() -> String {
    for key in ["VELNOR_DOCKER_HOST", "DOCKER_HOST"] {
        if let Ok(value) = env::var(key)
            && !value.is_empty()
        {
            return format!("{value} (from {key})");
        }
    }
    if PathBuf::from("/var/run/docker.sock").exists() {
        return "unix:///var/run/docker.sock".into();
    }
    if let Some(home) = env::var_os("HOME") {
        let orb = PathBuf::from(home).join(".orbstack/run/docker.sock");
        if orb.exists() {
            return format!("unix://{} (orbstack)", orb.display());
        }
    }
    "unavailable (start Docker/OrbStack; host start will not switch backends)".into()
}

fn execution_platform() -> String {
    if std::env::consts::OS == "macos" {
        "linux-container (Docker VM on macOS; not a native macOS job)".into()
    } else {
        format!(
            "linux-container on {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    }
}

fn print_pr_scheduling(pr: u64) {
    println!(
        "  pr_target        #{pr}: GitHub remains the scheduler. This session registers \
         repository-scoped runners only."
    );
    println!(
        "  pr_target        Jobs already queued with `group: velnor-trusted` cannot be \
         claimed by this host. Changing labels or YAML does not rewrite those jobs."
    );
    println!(
        "  pr_target        A workflow_dispatch against the PR branch is a new run and \
         does not satisfy the original PR checks unless GitHub associates it."
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests may panic")]
mod tests {
    use super::*;

    #[test]
    fn default_and_repo_urls_are_repository_scoped() {
        let default = resolve_repo_url(&HostStartArgs {
            repo: None,
            url: None,
            name: None,
            slots: 1,
            pr: None,
            work_dir: None,
            config_dir: None,
            docker_host_work_dir: None,
            docker_image: None,
        })
        .expect("default repo");
        assert_eq!(default, "https://github.com/tailrocks/velnor");

        let named = resolve_repo_url(&HostStartArgs {
            repo: Some("tailrocks/velnor".into()),
            url: None,
            name: None,
            slots: 1,
            pr: None,
            work_dir: None,
            config_dir: None,
            docker_host_work_dir: None,
            docker_image: None,
        })
        .expect("named repo");
        assert_eq!(named, "https://github.com/tailrocks/velnor");
    }

    #[test]
    fn docker_arch_maps_to_dockerfile_targetarch() {
        assert_eq!(docker_arch_to_targetarch("arm64"), Some("arm64"));
        assert_eq!(docker_arch_to_targetarch("aarch64"), Some("arm64"));
        assert_eq!(docker_arch_to_targetarch("amd64"), Some("amd64"));
        assert_eq!(docker_arch_to_targetarch("x86_64"), Some("amd64"));
        assert_eq!(docker_arch_to_targetarch("ppc64le"), None);
    }

    #[test]
    fn x64_target_pack_is_x86_64_only() {
        assert!(arch_claims_x64_target_pack("x86_64"));
        assert!(!arch_claims_x64_target_pack("aarch64"));
        assert!(!arch_claims_x64_target_pack("arm64"));
    }

    #[test]
    fn host_start_labels_exclude_the_trust_gated_label() {
        let labels = host_start_labels("untrusted");
        assert_eq!(labels, vec!["self-hosted", "velnor-target-mvp"]);
        assert!(
            !labels
                .iter()
                .any(|label| label == velnor_runner::runner::TRUST_GATED_RUNNER_LABEL),
            "untrusted on-demand hosts must never attract trust-gated jobs: {labels:?}"
        );
    }

    #[test]
    fn host_start_labels_include_the_trust_gated_label_when_trusted() {
        assert_eq!(
            host_start_labels("trusted"),
            vec![
                "self-hosted",
                "velnor-target-mvp",
                velnor_runner::runner::TRUST_GATED_RUNNER_LABEL
            ]
        );
        assert_eq!(
            host_start_labels("  Trusted  "),
            host_start_labels("trusted")
        );
        assert_eq!(host_start_labels("public"), host_start_labels("untrusted"));
    }

    #[test]
    fn mac_host_claims_the_exact_normalized_arm_label_set() {
        // Apple Silicon claims no x64 target pack; registration normalizes
        // the host labels by appending the ARM claim instead.
        let normalized = velnor_runner::runner::normalize_labels(
            host_start_labels("untrusted"),
            arch_claims_x64_target_pack("aarch64"),
            true,
        );
        assert_eq!(
            normalized,
            vec!["self-hosted", "ubuntu-24.04-arm", "velnor-target-mvp"]
        );
        assert!(
            !normalized
                .iter()
                .any(|label| label == velnor_runner::runner::TRUST_GATED_RUNNER_LABEL),
            "untrusted Mac hosts must never attract trust-gated jobs: {normalized:?}"
        );
    }

    #[test]
    fn trusted_mac_host_claims_velnor_host_docker() {
        let normalized = velnor_runner::runner::normalize_labels(
            host_start_labels("trusted"),
            arch_claims_x64_target_pack("aarch64"),
            true,
        );
        assert_eq!(
            normalized,
            vec![
                "self-hosted",
                "ubuntu-24.04-arm",
                velnor_runner::runner::TRUST_GATED_RUNNER_LABEL,
                "velnor-target-mvp"
            ]
        );
    }

    #[test]
    fn org_urls_are_refused() {
        let error = resolve_repo_url(&HostStartArgs {
            repo: None,
            url: Some("https://github.com/tailrocks".into()),
            name: None,
            slots: 1,
            pr: None,
            work_dir: None,
            config_dir: None,
            docker_host_work_dir: None,
            docker_image: None,
        })
        .expect_err("org URL");
        assert_eq!(error.reason, "host.org_scope_refused");
    }

    #[test]
    fn default_config_dir_is_isolated_per_host_name() {
        let args = HostStartArgs {
            repo: None,
            url: None,
            name: None,
            slots: 1,
            pr: None,
            work_dir: None,
            config_dir: None,
            docker_host_work_dir: None,
            docker_image: None,
        };
        let dir = resolve_host_config_dir(&args, "velnor-macos-smoke").expect("config dir");
        assert!(
            dir.ends_with("hosts/velnor-macos-smoke"),
            "{}",
            dir.display()
        );
    }

    #[test]
    fn explicit_config_dir_is_unchanged() {
        let args = HostStartArgs {
            repo: None,
            url: None,
            name: None,
            slots: 1,
            pr: None,
            work_dir: None,
            config_dir: Some(PathBuf::from("/tmp/explicit-host")),
            docker_host_work_dir: None,
            docker_image: None,
        };
        assert_eq!(
            resolve_host_config_dir(&args, "ignored").expect("explicit"),
            PathBuf::from("/tmp/explicit-host")
        );
    }
}

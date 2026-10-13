#![allow(dead_code)]

use crate::{
    action::{
        CacheActionKind, DockerActionInvocation, JavaScriptActionInvocation, NativeActionAdapter,
        NativeActionInvocation,
    },
    cache::CacheEntryLock,
    checkout::{configure_safe_directory, execute_checkout_with_mirror, CheckoutPlan},
    container::{JobContainerSpec, Shell},
    script_step::{ScriptStep, ScriptStepPlan, StepAnnotation, StepCommandState},
    workflow_command::parse_workflow_commands,
};
use anyhow::{bail, Context, Result};
use globset::{Glob, GlobBuilder, GlobSetBuilder};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc::UnboundedSender;

const DOCKER_MOUNT_CHECK_FILE: &str = ".velnor-mount-check";
const CACHE_GLOB_MANIFEST_FILE: &str = ".velnor-cache-glob-v1.json";
const DEFAULT_STEP_TIMEOUT_MINUTES: u64 = 360;
const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(DEFAULT_STEP_TIMEOUT_MINUTES * 60);
/// `docker rm --force` of a Created/removing BuildKit daemon can block until
/// dockerd finishes. Job teardown must not inherit the 6h step timeout or the
/// job container (and every guest sibling) stays Up for hours.
const TEARDOWN_RM_TIMEOUT: Duration = Duration::from_secs(20);
const SETUP_QEMU_BINFMT_IMAGE: &str =
    "docker.io/tonistiigi/binfmt@sha256:400a4873b838d1b89194d982c45e5fb3cda4593fbfd7e08a02e76b03b21166f0";
const RESULTS_ARTIFACT_MAX_UPLOAD_FILES: usize = 100_000;
const RESULTS_ARTIFACT_MAX_UPLOAD_FILE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const RESULTS_ARTIFACT_MAX_UPLOAD_TOTAL_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const RESULTS_ARTIFACT_MAX_UPLOAD_PATH_BYTES: u64 = 64 * 1024 * 1024;
const RESULTS_ARTIFACT_MAX_UPLOAD_PATH_DEPTH: usize = 256;
const MAX_TOOL_PREP_TELEMETRY_MS: u64 = 24 * 60 * 60 * 1000;
const PREPARED_ARTIFACT_MAX_RESPONSE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const PREPARED_ARTIFACT_MAX_CONTROL_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const PREPARED_ARTIFACT_MAX_ZIP_MEMBERS: usize = 100_000;
const PREPARED_ARTIFACT_MAX_ZIP_PATH_BYTES: u64 = 64 * 1024 * 1024;
const PREPARED_ARTIFACT_MAX_PATH_DEPTH: usize = 256;
const PREPARED_ARTIFACT_MAX_ZIP_UNCOMPRESSED_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MAX_CACHE_LOOKUP_TELEMETRY_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_ARTIFACT_MATERIALIZE_TELEMETRY_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_TEST_END_TELEMETRY_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_COMPILE_TELEMETRY_MS: u64 = 24 * 60 * 60 * 1000;
const PREPARED_ARTIFACT_SHA256_PREFIX: &str = "sha256:";
const CACHE_GLOB_MAX_MATCHES: usize = 100_000;
const CACHE_GLOB_MAX_VISITED_ENTRIES: usize = 1_000_000;
const CACHE_GLOB_MAX_PATH_BYTES: u64 = 64 * 1024 * 1024;
const CACHE_GLOB_MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const PAGES_ARCHIVE_MAX_MEMBERS: usize = 100_000;
const PAGES_ARCHIVE_MAX_PATH_BYTES: u64 = 64 * 1024 * 1024;
const PAGES_ARCHIVE_MAX_PATH_DEPTH: usize = 256;
const PAGES_ARCHIVE_MAX_FILE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const PAGES_ARCHIVE_MAX_TOTAL_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const PAGES_ARCHIVE_MAX_ARCHIVE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
static CACHE_STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

/// Return a stable test-runner label only when a shell segment starts with a
/// recognized test command. Matching the command position avoids treating
/// arbitrary output such as `echo cargo test` as a completed test boundary.
fn test_command_kind(script: &str) -> Option<&'static str> {
    script
        .lines()
        .flat_map(|line| line.split([';', '|', '&']))
        .filter_map(|line| {
            let mut tokens = line.split_whitespace().peekable();
            while matches!(tokens.peek().copied(), Some("env" | "sudo")) {
                tokens.next();
            }
            let first = tokens.next()?;
            let second = tokens.next();
            let third = tokens.next();
            match (first, second, third) {
                ("cargo", Some("test"), _) => Some("cargo_test"),
                ("cargo", Some("nextest"), Some("run")) => Some("cargo_nextest"),
                ("go", Some("test"), _) => Some("go_test"),
                ("pytest", _, _) => Some("pytest"),
                ("python" | "python3", Some("-m"), Some("pytest")) => Some("pytest"),
                ("npm", Some("test"), _) => Some("npm_test"),
                ("pnpm", Some("test"), _) => Some("pnpm_test"),
                ("yarn", Some("test"), _) => Some("yarn_test"),
                ("gradle", Some("test"), _) => Some("gradle_test"),
                ("./gradlew", Some("test"), _) => Some("gradle_test"),
                _ => None,
            }
        })
        .next()
}

/// Return a stable compiler label only when a shell segment starts with an
/// explicit compile command. Test commands are intentionally excluded: their
/// compilation and test phases are not separately observable at this layer.
fn compile_command_kind(script: &str) -> Option<&'static str> {
    script
        .lines()
        .flat_map(|line| line.split([';', '|', '&']))
        .filter_map(|line| {
            let mut tokens = line.split_whitespace().peekable();
            while let Some(token) = tokens.peek().copied() {
                if matches!(token, "env" | "sudo") || token.contains('=') {
                    tokens.next();
                } else {
                    break;
                }
            }
            let first = tokens.next()?;
            let second = tokens.next()?;
            match (first, second) {
                ("cargo", "build" | "check" | "clippy" | "rustc") => Some("cargo"),
                ("go", "build") => Some("go"),
                _ => None,
            }
        })
        .next()
}

/// Return a stable linker label only when a shell segment starts with an
/// explicit command whose successful completion includes linking. Do not
/// infer a link boundary from test or check commands.
fn link_command_kind(script: &str) -> Option<&'static str> {
    script
        .lines()
        .flat_map(|line| line.split([';', '|', '&']))
        .filter_map(|line| {
            let mut tokens = line.split_whitespace().peekable();
            while let Some(token) = tokens.peek().copied() {
                if matches!(token, "env" | "sudo") || token.contains('=') {
                    tokens.next();
                } else {
                    break;
                }
            }
            let first = tokens.next()?;
            let second = tokens.next()?;
            match (first, second) {
                ("cargo", "build" | "rustc") => Some("cargo"),
                ("go", "build") => Some("go"),
                _ => None,
            }
        })
        .next()
}

fn docker_lifecycle_guard() -> Result<crate::capacity::DockerLifecycleGuard> {
    let run_root = crate::storage::StorageLayout::resolve()
        .map(|layout| layout.run_root)
        .unwrap_or_else(|| std::env::temp_dir().join("velnor"));
    crate::capacity::DockerLifecycleGuard::lock(&run_root)
}

// ANSI color helpers for Velnor-authored adapter output.
// GitHub's UI renders these as colored spans (ansifg-* classes).
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_RESET: &str = "\x1b[0m";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Token for a long-lived child (jailer). Kill uses the retained pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnedProcess {
    pub pid: u32,
}

pub trait CommandRunner {
    fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult>;

    /// Spawn without waiting. Default refuses so accidental long waits stay fail-closed.
    ///
    /// # Errors
    /// Spawn unsupported or OS failure.
    fn spawn(&mut self, program: &str, _args: &[String]) -> Result<SpawnedProcess> {
        bail!("command runner does not support spawn for {program}")
    }

    /// Kill a process returned by [`CommandRunner::spawn`].
    ///
    /// # Errors
    /// Kill unsupported or OS failure.
    fn kill(&mut self, process: &SpawnedProcess) -> Result<()> {
        bail!(
            "command runner does not support kill of pid {}",
            process.pid
        )
    }

    fn run_timeout(
        &mut self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<CommandResult> {
        let _ = timeout;
        self.run(program, args)
    }

    fn run_timeout_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<CommandResult> {
        if !env.is_empty() {
            bail!("command runner does not support process environment forwarding");
        }
        self.run_timeout(program, args, timeout)
    }

    fn run_streaming_timeout(
        &mut self,
        program: &str,
        args: &[String],
        timeout: Duration,
        on_output: &mut dyn FnMut(CommandStream, &str),
    ) -> Result<CommandResult> {
        let _ = timeout;
        self.run_streaming(program, args, on_output)
    }

    fn run_streaming_timeout_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
        on_output: &mut dyn FnMut(CommandStream, &str),
    ) -> Result<CommandResult> {
        if !env.is_empty() {
            bail!("command runner does not support streaming process environment forwarding");
        }
        self.run_streaming_timeout(program, args, timeout, on_output)
    }

    fn run_streaming(
        &mut self,
        program: &str,
        args: &[String],
        _on_output: &mut dyn FnMut(CommandStream, &str),
    ) -> Result<CommandResult> {
        self.run(program, args)
    }

    fn run_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<CommandResult> {
        let _ = env;
        self.run(program, args)
    }

    fn run_with_stdin_timeout(
        &mut self,
        program: &str,
        args: &[String],
        stdin: &str,
        timeout: Duration,
    ) -> Result<CommandResult> {
        let _ = timeout;
        self.run_with_stdin(program, args, stdin)
    }

    fn run_with_stdin_timeout_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        stdin: &str,
        timeout: Duration,
    ) -> Result<CommandResult> {
        if !env.is_empty() {
            bail!("command runner does not support stdin process environment forwarding");
        }
        self.run_with_stdin_timeout(program, args, stdin, timeout)
    }

    fn run_with_stdin(
        &mut self,
        program: &str,
        args: &[String],
        stdin: &str,
    ) -> Result<CommandResult> {
        let _ = stdin;
        self.run(program, args)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStream {
    Stdout,
    Stderr,
}

pub fn render_context_expressions(value: &str, context_data: &[(String, Value)]) -> String {
    JobExecutionState::new_with_context(&[], context_data).resolve_expressions(value)
}

pub fn render_expressions_with_context(
    value: &str,
    base_env: &[(String, String)],
    context_data: &[(String, Value)],
) -> String {
    JobExecutionState::new_with_context(base_env, context_data).resolve_expressions(value)
}

fn node_action_image(runtime: &str, fallback: &str) -> String {
    if !fallback.trim().is_empty() {
        return fallback.to_string();
    }
    match runtime.strip_prefix("node") {
        Some("20") => "node:20-bookworm".to_string(),
        Some("24") => "node:24-bookworm".to_string(),
        Some(major) if major.chars().all(|ch| ch.is_ascii_digit()) && !major.is_empty() => {
            format!("node:{major}")
        }
        _ => fallback.to_string(),
    }
}

fn spawned_children() -> &'static Mutex<HashMap<u32, Child>> {
    static CHILDREN: OnceLock<Mutex<HashMap<u32, Child>>> = OnceLock::new();
    CHILDREN.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Default)]
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn spawn(&mut self, program: &str, args: &[String]) -> Result<SpawnedProcess> {
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawn {program} {}", args.join(" ")))?;
        let pid = child.id();
        spawned_children()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(pid, child);
        Ok(SpawnedProcess { pid })
    }

    fn kill(&mut self, process: &SpawnedProcess) -> Result<()> {
        if let Some(mut child) = spawned_children()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&process.pid)
        {
            child
                .kill()
                .with_context(|| format!("kill {}", process.pid))?;
            child
                .wait()
                .with_context(|| format!("wait for {}", process.pid))?;
            return Ok(());
        }
        let status = Command::new("kill")
            .args(["-TERM", &process.pid.to_string()])
            .status()
            .with_context(|| format!("kill {}", process.pid))?;
        if !status.success() {
            bail!("kill {} exited {:?}", process.pid, status.code());
        }
        Ok(())
    }

    fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
        self.run_timeout(program, args, DEFAULT_STEP_TIMEOUT)
    }

    fn run_timeout(
        &mut self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<CommandResult> {
        self.run_timeout_with_env(program, args, &[], timeout)
    }

    fn run_timeout_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<CommandResult> {
        // Called from spawn_blocking context — synchronous blocking is fine here.
        let timeout = if program == "docker" {
            crate::docker_lease::docker_cli_timeout(args, timeout)
        } else {
            timeout
        };
        let rm_claim = if program == "docker" {
            crate::docker_lease::claim_docker_container_rm(args)
        } else {
            None
        };
        if let Some(claim) = rm_claim.as_ref()
            && claim.ids.is_empty()
        {
            return Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            });
        }
        let claimed_args = rm_claim
            .as_ref()
            .map(|claim| crate::docker_lease::container_rm_args_with_claimed_ids(args, &claim.ids));
        let args = claimed_args.as_deref().unwrap_or(args);
        let mut command = Command::new(program);
        command
            .args(args)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command
            .spawn()
            .with_context(|| format!("spawn {program} {}", args.join(" ")))?;
        let (timed_out, watchdog_cancel, watchdog) =
            spawn_docker_timeout_watchdog(program, args, child.id(), timeout);
        let output = child
            .wait_with_output()
            .with_context(|| format!("wait for {program} {}", args.join(" ")))?;
        let _ = watchdog_cancel.send(());
        if let Some(watchdog) = watchdog {
            let _ = watchdog.join();
        }
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(timeout_command_result(stdout, stderr));
        }

        Ok(CommandResult {
            code: exit_code(output.status)?,
            stdout,
            stderr,
        })
    }

    fn run_streaming(
        &mut self,
        program: &str,
        args: &[String],
        on_output: &mut dyn FnMut(CommandStream, &str),
    ) -> Result<CommandResult> {
        self.run_streaming_timeout(program, args, DEFAULT_STEP_TIMEOUT, on_output)
    }

    fn run_streaming_timeout(
        &mut self,
        program: &str,
        args: &[String],
        timeout: Duration,
        on_output: &mut dyn FnMut(CommandStream, &str),
    ) -> Result<CommandResult> {
        self.run_streaming_timeout_with_env(program, args, &[], timeout, on_output)
    }

    fn run_streaming_timeout_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
        on_output: &mut dyn FnMut(CommandStream, &str),
    ) -> Result<CommandResult> {
        let mut command = Command::new(program);
        command
            .args(args)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .with_context(|| format!("run {program} {}", args.join(" ")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing stdout pipe for {program}"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing stderr pipe for {program}"))?;
        let (sender, receiver) = mpsc::channel();
        let stdout_sender = sender.clone();
        thread::spawn(move || stream_reader(stdout, CommandStream::Stdout, stdout_sender));
        thread::spawn(move || stream_reader(stderr, CommandStream::Stderr, sender));
        let (timed_out, watchdog_cancel, watchdog) =
            spawn_docker_timeout_watchdog(program, args, child.id(), timeout);

        let mut stdout = String::new();
        let mut stderr = String::new();
        for (stream, line) in receiver {
            match stream {
                CommandStream::Stdout => {
                    stdout.push_str(&line);
                    stdout.push('\n');
                }
                CommandStream::Stderr => {
                    stderr.push_str(&line);
                    stderr.push('\n');
                }
            }
            on_output(stream, &line);
        }

        let status = child
            .wait()
            .with_context(|| format!("wait for {program} {}", args.join(" ")))?;
        let _ = watchdog_cancel.send(());
        if let Some(watchdog) = watchdog {
            let _ = watchdog.join();
        }
        if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(timeout_command_result(stdout, stderr));
        }
        Ok(CommandResult {
            code: exit_code(status)?,
            stdout,
            stderr,
        })
    }

    fn run_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<CommandResult> {
        let mut command = Command::new(program);
        command.args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command
            .output()
            .with_context(|| format!("run {program} {}", args.join(" ")))?;

        Ok(CommandResult {
            code: exit_code(output.status)?,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn run_with_stdin(
        &mut self,
        program: &str,
        args: &[String],
        stdin: &str,
    ) -> Result<CommandResult> {
        self.run_with_stdin_timeout(program, args, stdin, DEFAULT_STEP_TIMEOUT)
    }

    fn run_with_stdin_timeout(
        &mut self,
        program: &str,
        args: &[String],
        stdin: &str,
        timeout: Duration,
    ) -> Result<CommandResult> {
        self.run_with_stdin_timeout_with_env(program, args, &[], stdin, timeout)
    }

    fn run_with_stdin_timeout_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        stdin: &str,
        timeout: Duration,
    ) -> Result<CommandResult> {
        let mut command = Command::new(program);
        command
            .args(args)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn {program} {}", args.join(" ")))?;
        let (timed_out, watchdog_cancel, watchdog) =
            spawn_docker_timeout_watchdog(program, args, child.id(), timeout);
        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin
                .write_all(stdin.as_bytes())
                .with_context(|| format!("write stdin for {program} {}", args.join(" ")))?;
        }
        let output = child
            .wait_with_output()
            .with_context(|| format!("wait for {program} {}", args.join(" ")))?;
        let _ = watchdog_cancel.send(());
        if let Some(watchdog) = watchdog {
            let _ = watchdog.join();
        }
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(timeout_command_result(stdout, stderr));
        }

        Ok(CommandResult {
            code: exit_code(output.status)?,
            stdout,
            stderr,
        })
    }
}

fn stream_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    stream: CommandStream,
    sender: mpsc::Sender<(CommandStream, String)>,
) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                }
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
                let line = String::from_utf8_lossy(&buf).into_owned();
                if sender.send((stream, line)).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepExecutionResult {
    pub exit_code: i32,
    pub state: StepCommandState,
    pub skipped: bool,
    pub failure_ignored: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug)]
pub(crate) struct StepLogicFailure {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

impl StepLogicFailure {
    pub(crate) fn new(
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    fn to_step_result(&self, stdout_prefix: Option<String>) -> StepExecutionResult {
        let mut stdout = stdout_prefix.unwrap_or_default();
        if !self.stdout.is_empty() {
            if !stdout.is_empty() {
                stdout.push('\n');
            }
            stdout.push_str(&self.stdout);
        }
        StepExecutionResult {
            exit_code: self.exit_code,
            state: StepCommandState::default(),
            skipped: false,
            failure_ignored: false,
            stdout,
            stderr: self.stderr.clone(),
        }
    }
}

impl std::fmt::Display for StepLogicFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.stderr)
    }
}

impl std::error::Error for StepLogicFailure {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobExecutionSummary {
    pub step_results: Vec<StepExecutionResult>,
    /// Number of physical actions that returned an execution result. This is
    /// separate from `step_logs`, which also contains presentation and
    /// synthetic lifecycle rows.
    pub executed_physical_actions: usize,
    pub job_outputs: BTreeMap<String, String>,
    pub environment_url: Option<String>,
    pub step_logs: Vec<StepLog>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLog {
    pub step_id: String,
    pub display_name: String,
    pub order: i32,
    /// RFC3339 timestamp when this step began executing (empty if not tracked).
    pub started_at: String,
    /// RFC3339 timestamp when this step finished (empty if not tracked).
    pub completed_at: String,
    pub lines: Vec<String>,
    pub masks: Vec<String>,
    pub annotations: Vec<StepAnnotation>,
    pub telemetry: Vec<crate::script_step::StepCommandTelemetry>,
    pub exit_code: i32,
    pub skipped: bool,
    pub failure_ignored: bool,
    pub error_count: i32,
    pub warning_count: i32,
    pub notice_count: i32,
    /// GITHUB_STEP_SUMMARY content for this step (empty when none).
    /// Uploaded separately to the Results Service summary endpoint.
    pub summary: String,
}

#[derive(Debug, Clone)]
pub enum ExecutableStep {
    /// Opens a composite action: registers ONE timeline step (GitHub renders
    /// a composite as a single step; embedded steps write into the parent's
    /// record — actions/runner CompositeActionHandler).
    CompositeStart {
        step_id: String,
        display_name: String,
        inputs: BTreeMap<String, String>,
        env: Vec<(String, String)>,
        condition: Option<String>,
    },
    CompositeEnd {
        step_id: String,
    },
    Checkout(CheckoutPlan),
    Script(ScriptStep),
    JavaScript {
        step_id: String,
        display_name: String,
        invocation: JavaScriptActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
        timeout_minutes: Option<u64>,
    },
    Docker {
        step_id: String,
        display_name: String,
        invocation: DockerActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
        timeout_minutes: Option<u64>,
    },
    Native {
        step_id: String,
        display_name: String,
        invocation: NativeActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
        timeout_minutes: Option<u64>,
    },
    CompositeOutputs {
        step_id: String,
        outputs: BTreeMap<String, String>,
        condition: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepStartEvent {
    pub step_id: String,
    pub display_name: String,
    pub order: i32,
}

/// Aggregates an executing composite action into ONE step record. GitHub
/// registers a single timeline step per composite; embedded steps append
/// `##[group]<name>`-headed sections into the parent's log instead of
/// registering their own steps (actions/runner CompositeActionHandler).
#[derive(Debug, Default)]
struct CompositeFrame {
    backend_step_id: String,
    display_name: String,
    order: i32,
    started_at: String,
    lines: Vec<String>,
    masks: Vec<String>,
    annotations: Vec<StepAnnotation>,
    telemetry: Vec<crate::script_step::StepCommandTelemetry>,
    exit_code: i32,
    skipped: bool,
    failure_ignored: bool,
    error_count: i32,
    warning_count: i32,
    notice_count: i32,
    summary: String,
    /// Nested composite depth: only the outermost frame registers a step.
    depth: usize,
}

impl CompositeFrame {
    fn append_inner(
        &mut self,
        display_name: &str,
        prelude: &[String],
        result: &StepExecutionResult,
    ) {
        self.absorb(result);
        if result.skipped {
            return;
        }
        let name = if display_name.is_empty() {
            "step"
        } else {
            display_name
        };
        self.lines.push(format!("##[group]{name}"));
        self.lines.extend(prelude.iter().cloned());
        self.lines.push("##[endgroup]".to_string());
        self.lines
            .extend(rendered_output_lines(&result.stdout, &result.stderr));
    }

    fn absorb(&mut self, result: &StepExecutionResult) {
        self.masks.extend(result.state.masks.iter().cloned());
        self.annotations
            .extend(result.state.annotations.iter().cloned());
        self.telemetry
            .extend(result.state.telemetry.iter().cloned());
        self.error_count += result.state.error_count;
        self.warning_count += result.state.warning_count;
        self.notice_count += result.state.notice_count;
        if !result.state.summary.is_empty() {
            if !self.summary.is_empty() {
                self.summary.push('\n');
            }
            self.summary.push_str(&result.state.summary);
        }
        if result.exit_code != 0 && !result.failure_ignored && self.exit_code == 0 {
            self.exit_code = result.exit_code;
        }
        self.failure_ignored |= result.failure_ignored;
    }

    fn into_step_log(self, completed_at: &str) -> StepLog {
        StepLog {
            step_id: self.backend_step_id,
            display_name: self.display_name,
            order: self.order,
            started_at: self.started_at,
            completed_at: completed_at.to_string(),
            lines: if self.skipped {
                skipped_step_log_lines()
            } else {
                self.lines
            },
            masks: self.masks,
            annotations: self.annotations,
            telemetry: self.telemetry,
            exit_code: self.exit_code,
            skipped: self.skipped,
            failure_ignored: self.failure_ignored,
            error_count: self.error_count,
            warning_count: self.warning_count,
            notice_count: self.notice_count,
            summary: self.summary,
        }
    }
}

#[derive(Debug, Clone)]
struct PostJavaScriptAction {
    step_id: String,
    display_name: String,
    invocation: JavaScriptActionInvocation,
    condition: Option<String>,
    continue_on_error: bool,
    timeout_minutes: Option<u64>,
    umbrella_display: Option<String>,
}

#[derive(Debug, Clone)]
struct PostNativeAction {
    step_id: String,
    display_name: String,
    invocation: NativeActionInvocation,
    condition: Option<String>,
    continue_on_error: bool,
    timeout_minutes: Option<u64>,
    /// Display name of the enclosing composite, when the owning step was
    /// embedded: GitHub runs embedded posts under ONE `Post Run <composite>`
    /// step (EmbeddedStepsWithPostRegistered).
    umbrella_display: Option<String>,
}

impl ExecutableStep {
    pub(crate) fn id(&self) -> &str {
        match self {
            ExecutableStep::CompositeStart { step_id, .. } => step_id,
            ExecutableStep::CompositeEnd { step_id } => step_id,
            ExecutableStep::Checkout(plan) => &plan.step_id,
            ExecutableStep::Script(step) => &step.id,
            ExecutableStep::JavaScript { step_id, .. } => step_id,
            ExecutableStep::Docker { step_id, .. } => step_id,
            ExecutableStep::Native { step_id, .. } => step_id,
            ExecutableStep::CompositeOutputs { step_id, .. } => step_id,
        }
    }

    pub(crate) fn condition(&self) -> Option<&str> {
        match self {
            ExecutableStep::CompositeStart { condition, .. } => condition.as_deref(),
            ExecutableStep::CompositeEnd { .. } => None,
            ExecutableStep::Checkout(plan) => plan.condition.as_deref(),
            ExecutableStep::Script(step) => step.condition.as_deref(),
            ExecutableStep::JavaScript { condition, .. } => condition.as_deref(),
            ExecutableStep::Docker { condition, .. } => condition.as_deref(),
            ExecutableStep::Native { condition, .. } => condition.as_deref(),
            ExecutableStep::CompositeOutputs { condition, .. } => condition.as_deref(),
        }
    }

    pub(crate) fn continue_on_error(&self) -> bool {
        match self {
            ExecutableStep::CompositeStart { .. } => false,
            ExecutableStep::CompositeEnd { .. } => false,
            ExecutableStep::Checkout(plan) => plan.continue_on_error,
            ExecutableStep::Script(step) => step.continue_on_error,
            ExecutableStep::JavaScript {
                continue_on_error, ..
            } => *continue_on_error,
            ExecutableStep::Docker {
                continue_on_error, ..
            } => *continue_on_error,
            ExecutableStep::Native {
                continue_on_error, ..
            } => *continue_on_error,
            ExecutableStep::CompositeOutputs { .. } => false,
        }
    }

    pub(crate) fn timeout_minutes(&self) -> Option<u64> {
        match self {
            ExecutableStep::Checkout(plan) => plan.timeout_minutes,
            ExecutableStep::Script(step) => step.timeout_minutes,
            ExecutableStep::JavaScript {
                timeout_minutes, ..
            }
            | ExecutableStep::Docker {
                timeout_minutes, ..
            }
            | ExecutableStep::Native {
                timeout_minutes, ..
            } => *timeout_minutes,
            _ => None,
        }
    }

    fn effective_timeout(&self, job_timeout_minutes: Option<u64>) -> Duration {
        effective_step_timeout(self.timeout_minutes(), job_timeout_minutes)
    }

    fn reports_timeline_start(&self) -> bool {
        !matches!(
            self,
            ExecutableStep::CompositeStart { .. }
                | ExecutableStep::CompositeEnd { .. }
                | ExecutableStep::CompositeOutputs { .. }
        )
    }

    pub fn display_name(&self) -> &str {
        match self {
            ExecutableStep::CompositeStart { display_name, .. } => display_name,
            ExecutableStep::CompositeEnd { step_id } => step_id,
            ExecutableStep::Checkout(plan) => {
                if plan.display_name.is_empty() {
                    &plan.step_id
                } else {
                    &plan.display_name
                }
            }
            ExecutableStep::Script(step) => &step.display_name,
            ExecutableStep::JavaScript { display_name, .. } => display_name,
            ExecutableStep::Docker { display_name, .. } => display_name,
            ExecutableStep::Native { display_name, .. } => display_name,
            ExecutableStep::CompositeOutputs { step_id, .. } => step_id,
        }
    }
}

#[derive(Clone)]
struct LifecycleTelemetry {
    sink: Arc<crate::ops::OpsSink>,
    admission: crate::ops::JobAdmission,
}

impl LifecycleTelemetry {
    fn emit_tool_prep(&self, elapsed: Duration) {
        let elapsed_ms = u64::try_from(elapsed.as_millis())
            .unwrap_or(u64::MAX)
            .min(MAX_TOOL_PREP_TELEMETRY_MS);
        let fields = BTreeMap::from([
            ("ms".to_owned(), Value::from(elapsed_ms)),
            ("tool".to_owned(), Value::from("mise")),
        ]);
        let _ = self.sink.emit_telemetry_for_admission(
            &self.admission,
            velnor_model::TelemetryEvent::ToolPrep,
            fields,
        );
    }

    fn emit_cache_lookup(
        &self,
        store: &'static str,
        result: Option<&StepExecutionResult>,
        lookup_error: bool,
        elapsed: Duration,
    ) {
        let lookup_ms = u64::try_from(elapsed.as_millis())
            .unwrap_or(u64::MAX)
            .min(MAX_CACHE_LOOKUP_TELEMETRY_MS);
        let hit = result
            .and_then(|result| result.state.outputs.get("cache-hit"))
            .is_some_and(|value| value == "true");
        let mut fields = BTreeMap::from([
            ("hit".to_owned(), Value::from(hit)),
            ("lookup_ms".to_owned(), Value::from(lookup_ms)),
            ("store".to_owned(), Value::from(store)),
        ]);
        if !hit {
            fields.insert(
                "miss_reason".to_owned(),
                Value::from(if lookup_error {
                    "lookup_error"
                } else {
                    "key_absent"
                }),
            );
        }
        let _ = self.sink.emit_telemetry_for_admission(
            &self.admission,
            velnor_model::TelemetryEvent::CacheLookup,
            fields,
        );
    }

    fn emit_artifact_materialize(&self, result: &StepExecutionResult, elapsed: Duration) {
        let Some(digest) = result.state.outputs.get("artifact-digest") else {
            return;
        };
        let elapsed_ms = u64::try_from(elapsed.as_millis())
            .unwrap_or(u64::MAX)
            .min(MAX_ARTIFACT_MATERIALIZE_TELEMETRY_MS);
        let fields = BTreeMap::from([
            ("digest".to_owned(), Value::from(digest.as_str())),
            ("ms".to_owned(), Value::from(elapsed_ms)),
            ("subset".to_owned(), Value::from("artifact")),
        ]);
        let _ = self.sink.emit_telemetry_for_admission(
            &self.admission,
            velnor_model::TelemetryEvent::ArtifactMaterialize,
            fields,
        );
    }

    fn emit_test_end(&self, runner: &'static str, exit_code: i32, elapsed: Duration) {
        let elapsed_ms = u64::try_from(elapsed.as_millis())
            .unwrap_or(u64::MAX)
            .min(MAX_TEST_END_TELEMETRY_MS);
        let fields = BTreeMap::from([
            ("exit_code".to_owned(), Value::from(exit_code)),
            ("ms".to_owned(), Value::from(elapsed_ms)),
            ("passed".to_owned(), Value::from(exit_code == 0)),
            ("runner".to_owned(), Value::from(runner)),
        ]);
        let _ = self.sink.emit_telemetry_for_admission(
            &self.admission,
            velnor_model::TelemetryEvent::TestEnd,
            fields,
        );
    }

    fn emit_compile_start(&self, compiler: &'static str) {
        let fields = BTreeMap::from([
            ("compiler".to_owned(), Value::from(compiler)),
            // The executor has no compiler wrapper/cache counters yet. Keep
            // the contract's typed fields while making their availability
            // explicit so consumers cannot mistake zero for an observation.
            ("metrics_known".to_owned(), Value::from(false)),
            ("unit_count".to_owned(), Value::from(0_u64)),
            ("wrapper_calls".to_owned(), Value::from(0_u64)),
            ("hits".to_owned(), Value::from(0_u64)),
            ("misses".to_owned(), Value::from(0_u64)),
        ]);
        let _ = self.sink.emit_telemetry_for_admission(
            &self.admission,
            velnor_model::TelemetryEvent::CompileStart,
            fields,
        );
    }

    fn emit_compile_end(&self, compiler: &'static str, exit_code: i32, elapsed: Duration) {
        let elapsed_ms = u64::try_from(elapsed.as_millis())
            .unwrap_or(u64::MAX)
            .min(MAX_COMPILE_TELEMETRY_MS);
        let fields = BTreeMap::from([
            ("compiler".to_owned(), Value::from(compiler)),
            ("exit_code".to_owned(), Value::from(exit_code)),
            ("metrics_known".to_owned(), Value::from(false)),
            ("ms".to_owned(), Value::from(elapsed_ms)),
            ("unit_count".to_owned(), Value::from(0_u64)),
            ("wrapper_calls".to_owned(), Value::from(0_u64)),
            ("hits".to_owned(), Value::from(0_u64)),
            ("misses".to_owned(), Value::from(0_u64)),
        ]);
        let _ = self.sink.emit_telemetry_for_admission(
            &self.admission,
            velnor_model::TelemetryEvent::CompileEnd,
            fields,
        );
    }

    fn emit_link_end(&self, runner_kind: &'static str, exit_code: i32, elapsed: Duration) {
        let elapsed_ms = u64::try_from(elapsed.as_millis())
            .unwrap_or(u64::MAX)
            .min(MAX_COMPILE_TELEMETRY_MS);
        let fields = BTreeMap::from([
            ("elapsed_ms".to_owned(), Value::from(elapsed_ms)),
            ("exit_code".to_owned(), Value::from(exit_code)),
            ("runner_kind".to_owned(), Value::from(runner_kind)),
            ("success".to_owned(), Value::from(exit_code == 0)),
        ]);
        let _ = self.sink.emit_telemetry_for_admission(
            &self.admission,
            velnor_model::TelemetryEvent::LinkEnd,
            fields,
        );
    }
}

/// Host-Docker step engine owned by the docker backend.
///
/// Jobs enter through [`crate::execution::run_validated_job`]. This type is not
/// a second executor and must not be constructed on the microvm path.
pub(crate) struct DockerJobEngine<R> {
    runner: R,
    step_start_sender: Option<UnboundedSender<StepStartEvent>>,
    step_log_sender: Option<UnboundedSender<StepLog>>,
    initial_order: i32,
    trailing_post_action_count: usize,
    /// Workflow-level env from the job message — shown first in every step's
    /// `env:` prelude block, the way GitHub renders it.
    workflow_env: Vec<(String, String)>,
    job_timeout_minutes: Option<u64>,
    /// Job-secret mask values supplied by the runner. Combined with runtime
    /// ::add-mask:: values before building each docker exec argv.
    secret_masks: Vec<String>,
    /// Operator-selected trust boundary. Credential-bearing native adapters
    /// enforce this independently of Docker-socket availability.
    trust_scope: String,
    /// Identity of the step currently executing — lets native adapters stream
    /// their output to the live feed (GitHub streams every step live, not
    /// just `run:` steps).
    live_step: Option<LiveStepIdentity>,
    job_environment_started: bool,
    docker_lease: Option<crate::docker_lease::DockerLeaseGuard>,
    /// Armed when the job network is created, defused once terminal cleanup
    /// removed it. Drop then removes the network, so no executor exit path can
    /// leak a `velnor-net-*` network (address-pool exhaustion class).
    job_network_guard: Option<crate::docker_lease::JobNetworkGuard>,
    lifecycle_telemetry: Option<LifecycleTelemetry>,
}

#[derive(Debug, Clone)]
struct LiveStepIdentity {
    step_id: String,
    display_name: String,
    order: i32,
    started_at: String,
}

impl<R> DockerJobEngine<R>
where
    R: CommandRunner,
{
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            step_start_sender: None,
            step_log_sender: None,
            initial_order: 0,
            trailing_post_action_count: 0,
            workflow_env: Vec::new(),
            job_timeout_minutes: None,
            secret_masks: Vec::new(),
            trust_scope: "trusted".to_string(),
            live_step: None,
            job_environment_started: false,
            docker_lease: None,
            job_network_guard: None,
            lifecycle_telemetry: None,
        }
    }

    pub fn with_job_timeout_minutes(mut self, timeout_minutes: Option<u64>) -> Self {
        self.job_timeout_minutes = timeout_minutes;
        self
    }

    pub fn with_secret_masks(mut self, masks: Vec<String>) -> Self {
        self.secret_masks = masks;
        self
    }

    pub fn with_trust_scope(mut self, trust_scope: impl Into<String>) -> Self {
        self.trust_scope = trust_scope.into();
        self
    }

    pub fn with_workflow_env(mut self, env: Vec<(String, String)>) -> Self {
        self.workflow_env = env;
        self
    }

    pub fn with_initial_order(mut self, order: i32) -> Self {
        self.initial_order = order;
        self
    }

    pub fn with_trailing_post_action_count(mut self, count: usize) -> Self {
        self.trailing_post_action_count = count;
        self
    }

    pub fn with_step_start_sender(mut self, sender: UnboundedSender<StepStartEvent>) -> Self {
        self.step_start_sender = Some(sender);
        self
    }

    pub fn with_step_log_sender(mut self, sender: UnboundedSender<StepLog>) -> Self {
        self.step_log_sender = Some(sender);
        self
    }

    pub fn with_job_environment_started(mut self, started: bool) -> Self {
        self.job_environment_started = started;
        self
    }

    pub(crate) fn with_tool_prep_telemetry(
        mut self,
        sink: Arc<crate::ops::OpsSink>,
        admission: crate::ops::JobAdmission,
    ) -> Self {
        self.lifecycle_telemetry = Some(LifecycleTelemetry { sink, admission });
        self
    }

    /// Adopt the Docker lease guard bound by an earlier (pre-created)
    /// environment start. The guard must live as long as the job container:
    /// the container holds the proxy socket bind-mounted, and dropping the
    /// guard deletes the socket inode, killing in-container docker clients
    /// (0.1.185 precreate regression, tailrocks/velnor#311 follow-up).
    pub fn with_docker_lease(mut self, lease: crate::docker_lease::DockerLeaseGuard) -> Self {
        self.docker_lease = Some(lease);
        self
    }

    pub(crate) fn take_docker_lease(&mut self) -> Option<crate::docker_lease::DockerLeaseGuard> {
        self.docker_lease.take()
    }

    pub fn into_runner(self) -> R {
        self.runner
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }

    fn emit_step_started(
        &self,
        step_id: impl Into<String>,
        display_name: impl Into<String>,
        order: &mut i32,
    ) -> String {
        *order += 1;
        let started_at = unix_now_rfc3339();
        let Some(sender) = &self.step_start_sender else {
            return started_at;
        };
        let _ = sender.send(StepStartEvent {
            step_id: step_id.into(),
            display_name: display_name.into(),
            order: *order,
        });
        started_at
    }

    fn emit_step_log(&self, log: &StepLog) {
        if let Some(sender) = &self.step_log_sender {
            let _ = sender.send(log.clone());
        }
    }

    pub fn execute_step(
        &mut self,
        container: &JobContainerSpec,
        step: &ScriptStep,
        temp_host: &Path,
    ) -> Result<StepExecutionResult> {
        let mut results = self.execute_steps(container, std::slice::from_ref(step), temp_host)?;
        results
            .pop()
            .ok_or_else(|| anyhow::anyhow!("script step did not produce a result"))
    }

    pub fn execute_steps(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ScriptStep],
        temp_host: &Path,
    ) -> Result<Vec<StepExecutionResult>> {
        if !self.job_environment_started {
            self.start_job_environment(container)?;
            self.job_environment_started = true;
        }

        let ordered = steps
            .iter()
            .cloned()
            .map(ExecutableStep::Script)
            .collect::<Vec<_>>();
        let result = self
            .execute_ordered_steps_in_started_container(
                container,
                &ordered,
                &[],
                &[],
                None,
                None,
                temp_host,
            )
            .map(|summary| summary.step_results);

        if let Err(error) = self.cleanup(container) {
            eprintln!("Warning: cleanup failed after script step: {error:#}");
        }
        result
    }

    pub fn execute_ordered_steps(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ExecutableStep],
        base_env: &[(String, String)],
        temp_host: &Path,
    ) -> Result<Vec<StepExecutionResult>> {
        self.execute_ordered_steps_with_context(container, steps, base_env, &[], temp_host)
    }

    pub fn execute_ordered_steps_with_context(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ExecutableStep],
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        temp_host: &Path,
    ) -> Result<Vec<StepExecutionResult>> {
        Ok(self
            .execute_ordered_steps_with_job_outputs(
                container,
                steps,
                base_env,
                context_data,
                None,
                temp_host,
            )?
            .step_results)
    }

    pub fn execute_ordered_steps_with_job_outputs(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ExecutableStep],
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        job_outputs: Option<&Value>,
        temp_host: &Path,
    ) -> Result<JobExecutionSummary> {
        self.execute_ordered_steps_with_completion(
            container,
            steps,
            base_env,
            context_data,
            job_outputs,
            None,
            temp_host,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn execute_ordered_steps_with_completion(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ExecutableStep],
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        job_outputs: Option<&Value>,
        environment_url: Option<&Value>,
        temp_host: &Path,
    ) -> Result<JobExecutionSummary> {
        let result = self.execute_ordered_steps_without_cleanup(
            container,
            steps,
            base_env,
            context_data,
            job_outputs,
            environment_url,
            temp_host,
        );

        if let Err(error) = self.cleanup(container) {
            eprintln!("Warning: cleanup failed after ordered job steps: {error:#}");
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub fn execute_ordered_steps_without_cleanup(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ExecutableStep],
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        job_outputs: Option<&Value>,
        environment_url: Option<&Value>,
        temp_host: &Path,
    ) -> Result<JobExecutionSummary> {
        if !self.job_environment_started {
            self.start_job_environment(container)?;
            self.job_environment_started = true;
        }
        let mut runtime_context = context_data.to_vec();
        if let Some(services) = self.service_context(container)? {
            runtime_context.retain(|(name, _)| !name.eq_ignore_ascii_case("services"));
            runtime_context.push(("services".to_string(), services));
        }

        self.execute_ordered_steps_in_started_container(
            container,
            steps,
            base_env,
            &runtime_context,
            job_outputs,
            environment_url,
            temp_host,
        )
    }

    fn service_context(&mut self, container: &JobContainerSpec) -> Result<Option<Value>> {
        if container.services.is_empty() {
            return Ok(None);
        }
        let mut services = serde_json::Map::new();
        for service in &container.services {
            let id = self
                .run_docker(&service.id_args())?
                .stdout
                .trim()
                .to_string();
            let ports_output = self.run_docker(&service.mapped_ports_args())?.stdout;
            let mut ports = serde_json::Map::new();
            for line in ports_output.lines() {
                let Some((container_port, address)) = line.split_once(" -> ") else {
                    continue;
                };
                let Some((_, host_port)) = address.rsplit_once(':') else {
                    continue;
                };
                ports
                    .entry(
                        container_port
                            .trim_end_matches("/tcp")
                            .trim_end_matches("/udp")
                            .to_string(),
                    )
                    .or_insert_with(|| Value::String(host_port.to_string()));
            }
            services.insert(
                service.network_alias.clone(),
                serde_json::json!({
                    "id": id,
                    "network": service.network,
                    "ports": ports,
                }),
            );
        }
        Ok(Some(Value::Object(services)))
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_ordered_steps_in_started_container(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ExecutableStep],
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        job_outputs: Option<&Value>,
        environment_url: Option<&Value>,
        temp_host: &Path,
    ) -> Result<JobExecutionSummary> {
        let mut results = Vec::new();
        let mut executed_physical_actions = 0;
        let mut step_logs = Vec::new();
        let mut effective_base_env = container_runtime_env(container);
        effective_base_env.extend_from_slice(base_env);
        if let Some(event_path) = prepare_github_event_path(temp_host, context_data)? {
            effective_base_env.push(("GITHUB_EVENT_PATH".to_string(), event_path));
        }
        let mut state = JobExecutionState::new_with_workspace(
            &effective_base_env,
            context_data,
            &container.workspace_host,
            temp_host,
        );
        state.persistent_workspace_target = container.cargo_target_host.is_some();
        state.cargo_target_host = container
            .cargo_target_host
            .as_ref()
            .map(|_| container.workspace_host.join("target"));
        state.workflow_env = self
            .workflow_env
            .iter()
            .map(|(name, value)| (name.clone(), state.resolve_expressions(value)))
            .collect();
        let mut step_error = None;
        let mut post_actions = Vec::new();
        let mut native_post_actions = Vec::new();
        let mut timeline_order = self.initial_order;
        let mut composite_frame: Option<CompositeFrame> = None;
        let mut target_materialized = false;
        for step in steps {
            if !target_materialized
                && container.cargo_target_host.is_some()
                && !matches!(
                    step,
                    ExecutableStep::Native {
                        invocation,
                        ..
                    } if invocation.adapter == NativeActionAdapter::Checkout
                )
            {
                materialize_persistent_target(
                    container,
                    state.env.get("GITHUB_SHA").map(String::as_str),
                )?;
                target_materialized = true;
            }
            match step {
                ExecutableStep::CompositeStart {
                    step_id,
                    display_name,
                    inputs,
                    env,
                    condition,
                } => {
                    state.push_composite(step_id);
                    if let Some(frame) = composite_frame.as_mut() {
                        frame.depth += 1;
                    } else {
                        let frame_state = state.with_step_action(step_id);
                        let resolved_display = frame_state.resolve_expressions(display_name);
                        let backend_step_id = github_backend_step_id(step_id);
                        let skipped = !frame_state.evaluate_condition(condition.as_deref());
                        if skipped {
                            eprintln!(
                                "forensics.lifecycle event=step-skipped step={resolved_display}"
                            );
                        }
                        let started_at = self.emit_step_started(
                            backend_step_id.clone(),
                            &resolved_display,
                            &mut timeline_order,
                        );
                        let mut lines = Vec::new();
                        if !skipped {
                            lines.push(format!("##[group]{resolved_display}"));
                            lines.extend(action_log_prelude(inputs, env, &frame_state));
                            lines.push("##[endgroup]".to_string());
                        }
                        composite_frame = Some(CompositeFrame {
                            backend_step_id,
                            display_name: resolved_display,
                            order: timeline_order,
                            started_at,
                            lines,
                            skipped,
                            ..CompositeFrame::default()
                        });
                    }
                    continue;
                }
                ExecutableStep::CompositeEnd { step_id } => {
                    state.pop_composite(step_id);
                    match composite_frame.as_mut() {
                        Some(frame) if frame.depth > 0 => frame.depth -= 1,
                        Some(_) => {
                            if let Some(frame) = composite_frame.take() {
                                let log = frame.into_step_log(&unix_now_rfc3339());
                                self.emit_step_log(&log);
                                step_logs.push(log);
                            }
                        }
                        None => {}
                    }
                    continue;
                }
                _ => {}
            }
            let step_context_id = step.id().to_string();
            let step_backend_id = github_backend_step_id(&step_context_id);
            let step_state = state.with_step_action(&step_context_id);
            // GitHub evaluates `${{ }}` in step display names at runtime
            // (ActionRunner.TryUpdateDisplayName); unresolvable expressions
            // stay raw, matching the upstream prettified-token fallback.
            let display_name = step_state.resolve_expressions(step.display_name());
            if !step_state.evaluate_condition(step.condition()) {
                // Embedded composite steps never surface their own rows;
                // a skipped one simply leaves no trace in the parent's log.
                eprintln!("forensics.lifecycle event=step-skipped step={display_name}");
                let reports = composite_frame.is_none() && step.reports_timeline_start();
                let mut skipped_started_at = String::new();
                if reports {
                    skipped_started_at = self.emit_step_started(
                        step_backend_id.clone(),
                        &display_name,
                        &mut timeline_order,
                    );
                }
                let result = StepExecutionResult {
                    exit_code: 0,
                    state: StepCommandState::default(),
                    skipped: true,
                    failure_ignored: false,
                    stdout: String::new(),
                    stderr: String::new(),
                };
                if reports {
                    let log = step_log_with_name(
                        &step_backend_id,
                        &display_name,
                        timeline_order,
                        &skipped_started_at,
                        &unix_now_rfc3339(),
                        &result,
                        &step_log_prelude(step, &step_state),
                    );
                    self.emit_step_log(&log);
                    step_logs.push(log);
                }
                state.apply(&step_context_id, &result);
                results.push(result);
                continue;
            }
            let mut post_registered = false;
            if let ExecutableStep::JavaScript {
                step_id,
                invocation,
                continue_on_error,
                timeout_minutes,
                ..
            } = step
                && let Some(pre_container_path) = invocation.pre_container_path.as_deref()
                && step_state.evaluate_post_condition(invocation.pre_condition.as_deref())
            {
                if invocation.post_container_path.is_some() {
                    post_actions.push(PostJavaScriptAction {
                        step_id: step_id.clone(),
                        display_name: display_name.clone(),
                        invocation: invocation.clone(),
                        condition: invocation.post_condition.clone(),
                        continue_on_error: *continue_on_error,
                        timeout_minutes: *timeout_minutes,
                        umbrella_display: composite_frame
                            .as_ref()
                            .map(|frame| frame.display_name.clone()),
                    });
                    post_registered = true;
                }
                let pre_step_id = uuid::Uuid::new_v4().to_string();
                let pre_started_at = if composite_frame.is_none() {
                    self.emit_step_started(pre_step_id.clone(), &display_name, &mut timeline_order)
                } else {
                    unix_now_rfc3339()
                };
                let mut result = self.execute_javascript_action_in_started_container(
                    container,
                    step_id,
                    invocation,
                    pre_container_path,
                    &step_state.action_state_env(step_id),
                    temp_host,
                    &step_state,
                    effective_step_timeout(*timeout_minutes, self.job_timeout_minutes),
                )?;
                let failed = result.exit_code != 0;
                if failed && *continue_on_error {
                    result.failure_ignored = true;
                }
                if let Some(frame) = composite_frame.as_mut() {
                    frame.append_inner(
                        &display_name,
                        &step_log_prelude(step, &step_state),
                        &result,
                    );
                } else {
                    let log = step_log_with_name(
                        &pre_step_id,
                        &display_name,
                        timeline_order,
                        &pre_started_at,
                        &unix_now_rfc3339(),
                        &result,
                        &step_log_prelude(step, &step_state),
                    );
                    self.emit_step_log(&log);
                    step_logs.push(log);
                }
                state.apply(step_id, &result);
                executed_physical_actions += 1;
                results.push(result);
                if failed {
                    continue;
                }
            }
            let step_state = state.with_step_action(&step_context_id);
            let main_started_at = if composite_frame.is_none() && step.reports_timeline_start() {
                self.emit_step_started(step_backend_id.clone(), &display_name, &mut timeline_order)
            } else {
                String::new()
            };
            // Live lines from embedded composite steps stream under the
            // composite's registered step, not a step of their own.
            let (live_step_id, live_display_name, live_order, live_started_at) =
                match composite_frame.as_ref() {
                    Some(frame) => (
                        frame.backend_step_id.clone(),
                        frame.display_name.clone(),
                        frame.order,
                        frame.started_at.clone(),
                    ),
                    None => (
                        step_backend_id.clone(),
                        display_name.clone(),
                        timeline_order,
                        main_started_at.clone(),
                    ),
                };
            self.live_step = Some(LiveStepIdentity {
                step_id: live_step_id.clone(),
                display_name: live_display_name.clone(),
                order: live_order,
                started_at: live_started_at.clone(),
            });
            let result = (|| match step {
                ExecutableStep::CompositeStart { .. } | ExecutableStep::CompositeEnd { .. } => {
                    unreachable!("composite boundary steps are handled before execution")
                }
                ExecutableStep::Checkout(plan) => {
                    self.execute_checkout_step(container, plan, &step_state)
                }
                ExecutableStep::Script(step) => {
                    let step = step_state.resolve_script_step(step);
                    let test_runner = test_command_kind(&step.script);
                    let test_started = test_runner.map(|_| Instant::now());
                    let compiler = compile_command_kind(&step.script);
                    let linker = link_command_kind(&step.script);
                    let plan =
                        ScriptStepPlan::prepare_with_path(&step, temp_host, &step_state.path)?;
                    let mut env = step_state.step_env(&[]);
                    env.extend(step.env.iter().cloned());
                    env.extend(plan.env.iter().cloned());
                    let secret_masks = step_state.secret_masks(&self.secret_masks);
                    let exec_args = container.prepare_exec_script_args(
                        &plan.script_container_path,
                        plan.shell,
                        &plan.working_directory_container,
                        &env,
                        &secret_masks,
                    )?;
                    let live_sender = self.step_log_sender.clone();
                    let mut live_masks = Vec::new();
                    let mut on_output = |_: CommandStream, line: &str| {
                        live_masks.extend(parse_workflow_commands(line).masks);
                        emit_live_step_log(
                            &live_sender,
                            &live_step_id,
                            &live_display_name,
                            live_order,
                            &live_started_at,
                            line,
                            &live_masks,
                        );
                    };
                    let compile_started = match (compiler, self.lifecycle_telemetry.as_ref()) {
                        (Some(compiler), Some(telemetry)) => {
                            telemetry.emit_compile_start(compiler);
                            Some(Instant::now())
                        }
                        _ => None,
                    };
                    let link_started = match (linker, self.lifecycle_telemetry.as_ref()) {
                        (Some(_), Some(_)) => Some(Instant::now()),
                        _ => None,
                    };
                    let step_result = self.runner.run_streaming_timeout_with_env(
                        "docker",
                        exec_args.args(),
                        exec_args.process_env(),
                        effective_step_timeout(step.timeout_minutes, self.job_timeout_minutes),
                        &mut on_output,
                    )?;
                    if let (Some(compiler), Some(compile_started), Some(telemetry)) =
                        (compiler, compile_started, self.lifecycle_telemetry.as_ref())
                    {
                        telemetry.emit_compile_end(
                            compiler,
                            step_result.code,
                            compile_started.elapsed(),
                        );
                    }
                    if let (Some(runner_kind), Some(link_started), Some(telemetry)) =
                        (linker, link_started, self.lifecycle_telemetry.as_ref())
                    {
                        telemetry.emit_link_end(
                            runner_kind,
                            step_result.code,
                            link_started.elapsed(),
                        );
                    }
                    if let (Some(test_runner), Some(test_started), Some(telemetry)) =
                        (test_runner, test_started, self.lifecycle_telemetry.as_ref())
                    {
                        telemetry.emit_test_end(
                            test_runner,
                            step_result.code,
                            test_started.elapsed(),
                        );
                    }
                    let mut command_state = plan.collect_state()?;
                    command_state.merge(parse_workflow_commands_from_output(
                        &step_result.stdout,
                        &step_result.stderr,
                    ));
                    Ok(StepExecutionResult {
                        exit_code: step_result.code,
                        state: command_state,
                        skipped: false,
                        failure_ignored: false,
                        stdout: step_result.stdout,
                        stderr: step_result.stderr,
                    })
                }
                ExecutableStep::JavaScript {
                    step_id,
                    invocation,
                    timeout_minutes,
                    ..
                } => self.execute_javascript_action_in_started_container(
                    container,
                    step_id,
                    invocation,
                    &invocation.main_container_path,
                    &step_state.action_state_env(step_id),
                    temp_host,
                    &step_state,
                    effective_step_timeout(*timeout_minutes, self.job_timeout_minutes),
                ),
                ExecutableStep::Docker {
                    step_id,
                    invocation,
                    timeout_minutes,
                    ..
                } => self.execute_docker_action_in_started_container(
                    container,
                    step_id,
                    invocation,
                    temp_host,
                    &step_state,
                    effective_step_timeout(*timeout_minutes, self.job_timeout_minutes),
                ),
                ExecutableStep::Native {
                    step_id,
                    invocation,
                    timeout_minutes,
                    ..
                } => self.execute_native_action_in_started_container(
                    container,
                    step_id,
                    invocation,
                    &step_state,
                    effective_step_timeout(*timeout_minutes, self.job_timeout_minutes),
                ),
                ExecutableStep::CompositeOutputs { outputs, .. } => Ok(StepExecutionResult {
                    exit_code: 0,
                    state: StepCommandState {
                        outputs: step_state.evaluate_named_outputs(outputs),
                        ..StepCommandState::default()
                    },
                    skipped: false,
                    failure_ignored: false,
                    stdout: String::new(),
                    stderr: String::new(),
                }),
            })();

            match result {
                Ok(mut result) => {
                    let failed = result.exit_code != 0;
                    if let ExecutableStep::JavaScript {
                        invocation,
                        continue_on_error,
                        timeout_minutes,
                        ..
                    } = step
                        && invocation.post_container_path.is_some()
                        && !post_registered
                    {
                        post_actions.push(PostJavaScriptAction {
                            step_id: step_context_id.clone(),
                            display_name: display_name.clone(),
                            invocation: invocation.clone(),
                            condition: invocation.post_condition.clone(),
                            continue_on_error: *continue_on_error,
                            timeout_minutes: *timeout_minutes,
                            umbrella_display: composite_frame
                                .as_ref()
                                .map(|frame| frame.display_name.clone()),
                        });
                    }
                    if let ExecutableStep::Native {
                        invocation,
                        continue_on_error,
                        timeout_minutes,
                        ..
                    } = step
                        && let Some(condition) =
                            native_post_condition(invocation.adapter, invocation.cache_kind)
                    {
                        native_post_actions.push(PostNativeAction {
                            step_id: step_context_id.clone(),
                            display_name: display_name.clone(),
                            invocation: invocation.clone(),
                            condition: Some(condition.to_string()),
                            continue_on_error: *continue_on_error,
                            timeout_minutes: *timeout_minutes,
                            umbrella_display: composite_frame
                                .as_ref()
                                .map(|frame| frame.display_name.clone()),
                        });
                    }
                    if failed && step.continue_on_error() {
                        result.failure_ignored = true;
                    }
                    if let Some(frame) = composite_frame.as_mut() {
                        // Bookkeeping steps (CompositeOutputs) contribute state
                        // but never a visible section — otherwise their raw
                        // step id leaks as a `##[group]<uuid>` header.
                        if step.reports_timeline_start() {
                            frame.append_inner(
                                &display_name,
                                &step_log_prelude(step, &step_state),
                                &result,
                            );
                        } else {
                            frame.absorb(&result);
                        }
                    } else if step.reports_timeline_start() {
                        let log = step_log_with_name(
                            &step_backend_id,
                            &display_name,
                            timeline_order,
                            &main_started_at,
                            &unix_now_rfc3339(),
                            &result,
                            &step_log_prelude(step, &step_state),
                        );
                        self.emit_step_log(&log);
                        step_logs.push(log);
                    }
                    state.apply(&step_context_id, &result);
                    if step.reports_timeline_start() {
                        executed_physical_actions += 1;
                    }
                    results.push(result);
                }
                Err(error) => {
                    // actions/runner parity (RunStepAsync catch + ApplyContinueOnError):
                    // a step that fails by throwing still honors
                    // continue-on-error — outcome stays failure, the reported
                    // conclusion is success, and the job runs remaining steps.
                    if step.continue_on_error() {
                        eprintln!(
                            "Step '{display_name}' failed with an execution error but continue-on-error is set: {error:#}"
                        );
                        let result = StepExecutionResult {
                            exit_code: 1,
                            state: StepCommandState::default(),
                            skipped: false,
                            failure_ignored: true,
                            stdout: String::new(),
                            stderr: format!("{error:#}"),
                        };
                        if let Some(frame) = composite_frame.as_mut() {
                            if step.reports_timeline_start() {
                                frame.append_inner(
                                    &display_name,
                                    &step_log_prelude(step, &step_state),
                                    &result,
                                );
                            } else {
                                frame.absorb(&result);
                            }
                        } else if step.reports_timeline_start() {
                            let log = step_log_with_name(
                                &step_backend_id,
                                &display_name,
                                timeline_order,
                                &main_started_at,
                                &unix_now_rfc3339(),
                                &result,
                                &step_log_prelude(step, &step_state),
                            );
                            self.emit_step_log(&log);
                            step_logs.push(log);
                        }
                        state.apply(&step_context_id, &result);
                        if step.reports_timeline_start() {
                            executed_physical_actions += 1;
                        }
                        results.push(result);
                    } else {
                        step_error = Some(error);
                        break;
                    }
                }
            }
            self.live_step = None;
        }
        self.live_step = None;
        // An execution error mid-composite breaks the loop before the
        // CompositeEnd boundary: flush the frame so the step still closes
        // (failed) instead of silently disappearing from the UI.
        if let Some(mut frame) = composite_frame.take() {
            if frame.exit_code == 0 {
                frame.exit_code = 1;
            }
            let log = frame.into_step_log(&unix_now_rfc3339());
            self.emit_step_log(&log);
            step_logs.push(log);
        }
        let native_post_actions = native_post_actions
            .into_iter()
            .rev()
            .filter(|post_action| state.evaluate_post_condition(post_action.condition.as_deref()))
            .collect::<Vec<_>>();
        let post_actions = post_actions
            .into_iter()
            .rev()
            .filter(|post_action| state.evaluate_post_condition(post_action.condition.as_deref()))
            .collect::<Vec<_>>();
        reserve_github_post_step_orders(
            &mut timeline_order,
            native_post_actions.len() + post_actions.len() + self.trailing_post_action_count,
        );
        // GitHub runs all embedded-composite posts as ONE `Post Run
        // <composite>` step (EmbeddedStepsWithPostRegistered); consecutive
        // posts sharing an umbrella collapse into a single step record.
        let mut native_post_iter = native_post_actions.into_iter().peekable();
        while let Some(first) = native_post_iter.next() {
            let mut group = vec![first];
            if let Some(umbrella) = group[0].umbrella_display.clone() {
                while native_post_iter
                    .peek()
                    .is_some_and(|next| next.umbrella_display.as_deref() == Some(umbrella.as_str()))
                {
                    group.push(native_post_iter.next().expect("peeked"));
                }
            }
            let display_base = group[0]
                .umbrella_display
                .clone()
                .unwrap_or_else(|| group[0].display_name.clone());
            let post_step_id = uuid::Uuid::new_v4().to_string();
            let post_name_native = post_step_display_name(&display_base);
            let native_post_started_at = self.emit_step_started(
                post_step_id.clone(),
                post_name_native.clone(),
                &mut timeline_order,
            );
            let umbrella_group = group.len() > 1 || group[0].umbrella_display.is_some();
            let mut combined_lines = vec![format!("##[group]{post_name_native}")];
            if !umbrella_group {
                combined_lines.extend(native_post_log_prelude(&group[0].invocation, &state));
            }
            combined_lines.push("##[endgroup]".to_string());
            let mut combined = StepExecutionResult {
                exit_code: 0,
                state: StepCommandState::default(),
                skipped: false,
                failure_ignored: false,
                stdout: String::new(),
                stderr: String::new(),
            };
            let mut errored = false;
            for member in &group {
                let result = self.execute_native_post_action(
                    container,
                    &member.step_id,
                    &member.invocation,
                    &state,
                    effective_step_timeout(member.timeout_minutes, self.job_timeout_minutes),
                );
                match result {
                    Ok(mut result) => {
                        if result.exit_code != 0 && member.continue_on_error {
                            result.failure_ignored = true;
                        }
                        if umbrella_group {
                            combined_lines.push(format!("##[group]Post {}", member.display_name));
                            combined_lines
                                .extend(native_post_log_prelude(&member.invocation, &state));
                            combined_lines.push("##[endgroup]".to_string());
                        }
                        combined_lines
                            .extend(rendered_output_lines(&result.stdout, &result.stderr));
                        if result.exit_code != 0
                            && !result.failure_ignored
                            && combined.exit_code == 0
                        {
                            combined.exit_code = result.exit_code;
                        }
                        combined.failure_ignored |= result.failure_ignored;
                        combined.state.merge(result.state.clone());
                        state.apply(&post_step_id, &result);
                        executed_physical_actions += 1;
                        results.push(result);
                    }
                    Err(error) => {
                        errored = true;
                        if step_error.is_none() {
                            step_error = Some(error);
                        }
                    }
                }
            }
            if errored && combined.exit_code == 0 {
                combined.exit_code = 1;
            }
            let log = StepLog {
                step_id: post_step_id.clone(),
                display_name: post_name_native.clone(),
                order: timeline_order,
                started_at: native_post_started_at,
                completed_at: unix_now_rfc3339(),
                lines: combined_lines,
                masks: combined.state.masks.clone(),
                annotations: combined.state.annotations.clone(),
                telemetry: combined.state.telemetry.clone(),
                exit_code: combined.exit_code,
                skipped: false,
                failure_ignored: combined.failure_ignored,
                error_count: combined.state.error_count,
                warning_count: combined.state.warning_count,
                notice_count: combined.state.notice_count,
                summary: combined.state.summary.clone(),
            };
            self.emit_step_log(&log);
            step_logs.push(log);
        }
        for post_action in post_actions {
            let js_post_step_id = uuid::Uuid::new_v4().to_string();
            let js_post_name = post_step_display_name(&post_action.display_name);
            let js_post_started_at = self.emit_step_started(
                js_post_step_id.clone(),
                js_post_name.clone(),
                &mut timeline_order,
            );
            let result = self.execute_javascript_action_in_started_container(
                container,
                &js_post_step_id,
                &post_action.invocation,
                post_action
                    .invocation
                    .post_container_path
                    .as_deref()
                    .expect("post action must have post entrypoint"),
                &state.action_state_env(&post_action.step_id),
                temp_host,
                &state,
                effective_step_timeout(post_action.timeout_minutes, self.job_timeout_minutes),
            );
            match result {
                Ok(mut result) => {
                    if result.exit_code != 0 && post_action.continue_on_error {
                        result.failure_ignored = true;
                    }
                    let log = step_log_with_name(
                        &js_post_step_id,
                        &js_post_name,
                        timeline_order,
                        &js_post_started_at,
                        &unix_now_rfc3339(),
                        &result,
                        &javascript_post_log_prelude(&post_action.invocation, &state),
                    );
                    self.emit_step_log(&log);
                    step_logs.push(log);
                    state.apply(&js_post_step_id, &result);
                    executed_physical_actions += 1;
                    results.push(result);
                }
                Err(error) => {
                    if step_error.is_none() {
                        step_error = Some(error);
                    }
                }
            }
        }
        if target_materialized
            && step_error.is_none()
            && persistent_target_results_publishable(&results)
            && let Err(error) = publish_persistent_target(
                container,
                state.env.get("GITHUB_SHA").map(String::as_str),
            )
        {
            eprintln!(
                "forensics.lifecycle: persistent target publish skipped for '{}': {error:#}",
                container.name
            );
        }
        if let Some(error) = step_error {
            return Err(error);
        }
        Ok(JobExecutionSummary {
            job_outputs: evaluate_job_outputs(job_outputs, &state),
            environment_url: evaluate_environment_url(environment_url, &state),
            step_results: results,
            executed_physical_actions,
            step_logs,
        })
    }

    pub fn execute_javascript_action(
        &mut self,
        container: &JobContainerSpec,
        step_id: &str,
        action: &JavaScriptActionInvocation,
        temp_host: &Path,
    ) -> Result<StepExecutionResult> {
        self.start_job_environment(container)?;

        let result = self.execute_javascript_action_in_started_container(
            container,
            step_id,
            action,
            &action.main_container_path,
            &[],
            temp_host,
            &JobExecutionState::default(),
            DEFAULT_STEP_TIMEOUT,
        );

        if let Err(error) = self.cleanup(container) {
            eprintln!("Warning: cleanup failed after JavaScript action: {error:#}");
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_javascript_action_in_started_container(
        &mut self,
        container: &JobContainerSpec,
        step_id: &str,
        action: &JavaScriptActionInvocation,
        entrypoint_container_path: &str,
        action_state_env: &[(String, String)],
        temp_host: &Path,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let command_files = ScriptStepPlan::prepare(
            &ScriptStep {
                id: step_id.to_string(),
                display_name: String::new(),
                script: String::new(),
                shell: crate::container::Shell::Sh,
                working_directory_container: "/__w".to_string(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            temp_host,
        )?;
        let action_state = state.with_env(action_context_env(&action.env));
        let mut env = action_state.step_env(&[]);
        env.extend(action_state.resolve_env(&action.env));
        env.extend(action_state_env.iter().cloned());
        env.extend(command_files.env.iter().cloned());
        rewrite_command_file_env_for_action_container(&mut env);
        let node_image = node_action_image(&action.node, &container.node_action_image);
        let secret_masks = action_state.secret_masks(&self.secret_masks);
        let exec_args = container.prepare_run_node_action_args(
            "/__w",
            &env,
            &secret_masks,
            &state.path,
            &node_image,
            entrypoint_container_path,
        )?;
        let step_result = self.runner.run_timeout_with_env(
            "docker",
            exec_args.args(),
            exec_args.process_env(),
            timeout,
        )?;
        let mut state = command_files.collect_state()?;
        state.merge(parse_workflow_commands_from_output(
            &step_result.stdout,
            &step_result.stderr,
        ));
        Ok(StepExecutionResult {
            exit_code: step_result.code,
            state,
            skipped: false,
            failure_ignored: false,
            stdout: step_result.stdout,
            stderr: step_result.stderr,
        })
    }

    fn execute_checkout_step(
        &mut self,
        container: &JobContainerSpec,
        plan: &CheckoutPlan,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let plan = resolve_checkout_plan_expressions(plan, state)?;
        let mut trace = Vec::new();
        let mirror_store =
            crate::container::git_mirror_store_host(&container.temp_host, &self.trust_scope);
        let checkout_result = {
            let _span = tracing::info_span!("job-checkout").entered();
            execute_checkout_with_mirror(&mut self.runner, &plan, &mut trace, Some(&mirror_store))
        };
        if let Err(error) = checkout_result {
            if let Some(failure) = error.downcast_ref::<StepLogicFailure>() {
                return Ok(failure.to_step_result(Some(trace.join("\n"))));
            }
            return Err(error);
        }
        configure_safe_directory(
            &container.home_host,
            &container.workspace_host,
            &plan.destination,
        )?;
        // Surface the git-command trace as the step's output so the runtime
        // checkout step log matches the eager path (and GitHub-hosted).
        Ok(StepExecutionResult {
            exit_code: 0,
            state: StepCommandState::default(),
            skipped: false,
            failure_ignored: false,
            stdout: trace.join("\n"),
            stderr: String::new(),
        })
    }

    fn execute_docker_action_in_started_container(
        &mut self,
        container: &JobContainerSpec,
        step_id: &str,
        action: &DockerActionInvocation,
        temp_host: &Path,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        if let (Some(context_host), Some(dockerfile_host)) =
            (&action.build_context_host, &action.dockerfile_host)
        {
            let docker_config = temp_host.join("_velnor/docker-client");
            fs::create_dir_all(&docker_config).with_context(|| {
                format!(
                    "create isolated Docker client config {}",
                    docker_config.display()
                )
            })?;
            fs::set_permissions(&docker_config, fs::Permissions::from_mode(0o700)).with_context(
                || {
                    format!(
                        "secure isolated Docker client config {}",
                        docker_config.display()
                    )
                },
            )?;
            self.run_docker_with_env(
                &container.build_docker_action_args(&action.image, dockerfile_host, context_host),
                &[(
                    "DOCKER_CONFIG".to_string(),
                    docker_config.display().to_string(),
                )],
            )?;
        }
        let command_files = ScriptStepPlan::prepare(
            &ScriptStep {
                id: step_id.to_string(),
                display_name: String::new(),
                script: String::new(),
                shell: crate::container::Shell::Sh,
                working_directory_container: "/__w".to_string(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            temp_host,
        )?;
        let action_state = state.with_env(action_context_env(&action.env));
        let mut env = action_state.step_env(&[]);
        env.extend(action_state.resolve_env(&action.env));
        set_env_value(&mut env, "GITHUB_WORKSPACE", "/github/workspace");
        set_env_value(&mut env, "RUNNER_TEMP", "/github/runner_temp");
        env.extend(command_files.env.iter().cloned());
        rewrite_command_file_env_for_action_container(&mut env);
        let entrypoint = action
            .entrypoint
            .as_ref()
            .map(|value| state.resolve_expressions(value));
        let args = action
            .args
            .iter()
            .map(|value| state.resolve_expressions(value))
            .collect::<Vec<_>>();
        let secret_masks = action_state.secret_masks(&self.secret_masks);
        let exec_args = container.prepare_run_docker_action_args(
            "/github/workspace",
            &env,
            &secret_masks,
            &action.image,
            entrypoint.as_deref(),
            &args,
        )?;
        let step_result = self.runner.run_timeout_with_env(
            "docker",
            exec_args.args(),
            exec_args.process_env(),
            timeout,
        )?;
        let mut state = command_files.collect_state()?;
        state.merge(parse_workflow_commands_from_output(
            &step_result.stdout,
            &step_result.stderr,
        ));
        Ok(StepExecutionResult {
            exit_code: step_result.code,
            state,
            skipped: false,
            failure_ignored: false,
            stdout: step_result.stdout,
            stderr: step_result.stderr,
        })
    }

    fn execute_native_action_in_started_container(
        &mut self,
        _container: &JobContainerSpec,
        step_id: &str,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let cache_store = match action.adapter {
            NativeActionAdapter::Cache
                if !matches!(action.cache_kind, Some(CacheActionKind::Save)) =>
            {
                Some("actions_cache")
            }
            NativeActionAdapter::RustCache => Some("rust_cache"),
            _ => None,
        };
        let lookup_started = cache_store.map(|_| Instant::now());
        let artifact_started =
            (action.adapter == NativeActionAdapter::UploadArtifact).then(Instant::now);
        let result = match action.adapter {
            NativeActionAdapter::Cache => native_cache(action, state),
            NativeActionAdapter::UploadArtifact => native_upload_artifact(action, state),
            NativeActionAdapter::DownloadArtifact => native_download_artifact(action, state),
            NativeActionAdapter::UploadPagesArtifact => native_upload_pages_artifact(action, state),
            NativeActionAdapter::ConfigurePages => native_configure_pages(action, state),
            NativeActionAdapter::DeployPages => native_deploy_pages(action, state),
            NativeActionAdapter::AttestBuildProvenance => {
                native_attest_build_provenance(action, state)
            }
            NativeActionAdapter::CreateGitHubAppToken => {
                native_create_github_app_token(action, state)
            }
            NativeActionAdapter::Mise => self.native_mise(_container, action, state, timeout),
            NativeActionAdapter::Sccache => self.native_sccache(_container, action, state, timeout),
            NativeActionAdapter::Kache => self.native_kache(_container, action, state, timeout),
            NativeActionAdapter::SetupMold => {
                self.native_setup_mold(_container, action, state, timeout)
            }
            NativeActionAdapter::SetupJust => {
                self.native_setup_just(_container, action, state, timeout)
            }
            NativeActionAdapter::RustCache => native_rust_cache(action, state),
            NativeActionAdapter::Renovate => {
                self.native_renovate(_container, action, state, timeout)
            }
            NativeActionAdapter::GitHubRuntimeExport => {
                Ok(native_github_runtime_export(action, state))
            }
            NativeActionAdapter::GitHubScript => native_github_script(action, state),
            NativeActionAdapter::PathsFilter => self.native_paths_filter(action, state),
            NativeActionAdapter::DockerSetupBuildx => {
                self.native_docker_setup_buildx(_container, action, state, timeout)
            }
            NativeActionAdapter::DockerLogin => {
                self.native_docker_login(_container, action, state, timeout)
            }
            NativeActionAdapter::DockerMetadata => Ok(native_docker_metadata(action, state)),
            NativeActionAdapter::DockerBuildPush => {
                self.native_docker_build_push(_container, action, state, timeout)
            }
            NativeActionAdapter::DockerBake => {
                self.native_docker_bake(_container, action, state, timeout)
            }
            NativeActionAdapter::Hadolint => {
                self.native_hadolint(_container, action, state, timeout)
            }
            NativeActionAdapter::SetupQemu => self.native_setup_qemu(action, state, timeout),
            NativeActionAdapter::CosignInstaller => {
                self.native_cosign_installer(_container, action, state, timeout)
            }
            _ => {
                bail!(
                    "native action adapter {:?} for step '{}' is declared but not implemented yet",
                    action.adapter,
                    step_id
                )
            }
        };
        if let (Some(store), Some(started), Some(telemetry)) = (
            cache_store,
            lookup_started,
            self.lifecycle_telemetry.as_ref(),
        ) {
            telemetry.emit_cache_lookup(
                store,
                result.as_ref().ok(),
                result.is_err(),
                started.elapsed(),
            );
        }
        if let (Some(started), Some(telemetry), Ok(result)) = (
            artifact_started,
            self.lifecycle_telemetry.as_ref(),
            result.as_ref(),
        ) {
            telemetry.emit_artifact_materialize(result, started.elapsed());
        }
        result
    }

    fn execute_native_post_action(
        &mut self,
        _container: &JobContainerSpec,
        step_id: &str,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        match action.adapter {
            NativeActionAdapter::Cache => native_cache_save(step_id, action, state),
            NativeActionAdapter::RustCache => native_rust_cache_save(step_id, action, state),
            // Sccache post: show stats then stop server. Soft-fail if not running.
            NativeActionAdapter::Sccache => {
                let result = self.native_shell(
                    _container,
                    state,
                    "stats=$(sccache --show-stats 2>&1 || true); printf '%s\\n' \"$stats\"; if [ -n \"${GITHUB_STEP_SUMMARY:-}\" ]; then printf '## sccache statistics\\n```text\\n%s\\n```\\n' \"$stats\" >> \"$GITHUB_STEP_SUMMARY\"; fi; sccache --stop-server 2>/dev/null || true",
                    timeout,
                )?;
                Ok(native_command_result(result, StepCommandState::default()))
            }
            NativeActionAdapter::Kache => {
                let result = self.native_shell(
                    _container,
                    state,
                    "stats=$(kache stats 2>&1 || true); printf '%s\\n' \"$stats\"; if [ -n \"${GITHUB_STEP_SUMMARY:-}\" ]; then printf '## Kache statistics\\n```text\\n%s\\n```\\n' \"$stats\" >> \"$GITHUB_STEP_SUMMARY\"; kache report --format github >> \"$GITHUB_STEP_SUMMARY\" 2>/dev/null || true; fi",
                    timeout,
                )?;
                Ok(native_command_result(result, StepCommandState::default()))
            }
            NativeActionAdapter::DockerSetupBuildx => {
                let action_state = state.with_env(state.resolve_env(&action.env));
                let requested_name =
                    native_input_or(&action_state, action, "name", "velnor-builder");
                let name = job_scoped_buildx_builder_name(&requested_name, state);
                if !input_truthy(&native_input_or(&action_state, action, "cleanup", "true")) {
                    return Ok(StepExecutionResult {
                        exit_code: 0,
                        state: StepCommandState::default(),
                        skipped: false,
                        failure_ignored: false,
                        stdout: String::new(),
                        stderr: String::new(),
                    });
                }
                let keep_state = input_truthy(&native_input_or(
                    &action_state,
                    action,
                    "keep-state",
                    "false",
                ));
                let keep_state_arg = if keep_state { " --keep-state" } else { "" };
                let script = format!(
                    "docker buildx rm{keep_state_arg} {name} 2>/dev/null || true; echo \"Removing builder {name}\""
                );
                let result = self.native_shell(_container, state, &script, timeout)?;
                Ok(native_command_result(result, StepCommandState::default()))
            }
            NativeActionAdapter::DockerLogin => {
                let action_state = state.with_env(state.resolve_env(&action.env));
                let registry = native_input_or(&action_state, action, "registry", "");
                let script = if registry.is_empty() {
                    "docker logout 2>/dev/null || true".to_string()
                } else {
                    format!("docker logout {registry} 2>/dev/null || true")
                };
                let result = self.native_shell(_container, state, &script, timeout)?;
                Ok(native_command_result(result, StepCommandState::default()))
            }
            NativeActionAdapter::CreateGitHubAppToken => native_revoke_github_app_token(state),
            _ => bail!(
                "native action adapter {:?} for step '{}' does not have a post action",
                action.adapter,
                step_id
            ),
        }
    }

    fn native_mise(
        &mut self,
        container: &JobContainerSpec,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let prep_started = Instant::now();
        let result = self.native_mise_inner(container, action, state, timeout);
        if let Some(telemetry) = &self.lifecycle_telemetry {
            telemetry.emit_tool_prep(prep_started.elapsed());
        }
        result
    }

    fn native_mise_inner(
        &mut self,
        container: &JobContainerSpec,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let install = input_truthy(&native_input_or(&action_state, action, "install", "true"));
        let version = native_input(action, &action_state, "version");
        let install_args = native_input(action, &action_state, "install_args");
        let working_directory = native_input(action, &action_state, "working_directory");
        let cache_key_prefix =
            native_input_or(&action_state, action, "cache_key_prefix", "mise-v2");
        let cache_save = input_truthy(&native_input_or(
            &action_state,
            action,
            "cache_save",
            "true",
        ));
        // 008-R5: resolve the exact mise binary version now (an omitted version
        // becomes the fleet pin — never a live "latest" lookup) so it is
        // auditable and keys the persistent binary store.
        let exact_version = crate::mise::resolve_effective_mise_version(&version)
            .context("resolve exact mise binary version")?;
        eprintln!(
            "mise: resolved binary version {exact_version} (requested {:?})",
            version.trim()
        );
        // Fail closed before any install: the effective config must have a
        // tracked, clean, adjacent lock with lockfile=true, and every
        // install_args token must be committed in that lock.
        if install {
            let checkout_root = container.workspace_host.as_path();
            let workdir_host = mise_working_directory_host(checkout_root, &working_directory);
            crate::mise::enforce_locked_mise_policy(
                checkout_root,
                &workdir_host,
                &install_args,
                &crate::mise::GitCli,
            )
            .context("mise locked-install policy rejected this job before running any command")?;
        }
        let script = setup_mise_script(
            install,
            &exact_version,
            &install_args,
            &working_directory,
            &cache_key_prefix,
            cache_save,
        );
        let mut result = self.native_shell(container, state, &script, timeout)?;
        // A shell failure can occur before the environment export files are
        // written (for example, a failed tool install or `mise env`). Preserve
        // that step failure and its diagnostics instead of turning it into a
        // daemon-cycle error while trying to read a file that cannot exist.
        if result.code != 0 {
            return Ok(native_command_result(result, StepCommandState::default()));
        }
        let temp_host = state
            .temp_host
            .as_deref()
            .context("mise adapter requires the mounted job temp directory")?;
        let env_path = temp_host.join("_velnor/mise-env.json");
        let redacted_path = temp_host.join("_velnor/mise-env-redacted.json");
        let exported_env = fs::read_to_string(&env_path)
            .with_context(|| format!("read mise environment from {}", env_path.display()))?;
        let redacted_env = fs::read_to_string(&redacted_path).with_context(|| {
            format!(
                "read mise redacted environment from {}",
                redacted_path.display()
            )
        })?;
        let (mut env, masks) = parse_mise_environment(&exported_env, &redacted_env)?;
        let _ = fs::remove_file(&env_path);
        let _ = fs::remove_file(&redacted_path);
        let path = mise_step_path(&result.stdout);
        if result.stdout.contains("__VELNOR_MISE_BIN__")
            || result.stdout.contains("__VELNOR_MISE_SELF__")
        {
            let filtered: Vec<&str> = result
                .stdout
                .lines()
                .filter(|l| {
                    !l.starts_with("__VELNOR_MISE_BIN__") && !l.starts_with("__VELNOR_MISE_SELF__")
                })
                .collect();
            result.stdout = filtered.join("\n");
            if !result.stdout.is_empty() {
                result.stdout.push('\n');
            }
        }
        env.extend([
            ("MISE_DATA_DIR".to_string(), "/opt/mise".to_string()),
            ("MISE_CACHE_DIR".to_string(), "/opt/mise/cache".to_string()),
            (
                "MISE_CONFIG_DIR".to_string(),
                "/opt/mise/config".to_string(),
            ),
            ("MISE_TRUSTED_CONFIG_PATHS".to_string(), "/__w".to_string()),
        ]);
        Ok(native_command_result(
            result,
            StepCommandState {
                env,
                path,
                masks,
                ..StepCommandState::default()
            },
        ))
    }

    fn native_sccache(
        &mut self,
        container: &JobContainerSpec,
        _action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let result = self.native_shell(container, state, &sccache_setup_script(), timeout)?;
        Ok(native_command_result(result, StepCommandState::default()))
    }

    fn native_kache(
        &mut self,
        container: &JobContainerSpec,
        _action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let result = self.native_shell(container, state, &kache_setup_script(), timeout)?;
        let mut command_state = StepCommandState::default();
        command_state.set_env("KACHE_CACHE_DIR".into(), "/var/cache/kache".into());
        command_state.set_env("KACHE_MAX_SIZE".into(), "20GiB".into());
        command_state.set_env("RUSTC_WRAPPER".into(), "kache".into());
        Ok(native_command_result(result, command_state))
    }

    fn native_setup_mold(
        &mut self,
        container: &JobContainerSpec,
        _action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let result = self.native_shell(container, state, &setup_mold_script(), timeout)?;
        Ok(native_command_result(result, StepCommandState::default()))
    }

    /// Native `hadolint/hadolint-action`: runs the hadolint binary preinstalled
    /// in the job image. hadolint natively reads the same HADOLINT_* /
    /// NO_COLOR environment variables the marketplace action's Docker image
    /// sets, so input mapping mirrors the action's `runs.env` block exactly;
    /// like the action's wrapper script, the findings are exposed as the
    /// `results` step output and the newline-stripped HADOLINT_RESULTS env.
    fn native_hadolint(
        &mut self,
        container: &JobContainerSpec,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let inputs = HadolintInputs {
            dockerfile: native_input_or(&action_state, action, "dockerfile", "Dockerfile"),
            config: native_input(action, &action_state, "config"),
            recursive: native_input_or(&action_state, action, "recursive", "false"),
            output_file: native_input_or(&action_state, action, "output-file", "/dev/stdout"),
            no_color: native_input_or(&action_state, action, "no-color", "false"),
            no_fail: native_input_or(&action_state, action, "no-fail", "false"),
            verbose: native_input_or(&action_state, action, "verbose", "false"),
            format: native_input_or(&action_state, action, "format", "tty"),
            failure_threshold: native_input_or(&action_state, action, "failure-threshold", "info"),
            override_error: native_input(action, &action_state, "override-error"),
            override_warning: native_input(action, &action_state, "override-warning"),
            override_info: native_input(action, &action_state, "override-info"),
            override_style: native_input(action, &action_state, "override-style"),
            ignore: native_input(action, &action_state, "ignore"),
            trusted_registries: native_input(action, &action_state, "trusted-registries"),
        };
        let result =
            self.native_shell(container, &action_state, &hadolint_script(&inputs), timeout)?;
        let findings = result.stdout.trim_end().to_string();
        let mut outputs = BTreeMap::new();
        outputs.insert("results".to_string(), findings.clone());
        let mut env = BTreeMap::new();
        env.insert("HADOLINT_RESULTS".to_string(), findings.replace('\n', ""));
        Ok(native_command_result(
            result,
            StepCommandState {
                outputs,
                env,
                ..StepCommandState::default()
            },
        ))
    }

    fn native_setup_just(
        &mut self,
        container: &JobContainerSpec,
        _action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let result = self.native_shell(container, state, &setup_just_script(), timeout)?;
        Ok(native_command_result(
            result,
            StepCommandState {
                // just is now a locked mise tool; expose the mise shims dir.
                path: vec!["/opt/mise/shims".to_string()],
                ..StepCommandState::default()
            },
        ))
    }

    /// Native `docker/setup-qemu-action`: installs binfmt handlers via a pinned
    /// QEMU static-binaries image. This still needs `--privileged` because it
    /// mutates host-global binfmt_misc state; workflow-supplied image overrides
    /// are ignored so that privileged container is not attacker-controlled.
    fn native_setup_qemu(
        &mut self,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let requested_image =
            native_input_or(&action_state, action, "image", SETUP_QEMU_BINFMT_IMAGE);
        if requested_image.trim() != SETUP_QEMU_BINFMT_IMAGE {
            eprintln!(
                "Velnor ignored docker/setup-qemu-action image override `{}`; using pinned {}",
                requested_image.trim(),
                SETUP_QEMU_BINFMT_IMAGE
            );
        }
        let image = SETUP_QEMU_BINFMT_IMAGE.to_string();
        let platforms = native_input_or(&action_state, action, "platforms", "all");
        let reset = native_input_or(&action_state, action, "reset", "false");
        if reset.trim() == "true" {
            let uninstall = vec![
                "run".to_string(),
                "--rm".to_string(),
                "--privileged".to_string(),
                image.clone(),
                "--uninstall".to_string(),
                "qemu-*".to_string(),
            ];
            let result = self.run_docker_args_timeout(&uninstall, timeout)?;
            if result.code != 0 {
                return Ok(native_command_result(result, StepCommandState::default()));
            }
        }
        let args = vec![
            "run".to_string(),
            "--rm".to_string(),
            "--privileged".to_string(),
            image,
            "--install".to_string(),
            platforms.clone(),
        ];
        let result = self.run_docker_args_timeout(&args, timeout)?;
        let mut outputs = BTreeMap::new();
        outputs.insert("platforms".to_string(), platforms);
        Ok(native_command_result(
            result,
            StepCommandState {
                outputs,
                ..StepCommandState::default()
            },
        ))
    }

    /// Native `sigstore/cosign-installer`: install the admitted locked `cosign`
    /// mise key fail-closed, link it into install-dir (added to PATH like the
    /// action), and verify the version. A requested release differing from the
    /// committed lock fails closed — Velnor never downloads cosign outside mise.
    fn native_cosign_installer(
        &mut self,
        container: &JobContainerSpec,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let release = native_input(action, &action_state, "cosign-release");
        let install_dir = native_input_or(&action_state, action, "install-dir", "$HOME/.cosign");
        // install-dir is interpolated inside double quotes so $HOME expands
        // in-shell (the action's documented default) — reject anything that
        // could escape the quoting or substitute commands.
        if install_dir.contains(['"', '`', ';', '&', '|', '\n', '\\']) || install_dir.contains("$(")
        {
            bail!("cosign-installer install-dir contains shell metacharacters: {install_dir:?}");
        }
        let script = cosign_installer_script(&release, &install_dir);
        let mut result = self.native_shell(container, &action_state, &script, timeout)?;
        // The action adds install-dir to GITHUB_PATH; mirror that by parsing
        // the resolved directory marker (install-dir may contain $HOME).
        let mut path = Vec::new();
        let mut kept = Vec::new();
        for line in result.stdout.lines() {
            if let Some(dir) = line.strip_prefix("__VELNOR_COSIGN_DIR__") {
                path.push(dir.to_string());
            } else {
                kept.push(line.to_string());
            }
        }
        result.stdout = kept.join("\n");
        Ok(native_command_result(
            result,
            StepCommandState {
                path,
                ..StepCommandState::default()
            },
        ))
    }

    /// Run a docker CLI command inside the job container (docker exec), so
    /// client-side state — buildx builder registry, registry logins — lives
    /// in the job container's config and is visible to run steps too
    /// (host-side invocations write the daemon host's ~/.docker instead).
    fn container_docker(
        &mut self,
        container: &JobContainerSpec,
        state: &JobExecutionState,
        docker_args: &[String],
        stdin: Option<&str>,
        timeout: Duration,
    ) -> Result<CommandResult> {
        let mut cmd = String::from("docker");
        for arg in docker_args {
            cmd.push(' ');
            cmd.push_str(&shell_single_quote(arg));
        }
        if let Some(stdin) = stdin {
            let env = state.step_env(&[]);
            // Docker client state (e.g. the login written here) lands in the
            // job-home config dir (/github/home/.docker, via the exec base env
            // HOME=/github/home) — the same dir run steps and every other
            // adapter invocation read; it persists for the whole job through
            // the home bind mount.
            let secret_masks = state.secret_masks(&self.secret_masks);
            let exec_args = container.prepare_exec_process_stdin_args(
                "/__w",
                &env,
                &secret_masks,
                &["sh".to_string(), "-c".to_string(), cmd],
            )?;
            return self.runner.run_with_stdin_timeout_with_env(
                "docker",
                exec_args.args(),
                exec_args.process_env(),
                stdin,
                timeout,
            );
        }
        self.native_shell(container, state, &cmd, timeout)
    }

    fn run_docker_args(&mut self, args: &[String]) -> Result<CommandResult> {
        self.runner.run("docker", args)
    }

    fn run_docker_args_timeout(
        &mut self,
        args: &[String],
        timeout: Duration,
    ) -> Result<CommandResult> {
        self.runner.run_timeout("docker", args, timeout)
    }

    fn native_shell(
        &mut self,
        container: &JobContainerSpec,
        state: &JobExecutionState,
        script: &str,
        timeout: Duration,
    ) -> Result<CommandResult> {
        let env = state.step_env(&[]);
        // Build PATH from accumulated state paths plus the container's default paths.
        // We must set it explicitly because OrbStack (on macOS) injects the host
        // macOS PATH into all container processes, which overrides the Dockerfile ENV.
        // /root/.cargo/bin (image-baked rustup proxies) stays ahead of the mise
        // shims so cargo resolves under the truthful CARGO_HOME (the mise rust
        // shim derives its bin path from $CARGO_HOME, which is an empty bind at
        // job start). HOME/RUSTUP_HOME/CARGO_HOME come from the exec base env
        // (JobContainerSpec::append_base_exec_env) — the old HOME=/root +
        // CARGO_HOME=/root/.cargo exports here redirected cargo downloads into
        // the unmounted container /root, making `~` caches unsaveable.
        let container_default_path =
            "/root/.cargo/bin:/opt/mise/bin:/opt/mise/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
        let path_entries: Vec<&str> = state
            .path
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(container_default_path))
            .collect();
        let path = path_entries.join(":");
        let wrapped = format!("export PATH={path}; {script}");
        let secret_masks = state.secret_masks(&self.secret_masks);
        let args = container.prepare_exec_process_args(
            "/__w",
            &env,
            &secret_masks,
            &["sh".to_string(), "-c".to_string(), wrapped],
        )?;
        // Stream adapter output to the live feed as it happens — a minutes-long
        // native step (mise install on a cold store) must not look frozen in
        // the live view while the GitHub lane streams continuously.
        let live_sender = self.step_log_sender.clone();
        let live_step = self.live_step.clone();
        let mut live_masks = Vec::new();
        let mut on_output = |_: CommandStream, line: &str| {
            // Internal adapter markers are stripped from the blob after the
            // run; keep them out of the live feed too.
            if line.starts_with("__VELNOR_MISE_BIN__") {
                return;
            }
            live_masks.extend(parse_workflow_commands(line).masks);
            if let Some(live) = live_step.as_ref() {
                emit_live_step_log(
                    &live_sender,
                    &live.step_id,
                    &live.display_name,
                    live.order,
                    &live.started_at,
                    line,
                    &live_masks,
                );
            }
        };
        self.runner.run_streaming_timeout_with_env(
            "docker",
            args.args(),
            args.process_env(),
            timeout,
            &mut on_output,
        )
    }

    fn native_renovate(
        &mut self,
        container: &JobContainerSpec,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let version = native_input(action, &action_state, "renovate-version");
        let image = native_input(action, &action_state, "renovate-image");
        let image = if !image.is_empty() {
            image
        } else if !version.is_empty() {
            format!("ghcr.io/renovatebot/renovate:{version}")
        } else {
            "ghcr.io/renovatebot/renovate:latest".to_string()
        };
        let token = native_input(action, &action_state, "token");
        let mut env = action_state.step_env(&[]);
        env.extend(action_state.resolve_env(&action.env));
        if !token.is_empty() {
            set_env_value(&mut env, "RENOVATE_TOKEN", &token);
            set_env_value(&mut env, "INPUT_TOKEN", &token);
        }
        if let Some(repository) = action_state.env.get("GITHUB_REPOSITORY") {
            set_env_value(&mut env, "RENOVATE_REPOSITORIES", repository);
        }
        let secret_masks = action_state.secret_masks(&self.secret_masks);
        let args = container.prepare_run_docker_action_args(
            "/github/workspace",
            &env,
            &secret_masks,
            &image,
            None,
            &[],
        )?;
        Ok(native_command_result(
            self.runner
                .run_timeout_with_env("docker", args.args(), args.process_env(), timeout)?,
            StepCommandState::default(),
        ))
    }

    fn native_docker_setup_buildx(
        &mut self,
        container: &JobContainerSpec,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let requested_name = native_input_or(&action_state, action, "name", "velnor-builder");
        let name = job_scoped_buildx_builder_name(&requested_name, state);
        let driver = native_input_or(&action_state, action, "driver", "docker-container");
        let buildkitd_config_inline =
            native_input(action, &action_state, "buildkitd-config-inline");
        let buildkitd_config_container = if buildkitd_config_inline.is_empty() {
            None
        } else {
            let config_name = format!("buildkitd-config-{}.toml", sanitize_artifact_name(&name));
            let config_host = state
                .temp_host
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("setup-buildx requires a runner temp directory"))?
                .join(&config_name);
            fs::write(&config_host, &buildkitd_config_inline)
                .with_context(|| format!("write BuildKit config {}", config_host.display()))?;
            Some(format!("/__t/{config_name}"))
        };
        let inspect_args = vec!["buildx".to_string(), "inspect".to_string(), name.clone()];
        let inspect_result =
            self.container_docker(container, &action_state, &inspect_args, None, timeout)?;
        let result = if inspect_result.code == 0 {
            let use_args = vec!["buildx".to_string(), "use".to_string(), name.clone()];
            self.container_docker(container, &action_state, &use_args, None, timeout)?
        } else {
            let mut args = vec![
                "buildx".to_string(),
                "create".to_string(),
                "--name".to_string(),
                name.clone(),
                "--driver".to_string(),
                driver,
                "--use".to_string(),
            ];
            let resource_options = buildx_driver_resource_options(&container.resource_options)?;
            if !resource_options.is_empty() {
                args.extend(["--driver-opt".to_string(), resource_options.join(",")]);
            }
            if let Some(config) = buildkitd_config_container {
                args.extend(["--config".to_string(), config]);
            }
            if input_truthy(&native_input_or(&action_state, action, "install", "false")) {
                args.push("--bootstrap".to_string());
            }
            self.container_docker(container, &action_state, &args, None, timeout)?
        };
        Ok(native_command_result(
            result,
            StepCommandState {
                outputs: [
                    ("name".to_string(), name.clone()),
                    (
                        "driver".to_string(),
                        native_input_or(&action_state, action, "driver", "docker-container"),
                    ),
                    (
                        "platforms".to_string(),
                        "linux/amd64,linux/arm64".to_string(),
                    ),
                ]
                .into(),
                env: [("BUILDX_BUILDER".to_string(), name)].into(),
                ..StepCommandState::default()
            },
        ))
    }

    fn native_docker_login(
        &mut self,
        container: &JobContainerSpec,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let registry = native_input_or(
            &action_state,
            action,
            "registry",
            "https://index.docker.io/v1/",
        );
        let username = native_input(action, &action_state, "username");
        let password = native_input(action, &action_state, "password");
        if !password.is_empty()
            && !crate::github_adapter::github_trust_scope_allows_host_docker(&self.trust_scope)
        {
            bail!(
                "docker/login-action refuses registry credentials in trust scope '{}'; accepted trust scope: trusted",
                self.trust_scope
            );
        }
        let args = vec![
            "login".to_string(),
            registry,
            "--username".to_string(),
            username,
            "--password-stdin".to_string(),
        ];
        Ok(native_command_result(
            self.container_docker(container, &action_state, &args, Some(&password), timeout)?,
            StepCommandState::default(),
        ))
    }

    fn native_docker_build_push(
        &mut self,
        container: &JobContainerSpec,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let context = native_input_or(&action_state, action, "context", ".");
        let mut args = vec!["buildx".to_string(), "build".to_string()];
        // build-push-action resolves an explicit `file` from the workspace,
        // independently from `context`. Passing context/file twice here turned
        // `context: docker`, `file: docker/Dockerfile` into
        // `docker/docker/Dockerfile`.
        let file_input = native_input(action, &action_state, "file");
        if !file_input.trim().is_empty() {
            push_arg(&mut args, "--file", &file_input);
        }
        push_arg(
            &mut args,
            "--platform",
            &native_input(action, &action_state, "platforms"),
        );
        for tag in input_values(&native_input(action, &action_state, "tags")) {
            push_arg(&mut args, "--tag", &tag);
        }
        for label in input_values(&native_input(action, &action_state, "labels")) {
            push_arg(&mut args, "--label", &label);
        }
        for build_arg in input_values(&native_input(action, &action_state, "build-args")) {
            push_arg(&mut args, "--build-arg", &build_arg);
        }
        let secret_input = native_input(action, &action_state, "secrets");
        if !secret_input.trim().is_empty() && self.trust_scope != "trusted" {
            bail!(
                "docker/build-push-action refuses BuildKit secrets in trust scope '{}'; accepted trust scope: trusted",
                self.trust_scope
            );
        }
        let mut secret_files = Vec::new();
        for secret in secret_input.lines().filter(|line| !line.trim().is_empty()) {
            let (id, value) = secret
                .split_once('=')
                .filter(|(id, value)| !id.is_empty() && !value.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("docker/build-push-action received an invalid secret entry")
                })?;
            let secret_file = BuildSecretFile::create(
                action_state.temp_host.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("docker/build-push-action requires a runner temp directory")
                })?,
                value,
            )?;
            push_arg(
                &mut args,
                "--secret",
                &format!("id={id},src={}", secret_file.container_path()),
            );
            secret_files.push(secret_file);
        }
        for cache in input_values(&native_input(action, &action_state, "cache-from")) {
            push_arg(&mut args, "--cache-from", &cache);
        }
        for cache in input_values(&native_input(action, &action_state, "cache-to")) {
            push_arg(&mut args, "--cache-to", &cache);
        }
        // The `outputs` input maps to buildx --output (e.g. the publish
        // workflows' push-by-digest exporter: type=image,push-by-digest=true,
        // name=...,push=true).
        let mut has_output = false;
        for output in input_values(&native_input(action, &action_state, "outputs")) {
            push_arg(&mut args, "--output", &output);
            has_output = true;
        }
        if input_truthy(&native_input(action, &action_state, "push")) && !has_output {
            args.push("--push".to_string());
        }
        if input_truthy(&native_input(action, &action_state, "load")) && !has_output {
            args.push("--load".to_string());
        }
        // build-push-action exposes digest/imageid/metadata step outputs from
        // buildx's metadata file; downstream jobs (digest fan-in publish flows)
        // depend on `digest`.
        // The command runs inside the job container; /tmp there is the shared
        // job temp mount, so the daemon reads the file back via temp_host.
        let metadata_name = format!("velnor-buildx-metadata-{}.json", uuid::Uuid::new_v4());
        push_arg(
            &mut args,
            "--metadata-file",
            &format!("/tmp/{metadata_name}"),
        );
        args.push(container_context_path(&context));
        let result = self.container_docker(container, &action_state, &args, None, timeout)?;
        let metadata_path = action_state
            .temp_host
            .clone()
            .unwrap_or_else(std::env::temp_dir)
            .join(&metadata_name);
        let mut command_state = StepCommandState::default();
        if let Ok(raw) = fs::read_to_string(&metadata_path) {
            if let Ok(metadata) = serde_json::from_str::<Value>(&raw) {
                if let Some(digest) = metadata
                    .get("containerimage.digest")
                    .and_then(Value::as_str)
                {
                    command_state
                        .outputs
                        .insert("digest".to_string(), digest.to_string());
                }
                if let Some(image_id) = metadata
                    .get("containerimage.config.digest")
                    .and_then(Value::as_str)
                {
                    command_state
                        .outputs
                        .insert("imageid".to_string(), image_id.to_string());
                }
                command_state
                    .outputs
                    .insert("metadata".to_string(), raw.trim().to_string());
            }
            let _ = fs::remove_file(&metadata_path);
        }
        Ok(native_command_result(result, command_state))
    }

    fn native_docker_bake(
        &mut self,
        container: &JobContainerSpec,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
        timeout: Duration,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let mut args = vec!["buildx".to_string(), "bake".to_string()];
        // The command runs inside the job container with CWD /__w (the
        // workspace), so workflow-relative bake file paths resolve as-is.
        for file in input_values(&native_input(action, &action_state, "files")) {
            push_arg(&mut args, "--file", &file);
        }
        for set in input_values(&native_input(action, &action_state, "set")) {
            push_arg(&mut args, "--set", &set);
        }
        if input_truthy(&native_input(action, &action_state, "push")) {
            args.push("--push".to_string());
        }
        args.extend(input_values(&native_input(
            action,
            &action_state,
            "targets",
        )));
        let result = self.container_docker(container, &action_state, &args, None, timeout)?;
        Ok(native_command_result(result, StepCommandState::default()))
    }

    fn native_paths_filter(
        &mut self,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let filters = parse_paths_filter_rules(
            action
                .inputs
                .get("filters")
                .map(|value| state.resolve_expressions(value))
                .as_deref()
                .unwrap_or_default(),
        )?;
        let changed_files = self.changed_files_for_paths_filter(state)?;
        let mut outputs = BTreeMap::new();
        let mut matched_names = Vec::new();

        for (name, patterns) in filters {
            let globs = build_globs(&patterns)
                .with_context(|| format!("build paths-filter globs for '{name}'"))?;
            let matches = changed_files
                .iter()
                .filter(|file| globs.is_match(file.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let matched = !matches.is_empty();
            if matched {
                matched_names.push(name.clone());
            }
            outputs.insert(name.clone(), matched.to_string());
            outputs.insert(format!("{name}_count"), matches.len().to_string());
            outputs.insert(format!("{name}_files"), matches.join("\n"));
        }
        outputs.insert(
            "changes".to_string(),
            serde_json::to_string(&matched_names).context("serialize paths-filter changes")?,
        );

        Ok(StepExecutionResult {
            exit_code: 0,
            state: StepCommandState {
                outputs,
                ..StepCommandState::default()
            },
            skipped: false,
            failure_ignored: false,
            stdout: changed_files
                .iter()
                .map(|file| format!("changed: {file}"))
                .collect::<Vec<_>>()
                .join("\n"),
            stderr: String::new(),
        })
    }

    fn changed_files_for_paths_filter(&mut self, state: &JobExecutionState) -> Result<Vec<String>> {
        let workspace = state
            .workspace_host
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("paths-filter requires a workspace"))?;
        let base = paths_filter_base_ref(state);
        let head = paths_filter_head_ref(state);
        if let (Some(base), Some(head)) = (base.as_deref(), head.as_deref()) {
            let missing = [base, head].iter().any(|git_ref| {
                let args = vec![
                    "-C".to_string(),
                    workspace.display().to_string(),
                    "cat-file".to_string(),
                    "-e".to_string(),
                    format!("{git_ref}^{{commit}}"),
                ];
                match self.runner.run("git", &args) {
                    Ok(result) => result.code != 0,
                    Err(_) => true,
                }
            });
            if missing {
                let fetch_base = base.strip_prefix("origin/").map_or_else(
                    || base.to_string(),
                    |branch| format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
                );
                let fetch_args = vec![
                    "-C".to_string(),
                    workspace.display().to_string(),
                    "fetch".to_string(),
                    "--no-tags".to_string(),
                    "--depth=10".to_string(),
                    "origin".to_string(),
                    fetch_base,
                    head.to_string(),
                ];
                let fetched = self.runner.run("git", &fetch_args)?;
                if fetched.code != 0 {
                    bail!(
                        "git {} failed with code {}: {}",
                        fetch_args.join(" "),
                        fetched.code,
                        fetched.stderr
                    );
                }
            }
        }
        let mut args = vec![
            "-C".to_string(),
            workspace.display().to_string(),
            "diff".to_string(),
            "--name-only".to_string(),
        ];
        args.push(match (base.as_deref(), head.as_deref()) {
            (Some(base), Some(head)) if !base.is_empty() && !head.is_empty() => {
                format!("{base}...{head}")
            }
            (Some(base), _) if !base.is_empty() => format!("{base}..HEAD"),
            _ => "HEAD".to_string(),
        });
        let result = self.runner.run("git", &args)?;
        if result.code != 0 {
            bail!(
                "git {} failed with code {}: {}",
                args.join(" "),
                result.code,
                result.stderr
            );
        }
        let mut files = result
            .stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        Ok(files)
    }

    /// Disarm the job-network guard after terminal cleanup removed the
    /// network; a failed cleanup keeps it armed so its drop retries removal.
    fn defuse_job_network_guard(&mut self) {
        if let Some(guard) = self.job_network_guard.take() {
            guard.defuse();
        }
    }

    pub(crate) fn cleanup(&mut self, container: &JobContainerSpec) -> Result<()> {
        let _lifecycle = docker_lifecycle_guard()?;
        // Service containers hold endpoints on the job network. Remove them
        // BEFORE reclaiming job-owned resources: reclaim includes the network,
        // and `docker network rm` fails while an endpoint is still attached.
        // The old order (reclaim first) failed the network removal on every
        // service job and orphaned the network once the services went away —
        // the `velnor-net-*` address-pool exhaustion leak.
        let service_result = self.cleanup_services_unlocked(container);
        let container_result = self.run_docker_remove_container(&container.remove_container_args());
        // Job container is gone; abort in-flight Engine HTTP (BuildKit start)
        // before reclaim. `docker rm` of Created BuildKit waits forever on
        // that lock if the lease still holds `POST /containers/{id}/start`.
        self.abort_docker_lease();
        let owned_result = self.reclaim_job_owned_docker(&container.name);
        let buildkit_result = self.cleanup_job_buildkit_unlocked(container);

        let result = (|| {
            container_result?;
            owned_result?;
            buildkit_result?;
            service_result
        })();
        if result.is_ok() {
            self.defuse_job_network_guard();
        }
        result
    }

    /// Remove the job-owned containers, services, network, and lease without
    /// waiting for BuildKit's asynchronous container/volume deletion.
    ///
    /// BuildKit cleanup is intentionally separate: Docker can acknowledge a
    /// forced container removal while the container still holds its state
    /// volume. Keeping that delete on the slot's critical path turns the
    /// volume retry timeout into slot turnover latency.
    pub(crate) fn cleanup_without_buildkit(&mut self, container: &JobContainerSpec) -> Result<()> {
        let _lifecycle = docker_lifecycle_guard()?;
        self.cleanup_without_buildkit_unlocked(container)
    }

    fn cleanup_without_buildkit_unlocked(&mut self, container: &JobContainerSpec) -> Result<()> {
        // Services first: their endpoints block the network removal inside
        // reclaim (see `cleanup`).
        let service_result = self.cleanup_services_unlocked(container);
        let container_result = self.run_docker_remove_container(&container.remove_container_args());
        // Abort in-flight Engine HTTP before the deferred BuildKit worker
        // runs. Dropping the lease at the end used to leave ContainerStart
        // held, so the worker's `docker rm` of Created BuildKit hung.
        self.abort_docker_lease();
        let owned_result = self.reclaim_job_owned_docker(&container.name);

        let result = (|| {
            container_result?;
            owned_result?;
            service_result
        })();
        if result.is_ok() {
            self.defuse_job_network_guard();
        }
        result
    }

    pub(crate) fn cleanup_services(&mut self, container: &JobContainerSpec) -> Result<()> {
        let _lifecycle = docker_lifecycle_guard()?;
        self.cleanup_services_unlocked(container)
    }

    fn cleanup_services_unlocked(&mut self, container: &JobContainerSpec) -> Result<()> {
        let service_results = container
            .services
            .iter()
            .rev()
            .map(|service| self.run_docker_remove_container(&service.remove_args()))
            .collect::<Vec<_>>();
        for service_result in service_results {
            service_result?;
        }
        Ok(())
    }

    pub(crate) fn cleanup_job_and_network(&mut self, container: &JobContainerSpec) -> Result<()> {
        let container_result = self.run_docker_remove_container(&container.remove_container_args());
        self.abort_docker_lease();
        let owned_result = self.reclaim_job_owned_docker(&container.name);
        let buildkit_result = self.cleanup_job_buildkit(container);
        let result = (|| {
            container_result?;
            owned_result?;
            buildkit_result
        })();
        if result.is_ok() {
            self.defuse_job_network_guard();
        }
        result
    }

    pub(crate) fn cleanup_job_and_network_without_buildkit(
        &mut self,
        container: &JobContainerSpec,
    ) -> Result<()> {
        let _lifecycle = docker_lifecycle_guard()?;
        self.cleanup_job_and_network_without_buildkit_unlocked(container)
    }

    fn cleanup_job_and_network_without_buildkit_unlocked(
        &mut self,
        container: &JobContainerSpec,
    ) -> Result<()> {
        let container_result = self.run_docker_remove_container(&container.remove_container_args());
        self.abort_docker_lease();
        let owned_result = self.reclaim_job_owned_docker(&container.name);
        let result = (|| {
            container_result?;
            owned_result
        })();
        if result.is_ok() {
            self.defuse_job_network_guard();
        }
        result
    }

    fn reclaim_job_owned_docker(&mut self, job_id: &str) -> Result<()> {
        crate::docker_lease::reclaim_job_owned(job_id, |args| {
            self.run_docker_cleanup(args).map(|result| result.stdout)
        })
    }

    /// Remove every BuildKit daemon whose buildx builder belongs to this job.
    ///
    /// A cancelled job can skip setup-buildx's post action. The buildx client
    /// configuration lives inside the disposable job container, so host-side
    /// teardown cannot use `docker buildx rm`. Buildx names its daemon and
    /// state volume from the builder name; every native builder is suffixed
    /// with the job's unique scope. Match that exact suffix, then remove the
    /// daemon together with its anonymous/named state volume.
    pub(crate) fn cleanup_job_buildkit(&mut self, container: &JobContainerSpec) -> Result<()> {
        let _lifecycle = docker_lifecycle_guard()?;
        self.cleanup_job_buildkit_unlocked(container)
    }

    fn cleanup_job_buildkit_unlocked(&mut self, container: &JobContainerSpec) -> Result<()> {
        let scope = job_scope_from_temp(Some(&container.temp_host));
        let listed = self.run_docker(&crate::docker_lease::list_job_buildkit_format_args())?;
        let ids =
            crate::docker_lease::job_buildkit_ids_for_job(&listed.stdout, &container.name, &scope);
        if !ids.is_empty() {
            crate::docker_lease::force_remove_containers_serially(&ids, |args| {
                self.run_docker_remove_container(args).map(|_| ())
            })?;
        }

        // Buildx creates a named `<container>_state` volume. Docker's
        // `rm --volumes` deliberately removes only anonymous volumes, so the
        // state volume requires a separate prefix query and removal.
        let volume_filter = format!(
            "name={}{scope}",
            crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX
        );
        let listed_volumes = self.run_docker(&[
            "volume".into(),
            "ls".into(),
            "--quiet".into(),
            "--filter".into(),
            volume_filter,
        ])?;
        let volumes = listed_volumes
            .stdout
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if !volumes.is_empty() {
            let mut args = vec!["volume".into(), "rm".into(), "--force".into()];
            args.extend(volumes);
            self.run_docker(&args)?;
        }
        Ok(())
    }

    pub(crate) fn start_job_environment(&mut self, container: &JobContainerSpec) -> Result<()> {
        let _span = tracing::info_span!("job-container-boot").entered();
        let mut attempt = 1_u32;
        loop {
            let Err(error) = self.start_job_environment_once(container) else {
                return Ok(());
            };
            self.cleanup_stale(container);

            // A daemon/containerd restart briefly returns transport and shim
            // errors for every `docker run`. The historical single immediate
            // retry always landed inside the same restart window, turning a
            // healthy workflow into a permanent pre-execution rejection.
            // Retry only this closed transient class with bounded backoff.
            // Every other failure retains the one stale-resource retry.
            let transient = docker_start_error_is_transient(&error);
            let max_attempts = if transient { 5 } else { 2 };
            if attempt >= max_attempts {
                return Err(error);
            }
            let delay = if transient {
                docker_start_retry_delay(attempt)
            } else {
                Duration::ZERO
            };
            eprintln!(
                "Docker job environment start failed (attempt {attempt}/{max_attempts}); \
                 removed stale resources; retrying in {}ms: {error:#}",
                delay.as_millis()
            );
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            attempt += 1;
        }
    }

    fn start_job_environment_once(&mut self, container: &JobContainerSpec) -> Result<()> {
        fs::create_dir_all(container.temp_host.join("_github_workflow")).with_context(|| {
            format!(
                "create GitHub workflow directory under {}",
                container.temp_host.display()
            )
        })?;
        // Resolve the same trust-scoped runtime used for mounts and env so
        // Plan 066's shared cache root is initialized without crossing scopes.
        let cache_runtime = container.compiler_cache_runtime();
        if let Some(cache_host) = cache_runtime.host_path() {
            fs::create_dir_all(cache_host).with_context(|| {
                format!(
                    "create shared compiler-cache directory for {}",
                    container.temp_host.display()
                )
            })?;
        }
        // The temp dir is bind-mounted over the job container's /tmp. A plain
        // create_dir_all yields 0755 owned by the daemon user, which leaves the
        // container's /tmp non-sticky and unwritable by non-root sandbox users
        // (e.g. apt's `_apt`, uid 100). That makes `apt-get update` fail with
        // "Couldn't create temporary file /tmp/apt.conf.*". A real /tmp is
        // world-writable and sticky (1777); force that so the mount matches.
        fs::set_permissions(
            &container.temp_host,
            std::fs::Permissions::from_mode(0o1777),
        )
        .with_context(|| {
            format!(
                "set 1777 (sticky, world-writable) on job temp dir {}",
                container.temp_host.display()
            )
        })?;
        self.seed_mise_store(container)?;
        if container.mount_docker_socket {
            self.docker_lease = Some(crate::docker_lease::DockerLeaseGuard::bind(
                container.guest_docker_socket_host(),
                container.name.clone(),
                container.daemon_id.clone(),
            )?);
        }
        // Hold the host-wide Docker permit only for state-changing Engine
        // calls. Readiness polling below can take up to 30s and does not
        // mutate Docker state; keeping the permit across it serializes other
        // slots behind a non-mutating wait.
        // Retrying this function reuses the same network name: retire any
        // guard from a previous attempt BEFORE creating, so its drop can never
        // remove the freshly created network.
        if let Some(previous) = self.job_network_guard.take() {
            drop(previous);
        }
        self.with_docker_lifecycle(|executor| {
            executor.run_docker(&container.create_network_args())
        })?;
        // Own the network from creation to terminal cleanup. Dropping the
        // executor on any error path now removes it instead of leaking it.
        // Defuse only after cleanup reclaimed it; Docker refuses to remove a
        // network with active endpoints, so a late guard fire cannot break a
        // live job.
        self.job_network_guard = Some(crate::docker_lease::JobNetworkGuard::arm(
            container.network.clone(),
        ));
        for service in &container.services {
            self.with_docker_lifecycle(|executor| executor.run_docker(&service.start_args()))?;
            self.wait_for_service(service)?;
        }
        self.with_docker_lifecycle(|executor| executor.run_docker(&container.start_args()))?;
        // Docker accepts repeated network-shaped create options with behavior
        // that depends on option placement. Reconcile the runner-owned
        // topology explicitly after every container exists, before any step
        // can observe it. This also makes each workflow service key the exact
        // embedded-DNS alias on the per-job network.
        if !container.services.is_empty() {
            self.with_docker_lifecycle(|executor| {
                executor.run_docker(&container.disconnect_network_args())?;
                executor.run_docker(&container.connect_network_args())?;
                for service in &container.services {
                    executor.run_docker(&service.disconnect_network_args())?;
                    executor.run_docker(&service.connect_network_args())?;
                }
                Ok(())
            })?;
            self.verify_service_dns(container)?;
        }
        if container.verify_bind_mounts {
            self.verify_bind_mounts(container)?;
        }
        Ok(())
    }

    fn verify_service_dns(&mut self, container: &JobContainerSpec) -> Result<()> {
        for service in &container.services {
            let lookup = self.runner.run(
                "docker",
                &container.service_dns_args(&service.network_alias),
            )?;
            if lookup.code == 0 {
                continue;
            }
            let network = self
                .runner
                .run("docker", &container.inspect_network_args())?;
            let resolver = self
                .runner
                .run("docker", &container.resolver_state_args())?;
            bail!(
                "service DNS preflight failed for alias '{}' in job '{}': getent code={}, stderr={}; network={}; resolv.conf={}",
                service.network_alias,
                container.name,
                lookup.code,
                lookup.stderr.trim(),
                network.stdout.trim(),
                resolver.stdout.trim()
            );
        }
        Ok(())
    }

    /// Seed the shared mise store from the job image's baked installs, once
    /// per image id (marker file in the store). An empty shared store mounted
    /// over /opt/mise/installs dangles the image-baked shims (the `gh is not
    /// a valid shim` class); seeding makes the store a superset of the image.
    fn seed_mise_store(&mut self, container: &JobContainerSpec) -> Result<()> {
        let store = crate::container::mise_store_host(&container.temp_host);
        let inspect = self.runner.run(
            "docker",
            &[
                "image".to_string(),
                "inspect".to_string(),
                "-f".to_string(),
                "{{.Id}}".to_string(),
                container.image.clone(),
            ],
        )?;
        if inspect.code != 0 {
            // Image not present yet — container start will surface the real
            // error; a missing seed must not mask it.
            return Ok(());
        }
        let image_id = inspect.stdout.trim().to_string();
        if image_id.is_empty() {
            return Ok(());
        }
        // Executable installs are repository-scoped. A marker in the shared
        // download-cache root lets the first repository suppress seeding for
        // every later repository, leaving baked shims (notably `gh`) dangling.
        // Keep the image marker beside the exact executable store it governs.
        let marker = container
            .mise_executable_store_host()
            .join(".velnor-seeded-image");
        if fs::read_to_string(&marker)
            .map(|seeded| seeded.trim() == image_id)
            .unwrap_or(false)
        {
            return Ok(());
        }
        self.run_docker(&container.seed_mise_store_args())?;
        fs::create_dir_all(&store).ok();
        fs::write(&marker, &image_id)
            .with_context(|| format!("write mise store seed marker {}", marker.display()))?;
        Ok(())
    }

    fn verify_bind_mounts(&mut self, container: &JobContainerSpec) -> Result<()> {
        let marker = container.temp_host.join(DOCKER_MOUNT_CHECK_FILE);
        fs::write(&marker, "velnor\n")
            .with_context(|| format!("write Docker bind-mount marker {}", marker.display()))?;

        let args = container.prepare_exec_process_args(
            "/",
            &[],
            &[],
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("test -f /__t/{DOCKER_MOUNT_CHECK_FILE}"),
            ],
        )?;
        let result = self.runner.run("docker", args.args())?;
        fs::remove_file(&marker).ok();
        if result.code != 0 {
            bail!(
                "Docker daemon cannot see Velnor bind-mounted work directories. \
                 Expected host temp '{}' to appear at '/__t' in container '{}'. \
                 Use a local Docker daemon or set --work-dir/--config-dir to a path visible to the daemon. stderr: {}",
                container.temp_host.display(),
                container.name,
                result.stderr
            );
        }
        Ok(())
    }

    fn cleanup_stale(&mut self, container: &JobContainerSpec) {
        let Ok(_lifecycle) = docker_lifecycle_guard() else {
            return;
        };
        // Drop the lease before stale cleanup so an in-flight Engine request
        // cannot keep dockerd's container lock held while retry cleanup runs.
        // Unlike terminal cleanup, startup retry must not broad-reclaim a
        // container that is still live or whose state is unknown.
        self.abort_docker_lease();
        for service in container.services.iter().rev() {
            self.run_docker_cleanup(&crate::docker_lease::remove_one_container_args(
                &service.name,
            ))
            .ok();
        }
        if self.reclaim_stale_job_owned_docker(&container.name).is_ok() {
            // The stale reclaim can legitimately skip the network (its
            // liveness gate only guards containers). Remove it explicitly:
            // the job container is absent or stopped here, so no endpoint can
            // block the removal. A "not found" is success — already gone.
            let removed =
                match self.run_docker_cleanup(&crate::docker_lease::force_remove_network_args(
                    std::slice::from_ref(&container.network),
                )) {
                    Ok(_) => true,
                    Err(error) => error.to_string().contains("not found"),
                };
            if removed {
                self.defuse_job_network_guard();
            }
        }
    }

    fn reclaim_stale_job_owned_docker(&mut self, job_id: &str) -> Result<()> {
        crate::docker_lease::reclaim_stale_job_owned(job_id, |args| {
            self.run_docker_cleanup(args).map(|result| result.stdout)
        })
    }

    fn abort_docker_lease(&mut self) {
        self.docker_lease.take();
    }

    fn run_docker(&mut self, args: &[String]) -> Result<CommandResult> {
        let result = self.runner.run("docker", args)?;
        if result.code != 0 {
            bail!(
                "docker {} failed with code {}: {}",
                args.join(" "),
                result.code,
                result.stderr
            );
        }
        Ok(result)
    }

    fn with_docker_lifecycle<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        let _lifecycle = docker_lifecycle_guard()?;
        operation(self)
    }

    fn run_docker_cleanup(&mut self, args: &[String]) -> Result<CommandResult> {
        if args.first().is_some_and(|arg| arg == "rm") {
            self.run_docker_remove_container(args)
        } else {
            self.run_docker(args)
        }
    }

    fn run_docker_with_env(
        &mut self,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<CommandResult> {
        let result = self.runner.run_with_env("docker", args, env)?;
        if result.code != 0 {
            bail!(
                "docker {} failed with code {}: {}",
                args.join(" "),
                result.code,
                result.stderr
            );
        }
        Ok(result)
    }

    fn run_docker_remove_container(&mut self, args: &[String]) -> Result<CommandResult> {
        let result = self
            .runner
            .run_timeout("docker", args, TEARDOWN_RM_TIMEOUT)?;
        if result.code != 0 {
            // Docker reports this when another rm request is already in flight for
            // the same container (e.g. the container exited on its own and Docker
            // started its own GC pass concurrently with our `docker rm --force`).
            // The container IS being removed — treat it as success so a clean
            // container removal does not cause the slot to cycle unnecessarily.
            // A bounded timeout (Created/removing BuildKit) is the same class:
            // job teardown proceeds; doctor/boot retry until the object is gone.
            if result.code == 124
                || (result.stderr.contains("removal of container")
                    && result.stderr.contains("is already in progress"))
            {
                return Ok(result);
            }
            bail!(
                "docker {} failed with code {}: {}",
                args.join(" "),
                result.code,
                result.stderr
            );
        }
        Ok(result)
    }

    fn wait_for_service(&mut self, service: &crate::container::ServiceContainerSpec) -> Result<()> {
        // Exponential backoff (100ms → 1.6s cap, ~30s total budget): most
        // services report running/healthy within the first second, so a fixed
        // 1s poll added up to ~1s of dead time per service on the hot path.
        let mut delay = Duration::from_millis(100);
        let mut waited = Duration::ZERO;
        let budget = Duration::from_secs(30);
        loop {
            let result = self.run_docker(&service.health_status_args())?;
            match result.stdout.trim() {
                "healthy" | "running" | "" => return Ok(()),
                "exited" | "dead" => {
                    bail!(
                        "service container '{}' stopped before becoming ready",
                        service.name
                    )
                }
                _ => {
                    if waited >= budget {
                        break;
                    }
                    thread::sleep(delay);
                    waited += delay;
                    delay = (delay * 2).min(Duration::from_millis(1600));
                }
            }
        }
        bail!("service container '{}' did not become ready", service.name)
    }
}

fn resolve_checkout_plan_expressions(
    plan: &CheckoutPlan,
    state: &JobExecutionState,
) -> Result<CheckoutPlan> {
    let mut plan = plan.clone();
    if let Some(version) = plan.version.as_mut() {
        *version = state.resolve_expressions(version);
    }
    if let Some(token) = plan.token.as_mut() {
        *token = state.resolve_expressions(token);
        if token.is_empty() || token.contains("${{") {
            anyhow::bail!("explicit checkout token expression did not resolve");
        }
    }
    Ok(plan)
}

fn docker_start_error_is_transient(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    [
        "failed to create ttrpc connection",
        "error reading from server: eof",
        "unexpected eof",
        "connection reset by peer",
        "cannot connect to the docker daemon",
        "is the docker daemon running",
        "transport is closing",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn docker_start_retry_delay(failed_attempt: u32) -> Duration {
    Duration::from_secs(1_u64 << failed_attempt.saturating_sub(1).min(3))
}

fn emit_live_step_log(
    sender: &Option<UnboundedSender<StepLog>>,
    step_id: &str,
    display_name: &str,
    order: i32,
    started_at: &str,
    line: &str,
    masks: &[String],
) {
    let Some(sender) = sender else {
        return;
    };
    let _ = sender.send(StepLog {
        step_id: step_id.to_string(),
        display_name: display_name.to_string(),
        order,
        started_at: started_at.to_string(),
        completed_at: String::new(),
        lines: vec![line.to_string()],
        masks: masks.to_vec(),
        annotations: Vec::new(),
        telemetry: Vec::new(),
        exit_code: 0,
        skipped: false,
        failure_ignored: false,
        error_count: 0,
        warning_count: 0,
        notice_count: 0,
        summary: String::new(),
    });
}

fn post_step_display_name(display_name: &str) -> String {
    format!("Post {display_name}")
}

pub(crate) fn github_backend_step_id(context_step_id: &str) -> String {
    if uuid::Uuid::parse_str(context_step_id).is_ok() {
        context_step_id.to_string()
    } else {
        uuid::Uuid::new_v4().to_string()
    }
}

fn action_context_env(env: &[(String, String)]) -> Vec<(String, String)> {
    env.iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "GITHUB_ACTION"
                    | "GITHUB_ACTION_PATH"
                    | "GITHUB_ACTION_REPOSITORY"
                    | "GITHUB_ACTION_REF"
            )
        })
        .cloned()
        .collect()
}

fn set_env_value(env: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some((_, existing)) = env.iter_mut().find(|(key, _)| key == name) {
        *existing = value.to_string();
    } else {
        env.push((name.to_string(), value.to_string()));
    }
}

fn rewrite_command_file_env_for_action_container(env: &mut [(String, String)]) {
    for (name, value) in env {
        if matches!(
            name.as_str(),
            "GITHUB_OUTPUT" | "GITHUB_ENV" | "GITHUB_PATH" | "GITHUB_STATE" | "GITHUB_STEP_SUMMARY"
        ) && let Some(file_name) = value.strip_prefix("/__t/")
        {
            *value = format!("/github/file_commands/{file_name}");
        }
    }
}

/// Setup script for the native mise adapter (Plan 008).
///
/// `exact_version` is already resolved by the caller (an explicit exact version,
/// or the fleet pin for an omitted one — 008-R5). The script never bootstraps
/// mise over the network: it publishes a verified per-version binary into the
/// repository-scoped persistent store (`/opt/velnor/mise-binaries`) by copying
/// the read-only baked `/opt/mise/bin` bootstrap to same-filesystem staging,
/// self-updating it to the exact version with project config disabled, verifying
/// the reported version + SHA-256, then atomically publishing it. A fresh job
/// reuses that binary when version/hash match; corruption fails closed. Tool
/// installs are strictly `mise install --locked` under the fail-closed
/// `MISE_LOCKED`/`MISE_LOCKED_VERIFY_PROVENANCE` contract.
///
/// The bootstrap and store paths are env-overridable (`VELNOR_MISE_BOOTSTRAP`,
/// `VELNOR_MISE_BINARY_STORE`) so the reuse/integrity behavior is exercised by
/// tests with isolated stores and a controlled fake updater.
fn setup_mise_script(
    install: bool,
    exact_version: &str,
    install_args: &str,
    working_directory: &str,
    cache_key_prefix: &str,
    cache_save: bool,
) -> String {
    let install_flag = if install { "1" } else { "" };
    let cache_save_flag = if cache_save { "1" } else { "" };
    MISE_SETUP_TEMPLATE
        .replace("@@EXACT_VERSION@@", &shell_single_quote(exact_version))
        .replace("@@INSTALL_FLAG@@", install_flag)
        .replace("@@INSTALL_ARGS@@", &shell_single_quote(install_args))
        .replace(
            "@@WORKING_DIRECTORY@@",
            &shell_single_quote(working_directory),
        )
        .replace(
            "@@CACHE_KEY_PREFIX@@",
            &shell_single_quote(cache_key_prefix),
        )
        .replace("@@CACHE_SAVE_FLAG@@", cache_save_flag)
}

const MISE_SETUP_TEMPLATE: &str = r#"set -e
# Use the euid's home (/root) for the image-baked mise tool store.
export HOME=/root
# mise_home and the env-export dir default to the production mounts; tests point
# them at isolated dirs (no /opt or /__t write on the test host).
mise_home="${VELNOR_MISE_HOME:-/opt/mise}"
: "${VELNOR_MISE_ENV_DIR:=/__t/_velnor}"
mkdir -p "$mise_home/shims" "$mise_home/cache" "$mise_home/config"
export MISE_DATA_DIR="$mise_home"
export MISE_CACHE_DIR="$mise_home/cache"
export MISE_CONFIG_DIR="$mise_home/config"
export MISE_TRUSTED_CONFIG_PATHS="/__w"
# The read-only baked bootstrap and the repository-scoped persistent binary
# store. Overridable so tests can point them at isolated directories.
: "${VELNOR_MISE_BOOTSTRAP:=/opt/mise/bin/mise}"
: "${VELNOR_MISE_BINARY_STORE:=/opt/velnor/mise-binaries}"
_velnor_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}
exact_version=@@EXACT_VERSION@@
install_requested="@@INSTALL_FLAG@@"
install_args=@@INSTALL_ARGS@@
working_directory=@@WORKING_DIRECTORY@@
cache_key_prefix=@@CACHE_KEY_PREFIX@@
cache_save_requested="@@CACHE_SAVE_FLAG@@"
# Upstream cache_save controls archive publication after installation, not
# mutation of the action's local mise directory. Velnor replaces that remote
# archive transport with its repository-scoped persistent mount.
echo "Velnor local mise cache: generation=$cache_key_prefix cache_save=$cache_save_requested"
if [ -z "$exact_version" ]; then
  echo "mise: no exact version resolved; refusing a network 'latest' lookup under MISE_LOCKED" >&2
  exit 1
fi
case "$(uname -m)" in
  x86_64|amd64) os_arch="linux-x64" ;;
  aarch64|arm64) os_arch="linux-arm64" ;;
  *) os_arch="$(uname -s)-$(uname -m)" ;;
esac
store_dir="$VELNOR_MISE_BINARY_STORE/$os_arch/$exact_version"
mise_bin="$store_dir/mise"
meta="$store_dir/metadata.json"
mkdir -p "$VELNOR_MISE_BINARY_STORE"
# All slots share the repository-scoped store. One exclusive lock covers the
# binary publish, tool install, and environment export so a concurrent job
# never observes a mixed generation. flock is always present in the Linux job
# image (util-linux); the guard only matters on non-Linux test hosts.
exec 9>"$VELNOR_MISE_BINARY_STORE/.velnor-mise.lock"
if command -v flock >/dev/null 2>&1; then
  flock -x 9
fi
publish_needed=1
if [ -x "$mise_bin" ] && [ -f "$meta" ]; then
  recorded_hash=$(sed -n 's/.*"sha256":[ ]*"\([0-9a-f]*\)".*/\1/p' "$meta")
  recorded_version=$(sed -n 's/.*"version":[ ]*"\([^"]*\)".*/\1/p' "$meta")
  actual_hash=$(_velnor_sha256 "$mise_bin")
  if [ -n "$recorded_hash" ] && [ "$recorded_hash" = "$actual_hash" ] && [ "$recorded_version" = "$exact_version" ]; then
    publish_needed=0
    echo "mise: reusing persisted $exact_version ($os_arch) sha256=$actual_hash"
  else
    echo "mise: persisted binary failed integrity check (version/hash mismatch); failing closed" >&2
    exit 1
  fi
fi
if [ "$publish_needed" = "1" ]; then
  if [ ! -x "$VELNOR_MISE_BOOTSTRAP" ]; then
    echo "mise: baked bootstrap $VELNOR_MISE_BOOTSTRAP is missing; cannot publish persistent binary" >&2
    exit 1
  fi
  mkdir -p "$store_dir"
  # mise dispatches by argv[0]: an executable named `.mise.staging.*` is
  # interpreted as a shim instead of the mise CLI. Keep the executable's
  # basename exactly `mise` inside a private staging directory, and clean the
  # directory on every exit so interrupted publication cannot poison PATH.
  staging_dir="$store_dir/.staging.$$"
  staging="$staging_dir/mise"
  rm -rf "$staging_dir"
  mkdir -p "$staging_dir"
  trap 'rm -rf "$staging_dir"' EXIT HUP INT TERM
  # Copy the read-only baked bootstrap onto the same filesystem as the final
  # path so publication is an atomic rename. /opt/mise/bin is never written.
  cp "$VELNOR_MISE_BOOTSTRAP" "$staging"
  chmod 0755 "$staging"
  # Self-update to the exact version with project config disabled so no
  # workspace mise.toml can influence the binary update.
  ( cd / && MISE_CONFIG_FILE="" MISE_TRUSTED_CONFIG_PATHS="" "$staging" self-update "$exact_version" -y )
  reported=$("$staging" --version 2>/dev/null | awk '{print $1}' | sed 's/^v//')
  if [ "$reported" != "$exact_version" ]; then
    echo "mise: self-update produced '$reported', expected '$exact_version'; failing closed" >&2
    rm -f "$staging"
    exit 1
  fi
  staged_hash=$(_velnor_sha256 "$staging")
  sync 2>/dev/null || true
  chmod 0555 "$staging"
  mv -f "$staging" "$mise_bin"
  rmdir "$staging_dir"
  trap - EXIT HUP INT TERM
  printf '{"version":"%s","sha256":"%s","os_arch":"%s"}\n' "$exact_version" "$staged_hash" "$os_arch" > "$meta"
  sync 2>/dev/null || true
  echo "mise: published persistent $exact_version ($os_arch) sha256=$staged_hash"
fi
mise_bin_dir=$(dirname "$mise_bin")
# native_mise reads this marker and prepends the persisted binary dir to the
# PATH of subsequent steps (in place of the baked /opt/mise/bin).
echo "__VELNOR_MISE_SELF__$mise_bin_dir"
export PATH="$mise_bin_dir:/root/.cargo/bin:$mise_home/shims:$PATH"
if [ -n "$working_directory" ]; then
  cd "$working_directory"
fi
if [ -n "$install_requested" ]; then
  # Trust the workspace config so mise reads mise.toml from the checkout.
  for f in "/__w/mise.toml" "/__w/.mise.toml" "/__w/.mise/config.toml"; do
    [ -f "$f" ] && "$mise_bin" trust "$f" 2>/dev/null || true
  done
  "$mise_bin" trust --all 2>/dev/null || true
  # Drop poisoned version dirs a previous interrupted install may have left.
  # An interrupted install leaves a non-empty version dir without its bin/
  # output; mise treats any existing version dir as installed, so the partial
  # entry would persist ("all tools are installed" while the binary is
  # missing) and fail every later job on this store. The sweep covers the
  # backends whose install layout guarantees a bin/ directory (cargo-* source
  # compiles, pipx-* venv entry points); other backends legitimately place the
  # binary directly in the version dir (for example aqua cosign), so a missing
  # bin/ is only a poison signal for these. Treat a covered version dir as
  # valid only when bin/ exists and is non-empty; dropping the dir makes the
  # locked install below perform a real reinstall.
  find "$mise_home/installs" -mindepth 2 -maxdepth 2 -type d -empty -exec rm -rf {} + 2>/dev/null || true
  for tool_dir in "$mise_home/installs"/cargo-* "$mise_home/installs"/pipx-*; do
    [ -d "$tool_dir" ] || continue
    for version_dir in "$tool_dir"/*; do
      [ -d "$version_dir" ] || continue
      if [ ! -d "$version_dir/bin" ] || [ -z "$(ls -A "$version_dir/bin" 2>/dev/null)" ]; then
        echo "mise: dropping incomplete install $version_dir (missing bin/); will reinstall"
        rm -rf "$version_dir"
      fi
    done
  done
  echo "::group::mise install (locked)"
  # Fail closed: enforce the committed lockfile, refuse unlocked artifacts, and
  # treat a provenance-API failure as fatal.
  export MISE_LOCKFILE=1 MISE_LOCKED=1 MISE_LOCKED_VERIFY_PROVENANCE=1
  if [ -n "$install_args" ]; then
    "$mise_bin" install --locked --yes $install_args
  else
    "$mise_bin" install --locked --yes
  fi
  # Validate the repository-selected Rust environment. If a shared image store
  # is incomplete, reinstall the exact lockfile/toolchain declaration through
  # mise; never repair it with a direct rustup mutation.
  if "$mise_bin" current rust >/dev/null 2>&1; then
    if ! "$mise_bin" exec -- cargo --version >/dev/null 2>&1 \
      || ! "$mise_bin" exec -- cargo clippy --version >/dev/null 2>&1 \
      || ! "$mise_bin" exec -- cargo fmt --version >/dev/null 2>&1; then
      echo "mise: Rust component probes failed; reinstalling the locked toolchain" >&2
      "$mise_bin" install --locked --yes --force rust
    fi
    "$mise_bin" exec -- cargo --version
    "$mise_bin" exec -- cargo clippy --version
    "$mise_bin" exec -- cargo fmt --version
  fi
  echo "::endgroup::"
else
  "$mise_bin" --version >/dev/null 2>&1
fi
# Emit active mise tool bin dirs as markers. native_mise parses these and adds
# them to PATH. `mise bin-paths` is authoritative; nested `bin` dirs cover
# executables installed into managed runtimes (pipx version roots also contain a
# same-named virtualenv dir that would shadow the real executable).
{
  "$mise_bin" bin-paths 2>/dev/null || true
  find "$mise_home/installs" -mindepth 3 -maxdepth 3 -type d -name bin 2>/dev/null || true
} | awk 'NF && !seen[$0]++ { print "__VELNOR_MISE_BIN__" $0 }'
# Match mise-action's environment export through the job temp mount so values
# never enter the streamed action log; native_mise reads/deletes these files.
umask 077
mkdir -p "$VELNOR_MISE_ENV_DIR"
"$mise_bin" env --redacted --json > "$VELNOR_MISE_ENV_DIR/mise-env-redacted.json"
"$mise_bin" env --json > "$VELNOR_MISE_ENV_DIR/mise-env.json"
echo "mise install completed, cargo: $(command -v cargo 2>/dev/null || echo 'not found')"
"$mise_bin" --version
"#;

/// Map the mise action `working_directory` input (a container path under `/__w`,
/// or a workspace-relative path) to its host path inside the checkout mount so
/// the fail-closed lock policy can inspect the real files.
fn mise_working_directory_host(checkout_root: &Path, working_directory: &str) -> PathBuf {
    let working_directory = working_directory.trim();
    if working_directory.is_empty() || working_directory == "/__w" {
        return checkout_root.to_path_buf();
    }
    if let Some(rel) = working_directory.strip_prefix("/__w/") {
        return checkout_root.join(rel);
    }
    let path = Path::new(working_directory);
    if path.is_absolute() {
        // Passed through so the policy's inside-checkout check rejects it.
        path.to_path_buf()
    } else {
        checkout_root.join(path)
    }
}

fn mise_step_path(output: &str) -> Vec<String> {
    let mut path = Vec::new();
    // Prefer the persisted per-version mise binary dir the setup step published;
    // fall back to the baked bootstrap when no marker is present.
    for line in output.lines() {
        if let Some(dir) = line.strip_prefix("__VELNOR_MISE_SELF__") {
            let dir = dir.trim();
            if !dir.is_empty() && !path.iter().any(|entry| entry == dir) {
                path.push(dir.to_string());
            }
        }
    }
    if path.is_empty() {
        path.push("/opt/mise/bin".to_string());
    }
    // Add the active mise tool install bin dirs before shims. A pipx install
    // has both `<version>/reuse/` (the virtualenv directory) and
    // `<version>/bin/reuse` (the executable); a stale/shared shim can resolve
    // the former and fail with EACCES. `mise bin-paths` is authoritative for
    // the repository-selected versions.
    for line in output.lines() {
        if let Some(dir) = line.strip_prefix("__VELNOR_MISE_BIN__") {
            let dir = dir.trim();
            if !dir.is_empty() && !path.iter().any(|path| path == dir) {
                path.push(dir.to_string());
            }
        }
    }
    // Rust installed by mise may use root-owned backend proxies. Keep that
    // backend bin directory ahead of generic shims; direct pinned tool bins
    // above still take precedence.
    path.push("/root/.cargo/bin".to_string());
    path.push("/opt/mise/shims".to_string());
    path
}

fn parse_mise_environment(
    exported_json: &str,
    redacted_json: &str,
) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let exported: BTreeMap<String, Value> =
        serde_json::from_str(exported_json).context("parse mise environment JSON")?;
    let redacted: BTreeMap<String, Value> =
        serde_json::from_str(redacted_json).context("parse mise redacted environment JSON")?;
    let env = exported
        .into_iter()
        .filter_map(|(key, value)| {
            (key.to_uppercase() != "PATH")
                .then(|| value.as_str().map(|value| (key, value.to_string())))
                .flatten()
        })
        .collect();
    let masks = redacted
        .into_values()
        .filter_map(|value| value.as_str().map(str::to_string))
        .filter(|value| !value.is_empty())
        .collect();
    Ok((env, masks))
}

fn sccache_setup_script() -> String {
    // Mirror mozilla-actions/sccache-action: ensure the sccache binary is present
    // (download the release if the job image doesn't ship it), then start the
    // server. The workflow sets RUSTC_WRAPPER=sccache, so the binary MUST exist on
    // PATH or every rustc/clippy invocation fails with
    // "could not execute process `sccache ...`: No such file or directory".
    // SCCACHE_GHA_ENABLED + the ACTIONS_RESULTS_URL/ACTIONS_RUNTIME_TOKEN env that
    // Velnor injects let sccache use the GitHub Actions cache backend.
    r#"set -e
command -v sccache >/dev/null 2>&1 || { echo 'sccache v0.16.0 must be preinstalled in the job image' >&2; exit 1; }
sccache --version | grep -F 'sccache 0.16.0'
# Velnor provides a fast, host-shared sccache cache bind-mounted at
# /var/cache/sccache. Use that local backend instead of the GitHub Actions cache
# service: this is not a GitHub-hosted cache environment, so SCCACHE_GHA_ENABLED
# would make the server fail ("cache url for ghac not found") and every
# RUSTC_WRAPPER=sccache compile would error. Export the override via GITHUB_ENV so
# subsequent compile steps (clippy/test) pick it up, and disable the GHA backend.
SCCACHE_LOCAL_DIR=/var/cache/sccache
mkdir -p "$SCCACHE_LOCAL_DIR" 2>/dev/null || true
if [ -n "${GITHUB_ENV:-}" ]; then
  echo "SCCACHE_DIR=$SCCACHE_LOCAL_DIR" >> "$GITHUB_ENV"
  echo "SCCACHE_GHA_ENABLED=false" >> "$GITHUB_ENV"
fi
export SCCACHE_DIR="$SCCACHE_LOCAL_DIR"
export SCCACHE_GHA_ENABLED=false
export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-20G}"
if [ -n "${GITHUB_ENV:-}" ]; then
  echo "SCCACHE_CACHE_SIZE=$SCCACHE_CACHE_SIZE" >> "$GITHUB_ENV"
fi
# Best-effort: cargo will auto-start the server on first use anyway.
sccache --start-server 2>/dev/null || true
"#
    .to_string()
}

fn kache_setup_script() -> String {
    r#"set -e
command -v kache >/dev/null 2>&1 || { echo 'kache v0.14.2 must be preinstalled in the job image' >&2; exit 1; }
kache --version | grep -F 'kache 0.14.2'
mkdir -p /var/cache/kache
export KACHE_CACHE_DIR=/var/cache/kache
export KACHE_MAX_SIZE=20GiB
export RUSTC_WRAPPER=kache
if [ -n "${GITHUB_ENV:-}" ]; then
  echo 'KACHE_CACHE_DIR=/var/cache/kache' >> "$GITHUB_ENV"
  echo 'KACHE_MAX_SIZE=20GiB' >> "$GITHUB_ENV"
  echo 'RUSTC_WRAPPER=kache' >> "$GITHUB_ENV"
fi
kache stats >/dev/null
"#
    .to_string()
}

/// Inputs of `hadolint/hadolint-action`, defaults matching its action.yml.
struct HadolintInputs {
    dockerfile: String,
    config: String,
    recursive: String,
    output_file: String,
    no_color: String,
    no_fail: String,
    verbose: String,
    format: String,
    failure_threshold: String,
    override_error: String,
    override_warning: String,
    override_info: String,
    override_style: String,
    ignore: String,
    trusted_registries: String,
}

/// Shell script mirroring hadolint-action's wrapper: hadolint reads the
/// HADOLINT_* env vars natively; `-c` is the only flag the wrapper passes.
/// Like the wrapper, findings are captured and then printed (stdout stays
/// pure findings — the version banner and the output-file note go to
/// stderr). POSIX-sh compatible (the job container runs scripts via sh).
fn hadolint_script(inputs: &HadolintInputs) -> String {
    let mut script = String::from("set -u\n");
    for (name, value) in [
        ("NO_COLOR", &inputs.no_color),
        ("HADOLINT_NOFAIL", &inputs.no_fail),
        ("HADOLINT_VERBOSE", &inputs.verbose),
        ("HADOLINT_FORMAT", &inputs.format),
        ("HADOLINT_FAILURE_THRESHOLD", &inputs.failure_threshold),
        ("HADOLINT_OVERRIDE_ERROR", &inputs.override_error),
        ("HADOLINT_OVERRIDE_WARNING", &inputs.override_warning),
        ("HADOLINT_OVERRIDE_INFO", &inputs.override_info),
        ("HADOLINT_OVERRIDE_STYLE", &inputs.override_style),
        ("HADOLINT_IGNORE", &inputs.ignore),
    ] {
        script.push_str(&format!("export {name}={}\n", shell_single_quote(value)));
    }
    // The wrapper unsets an empty trusted-registries list (hadolint rejects
    // an empty value).
    if !inputs.trusted_registries.trim().is_empty() {
        script.push_str(&format!(
            "export HADOLINT_TRUSTED_REGISTRIES={}\n",
            shell_single_quote(&inputs.trusted_registries)
        ));
    }
    script.push_str("hadolint --version >&2\n");
    let config_flags = if inputs.config.trim().is_empty() {
        String::new()
    } else {
        format!("-c {} ", shell_single_quote(&inputs.config))
    };
    let dockerfile = shell_single_quote(&inputs.dockerfile);
    if inputs.recursive.trim() == "true" {
        // `find -exec ... {} +` propagates a non-zero hadolint exit.
        script.push_str(&format!(
            "out=$(find . -type f -name {dockerfile} -not -path '*/.git/*' -exec hadolint {config_flags}-- {{}} +)\n"
        ));
    } else {
        script.push_str(&format!("out=$(hadolint {config_flags}-- {dockerfile})\n"));
    }
    script.push_str("code=$?\nprintf '%s\\n' \"$out\"\n");
    if inputs.output_file != "/dev/stdout" && !inputs.output_file.trim().is_empty() {
        let output = shell_single_quote(&inputs.output_file);
        script.push_str(&format!(
            "printf '%s\\n' \"$out\" > {output}\necho \"Hadolint output saved to: \"{output} >&2\n"
        ));
    }
    script.push_str("exit $code\n");
    script
}

// Exact versions of the admitted locked mise keys backing the mold/just/cosign
// adapters. These MUST match docker/job-mise.lock (Plan 008 Step 3); the job
// image bakes these versions, so a job-time `mise install --locked` is an
// integrity re-check, never a download over an unlocked path.
const MOLD_LOCKED_VERSION: &str = "2.41.0";
const JUST_LOCKED_VERSION: &str = "1.58.0";
const COSIGN_LOCKED_VERSION: &str = "3.1.3";

/// Shared prologue for the project-tool adapters (Plan 008 N8): point mise at
/// the baked global locked config and install one admitted tool key in
/// fail-closed mode. There is deliberately no apt/Cargo/curl fallback.
fn locked_mise_install(key: &str) -> String {
    format!(
        "set -e\n\
         export MISE_DATA_DIR=/opt/mise MISE_CACHE_DIR=/opt/mise/cache MISE_CONFIG_DIR=/opt/mise/config\n\
         export MISE_LOCKFILE=1 MISE_LOCKED=1 MISE_LOCKED_VERIFY_PROVENANCE=1\n\
         export PATH=\"/opt/mise/bin:/opt/mise/shims:$PATH\"\n\
         mise install --locked --yes {}\n",
        shell_single_quote(key)
    )
}

/// Native `sigstore/cosign-installer`: install the admitted locked `cosign`
/// mise key, verify the version, and expose install-dir on PATH (matching the
/// action). A requested release that differs from the committed lock fails
/// closed — Velnor never downloads cosign outside mise.
fn cosign_installer_script(release: &str, install_dir: &str) -> String {
    let want = shell_single_quote(release.trim().trim_start_matches('v'));
    // install_dir may contain $HOME by contract (the action default) — expand
    // in-shell, so single-quoting must not apply to the whole value.
    let dir = install_dir.trim().replace('\'', "'\"'\"'");
    let mut script = locked_mise_install("cosign");
    script.push_str(&format!(
        r#"WANT={want}
DIR="{dir}"
LOCKED='{ver}'
if [ -n "$WANT" ] && [ "$WANT" != "$LOCKED" ]; then
  echo "cosign release '$WANT' requested but the committed locked version is '$LOCKED'; refusing a non-mise download" >&2
  exit 1
fi
mkdir -p "$DIR"
resolved="$(command -v cosign)"
ln -sf "$resolved" "$DIR/cosign"
cosign version 2>&1 | grep -F "$LOCKED"
echo "cosign $LOCKED (locked mise) linked into $DIR"
echo "__VELNOR_COSIGN_DIR__$DIR"
"#,
        want = want,
        dir = dir,
        ver = COSIGN_LOCKED_VERSION,
    ));
    script
}

/// Native `rui314/setup-mold`: install the admitted locked `mold` mise key,
/// verify the version, then wire it as the Cargo linker (mirrors upstream).
fn setup_mold_script() -> String {
    let mut script = locked_mise_install("mold");
    script.push_str(&format!(
        "mold --version | grep -F '{MOLD_LOCKED_VERSION}'\n"
    ));
    script.push_str(MOLD_LINKER_SNIPPET);
    script
}

// Cargo linker wiring for mold. No interpolation, so it stays a plain literal.
const MOLD_LINKER_SNIPPET: &str = r#"# Wire mold as the Cargo linker (mirrors rui314/setup-mold upstream behavior).
# Use cc (gcc) with -fuse-ld=mold rather than requiring clang, so this works on
# systems without clang installed. mold supports being invoked via gcc's linker
# flag on both x86_64 and aarch64 Linux.
CARGO_CFG="${CARGO_HOME:-$HOME/.cargo}/config.toml"
mkdir -p "$(dirname "$CARGO_CFG")"
if ! grep -qF '[target.x86_64-unknown-linux-gnu]' "$CARGO_CFG" 2>/dev/null; then
  cat >> "$CARGO_CFG" <<'MOLDEOF'
[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
MOLDEOF
fi
if ! grep -qF '[target.aarch64-unknown-linux-gnu]' "$CARGO_CFG" 2>/dev/null; then
  cat >> "$CARGO_CFG" <<'MOLDEOF'
[target.aarch64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
MOLDEOF
fi
"#;

/// Native `extractions/setup-just` (and equivalents): install the admitted
/// locked `just` mise key and verify the version. No apt/Cargo fallback.
fn setup_just_script() -> String {
    let mut script = locked_mise_install("just");
    script.push_str(&format!(
        "just --version | grep -F '{JUST_LOCKED_VERSION}'\n"
    ));
    script
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn native_post_condition(
    adapter: NativeActionAdapter,
    cache_kind: Option<CacheActionKind>,
) -> Option<&'static str> {
    match adapter {
        // Only the root cache action saves in post; `/restore` and `/save` do
        // not register a post step. Absent kind defaults to root.
        NativeActionAdapter::Cache => match cache_kind {
            Some(CacheActionKind::Restore) | Some(CacheActionKind::Save) => None,
            Some(CacheActionKind::Root) | None => Some("success()"),
        },
        NativeActionAdapter::RustCache => Some("success() || env.CACHE_ON_FAILURE == 'true'"),
        // Sccache post step stops the server (always run, matches GitHub's behavior).
        NativeActionAdapter::Sccache => Some("always()"),
        NativeActionAdapter::Kache => Some("always()"),
        // GitHub's setup-buildx post removes the builder it created.
        NativeActionAdapter::DockerSetupBuildx => Some("always()"),
        // GitHub's login-action post logs out (drops registry credentials).
        NativeActionAdapter::DockerLogin => Some("always()"),
        NativeActionAdapter::CreateGitHubAppToken => Some("always()"),
        _ => None,
    }
}

/// Dispatch an `actions/cache` main step by lifecycle. Root and `/restore`
/// restore in main; `/save` saves in main with no prior lookup. Root and
/// `/restore` differ only in the outputs they expose (see below); `/save`
/// exposes none, matching upstream.
fn native_cache(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    match action.cache_kind.unwrap_or(CacheActionKind::Root) {
        CacheActionKind::Root => native_cache_restore_main(action, state, CacheActionKind::Root),
        CacheActionKind::Restore => {
            native_cache_restore_main(action, state, CacheActionKind::Restore)
        }
        CacheActionKind::Save => native_cache_save_main(action, state),
    }
}

/// `actions/cache` root/`restore` main phase: look up the key (with restore-key
/// prefix fallback) and restore unless `lookup-only`. Output exposure is gated
/// by kind — root emits only `cache-hit`; `/restore` also emits
/// `cache-primary-key` and `cache-matched-key`.
fn native_cache_restore_main(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
    kind: CacheActionKind,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let key = native_cache_key(action, &action_state, "key");
    let path = native_input(action, &action_state, "path");
    validate_cache_paths(&action_state, &path)?;
    let restore_keys = native_input(action, &action_state, "restore-keys");
    let fail_on_cache_miss =
        input_truthy(&native_input(action, &action_state, "fail-on-cache-miss"));
    let lookup_only = input_truthy(&native_input(action, &action_state, "lookup-only"));
    let version = cache_scope_version_for(action, &action_state, &path);
    let t0 = Instant::now();
    let matched_key = find_cache_match(&action_state, &key, &restore_keys, &version, &path)?;
    if let Some(matched_key) = &matched_key
        && !lookup_only
    {
        restore_cache_paths(&action_state, matched_key, &path, &version)?;
    }
    let restore_ms = t0.elapsed().as_millis();
    let exact_hit = matched_key.as_deref() == Some(key.as_str());

    let mut outputs = BTreeMap::new();
    outputs.insert("cache-hit".to_string(), exact_hit.to_string());
    // Upstream root emits only `cache-hit`; `/restore` emits all three.
    if kind == CacheActionKind::Restore {
        outputs.insert("cache-primary-key".to_string(), key.clone());
        outputs.insert(
            "cache-matched-key".to_string(),
            matched_key.clone().unwrap_or_default(),
        );
    }

    let mut state_values = BTreeMap::new();
    if !key.is_empty() {
        state_values.insert("primaryKey".to_string(), key.clone());
    }
    if let Some(matched_key) = &matched_key {
        state_values.insert("matchedKey".to_string(), matched_key.clone());
    }

    let declared_paths = cache_paths(&path);
    let persistent_paths = declared_paths
        .iter()
        .filter(|path| velnor_persistent_cache_path(&action_state, path))
        .count();
    let mut stdout = String::new();
    if let Some(matched_key) = &matched_key {
        if lookup_only {
            stdout.push_str(&format!(
                "{ANSI_GREEN}Cache lookup found key: {matched_key}{ANSI_RESET}\n"
            ));
        } else {
            stdout.push_str(&format!(
                "{ANSI_GREEN}Cache restored from key: {matched_key}{ANSI_RESET} ({restore_ms}ms)\n"
            ));
        }
    } else if persistent_paths > 0 && persistent_paths == declared_paths.len() {
        stdout.push_str(&format!(
            "{ANSI_GREEN}Cache paths live on Velnor host-persistent storage (always warm){ANSI_RESET}\n"
        ));
    } else if persistent_paths > 0 {
        stdout.push_str(&format!(
            "{ANSI_GREEN}{persistent_paths}/{} cache path(s) live on Velnor host-persistent storage (always warm){ANSI_RESET}; keyed lookup missed\n",
            declared_paths.len()
        ));
    } else {
        stdout.push_str(&format!("{ANSI_YELLOW}Cache not found for input keys: "));
        stdout.push_str(&cache_lookup_keys(&key, &restore_keys).join(", "));
        stdout.push_str(&format!("{ANSI_RESET}\n"));
    }
    if !path.is_empty() {
        stdout.push_str(&format!("{ANSI_CYAN}Cache path: {path}{ANSI_RESET}\n"));
    }
    let summary = if persistent_paths > 0 && persistent_paths == declared_paths.len() {
        "## Velnor cache report\n- Backend: actions/cache (native)\n- Store: host-persistent cache class\n- Result: host-persistent store — restore/save skipped\n".to_string()
    } else if persistent_paths > 0 {
        format!(
            "## Velnor cache report\n- Backend: actions/cache (native)\n- Key: `{key}`\n- Result: keyed miss; {persistent_paths}/{} paths host-persistent\n- Restore: {restore_ms} ms\n",
            declared_paths.len()
        )
    } else {
        format!(
            "## Velnor cache report\n- Backend: actions/cache (native)\n- Key: `{key}`\n- Result: {}\n- Restore: {restore_ms} ms\n",
            matched_key.as_deref().unwrap_or("miss")
        )
    };

    Ok(StepExecutionResult {
        exit_code: if fail_on_cache_miss && matched_key.is_none() {
            1
        } else {
            0
        },
        state: StepCommandState {
            outputs,
            state: state_values,
            summary,
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout,
        stderr: if fail_on_cache_miss && matched_key.is_none() {
            "Cache not found and fail-on-cache-miss is true\n".to_string()
        } else {
            String::new()
        },
    })
}

/// `actions/cache/save` main phase: persist directly with no prior lookup or
/// restore, and expose no outputs. Never materializes an existing entry before
/// saving; an already-present key is reported as `AlreadyExists`.
fn native_cache_save_main(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let key = native_cache_key(action, &action_state, "key");
    let path = native_input(action, &action_state, "path");
    validate_cache_paths(&action_state, &path)?;
    let version = cache_scope_version_for(action, &action_state, &path);
    save_cache_result(&action_state, &key, &path, false, &version)
}

fn native_rust_cache(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let shared_key = native_cache_key(action, &action_state, "shared-key");
    let cache_directories = native_input(action, &action_state, "cache-directories");
    validate_cache_paths(&action_state, &cache_directories)?;
    let cache_on_failure = native_input_or(&action_state, action, "cache-on-failure", "false");
    let version = cache_scope_version_for(action, &action_state, &cache_directories);
    let t0 = Instant::now();
    let matched = find_cache_match(&action_state, &shared_key, "", &version, &cache_directories)?;
    if let Some(matched_key) = &matched {
        restore_cache_paths(&action_state, matched_key, &cache_directories, &version)?;
    }
    let restore_ms = t0.elapsed().as_millis();
    let persistent_target =
        rust_cache_covered_by_persistent_storage(&action_state, &cache_directories);
    let cache_hit = matched.is_some() || persistent_target;
    let mut outputs = BTreeMap::new();
    outputs.insert("cache-hit".to_string(), cache_hit.to_string());
    if !shared_key.is_empty() {
        outputs.insert("cache-primary-key".to_string(), shared_key.clone());
    }
    let stdout = if let Some(key) = matched {
        format!(
            "{ANSI_GREEN}Rust cache restored from shared key '{key}'{ANSI_RESET} ({restore_ms}ms)\n"
        )
    } else if persistent_target {
        format!(
            "{ANSI_GREEN}Rust cache paths live on Velnor host-persistent storage (always warm){ANSI_RESET}\n"
        )
    } else {
        format!("{ANSI_YELLOW}Rust cache miss for shared key '{shared_key}'{ANSI_RESET}\n")
    };
    let summary = if persistent_target {
        "## Velnor cache report\n- Backend: rust-cache (native)\n- Store: host-persistent target class\n- Result: host-persistent store — restore/save skipped\n".to_string()
    } else {
        format!(
            "## Velnor cache report\n- Backend: rust-cache (native)\n- Key: `{shared_key}`\n- Result: {}\n- Restore: {restore_ms} ms\n",
            if cache_hit { "hit" } else { "miss" }
        )
    };
    Ok(StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            outputs,
            env: [("CACHE_ON_FAILURE".to_string(), cache_on_failure)].into(),
            summary,
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout,
        stderr: String::new(),
    })
}

fn native_cache_save(
    step_id: &str,
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let key = native_cache_key(action, &action_state, "key");
    let path = native_input(action, &action_state, "path");
    let version = cache_scope_version_for(action, &action_state, &path);
    let exact_hit = state
        .outputs
        .get(step_id)
        .and_then(|outputs| outputs.get("cache-hit"))
        .is_some_and(|value| value == "true");
    save_cache_result(&action_state, &key, &path, exact_hit, &version)
}

fn native_rust_cache_save(
    step_id: &str,
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let key = native_cache_key(action, &action_state, "shared-key");
    let path = native_input(action, &action_state, "cache-directories");
    let version = cache_scope_version_for(action, &action_state, &path);
    let exact_hit = state
        .outputs
        .get(step_id)
        .and_then(|outputs| outputs.get("cache-hit"))
        .is_some_and(|value| value == "true");
    let mut result = save_cache_result(&action_state, &key, &path, exact_hit, &version)?;
    result.state.summary = result
        .state
        .summary
        .replace("actions/cache (native post)", "rust-cache (native post)");
    Ok(result)
}

fn cache_lookup_keys(key: &str, restore_keys: &str) -> Vec<String> {
    std::iter::once(key)
        .chain(restore_keys.lines().map(str::trim))
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn native_cache_key(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
    name: &str,
) -> String {
    native_input(action, state, name).trim().to_string()
}

/// Deterministic cache-version segment that isolates entries by runtime
/// compatibility boundary: the action pin (`git_ref`/SHA), `RUNNER_OS`,
/// `RUNNER_ARCH`, and the normalized declared paths. It sits below
/// `<trust>/caches/<repo>` so the same visible key never restores across an
/// incompatible variant. The user key (with evaluated lock hashes) remains the
/// toolchain/input identity inside this version.
fn cache_scope_version_for(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
    paths: &str,
) -> String {
    cache_scope_version(
        &action.git_ref,
        &cache_scope_runner_field(state, "RUNNER_OS"),
        &cache_scope_runner_field(state, "RUNNER_ARCH"),
        paths,
    )
}

fn cache_scope_runner_field(state: &JobExecutionState, name: &str) -> String {
    state.env.get(name).cloned().unwrap_or_default()
}

fn cache_scope_version(git_ref: &str, runner_os: &str, runner_arch: &str, paths: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(git_ref.as_bytes());
    hasher.update([0u8]);
    hasher.update(runner_os.as_bytes());
    hasher.update([0u8]);
    hasher.update(runner_arch.as_bytes());
    hasher.update([0u8]);
    hasher.update(normalize_cache_scope_paths(paths).as_bytes());
    let digest = hasher.finalize();
    format!("cv1-{}", hex_digest(&digest[..16]))
}

fn normalize_cache_scope_paths(paths: &str) -> String {
    let mut items: Vec<String> = cache_paths(paths)
        .into_iter()
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
        .collect();
    items.sort();
    items.join("\n")
}

fn validate_cache_paths(state: &JobExecutionState, paths: &str) -> Result<()> {
    for path in cache_paths(paths) {
        if has_glob_pattern(&path) {
            let Some((_, pattern)) = cache_glob_base_and_pattern(state, &path)? else {
                bail!("cache glob '{path}' cannot resolve to Velnor-mapped job storage");
            };
            Glob::new(&normalize_glob_pattern(&pattern))
                .with_context(|| format!("invalid cache glob syntax '{path}'"))?;
        } else if !velnor_persistent_cache_path(state, &path)
            && resolve_cache_path(state, &path).is_none()
        {
            bail!("cache path '{path}' cannot resolve to Velnor-mapped job storage");
        }
    }
    Ok(())
}

fn find_cache_match(
    state: &JobExecutionState,
    key: &str,
    restore_keys: &str,
    version: &str,
    paths: &str,
) -> Result<Option<String>> {
    let store = cache_store_dir(state, version)?;
    if key.is_empty() || !store.exists() {
        return Ok(None);
    }
    let exact = store.join(sanitize_artifact_name(key));
    if cache_entry_complete_for_paths(&exact, paths) {
        return Ok(Some(key.to_string()));
    }
    for restore_key in restore_keys
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let mut matches = cache_entries_with_prefix(&store, restore_key, paths)?;
        matches.sort_by(|left, right| {
            right
                .created
                .cmp(&left.created)
                .then_with(|| left.key.cmp(&right.key))
        });
        if let Some(matched) = matches.into_iter().next() {
            return Ok(Some(matched.key));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheEntryMatch {
    key: String,
    created: u128,
}

fn cache_entries_with_prefix(
    store: &Path,
    prefix: &str,
    paths: &str,
) -> Result<Vec<CacheEntryMatch>> {
    let sanitized_prefix = sanitize_artifact_name(prefix);
    let mut matches = Vec::new();
    for entry in fs::read_dir(store).with_context(|| format!("read {}", store.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || !cache_entry_complete_for_paths(&entry.path(), paths) {
            continue;
        }
        let sanitized_key = entry.file_name().to_string_lossy().to_string();
        if sanitized_key.starts_with(&sanitized_prefix) {
            let path = entry.path();
            matches.push(CacheEntryMatch {
                key: cache_key_from_metadata(&path).unwrap_or(sanitized_key),
                created: cache_entry_created(&path),
            });
        }
    }
    Ok(matches)
}

fn cache_entry_created(path: &Path) -> u128 {
    fs::read_to_string(path.join(".velnor-created"))
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .or_else(|| {
            path.metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(system_time_nanos)
        })
        .unwrap_or_default()
}

fn cache_timestamp() -> String {
    system_time_nanos(SystemTime::now())
        .unwrap_or_default()
        .to_string()
}

fn system_time_nanos(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

fn restore_cache_paths(
    state: &JobExecutionState,
    key: &str,
    paths: &str,
    version: &str,
) -> Result<()> {
    let declared_paths = cache_paths(paths);
    let cache_dir = cache_store_dir(state, version)?.join(sanitize_artifact_name(key));
    let _lock = CacheEntryLock::shared(&cache_dir)?;
    if !cache_entry_complete(&cache_dir) {
        bail!("cache entry '{key}' is incomplete");
    }
    for (index, path) in declared_paths.iter().enumerate() {
        // Paths that live on Velnor's host-persistent mounts (cargo
        // registry/git, mise installs, sccache) are always warm — copying
        // store bytes over them would be pure waste.
        if velnor_persistent_cache_path(state, path) {
            continue;
        }
        if has_glob_pattern(path) {
            restore_cache_glob_path(state, &cache_dir, index, path, &declared_paths)?;
            continue;
        }
        let source = cache_dir.join(index.to_string());
        if !source.exists() {
            continue;
        }
        let Some(destination) = resolve_cache_path(state, path) else {
            continue;
        };
        restore_cache_dir_contents(state, &source, &destination)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CacheGlobManifest {
    version: u8,
    entries: Vec<CacheGlobManifestEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CacheGlobManifestEntry {
    relative: String,
    kind: CacheGlobEntryKind,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CacheGlobEntryKind {
    Directory,
    File,
}

type CacheGlobSource = (PathBuf, CacheGlobEntryKind);
type CacheGlobSources = Vec<CacheGlobSource>;

fn cache_entry_complete_for_paths(path: &Path, paths: &str) -> bool {
    if !cache_entry_complete(path) {
        return false;
    }
    cache_paths(paths)
        .into_iter()
        .enumerate()
        .all(|(index, declared)| {
            !has_glob_pattern(&declared) || cache_glob_manifest_is_valid(path, index)
        })
}

fn cache_glob_manifest_path(cache_entry: &Path, index: usize) -> PathBuf {
    cache_entry
        .join(index.to_string())
        .join(CACHE_GLOB_MANIFEST_FILE)
}

fn cache_glob_manifest_is_valid(cache_entry: &Path, index: usize) -> bool {
    let manifest_path = cache_glob_manifest_path(cache_entry, index);
    read_cache_glob_manifest(&manifest_path)
        .and_then(|manifest| validate_cache_glob_manifest(&manifest).map(|_| manifest))
        .is_ok()
}

fn read_cache_glob_manifest(path: &Path) -> Result<CacheGlobManifest> {
    let file = fs::File::open(path)
        .with_context(|| format!("open cache glob manifest {}", path.display()))?;
    if file.metadata()?.len() > CACHE_GLOB_MAX_MANIFEST_BYTES {
        bail!(
            "cache glob manifest {} exceeds the {}-byte limit",
            path.display(),
            CACHE_GLOB_MAX_MANIFEST_BYTES
        );
    }
    let mut bytes = Vec::new();
    file.take(CACHE_GLOB_MAX_MANIFEST_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read cache glob manifest {}", path.display()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > CACHE_GLOB_MAX_MANIFEST_BYTES {
        bail!(
            "cache glob manifest {} exceeds the {}-byte limit",
            path.display(),
            CACHE_GLOB_MAX_MANIFEST_BYTES
        );
    }
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse cache glob manifest {}", path.display()))
}

fn validate_cache_glob_manifest(manifest: &CacheGlobManifest) -> Result<()> {
    if manifest.version != 1 {
        bail!(
            "unsupported cache glob manifest version {}",
            manifest.version
        );
    }
    if manifest.entries.len() > CACHE_GLOB_MAX_MATCHES {
        bail!(
            "cache glob manifest contains more than the {}-entry limit",
            CACHE_GLOB_MAX_MATCHES
        );
    }
    let mut seen = BTreeSet::new();
    let mut path_bytes = 0_u64;
    for entry in &manifest.entries {
        let relative = validate_cache_glob_relative(&entry.relative)?;
        if relative.components().count() > RESULTS_ARTIFACT_MAX_UPLOAD_PATH_DEPTH {
            bail!(
                "cache glob manifest path '{}' exceeds the {}-component depth limit",
                entry.relative,
                RESULTS_ARTIFACT_MAX_UPLOAD_PATH_DEPTH
            );
        }
        if !seen.insert(entry.relative.clone()) {
            bail!(
                "cache glob manifest contains duplicate path '{}'",
                entry.relative
            );
        }
        path_bytes = path_bytes
            .checked_add(u64::try_from(entry.relative.len()).unwrap_or(u64::MAX))
            .context("cache glob manifest path byte count overflowed")?;
        if path_bytes > CACHE_GLOB_MAX_PATH_BYTES {
            bail!(
                "cache glob manifest paths exceed the {}-byte limit",
                CACHE_GLOB_MAX_PATH_BYTES
            );
        }
    }
    Ok(())
}

fn cache_glob_base_and_pattern(
    state: &JobExecutionState,
    path: &str,
) -> Result<Option<(PathBuf, String)>> {
    let resolved = artifact_glob_base_and_pattern(state, path).with_context(|| {
        format!("cache glob '{path}' resolves outside Velnor-mapped job storage")
    })?;
    let Some((base, pattern)) = resolved else {
        bail!("cache glob '{path}' cannot resolve to Velnor-mapped job storage");
    };

    trusted_job_destination(state, &base).with_context(|| {
        format!(
            "cache glob '{path}' resolves outside Velnor-mapped job storage: {}",
            base.display()
        )
    })?;
    Ok(Some((base, pattern)))
}

fn validate_cache_glob_relative(relative: &str) -> Result<PathBuf> {
    if relative.is_empty() || relative.contains('\\') {
        bail!("invalid cache glob manifest path '{relative}'");
    }
    let path = Path::new(relative);
    if path.is_absolute() {
        bail!("cache glob manifest path must be relative: '{relative}'");
    }
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) => {}
            _ => bail!("cache glob manifest path escapes its root: '{relative}'"),
        }
    }
    Ok(path.to_path_buf())
}

fn cache_glob_sources(
    state: &JobExecutionState,
    path: &str,
) -> Result<
    Option<(
        PathBuf,
        Option<crate::fs_copy::NoFollowDir>,
        CacheGlobSources,
    )>,
> {
    let Some((base, pattern)) = cache_glob_base_and_pattern(state, path)? else {
        return Ok(None);
    };
    if !base.exists() {
        return Ok(Some((base, None, Vec::new())));
    }
    let directory = crate::fs_copy::NoFollowDir::open_absolute(&base)
        .with_context(|| format!("securely open cache glob root {}", base.display()))?;
    let matcher = Glob::new(&normalize_glob_pattern(&pattern))?.compile_matcher();
    let mut sources = Vec::new();
    let mut path_bytes = 0_u64;
    let mut visited_entries = 0_usize;
    let mut visited_path_bytes = 0_u64;
    collect_cache_glob_matches(
        &directory,
        &base,
        Path::new(""),
        &matcher,
        &mut sources,
        &mut path_bytes,
        &mut visited_entries,
        &mut visited_path_bytes,
    )?;
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    sources.dedup_by(|left, right| left.0 == right.0);

    // A matching directory contains every matching descendant. Keep only the
    // shallowest roots so one glob cannot publish duplicate overlapping trees.
    let mut roots = Vec::with_capacity(sources.len());
    let mut directory_roots = BTreeSet::new();
    for (relative, kind) in sources {
        if relative
            .ancestors()
            .skip(1)
            .any(|ancestor| directory_roots.contains(ancestor))
        {
            continue;
        }
        if kind == CacheGlobEntryKind::Directory {
            directory_roots.insert(relative.clone());
        }
        roots.push((relative, kind));
    }
    Ok(Some((base, Some(directory), roots)))
}

fn cache_glob_source_overlaps_persistent_exact(
    state: &JobExecutionState,
    declared_paths: &[String],
    source: &Path,
    relative: &Path,
) -> bool {
    for declared in declared_paths {
        if has_glob_pattern(declared) || !velnor_persistent_cache_path(state, declared) {
            continue;
        }
        if state.persistent_workspace_target
            && let Some(target_relative) = workspace_target_relative(declared)
            && (relative == target_relative || relative.starts_with(&target_relative))
        {
            return true;
        }
        if let Some(persistent_path) = resolve_cache_path(state, declared)
            && (source == persistent_path || source.starts_with(&persistent_path))
        {
            return true;
        }
    }
    false
}

fn workspace_target_relative(path: &str) -> Option<PathBuf> {
    let path = path.trim();
    let relative = match path {
        "target" | "./target" | "/__w/target" => "",
        _ => path
            .strip_prefix("target/")
            .or_else(|| path.strip_prefix("./target/"))
            .or_else(|| path.strip_prefix("/__w/target/"))?,
    };
    Some(Path::new("target").join(relative))
}

#[allow(clippy::too_many_arguments)]
fn collect_cache_glob_matches(
    current: &crate::fs_copy::NoFollowDir,
    root: &Path,
    current_relative: &Path,
    matcher: &globset::GlobMatcher,
    matches: &mut CacheGlobSources,
    path_bytes: &mut u64,
    visited_entries: &mut usize,
    visited_path_bytes: &mut u64,
) -> Result<()> {
    let current_path = root.join(current_relative);
    current.for_each_entry_filtered(
        |name| cache_source_entry_is_not_symlink(&current_path, name),
        |entry| {
            let relative = current_relative.join(&entry.name);
            *visited_entries = visited_entries
                .checked_add(1)
                .context("cache glob visited-entry count overflowed")?;
            if *visited_entries > CACHE_GLOB_MAX_VISITED_ENTRIES {
                bail!(
                    "cache glob traversal visited more than the {}-entry limit",
                    CACHE_GLOB_MAX_VISITED_ENTRIES
                );
            }
            *visited_path_bytes = visited_path_bytes
                .checked_add(
                    u64::try_from(relative.as_os_str().as_encoded_bytes().len())
                        .unwrap_or(u64::MAX),
                )
                .context("cache glob visited-path byte count overflowed")?;
            if *visited_path_bytes > CACHE_GLOB_MAX_PATH_BYTES {
                bail!(
                    "cache glob visited paths exceed the {}-byte limit",
                    CACHE_GLOB_MAX_PATH_BYTES
                );
            }
            if relative.components().count() > RESULTS_ARTIFACT_MAX_UPLOAD_PATH_DEPTH {
                bail!(
                    "cache glob path {} exceeds the {}-component depth limit",
                    relative.display(),
                    RESULTS_ARTIFACT_MAX_UPLOAD_PATH_DEPTH
                );
            }
            let matched = matcher.is_match(&relative);
            match entry.source {
                crate::fs_copy::NoFollowSource::Directory(directory) => {
                    if matched {
                        record_cache_glob_match(
                            matches,
                            path_bytes,
                            relative,
                            CacheGlobEntryKind::Directory,
                        )?;
                    } else {
                        // A matched directory is already a complete restore
                        // root. Do not scan its descendants for overlapping
                        // matches.
                        collect_cache_glob_matches(
                            &directory,
                            root,
                            &relative,
                            matcher,
                            matches,
                            path_bytes,
                            visited_entries,
                            visited_path_bytes,
                        )?;
                    }
                }
                crate::fs_copy::NoFollowSource::File(_) => {
                    if matched {
                        record_cache_glob_match(
                            matches,
                            path_bytes,
                            relative,
                            CacheGlobEntryKind::File,
                        )?;
                    }
                }
            }
            Ok(())
        },
    )
}

fn record_cache_glob_match(
    matches: &mut CacheGlobSources,
    path_bytes: &mut u64,
    relative: PathBuf,
    kind: CacheGlobEntryKind,
) -> Result<()> {
    if matches.len() >= CACHE_GLOB_MAX_MATCHES {
        bail!(
            "cache glob contains more than the {}-entry limit",
            CACHE_GLOB_MAX_MATCHES
        );
    }
    *path_bytes = path_bytes
        .checked_add(u64::try_from(relative.to_string_lossy().len()).unwrap_or(u64::MAX))
        .context("cache glob path byte count overflowed")?;
    if *path_bytes > CACHE_GLOB_MAX_PATH_BYTES {
        bail!(
            "cache glob paths exceed the {}-byte limit",
            CACHE_GLOB_MAX_PATH_BYTES
        );
    }
    matches.push((relative, kind));
    Ok(())
}

fn cache_source_entry_is_not_symlink(directory: &Path, name: &std::ffi::OsStr) -> bool {
    fs::symlink_metadata(directory.join(name))
        .map(|metadata| !metadata.file_type().is_symlink())
        .unwrap_or(true)
}

fn restore_cache_glob_path(
    state: &JobExecutionState,
    cache_entry: &Path,
    index: usize,
    path: &str,
    declared_paths: &[String],
) -> Result<()> {
    let Some((base, _)) = cache_glob_base_and_pattern(state, path)? else {
        return Ok(());
    };
    let index_dir = cache_entry.join(index.to_string());
    let manifest_path = index_dir.join(CACHE_GLOB_MANIFEST_FILE);
    let manifest = read_cache_glob_manifest(&manifest_path)
        .with_context(|| format!("read concrete-path manifest for cache glob '{path}'"))?;
    validate_cache_glob_manifest(&manifest)
        .with_context(|| format!("validate cache glob manifest for '{path}'"))?;

    let payload = index_dir.join("payload");
    let payload_directory = if manifest.entries.is_empty() {
        None
    } else {
        Some(
            crate::fs_copy::NoFollowDir::open_absolute(&payload).with_context(|| {
                format!("securely open cache glob payload {}", payload.display())
            })?,
        )
    };
    let mut seen = BTreeSet::new();
    let mut validated = Vec::with_capacity(manifest.entries.len());
    for entry in manifest.entries {
        let relative = validate_cache_glob_relative(&entry.relative)?;
        if !seen.insert(relative.clone()) {
            bail!(
                "cache glob manifest contains duplicate path '{}'",
                entry.relative
            );
        }
        let source_path = payload.join(&relative);
        let destination = base.join(&relative);
        if cache_glob_source_overlaps_persistent_exact(
            state,
            declared_paths,
            &destination,
            &relative,
        ) {
            continue;
        }
        let source = payload_directory
            .as_ref()
            .context("cache glob manifest has entries but no payload directory")?
            .open_source(&relative)?
            .with_context(|| {
                format!(
                    "cache glob payload is missing for '{}': {}",
                    entry.relative,
                    source_path.display()
                )
            })?;
        let kind_matches = matches!(
            (entry.kind, &source),
            (
                CacheGlobEntryKind::Directory,
                crate::fs_copy::NoFollowSource::Directory(_)
            ) | (
                CacheGlobEntryKind::File,
                crate::fs_copy::NoFollowSource::File(_)
            )
        );
        if !kind_matches {
            bail!(
                "cache glob manifest kind does not match payload for '{}'",
                entry.relative
            );
        }
        validated.push((entry.kind, source_path, relative));
    }
    if validated.is_empty() {
        return Ok(());
    }
    let destination_scope = trusted_job_destination(state, &base)?;
    let destination_root = destination_scope
        .open_relative_directory(Path::new(""))
        .with_context(|| format!("securely open cache glob restore root {}", base.display()))?;
    for (kind, source_path, relative) in validated {
        let source = payload_directory
            .as_ref()
            .context("cache glob manifest has entries but no payload directory")?
            .open_source(&relative)?
            .with_context(|| {
                format!(
                    "cache glob payload disappeared before restore: {}",
                    source_path.display()
                )
            })?;
        let kind_matches = matches!(
            (kind, &source),
            (
                CacheGlobEntryKind::Directory,
                crate::fs_copy::NoFollowSource::Directory(_)
            ) | (
                CacheGlobEntryKind::File,
                crate::fs_copy::NoFollowSource::File(_)
            )
        );
        if !kind_matches {
            bail!(
                "cache glob source changed type before restore: {}",
                source_path.display()
            );
        }
        copy_opened_cache_restore_source(
            source,
            &source_path,
            &destination_root,
            &destination_scope,
            &relative,
        )?;
    }
    Ok(())
}

/// True for cache paths whose container locations are backed by Velnor's
/// host-persistent or image-provided stores (mounted into every job container):
/// the cargo registry/git stores, the mise tool store, the image-baked rustup
/// toolchain store, and the shared sccache dir. These are always warm; the
/// actions/cache adapter neither tars them into the store nor copies store bytes
/// back over them.
fn velnor_persistent_cache_path(state: &JobExecutionState, path: &str) -> bool {
    if state.persistent_workspace_target && workspace_target_cache_path(path) {
        return true;
    }
    velnor_static_persistent_cache_path(path)
}

fn workspace_target_cache_path(path: &str) -> bool {
    let path = path.trim();
    matches!(path, "target" | "./target" | "/__w/target")
        || path.starts_with("target/")
        || path.starts_with("./target/")
        || path.starts_with("/__w/target/")
}

fn path_or_child(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn velnor_static_persistent_cache_path(path: &str) -> bool {
    let path = path.trim();
    // These are the relative aliases emitted by the canonical fleet cache
    // declarations. They resolve to Velnor's mounted Cargo/mise stores even
    // though Actions treats them as workspace-relative paths on GitHub.
    if path_or_child(path, ".cargo/registry")
        || path_or_child(path, ".cargo/git")
        || path_or_child(path, ".cache/mise")
        || path_or_child(path, ".local/share/mise/installs")
    {
        return true;
    }
    let home_relative = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("/github/home/"));
    if let Some(rest) = home_relative {
        return path_or_child(rest, ".cargo/registry")
            || path_or_child(rest, ".cargo/git")
            // Workflows usually cache ~/.rustup on GitHub-hosted runners. Velnor
            // points RUSTUP_HOME at the image-baked /root/.rustup store instead,
            // so a ~/.rustup cache restore is pure hosted-runner compatibility
            // noise on the Velnor lane.
            || path_or_child(rest, ".rustup");
    }
    path_or_child(path, "/opt/mise")
        || path_or_child(path, "/root/.rustup")
        || path_or_child(path, "/var/cache/sccache")
}

fn rust_cache_covered_by_persistent_storage(
    state: &JobExecutionState,
    cache_directories: &str,
) -> bool {
    let paths = cache_paths(cache_directories);
    if !paths.is_empty() {
        return paths
            .iter()
            .all(|path| velnor_persistent_cache_path(state, path));
    }
    state.persistent_workspace_target
}

fn container_runtime_env(container: &JobContainerSpec) -> Vec<(String, String)> {
    container.env.clone()
}

/// Structural phase of a save that lost a race for the shared store, so the
/// summary/stderr can name where contention happened instead of a generic skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveContentionPhase {
    Copy,
    Publish,
}

impl SaveContentionPhase {
    fn label(self) -> &'static str {
        match self {
            SaveContentionPhase::Copy => "copy",
            SaveContentionPhase::Publish => "publish",
        }
    }
}

/// Explicit result of a cache save. Replaces the previous string-sniffing on
/// stdout/stderr so a non-persisting outcome (exact hit, already-exists, empty
/// key, no paths, contention) can never be reported as "completed".
#[derive(Debug, Clone, PartialEq, Eq)]
enum SaveOutcome {
    Persisted {
        paths: usize,
    },
    ExactHit,
    AlreadyExists,
    HostPersistent,
    NoPaths,
    EmptyKey,
    Contended {
        phase: SaveContentionPhase,
        detail: String,
    },
}

/// Lock-wait, copy, and total durations captured while saving, surfaced in the
/// step summary for observability.
#[derive(Debug, Clone, Copy, Default)]
struct SaveTiming {
    lock_wait_ms: u128,
    copy_ms: u128,
    total_ms: u128,
}

fn save_cache_result(
    state: &JobExecutionState,
    key: &str,
    paths: &str,
    exact_hit: bool,
    version: &str,
) -> Result<StepExecutionResult> {
    // Validate the complete declaration before resolving or mutating the
    // shared cache entry. In particular, glob syntax must fail before the
    // entry lock, staging directory, or metadata can be created.
    validate_cache_paths(state, paths)?;
    let t0 = Instant::now();
    if key.is_empty() {
        return Ok(cache_save_step_result(
            key,
            SaveOutcome::EmptyKey,
            SaveTiming::default(),
        ));
    }
    if exact_hit {
        return Ok(cache_save_step_result(
            key,
            SaveOutcome::ExactHit,
            SaveTiming::default(),
        ));
    }
    let cache_dir = cache_store_dir(state, version)?.join(sanitize_artifact_name(key));
    let lock_start = Instant::now();
    let _lock = CacheEntryLock::exclusive(&cache_dir)?;
    let lock_wait_ms = lock_start.elapsed().as_millis();
    if cache_entry_complete_for_paths(&cache_dir, paths) {
        return Ok(cache_save_step_result(
            key,
            SaveOutcome::AlreadyExists,
            SaveTiming {
                lock_wait_ms,
                total_ms: t0.elapsed().as_millis(),
                ..SaveTiming::default()
            },
        ));
    }
    // The store is shared across daemon slots: stage the whole entry next to
    // its final location and swap with a rename, so a sibling slot restoring
    // this key never reads a half-written tree.
    let staging_name = cache_staging_name(
        cache_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cache"),
    );
    let staging_dir = cache_dir.with_file_name(staging_name);
    fs::remove_dir_all(&staging_dir).ok();
    fs::create_dir_all(&staging_dir)
        .with_context(|| format!("create cache staging directory {}", staging_dir.display()))?;
    fs::write(staging_dir.join(".velnor-key"), key)
        .with_context(|| format!("write cache metadata {}", staging_dir.display()))?;
    fs::write(staging_dir.join(".velnor-created"), cache_timestamp())
        .with_context(|| format!("write cache timestamp {}", staging_dir.display()))?;

    let copy_start = Instant::now();
    let mut saved = 0usize;
    let mut persistent = 0usize;
    let declared_paths = cache_paths(paths);
    for (index, path) in declared_paths.iter().enumerate() {
        if velnor_persistent_cache_path(state, path) {
            persistent += 1;
            continue;
        }
        let target = staging_dir.join(index.to_string());
        if has_glob_pattern(path) {
            let Some((base, base_directory, sources)) = cache_glob_sources(state, path)? else {
                continue;
            };
            let mut entries = Vec::new();
            let destination_root = if sources.is_empty() {
                None
            } else {
                Some(
                    crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
                        &staging_dir,
                        Path::new(&format!("{index}/payload")),
                    )
                    .with_context(|| {
                        format!("create cache glob staging directory {}", target.display())
                    })?,
                )
            };
            for (relative, kind) in sources {
                let source = base.join(&relative);
                if cache_glob_source_overlaps_persistent_exact(
                    state,
                    &declared_paths,
                    &source,
                    &relative,
                ) {
                    continue;
                }
                let relative_string = relative.to_str().ok_or_else(|| {
                    anyhow::anyhow!("cache glob path is not valid UTF-8: {}", relative.display())
                })?;
                validate_cache_glob_relative(relative_string)?;
                let copy_result = (|| {
                    let directory = base_directory
                        .as_ref()
                        .context("cache glob root disappeared before secure copy")?;
                    let opened = directory.open_source(&relative)?.with_context(|| {
                        format!("cache glob source disappeared: {}", source.display())
                    })?;
                    let kind_matches = matches!(
                        (kind, &opened),
                        (
                            CacheGlobEntryKind::Directory,
                            crate::fs_copy::NoFollowSource::Directory(_)
                        ) | (
                            CacheGlobEntryKind::File,
                            crate::fs_copy::NoFollowSource::File(_)
                        )
                    );
                    if !kind_matches {
                        bail!("cache glob source changed type: {}", source.display());
                    }
                    copy_opened_cache_glob_source(
                        opened,
                        &source,
                        destination_root
                            .as_ref()
                            .context("cache glob destination root disappeared")?,
                        &validate_cache_glob_relative(relative_string)?,
                    )
                })();
                if let Err(error) = copy_result {
                    fs::remove_dir_all(&staging_dir).ok();
                    return Ok(cache_save_step_result(
                        key,
                        SaveOutcome::Contended {
                            phase: SaveContentionPhase::Copy,
                            detail: format!("{error:#}"),
                        },
                        SaveTiming {
                            lock_wait_ms,
                            copy_ms: copy_start.elapsed().as_millis(),
                            total_ms: t0.elapsed().as_millis(),
                        },
                    ));
                }
                entries.push(CacheGlobManifestEntry {
                    relative: relative_string.to_owned(),
                    kind,
                });
            }
            fs::create_dir_all(&target)
                .with_context(|| format!("create cache entry {}", target.display()))?;
            let manifest = CacheGlobManifest {
                version: 1,
                entries,
            };
            let manifest_bytes = serde_json::to_vec(&manifest)
                .context("serialize cache glob concrete-path manifest")?;
            fs::write(target.join(CACHE_GLOB_MANIFEST_FILE), manifest_bytes)
                .with_context(|| format!("write cache glob manifest {}", target.display()))?;
            if !manifest.entries.is_empty() {
                saved += 1;
            }
            continue;
        }
        let Some(source) = resolve_cache_path(state, path) else {
            continue;
        };
        if !source.exists() {
            continue;
        }
        fs::create_dir_all(&target)
            .with_context(|| format!("create cache entry {}", target.display()))?;
        if let Err(error) = copy_cache_source(&source, &target) {
            fs::remove_dir_all(&staging_dir).ok();
            return Ok(cache_save_step_result(
                key,
                SaveOutcome::Contended {
                    phase: SaveContentionPhase::Copy,
                    detail: format!("{error:#}"),
                },
                SaveTiming {
                    lock_wait_ms,
                    copy_ms: copy_start.elapsed().as_millis(),
                    total_ms: t0.elapsed().as_millis(),
                },
            ));
        }
        saved += 1;
    }
    let copy_ms = copy_start.elapsed().as_millis();
    if saved == 0 {
        fs::remove_dir_all(&staging_dir).ok();
        let outcome = if persistent > 0 {
            SaveOutcome::HostPersistent
        } else {
            SaveOutcome::NoPaths
        };
        return Ok(cache_save_step_result(
            key,
            outcome,
            SaveTiming {
                lock_wait_ms,
                copy_ms,
                total_ms: t0.elapsed().as_millis(),
            },
        ));
    }
    // Completion marker is written only after every source was copied and
    // verified. Legacy or interrupted entries have no marker and are misses.
    // This prevents rustup and similar mutable trees from being restored from
    // a structurally incomplete cache generation.
    fs::write(staging_dir.join(".velnor-complete-v1"), b"complete\n")
        .with_context(|| format!("write cache completion marker {}", staging_dir.display()))?;
    fs::remove_dir_all(&cache_dir).ok();
    if let Err(error) = fs::rename(&staging_dir, &cache_dir)
        .with_context(|| format!("publish cache entry {}", cache_dir.display()))
    {
        fs::remove_dir_all(&staging_dir).ok();
        return Ok(cache_save_step_result(
            key,
            SaveOutcome::Contended {
                phase: SaveContentionPhase::Publish,
                detail: format!("{error:#}"),
            },
            SaveTiming {
                lock_wait_ms,
                copy_ms,
                total_ms: t0.elapsed().as_millis(),
            },
        ));
    }
    Ok(cache_save_step_result(
        key,
        SaveOutcome::Persisted { paths: saved },
        SaveTiming {
            lock_wait_ms,
            copy_ms,
            total_ms: t0.elapsed().as_millis(),
        },
    ))
}

fn cache_entry_complete(path: &Path) -> bool {
    path.is_dir() && path.join(".velnor-complete-v1").is_file()
}

const TARGET_SOURCE_REVISION_MARKER: &str = ".velnor-source-revision-v1";

fn materialize_persistent_target(
    container: &JobContainerSpec,
    source_revision: Option<&str>,
) -> Result<()> {
    let Some(store) = container.cargo_target_host.as_deref() else {
        return Ok(());
    };
    let _lock = CacheEntryLock::shared(store)?;
    let payload = store.join("data");
    if !store.join(".velnor-target-complete-v1").is_file() || !payload.is_dir() {
        return Ok(());
    }
    let stored_revision = fs::read_to_string(store.join(TARGET_SOURCE_REVISION_MARKER))
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let source_revision = workspace_source_revision(&container.workspace_host, source_revision);
    let target = container.workspace_host.join("target");
    if source_revision
        .as_deref()
        .is_some_and(|revision| stored_revision.as_deref() != Some(revision))
    {
        // Checkout pins source mtimes to the commit timestamp. Restoring a
        // different revision and then touching sources would poison the
        // published fingerprints for every later unchanged run.
        eprintln!(
            "forensics.lifecycle: persistent target generation invalidated (stored={}, current={})",
            stored_revision.as_deref().unwrap_or("unknown"),
            source_revision.as_deref().unwrap_or("unknown")
        );
        if target.exists() {
            fs::remove_dir_all(&target)
                .with_context(|| format!("clear stale job-local target {}", target.display()))?;
        }
        return Ok(());
    }
    if target.exists() {
        fs::remove_dir_all(&target)
            .with_context(|| format!("clear job-local target {}", target.display()))?;
    }
    fs::create_dir_all(&target)
        .with_context(|| format!("create job-local target {}", target.display()))?;
    copy_dir_contents(&payload, &target)?;
    Ok(())
}

fn workspace_source_revision(workspace: &Path, fallback: Option<&str>) -> Option<String> {
    std::process::Command::new("git")
        .args(["-C"])
        .arg(workspace)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|revision| revision.trim().to_owned())
        .filter(|revision| !revision.is_empty())
        .or_else(|| fallback.map(str::to_owned))
}

fn publish_persistent_target(
    container: &JobContainerSpec,
    source_revision: Option<&str>,
) -> Result<()> {
    let Some(store) = container.cargo_target_host.as_deref() else {
        return Ok(());
    };
    let target = container.workspace_host.join("target");
    if !target.is_dir() {
        return Ok(());
    }
    let name = store
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("target");
    let staging = store.with_file_name(cache_staging_name(name));
    fs::remove_dir_all(&staging).ok();
    let payload = staging.join("data");
    fs::create_dir_all(&payload)
        .with_context(|| format!("create target staging directory {}", payload.display()))?;
    if let Err(error) = copy_dir_contents(&target, &payload) {
        fs::remove_dir_all(&staging).ok();
        return Err(error).context("stage persistent target generation");
    }
    fs::write(staging.join(".velnor-target-complete-v1"), b"complete\n")
        .with_context(|| format!("write target completion marker {}", staging.display()))?;
    if let Some(revision) = workspace_source_revision(&container.workspace_host, source_revision) {
        fs::write(
            staging.join(TARGET_SOURCE_REVISION_MARKER),
            format!("{revision}\n"),
        )
        .with_context(|| format!("write target source revision {}", staging.display()))?;
    }

    let _lock = CacheEntryLock::exclusive(store)?;
    fs::remove_dir_all(store).ok();
    if let Err(error) = fs::rename(&staging, store)
        .with_context(|| format!("publish persistent target {}", store.display()))
    {
        fs::remove_dir_all(&staging).ok();
        return Err(error);
    }
    Ok(())
}

fn persistent_target_results_publishable(results: &[StepExecutionResult]) -> bool {
    results
        .iter()
        .all(|result| result.skipped || result.exit_code == 0)
}

fn cache_staging_name(cache_file_name: &str) -> String {
    let unique = CACHE_STAGING_SEQ.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}.staging-{}-{}",
        cache_file_name,
        std::process::id(),
        unique
    )
}

/// Render a save outcome. Contention stays non-fatal (exit 0) like upstream,
/// but only a real `Persisted` outcome prints "saved"; every non-persisting
/// outcome (including contention, which names its phase) says so explicitly.
fn cache_save_step_result(
    key: &str,
    outcome: SaveOutcome,
    timing: SaveTiming,
) -> StepExecutionResult {
    let (result, stdout, stderr) = match &outcome {
        SaveOutcome::Persisted { paths } => (
            "saved".to_string(),
            format!(
                "{ANSI_GREEN}Saved cache '{key}' with {paths} path(s){ANSI_RESET} ({}ms)\n",
                timing.total_ms
            ),
            String::new(),
        ),
        SaveOutcome::ExactHit => (
            "exact hit — not saved".to_string(),
            format!("Cache hit occurred on primary key '{key}', not saving cache\n"),
            String::new(),
        ),
        SaveOutcome::AlreadyExists => (
            "already exists — not saved".to_string(),
            format!("Cache entry already exists for primary key '{key}', not saving cache\n"),
            String::new(),
        ),
        SaveOutcome::HostPersistent => (
            "host-persistent store — restore/save skipped".to_string(),
            format!(
                "{ANSI_GREEN}Cache paths live on Velnor host-persistent storage (always warm); nothing to save for key '{key}'{ANSI_RESET}\n"
            ),
            String::new(),
        ),
        SaveOutcome::NoPaths => (
            "no paths — not saved".to_string(),
            String::new(),
            format!("Cache not saved because no paths exist for key '{key}'\n"),
        ),
        SaveOutcome::EmptyKey => (
            "empty key — not saved".to_string(),
            String::new(),
            "Cache save skipped because key is empty\n".to_string(),
        ),
        SaveOutcome::Contended { phase, detail } => (
            format!("contention ({}) — not saved", phase.label()),
            String::new(),
            format!(
                "Cache save skipped after {} contention for key '{key}': {detail}\n",
                phase.label()
            ),
        ),
    };
    let summary = format!(
        "## Velnor cache report\n- Backend: actions/cache (native post)\n- Result: {result}\n- Lock wait: {} ms\n- Copy: {} ms\n- Total: {} ms\n",
        timing.lock_wait_ms, timing.copy_ms, timing.total_ms
    );
    StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            summary,
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout,
        stderr,
    }
}

#[derive(Clone, Copy)]
struct ArtifactUploadSourceLimits {
    files: usize,
    file_bytes: u64,
    total_bytes: u64,
    path_bytes: u64,
    path_depth: usize,
}

const RESULTS_ARTIFACT_UPLOAD_SOURCE_LIMITS: ArtifactUploadSourceLimits =
    ArtifactUploadSourceLimits {
        files: RESULTS_ARTIFACT_MAX_UPLOAD_FILES,
        file_bytes: RESULTS_ARTIFACT_MAX_UPLOAD_FILE_BYTES,
        total_bytes: RESULTS_ARTIFACT_MAX_UPLOAD_TOTAL_BYTES,
        path_bytes: RESULTS_ARTIFACT_MAX_UPLOAD_PATH_BYTES,
        path_depth: RESULTS_ARTIFACT_MAX_UPLOAD_PATH_DEPTH,
    };

#[derive(Debug)]
struct ArtifactUploadSource {
    path: PathBuf,
    root: Arc<crate::fs_copy::NoFollowDir>,
    relative: PathBuf,
    file_name: String,
    metadata_bytes: u64,
}

fn artifact_upload_sources(
    artifact_name: &str,
    artifact_dir: &Path,
    limits: ArtifactUploadSourceLimits,
) -> Result<Vec<ArtifactUploadSource>> {
    let mut sources = Vec::new();
    let mut total_bytes = 0_u64;
    let mut total_path_bytes = 0_u64;
    let root_directory = Arc::new(
        crate::fs_copy::NoFollowDir::open_trusted_configured_root(artifact_dir).with_context(
            || {
                format!(
                    "enumerate local files for Results Service artifact '{artifact_name}' in {}",
                    artifact_dir.display()
                )
            },
        )?,
    );
    collect_artifact_upload_sources(
        artifact_name,
        &root_directory,
        &root_directory,
        artifact_dir,
        Path::new(""),
        limits,
        &mut sources,
        &mut total_bytes,
        &mut total_path_bytes,
    )?;
    Ok(sources)
}

#[allow(clippy::too_many_arguments)]
fn collect_artifact_upload_sources(
    artifact_name: &str,
    root_directory: &Arc<crate::fs_copy::NoFollowDir>,
    current_directory: &crate::fs_copy::NoFollowDir,
    root: &Path,
    current_relative: &Path,
    limits: ArtifactUploadSourceLimits,
    sources: &mut Vec<ArtifactUploadSource>,
    total_bytes: &mut u64,
    total_path_bytes: &mut u64,
) -> Result<()> {
    current_directory.for_each_entry_filtered(
        |_| true,
        |entry| {
            let relative = current_relative.join(&entry.name);
            let path = root.join(&relative);
            match entry.source {
                crate::fs_copy::NoFollowSource::Directory(directory) => {
                    collect_artifact_upload_sources(
                        artifact_name,
                        root_directory,
                        &directory,
                        root,
                        &relative,
                        limits,
                        sources,
                        total_bytes,
                        total_path_bytes,
                    )?;
                    return Ok(());
                }
                crate::fs_copy::NoFollowSource::File(file) => {
                    let metadata = file.metadata().with_context(|| {
                        format!(
                            "read metadata for Results Service artifact '{artifact_name}' source {}",
                            path.display()
                        )
                    })?;
                    if !metadata.is_file() {
                        return Ok(());
                    }

                    let file_count = sources
                        .len()
                        .checked_add(1)
                        .context("Results Service artifact upload file count overflowed")?;
                    if file_count > limits.files {
                        bail!(
                            "Results Service artifact '{artifact_name}' contains {file_count} files, exceeding the {}-file upload limit; split it into artifacts with fewer files",
                            limits.files
                        );
                    }

                    let depth = relative.components().count();
                    if depth > limits.path_depth {
                        bail!(
                            "Results Service artifact '{artifact_name}' source {} has depth {depth}, exceeding the {}-component upload limit",
                            path.display(),
                            limits.path_depth
                        );
                    }

                    let file_bytes = metadata.len();
                    if file_bytes > limits.file_bytes {
                        bail!(
                            "Results Service artifact '{artifact_name}' source {} is {file_bytes} bytes, exceeding the {}-byte per-file upload limit; split or reduce this file",
                            path.display(),
                            limits.file_bytes
                        );
                    }
                    *total_bytes = total_bytes.checked_add(file_bytes).with_context(|| {
                        format!(
                            "Results Service artifact '{artifact_name}' source byte count overflowed while adding {}",
                            path.display()
                        )
                    })?;
                    if *total_bytes > limits.total_bytes {
                        bail!(
                            "Results Service artifact '{artifact_name}' sources total {total_bytes} bytes, exceeding the {}-byte upload limit; split them into smaller artifacts",
                            limits.total_bytes
                        );
                    }

                    let file_name = relative
                        .components()
                        .map(|component| match component {
                            std::path::Component::Normal(name) => name.to_str(),
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>()
                        .map(|components| components.join("/"))
                        .filter(|name| !name.is_empty())
                        .with_context(|| {
                            format!(
                                "Results Service artifact '{artifact_name}' source {} has a non-UTF-8 or unsafe relative file name",
                                path.display()
                            )
                        })?;
                    *total_path_bytes = total_path_bytes
                        .checked_add(u64::try_from(file_name.len()).unwrap_or(u64::MAX))
                        .context("Results Service artifact upload path byte count overflowed")?;
                    if *total_path_bytes > limits.path_bytes {
                        bail!(
                            "Results Service artifact '{artifact_name}' source paths use {total_path_bytes} bytes, exceeding the {}-byte upload limit",
                            limits.path_bytes
                        );
                    }
                    sources.push(ArtifactUploadSource {
                        path,
                        root: root_directory.clone(),
                        relative,
                        file_name,
                        metadata_bytes: file_bytes,
                    });
                }
            }
            Ok(())
        },
    )
}

fn read_artifact_upload_sources(
    artifact_name: &str,
    sources: Vec<ArtifactUploadSource>,
    limits: ArtifactUploadSourceLimits,
) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::with_capacity(sources.len());
    let mut total_bytes = 0_u64;
    for source in sources {
        let Some(crate::fs_copy::NoFollowSource::File(mut file)) =
            source.root.open_source(&source.relative).with_context(|| {
                format!(
                    "open Results Service artifact '{artifact_name}' source {}",
                    source.path.display()
                )
            })?
        else {
            bail!(
                "Results Service artifact '{artifact_name}' source {} is not a regular file",
                source.path.display()
            );
        };
        file.seek(SeekFrom::Start(0)).with_context(|| {
            format!(
                "rewind Results Service artifact '{artifact_name}' source {}",
                source.path.display()
            )
        })?;
        let mut content = Vec::new();
        file.read_to_end(&mut content).with_context(|| {
            format!(
                "read Results Service artifact '{artifact_name}' source {}",
                source.path.display()
            )
        })?;
        let actual_bytes = u64::try_from(content.len()).with_context(|| {
            format!(
                "Results Service artifact '{artifact_name}' source {} size does not fit in u64",
                source.path.display()
            )
        })?;
        if actual_bytes > limits.file_bytes {
            bail!(
                "Results Service artifact '{artifact_name}' source {} grew from {} to {actual_bytes} bytes after admission, exceeding the {}-byte per-file upload limit; retry after writes finish or reduce this file",
                source.path.display(),
                source.metadata_bytes,
                limits.file_bytes
            );
        }
        total_bytes = total_bytes.checked_add(actual_bytes).with_context(|| {
            format!(
                "Results Service artifact '{artifact_name}' source byte count overflowed while reading {}",
                source.path.display()
            )
        })?;
        if total_bytes > limits.total_bytes {
            bail!(
                "Results Service artifact '{artifact_name}' sources grew after admission to {total_bytes} bytes, exceeding the {}-byte upload limit; retry after writes finish or split them into smaller artifacts",
                limits.total_bytes
            );
        }
        files.push((source.file_name, content));
    }
    Ok(files)
}

fn native_upload_artifact(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let name = native_input_or(&action_state, action, "name", "artifact");
    let path_input = native_input(action, &action_state, "path");
    let if_no_files_found =
        native_input_or(&action_state, action, "if-no-files-found", "warn").to_ascii_lowercase();
    let include_hidden_files =
        input_truthy(&native_input(action, &action_state, "include-hidden-files"));
    let overwrite = input_truthy(&native_input(action, &action_state, "overwrite"));
    // The strict manifest admits only the estate-approved v4 value `0`.
    // upload-artifact defines it as no compression (ZIP Stored).
    let store_uncompressed =
        artifact_store_uncompressed(&native_input(action, &action_state, "compression-level"));
    let retention_days = artifact_retention_days(
        &native_input(action, &action_state, "retention-days"),
        action_state
            .env
            .get("GITHUB_RETENTION_DAYS")
            .map(String::as_str),
    );
    let paths = artifact_paths(&path_input);
    let validated_paths = paths
        .iter()
        .map(|path| validate_artifact_input_path(path))
        .collect::<Result<Vec<_>>>()?;
    let mut sources = Vec::new();
    for path in validated_paths {
        sources.extend(resolve_artifact_sources(state, path)?);
    }
    sources.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.artifact_path.cmp(&right.artifact_path))
    });
    sources.dedup();

    let artifact_dir = artifact_store_dir(state)?.join(sanitize_artifact_name(&name));
    if artifact_dir.exists() {
        // Always overwrite in Velnor: re-runs on the same slot reuse the artifact store,
        // and the latest run's content is what compare-results should see.
        let _ = overwrite; // silence unused warning; we always overwrite
        fs::remove_dir_all(&artifact_dir).ok();
    }
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("create artifact directory {}", artifact_dir.display()))?;
    let artifact_destination =
        crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
            &artifact_dir,
            Path::new(""),
        )
        .with_context(|| {
            format!(
                "securely open artifact destination without following symlinks: {}",
                artifact_dir.display()
            )
        })?;

    let mut copy_budget = ArtifactCopyBudget::default();
    let mut uploaded_count = 0_usize;
    for source in sources {
        if !include_hidden_files && artifact_source_is_hidden(state, &source.path) {
            continue;
        }
        copy_artifact_source(
            &source,
            &artifact_destination,
            include_hidden_files,
            &mut copy_budget,
        )?;
        uploaded_count = uploaded_count
            .checked_add(1)
            .context("uploaded artifact path count overflowed")?;
    }

    if uploaded_count == 0 {
        fs::remove_dir_all(&artifact_dir).ok();
        let message = format!("No files were found with the provided path: {path_input}\n");
        return Ok(StepExecutionResult {
            exit_code: if if_no_files_found == "error" { 1 } else { 0 },
            state: StepCommandState::default(),
            skipped: false,
            failure_ignored: false,
            stdout: if if_no_files_found == "ignore" {
                String::new()
            } else {
                message.clone()
            },
            stderr: if if_no_files_found == "error" {
                message
            } else {
                String::new()
            },
        });
    }

    let mut artifact_id = artifact_id_for_name(state, &name);
    let mut digest = hash_artifact_dir(&artifact_dir)?;
    let results_url = action_state
        .env
        .get("ACTIONS_RESULTS_URL")
        .cloned()
        .unwrap_or_else(|| "https://results.actions".to_string());

    // Also upload to GitHub's Results Service artifact store so that GitHub-hosted
    // jobs (like compare/aggregate jobs) can download the artifact via
    // `actions/download-artifact`. Without this, only same-host Velnor jobs can
    // access local artifacts.
    if let Some(runtime_token) = action_state.env.get("ACTIONS_RUNTIME_TOKEN")
        && let Some((plan_id, job_id)) = artifact_backend_ids_from_token(runtime_token)
    {
        // Pass artifact-store paths directly. The Results Service upload
        // streams each source file and never materializes the artifact in RAM.
        let zip_files =
            artifact_upload_sources(&name, &artifact_dir, RESULTS_ARTIFACT_UPLOAD_SOURCE_LIMITS)?
                .into_iter()
                .map(|source| crate::protocol::ArtifactUploadFile {
                    archive_path: source.file_name,
                    source: crate::protocol::ArtifactUploadSource::Relative {
                        root: source.root,
                        relative: source.relative,
                    },
                    source_path: source.path,
                })
                .collect::<Vec<_>>();
        if !zip_files.is_empty() {
            match crate::protocol::upload_artifact_files_blocking(
                &results_url,
                runtime_token,
                &plan_id,
                &job_id,
                &name,
                zip_files,
                crate::protocol::ArtifactUploadOptions {
                    store_uncompressed,
                    retention_days,
                    overwrite,
                },
            ) {
                Ok(finalized) => {
                    artifact_id = finalized.id;
                    digest = finalized.digest;
                }
                Err(e) => {
                    let message =
                        format!("Results Service artifact upload failed for '{name}': {e:#}\n");
                    eprintln!("{message}");
                    return Ok(StepExecutionResult {
                        exit_code: 1,
                        state: StepCommandState::default(),
                        skipped: false,
                        failure_ignored: false,
                        stdout: format!(
                            "Saved local artifact '{name}' with {} path(s)\n",
                            uploaded_count
                        ),
                        stderr: message,
                    });
                }
            }
        }
    }

    let artifact_url = format!(
        "{}/artifacts/{artifact_id}",
        results_url.trim_end_matches('/')
    );
    let mut outputs = BTreeMap::new();
    outputs.insert("artifact-id".to_string(), artifact_id);
    outputs.insert("artifact-url".to_string(), artifact_url);
    outputs.insert("artifact-digest".to_string(), digest);

    Ok(StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            outputs,
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout: format!(
            "Uploaded artifact '{name}' with {} path(s)\n",
            uploaded_count
        ),
        stderr: String::new(),
    })
}

fn artifact_store_uncompressed(compression_level: &str) -> bool {
    compression_level.trim() == "0"
}

fn artifact_retention_days(input: &str, repository_max: Option<&str>) -> Option<u8> {
    let requested = input.trim().parse::<u8>().ok()?;
    let maximum = repository_max.and_then(|value| value.trim().parse::<u8>().ok());
    Some(maximum.map_or(requested, |limit| requested.min(limit)))
}

fn native_download_artifact(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let name = native_input(action, &action_state, "name");
    let pattern = native_input(action, &action_state, "pattern");
    let destination_input = native_input_or(&action_state, action, "path", ".");
    let merge_multiple = input_truthy(&native_input(action, &action_state, "merge-multiple"));
    let destination = resolve_host_path(state, &destination_input)
        .ok_or_else(|| anyhow::anyhow!("download-artifact requires a workspace or temp path"))?;
    let destination_scope = trusted_job_destination(state, &destination)?;
    let destination_root = destination_scope
        .open_relative_directory(Path::new(""))
        .with_context(|| {
            format!(
                "securely open artifact download dir {}",
                destination.display()
            )
        })?;

    let downloaded_count = if let Some(runtime_token) = action_state
        .env
        .get("ACTIONS_RUNTIME_TOKEN")
        .filter(|token| !token.is_empty())
    {
        let results_url = action_state
            .env
            .get("ACTIONS_RESULTS_URL")
            .filter(|url| !url.is_empty())
            .context("download-artifact requires ACTIONS_RESULTS_URL")?;
        let (plan_id, job_id) = artifact_backend_ids_from_token(runtime_token)
            .context("download-artifact runtime token is missing workflow backend IDs")?;
        // Name/pattern filtering happens inside the daemon download: artifacts
        // that were not requested are never signed or fetched (non-zip
        // `.dockerbuild` build records must not fail unrelated downloads).
        let selected = crate::protocol::download_artifacts_blocking(
            results_url,
            runtime_token,
            &plan_id,
            &job_id,
            &name,
            &pattern,
        )?;
        for artifact in &selected {
            let target_relative = if merge_multiple || !name.is_empty() {
                PathBuf::new()
            } else {
                PathBuf::from(&artifact.name)
            };
            for file in &artifact.files {
                let output_relative = target_relative.join(&file.relative_path);
                let input = file.file().with_context(|| {
                    format!("open downloaded artifact {}", file.relative_path.display())
                })?;
                copy_opened_artifact_download_file(
                    &input,
                    &destination_root,
                    &destination_scope,
                    &output_relative,
                )?;
            }
        }
        selected.len()
    } else {
        // Unit/offline fallback. Product jobs always carry Results Service
        // credentials and therefore use the host-independent v4 path above.
        let store = artifact_store_dir(state)?;
        let artifacts = matching_artifacts(&store, &name, &pattern)?;
        for (artifact_name, artifact_dir) in &artifacts {
            let target_relative = if merge_multiple || !name.is_empty() {
                PathBuf::new()
            } else {
                PathBuf::from(artifact_name)
            };
            let source =
                crate::fs_copy::NoFollowDir::open_absolute(artifact_dir).with_context(|| {
                    format!(
                        "securely open artifact download source {}",
                        artifact_dir.display()
                    )
                })?;
            let target_directory = destination_root
                .open_relative_directory(&target_relative)
                .with_context(|| {
                    format!(
                        "create artifact download directory {}",
                        target_relative.display()
                    )
                })?;
            target_directory.set_mode(0o755)?;
            copy_artifact_download_contents(
                &source,
                artifact_dir,
                &target_directory,
                Path::new(""),
            )?;
        }
        artifacts.len()
    };

    let mut outputs = BTreeMap::new();
    outputs.insert(
        "download-path".to_string(),
        resolve_container_path(state, &destination_input),
    );

    Ok(StepExecutionResult {
        exit_code: if downloaded_count == 0 { 1 } else { 0 },
        state: StepCommandState {
            outputs,
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout: format!("Downloaded {downloaded_count} artifact(s)\n"),
        stderr: if downloaded_count == 0 {
            "No artifacts matched the requested name or pattern\n".to_string()
        } else {
            String::new()
        },
    })
}

fn native_upload_pages_artifact(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let source_input = native_input_or(&action_state, action, "path", "_site/");
    let source = resolve_host_path(state, &source_input)
        .context("actions/upload-pages-artifact requires a workspace path")?;
    let workspace = state
        .workspace_host
        .as_deref()
        .context("actions/upload-pages-artifact requires a workspace path")?;
    let workspace = fs::canonicalize(workspace)
        .with_context(|| format!("resolve Pages workspace root {}", workspace.display()))?;
    let source = fs::canonicalize(&source)
        .with_context(|| format!("resolve Pages artifact directory {}", source.display()))?;
    if !source.starts_with(&workspace) {
        bail!(
            "actions/upload-pages-artifact path '{}' resolves outside the admitted workspace",
            source_input
        );
    }
    if !source.is_dir() {
        bail!(
            "actions/upload-pages-artifact path '{}' is not a directory",
            source_input
        );
    }
    let runner_temp = action_state
        .env
        .get("RUNNER_TEMP")
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .unwrap_or("/__t");
    let temp_host = state
        .temp_host
        .as_deref()
        .context("actions/upload-pages-artifact requires RUNNER_TEMP")?;
    create_pages_archive_from_canonical_source(&source, temp_host, Path::new("artifact.tar"))?;

    let mut page_action = action.clone();
    page_action.inputs.insert(
        "name".to_string(),
        native_input_or(&action_state, action, "name", "github-pages"),
    );
    page_action
        .inputs
        .insert("path".to_string(), format!("{runner_temp}/artifact.tar"));
    page_action
        .inputs
        .insert("if-no-files-found".to_string(), "error".to_string());
    let result = native_upload_artifact(&page_action, state)?;
    let mut outputs = result.state.outputs.clone();
    if let Some(artifact_id) = outputs.get("artifact-id").cloned() {
        outputs.insert("artifact_id".to_string(), artifact_id);
    }
    Ok(StepExecutionResult {
        state: StepCommandState {
            outputs,
            ..result.state
        },
        ..result
    })
}

fn create_pages_archive(
    source: &Path,
    trusted_temp_root: &Path,
    archive_relative: &Path,
) -> Result<()> {
    let canonical_source = fs::canonicalize(source)
        .with_context(|| format!("resolve Pages artifact directory {}", source.display()))?;
    create_pages_archive_from_canonical_source(
        &canonical_source,
        trusted_temp_root,
        archive_relative,
    )
}

fn create_pages_archive_from_canonical_source(
    canonical_source: &Path,
    trusted_temp_root: &Path,
    archive_relative: &Path,
) -> Result<()> {
    let archive_relative = normalized_destination_relative(archive_relative)?;
    let archive = trusted_temp_root.join(&archive_relative);
    let parent_relative = archive_relative
        .parent()
        .with_context(|| format!("Pages archive has no parent: {}", archive.display()))?;
    let file_name = archive_relative
        .file_name()
        .with_context(|| format!("Pages archive has no file name: {}", archive.display()))?;
    let destination = crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
        trusted_temp_root,
        parent_relative,
    )
    .with_context(|| {
        format!(
            "create Pages archive directory {}",
            trusted_temp_root.join(parent_relative).display()
        )
    })?;
    let parent = trusted_temp_root.join(parent_relative);
    let (file, staging_path) = create_pages_archive_staging_file(&parent, &destination)?;
    let mut builder = tar::Builder::new(BoundedPagesWriter::new(
        file,
        PAGES_ARCHIVE_MAX_ARCHIVE_BYTES,
    ));
    if !canonical_source.is_dir() {
        bail!(
            "Pages artifact path is not a directory: {}",
            canonical_source.display()
        );
    }
    let source_directory = crate::fs_copy::NoFollowDir::open_absolute(canonical_source)
        .with_context(|| {
            format!(
                "securely open Pages artifact directory without following links: {}",
                canonical_source.display()
            )
        })?;
    let mut bounds = PagesArchiveBounds::default();
    record_pages_archive_entry(&mut bounds, Path::new("."), 0)?;
    append_pages_directory_header(&mut builder, Path::new("."))?;
    let mut active_directories = BTreeSet::new();
    active_directories.insert(canonical_source.to_path_buf());
    append_pages_archive_dir(
        &mut builder,
        canonical_source,
        &source_directory,
        &source_directory,
        canonical_source,
        Path::new(""),
        &mut active_directories,
        &mut bounds,
    )?;
    builder
        .finish()
        .with_context(|| format!("finish Pages archive {}", archive.display()))?;
    let mut writer = builder
        .into_inner()
        .with_context(|| format!("close Pages archive writer {}", archive.display()))?;
    writer
        .flush()
        .with_context(|| format!("flush Pages archive {}", archive.display()))?;
    let (file, archive_size) = writer.into_parts();
    file.set_len(archive_size)
        .with_context(|| format!("truncate Pages archive {}", archive.display()))?;
    destination
        .publish_temporary_file(&staging_path.name, file_name)
        .with_context(|| format!("publish Pages archive {}", archive.display()))?;
    Ok(())
}

#[derive(Debug, Default)]
struct PagesArchiveBounds {
    members: usize,
    path_bytes: u64,
    total_bytes: u64,
}

struct BoundedPagesWriter<W> {
    inner: W,
    written: u64,
    limit: u64,
}

impl<W> BoundedPagesWriter<W> {
    fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            written: 0,
            limit,
        }
    }

    fn into_parts(self) -> (W, u64) {
        (self.inner, self.written)
    }
}

impl<W: Write> Write for BoundedPagesWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let end = self
            .written
            .checked_add(u64::try_from(buffer.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| std::io::Error::other("Pages archive byte count overflowed"))?;
        if end > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                format!("Pages archive exceeds the {}-byte limit", self.limit),
            ));
        }
        let written = self.inner.write(buffer)?;
        self.written = self
            .written
            .checked_add(u64::try_from(written).unwrap_or(u64::MAX))
            .ok_or_else(|| std::io::Error::other("Pages archive byte count overflowed"))?;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn record_pages_archive_entry(
    bounds: &mut PagesArchiveBounds,
    path: &Path,
    file_bytes: u64,
) -> Result<()> {
    bounds.members = bounds
        .members
        .checked_add(1)
        .context("Pages archive member count overflowed")?;
    if bounds.members > PAGES_ARCHIVE_MAX_MEMBERS {
        bail!(
            "Pages archive contains {} members, exceeding the {}-member limit",
            bounds.members,
            PAGES_ARCHIVE_MAX_MEMBERS
        );
    }
    let depth = path.components().count();
    if depth > PAGES_ARCHIVE_MAX_PATH_DEPTH {
        bail!(
            "Pages archive path {} has depth {depth}, exceeding the {}-component limit",
            path.display(),
            PAGES_ARCHIVE_MAX_PATH_DEPTH
        );
    }
    bounds.path_bytes = bounds
        .path_bytes
        .checked_add(u64::try_from(path.as_os_str().as_encoded_bytes().len()).unwrap_or(u64::MAX))
        .context("Pages archive path byte count overflowed")?;
    if bounds.path_bytes > PAGES_ARCHIVE_MAX_PATH_BYTES {
        bail!(
            "Pages archive paths use {} bytes, exceeding the {}-byte limit",
            bounds.path_bytes,
            PAGES_ARCHIVE_MAX_PATH_BYTES
        );
    }
    if file_bytes > PAGES_ARCHIVE_MAX_FILE_BYTES {
        bail!(
            "Pages archive file {} is {file_bytes} bytes, exceeding the {}-byte limit",
            path.display(),
            PAGES_ARCHIVE_MAX_FILE_BYTES
        );
    }
    bounds.total_bytes = bounds
        .total_bytes
        .checked_add(file_bytes)
        .context("Pages archive payload byte count overflowed")?;
    if bounds.total_bytes > PAGES_ARCHIVE_MAX_TOTAL_BYTES {
        bail!(
            "Pages archive payload uses {} bytes, exceeding the {}-byte limit",
            bounds.total_bytes,
            PAGES_ARCHIVE_MAX_TOTAL_BYTES
        );
    }
    Ok(())
}

#[derive(Debug)]
struct PagesArchiveStagingPath {
    parent: crate::fs_copy::NoFollowDestinationDir,
    name: OsString,
}

impl Drop for PagesArchiveStagingPath {
    fn drop(&mut self) {
        let _ = self.parent.remove_tree_entry(&self.name);
    }
}

#[derive(Debug)]
struct RepositoryArtifactStagingDirectory {
    parent: crate::fs_copy::NoFollowDestinationDir,
    name: OsString,
}

impl Drop for RepositoryArtifactStagingDirectory {
    fn drop(&mut self) {
        let _ = self.parent.remove_tree_entry(&self.name);
    }
}

#[derive(Debug)]
struct PrivateArtifactTempDirectory {
    parent: crate::fs_copy::NoFollowDestinationDir,
    root: crate::fs_copy::NoFollowDestinationDir,
    name: OsString,
    path: PathBuf,
}

impl Drop for PrivateArtifactTempDirectory {
    fn drop(&mut self) {
        let _ = self.parent.remove_tree_entry(&self.name);
    }
}

fn create_private_artifact_temp_directory(
    temp_host: &Path,
) -> Result<PrivateArtifactTempDirectory> {
    let parent_path = temp_host
        .parent()
        .context("runner temp directory has no parent for private artifact staging")?;
    let parent = crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
        parent_path,
        Path::new(""),
    )
    .with_context(|| {
        format!(
            "securely open runner temp parent for private artifact staging {}",
            parent_path.display()
        )
    })?;
    let cleanup_parent = parent
        .open_relative_directory(Path::new(""))
        .context("duplicate runner temp parent for private artifact staging cleanup")?;
    let (root, name) = parent
        .create_unique_directory(".velnor-private-artifact")
        .context("create private artifact staging directory")?;
    let path = parent_path.join(&name);
    Ok(PrivateArtifactTempDirectory {
        parent: cleanup_parent,
        root,
        name,
        path,
    })
}

fn create_repository_artifact_staging_directory(
    staging_parent: &Path,
    staging_parent_root: &crate::fs_copy::NoFollowDestinationDir,
) -> Result<(
    RepositoryArtifactStagingDirectory,
    crate::fs_copy::NoFollowDestinationDir,
)> {
    let cleanup_parent = staging_parent_root
        .open_relative_directory(Path::new(""))
        .with_context(|| {
            format!(
                "duplicate prepared CI artifact staging parent {}",
                staging_parent.display()
            )
        })?;
    let (staging_root, staging_name) = staging_parent_root
        .create_unique_directory(".velnor-repository-artifact")
        .with_context(|| {
            format!(
                "create prepared CI artifact staging directory in {}",
                staging_parent.display()
            )
        })?;
    Ok((
        RepositoryArtifactStagingDirectory {
            parent: cleanup_parent,
            name: staging_name,
        },
        staging_root,
    ))
}

fn create_pages_archive_staging_file(
    staging_directory: &Path,
    staging_parent: &crate::fs_copy::NoFollowDestinationDir,
) -> Result<(fs::File, PagesArchiveStagingPath)> {
    let cleanup_parent = staging_parent
        .open_relative_directory(Path::new(""))
        .with_context(|| {
            format!(
                "duplicate Pages archive staging parent {}",
                staging_directory.display()
            )
        })?;
    let (file, name) = staging_parent
        .create_temporary_file(".velnor-pages-archive")
        .with_context(|| {
            format!(
                "create Pages archive staging file in {}",
                staging_directory.display()
            )
        })?;
    Ok((
        file,
        PagesArchiveStagingPath {
            parent: cleanup_parent,
            name,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
fn append_pages_archive_dir<W: Write>(
    builder: &mut tar::Builder<W>,
    root: &Path,
    root_directory: &crate::fs_copy::NoFollowDir,
    current_directory: &crate::fs_copy::NoFollowDir,
    current: &Path,
    relative: &Path,
    active_directories: &mut BTreeSet<PathBuf>,
    bounds: &mut PagesArchiveBounds,
) -> Result<()> {
    let mut names = Vec::new();
    let mut enumerated_names = 0_usize;
    let mut enumerated_name_bytes = 0_u64;
    current_directory.for_each_entry_name(|name| {
        enumerated_names = enumerated_names
            .checked_add(1)
            .context("Pages directory entry count overflowed")?;
        if enumerated_names > PAGES_ARCHIVE_MAX_MEMBERS {
            bail!(
                "Pages directory contains more than the {}-entry traversal limit",
                PAGES_ARCHIVE_MAX_MEMBERS
            );
        }
        enumerated_name_bytes = enumerated_name_bytes
            .checked_add(u64::try_from(name.as_encoded_bytes().len()).unwrap_or(u64::MAX))
            .context("Pages directory entry-name byte count overflowed")?;
        if enumerated_name_bytes > PAGES_ARCHIVE_MAX_PATH_BYTES {
            bail!(
                "Pages directory entry names exceed the {}-byte traversal limit",
                PAGES_ARCHIVE_MAX_PATH_BYTES
            );
        }
        names.push(name);
        Ok(())
    })?;
    names.sort();
    for name in names {
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let child_relative = relative.join(&name);
        let child = current.join(&name);
        let is_symlink = fs::symlink_metadata(&child)
            .with_context(|| format!("inspect Pages artifact path {}", child.display()))?
            .file_type()
            .is_symlink();
        let (opened, canonical) = if is_symlink {
            let canonical = fs::canonicalize(&child)
                .with_context(|| format!("resolve Pages artifact path {}", child.display()))?;
            if !canonical.starts_with(root) {
                bail!(
                    "Pages artifact path resolves outside the admitted directory: {}",
                    child.display()
                );
            }
            let canonical_relative = canonical.strip_prefix(root).with_context(|| {
                format!(
                    "resolve Pages artifact path below root: {}",
                    canonical.display()
                )
            })?;
            (
                root_directory
                    .open_source(canonical_relative)?
                    .with_context(|| format!("open Pages artifact path {}", child.display()))?,
                Some(canonical),
            )
        } else {
            (
                current_directory
                    .open_source(Path::new(&name))?
                    .with_context(|| format!("open Pages artifact path {}", child.display()))?,
                None,
            )
        };
        match opened {
            crate::fs_copy::NoFollowSource::Directory(directory) => {
                if let Some(canonical) = &canonical
                    && !active_directories.insert(canonical.clone())
                {
                    bail!(
                        "Pages artifact directory contains a symlink cycle at {}",
                        child.display()
                    );
                }
                record_pages_archive_entry(bounds, &child_relative, 0)?;
                append_pages_directory_header(builder, &child_relative)?;
                append_pages_archive_dir(
                    builder,
                    root,
                    root_directory,
                    &directory,
                    &child,
                    &child_relative,
                    active_directories,
                    bounds,
                )?;
                if let Some(canonical) = canonical {
                    active_directories.remove(&canonical);
                }
            }
            crate::fs_copy::NoFollowSource::File(file) => {
                append_pages_file(builder, &child_relative, &file, bounds)?;
            }
        }
    }
    Ok(())
}

fn append_pages_directory_header<W: Write>(
    builder: &mut tar::Builder<W>,
    path: &Path,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(0o755);
    header.set_size(0);
    header.set_cksum();
    builder
        .append_data(&mut header, path, std::io::empty())
        .with_context(|| format!("archive Pages directory {}", path.display()))
}

fn append_pages_file<W: Write>(
    builder: &mut tar::Builder<W>,
    path: &Path,
    file: &fs::File,
    bounds: &mut PagesArchiveBounds,
) -> Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("read Pages artifact file {}", path.display()))?;
    if !metadata.is_file() {
        bail!("unsupported Pages artifact path {}", path.display());
    }
    record_pages_archive_entry(bounds, path, metadata.len())?;
    let mut input = file
        .try_clone()
        .with_context(|| format!("duplicate Pages artifact file {}", path.display()))?;
    input
        .seek(SeekFrom::Start(0))
        .with_context(|| format!("rewind Pages artifact file {}", path.display()))?;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    #[cfg(unix)]
    header.set_mode(metadata.permissions().mode());
    #[cfg(not(unix))]
    header.set_mode(0o644);
    header.set_size(metadata.len());
    header.set_cksum();
    builder
        .append_data(&mut header, path, &mut input)
        .with_context(|| format!("archive Pages file {}", path.display()))
}

fn native_attest_build_provenance(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let workspace = state
        .workspace_host
        .as_deref()
        .context("actions/attest-build-provenance requires a host workspace mapping")?;
    let runner_temp = state
        .temp_host
        .as_deref()
        .context("actions/attest-build-provenance requires RUNNER_TEMP")?;
    let oidc_url = required_immutable_env(state, "ACTIONS_ID_TOKEN_REQUEST_URL")?;
    let oidc_request_token = required_immutable_env(state, "ACTIONS_ID_TOKEN_REQUEST_TOKEN")?;
    let github_token = required_immutable_env(state, "GITHUB_TOKEN")?;
    let repository = required_immutable_env(state, "GITHUB_REPOSITORY")?;
    let api_url = trusted_github_api_base(state)?;
    let server_url = trusted_github_server_base(state)?;
    let visibility = action_state
        .resolve_context_data_expression("github.event.repository.visibility")
        .or_else(|| {
            action_state
                .immutable_env
                .get("GITHUB_REPOSITORY_VISIBILITY")
                .cloned()
        });
    let subject_path = action
        .inputs
        .get("subject-path")
        .context("actions/attest-build-provenance requires subject-path")?;

    let result =
        crate::attestation::attest_build_provenance(crate::attestation::AttestationRequest {
            workspace,
            subject_path,
            runner_temp,
            runner_temp_container: "/__t",
            oidc_url,
            oidc_request_token,
            github_token,
            api_url: &api_url,
            server_url: &server_url,
            repository,
            repository_visibility: visibility.as_deref(),
        })?;
    let mut outputs = BTreeMap::new();
    outputs.insert("bundle-path".to_string(), result.bundle_path.clone());
    outputs.insert("attestation-id".to_string(), result.attestation_id.clone());
    outputs.insert(
        "attestation-url".to_string(),
        result.attestation_url.clone(),
    );
    let summary = format!(
        "### Attestation Created\n\n- <a href=\"{}\">{}</a>\n",
        result.attestation_url, result.attestation_url
    );
    let instance = if result.public_good {
        "Public Good"
    } else {
        "GitHub"
    };
    Ok(StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            outputs,
            summary,
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout: format!(
            "Attestation created for {} subject(s)\nAttestation signed using {instance} Sigstore instance\nAttestation uploaded to repository\n{}\n",
            result.subjects.len(), result.attestation_url
        ),
        stderr: String::new(),
    })
}

fn native_configure_pages(
    _action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let repository = required_immutable_env(state, "GITHUB_REPOSITORY")?;
    let api_url = trusted_github_api_base(state)?;
    let token = required_immutable_env(state, "GITHUB_TOKEN")?;
    let endpoint = format!("{api_url}/repos/{repository}/pages");
    let response = reqwest::blocking::Client::new()
        .get(&endpoint)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .header("User-Agent", "velnor-runner")
        .send()
        .with_context(|| format!("get Pages site for {repository}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        bail!(
            "get Pages site for {repository} failed with HTTP {status}: {}",
            body.trim()
        );
    }
    let page: Value = response
        .json()
        .with_context(|| format!("decode Pages site for {repository}"))?;
    let html_url = page
        .get("html_url")
        .and_then(Value::as_str)
        .context("Pages response is missing html_url")?;
    let outputs = pages_site_outputs(html_url)?;
    let base_url = outputs["base_url"].clone();

    Ok(StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            outputs,
            env: [("GITHUB_PAGES".to_string(), "true".to_string())].into(),
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout: format!("Configured GitHub Pages at {base_url}\n"),
        stderr: String::new(),
    })
}

fn native_create_github_app_token(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let repository = required_immutable_env(state, "GITHUB_REPOSITORY")?;
    let (repository_owner, repository_name) = repository
        .split_once('/')
        .context("GITHUB_REPOSITORY must be owner/name")?;
    let owner = native_input_or(&action_state, action, "owner", repository_owner);
    let repositories = native_input_or(&action_state, action, "repositories", repository_name);
    if owner != repository_owner || repositories != repository_name {
        bail!("actions/create-github-app-token is restricted to current repository {repository}");
    }
    let api_url = trusted_github_api_base(state)?;
    if input_truthy(&native_input_or(
        &action_state,
        action,
        "skip-token-revoke",
        "false",
    )) {
        bail!("actions/create-github-app-token does not permit skip-token-revoke");
    }
    // v3 recommends client-id and retains app-id as a legacy alias. Resolve
    // the same pair here so admission and native execution cannot disagree.
    let client_id = native_input(action, &action_state, "client-id");
    let legacy_app_id = native_input(action, &action_state, "app-id");
    let app_id = github_app_identifier(client_id, legacy_app_id);
    let private_key = native_input(action, &action_state, "private-key");
    if app_id.trim().is_empty() || private_key.trim().is_empty() {
        bail!(
            "actions/create-github-app-token requires client-id (or legacy app-id) and private-key"
        );
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs();
    let claims = serde_json::json!({
        "iat": now.saturating_sub(60),
        "exp": now.saturating_add(540),
        "iss": app_id,
    });
    let jwt = jsonwebtoken::encode(
        &Header::new(Algorithm::RS256),
        &claims,
        &EncodingKey::from_rsa_pem(private_key.as_bytes())
            .context("parse GitHub App private key")?,
    )
    .context("sign GitHub App JWT")?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("build GitHub App API client")?;
    let installation_endpoint =
        format!("{api_url}/repos/{repository_owner}/{repository_name}/installation");
    let installation: Value = github_app_response(
        client
            .get(&installation_endpoint)
            .bearer_auth(&jwt)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .header("User-Agent", "velnor-runner")
            .send(),
        "resolve GitHub App installation",
    )?;
    let installation_id = installation
        .get("id")
        .and_then(Value::as_u64)
        .context("GitHub App installation response is missing id")?;
    let app_slug = installation
        .get("app_slug")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let token_endpoint = format!("{api_url}/app/installations/{installation_id}/access_tokens");
    let token_response: Value = github_app_response(
        client
            .post(&token_endpoint)
            .bearer_auth(&jwt)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .header("User-Agent", "velnor-runner")
            .json(&serde_json::json!({"repositories": [repository_name]}))
            .send(),
        "create GitHub App installation token",
    )?;
    let token = token_response
        .get("token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .context("GitHub App token response is missing token")?
        .to_string();
    let expires_at = token_response
        .get("expires_at")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(native_success_with_state(StepCommandState {
        outputs: [
            ("token".to_string(), token.clone()),
            ("installation-id".to_string(), installation_id.to_string()),
            ("app-slug".to_string(), app_slug.to_string()),
        ]
        .into(),
        state: [
            ("token".to_string(), token.clone()),
            ("expiresAt".to_string(), expires_at),
        ]
        .into(),
        masks: vec![token],
        ..StepCommandState::default()
    }))
}

fn github_app_identifier(client_id: String, legacy_app_id: String) -> String {
    if client_id.trim().is_empty() {
        legacy_app_id
    } else {
        client_id
    }
}

fn github_app_response(
    response: std::result::Result<reqwest::blocking::Response, reqwest::Error>,
    operation: &str,
) -> Result<Value> {
    let response = response.map_err(|error| {
        anyhow::anyhow!(
            "{operation}: {}",
            crate::protocol::redacted_reqwest_error(&error)
        )
    })?;
    let status = response.status();
    if !status.is_success() {
        bail!("{operation} failed with HTTP {status}");
    }
    response
        .json()
        .with_context(|| format!("decode response for {operation}"))
}

fn native_revoke_github_app_token(state: &JobExecutionState) -> Result<StepExecutionResult> {
    let token = state.env.get("STATE_token").cloned().unwrap_or_default();
    if token.is_empty() {
        return Ok(native_success_with_state(StepCommandState::default()));
    }
    let api_url = trusted_github_api_base(state)?;
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("build GitHub App revoke client")?
        .delete(format!("{api_url}/installation/token"))
        .bearer_auth(&token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .header("User-Agent", "velnor-runner")
        .send();
    let stderr = match response {
        Ok(response) if response.status().is_success() => String::new(),
        Ok(response) => format!(
            "Warning: failed to revoke GitHub App token: HTTP {}\n",
            response.status()
        ),
        Err(error) => format!(
            "Warning: failed to revoke GitHub App token: {}\n",
            crate::protocol::redacted_reqwest_error(&error)
        ),
    };
    Ok(StepExecutionResult {
        exit_code: 0,
        state: StepCommandState::default(),
        skipped: false,
        failure_ignored: false,
        stdout: "GitHub App token revoked\n".to_string(),
        stderr,
    })
}

fn pages_site_outputs(html_url: &str) -> Result<BTreeMap<String, String>> {
    let site_url = url::Url::parse(html_url).context("Pages html_url is invalid")?;
    let base_url = site_url.as_str().trim_end_matches('/').to_string();
    let base_path = site_url.path().trim_end_matches('/').to_string();
    Ok([
        ("base_url".to_string(), base_url),
        (
            "origin".to_string(),
            site_url.origin().ascii_serialization(),
        ),
        (
            "host".to_string(),
            site_url.host_str().unwrap_or_default().to_string(),
        ),
        ("base_path".to_string(), base_path),
    ]
    .into())
}

fn native_deploy_pages(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let repository = required_immutable_env(state, "GITHUB_REPOSITORY")?;
    let build_version = required_immutable_env(state, "GITHUB_SHA")?.to_owned();
    let github_token = required_immutable_env(state, "GITHUB_TOKEN")?;
    let runtime_token = required_immutable_env(state, "ACTIONS_RUNTIME_TOKEN")?;
    let results_url = required_immutable_env(state, "ACTIONS_RESULTS_URL")?;
    let oidc_url = required_immutable_env(state, "ACTIONS_ID_TOKEN_REQUEST_URL")?;
    let oidc_request_token = required_immutable_env(state, "ACTIONS_ID_TOKEN_REQUEST_TOKEN")?;
    let oidc_url = crate::protocol::validate_authenticated_url(oidc_url)?;
    let (plan_id, job_id) = artifact_backend_ids_from_token(runtime_token)
        .context("actions/deploy-pages runtime token is missing workflow backend IDs")?;
    let artifact_name = native_input_or(&action_state, action, "artifact_name", "github-pages");
    let api_url = trusted_github_api_base(state)?;
    let client = reqwest::blocking::Client::builder()
        .user_agent("velnor-runner")
        .timeout(Duration::from_secs(30))
        .build()
        .context("build Pages HTTP client")?;

    let artifact_id = crate::protocol::results_artifact_id_by_name_blocking(
        results_url,
        runtime_token,
        &plan_id,
        &job_id,
        &artifact_name,
    )?;

    let oidc: Value = pages_json_response(
        client
            .get(oidc_url.as_str())
            .bearer_auth(oidc_request_token)
            .send()
            .map_err(|error| {
                anyhow::anyhow!(
                    "request Pages OIDC token: {}",
                    crate::protocol::redacted_reqwest_error(&error)
                )
            })?,
        "request Pages OIDC token",
    )?;
    let oidc_token = oidc
        .get("value")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .context("Pages OIDC response is missing value")?;
    let preview = native_input_or(&action_state, action, "preview", "false") == "true";
    let mut payload = serde_json::json!({
        "artifact_id": artifact_id,
        "pages_build_version": build_version,
        "oidc_token": oidc_token
    });
    if preview {
        payload["preview"] = Value::Bool(true);
    }
    let deployment: Value = pages_json_response(
        client
            .post(format!("{api_url}/repos/{repository}/pages/deployments"))
            .bearer_auth(github_token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .json(&payload)
            .send()
            .map_err(|error| {
                anyhow::anyhow!(
                    "create Pages deployment: {}",
                    crate::protocol::redacted_reqwest_error(&error)
                )
            })?,
        "create Pages deployment",
    )?;
    let deployment_id = deployment
        .get("id")
        .and_then(json_scalar_string)
        .or_else(|| {
            deployment
                .get("status_url")
                .and_then(Value::as_str)
                .and_then(|url| url.rsplit('/').next())
                .map(str::to_string)
        })
        .unwrap_or_else(|| build_version.clone());
    let mut page_url = deployment
        .get("page_url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| pages_url_for_repository(repository));
    let requested_timeout = native_u64_input(action, &action_state, "timeout", 600_000);
    let timeout = if requested_timeout == 0 {
        600_000
    } else {
        requested_timeout.min(600_000)
    };
    let interval = native_u64_input(action, &action_state, "reporting_interval", 5_000);
    let max_errors = native_u64_input(action, &action_state, "error_count", 10).max(1);
    let started = std::time::Instant::now();
    let status_url = format!("{api_url}/repos/{repository}/pages/deployments/{deployment_id}");
    let mut errors = 0_u64;
    loop {
        if interval > 0 {
            std::thread::sleep(Duration::from_millis(interval));
        }
        let response = client
            .get(&status_url)
            .bearer_auth(github_token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .send();
        match response {
            Ok(response) if response.status().is_success() => {
                let status: Value = response.json().context("decode Pages deployment status")?;
                if let Some(url) = status.get("page_url").and_then(Value::as_str) {
                    page_url = url.to_string();
                }
                match status.get("status").and_then(Value::as_str).unwrap_or("") {
                    "succeed" => break,
                    "deployment_failed"
                    | "deployment_content_failed"
                    | "deployment_cancelled"
                    | "deployment_lost" => {
                        bail!(
                            "Pages deployment {deployment_id} failed with status {}",
                            status["status"]
                        )
                    }
                    _ => {}
                }
            }
            Ok(response) => {
                errors += 1;
                if errors >= max_errors {
                    cancel_pages_deployment(
                        &client,
                        &api_url,
                        repository,
                        &deployment_id,
                        github_token,
                    );
                    bail!(
                        "Pages deployment status failed with HTTP {}",
                        response.status()
                    );
                }
            }
            Err(error) => {
                errors += 1;
                if errors >= max_errors {
                    cancel_pages_deployment(
                        &client,
                        &api_url,
                        repository,
                        &deployment_id,
                        github_token,
                    );
                    return Err(anyhow::anyhow!(
                        "get Pages deployment status: {}",
                        crate::protocol::redacted_reqwest_error(&error)
                    ));
                }
            }
        }
        if started.elapsed() >= Duration::from_millis(timeout) {
            cancel_pages_deployment(&client, &api_url, repository, &deployment_id, github_token);
            bail!("Pages deployment {deployment_id} timed out after {timeout} ms");
        }
    }

    Ok(StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            outputs: [
                ("page_url".to_string(), page_url.clone()),
                ("status".to_string(), "succeed".to_string()),
            ]
            .into(),
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout: format!("Deployed Pages artifact '{artifact_name}' to {page_url}\n"),
        stderr: String::new(),
    })
}

fn pages_json_response(response: reqwest::blocking::Response, operation: &str) -> Result<Value> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        bail!("{operation} failed with HTTP {status}: {}", body.trim());
    }
    response
        .json()
        .with_context(|| format!("decode {operation} response"))
}

fn json_scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn native_u64_input(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
    name: &str,
    default: u64,
) -> u64 {
    native_input(action, state, name).parse().unwrap_or(default)
}

fn cancel_pages_deployment(
    client: &reqwest::blocking::Client,
    api_url: &str,
    repository: &str,
    deployment_id: &str,
    token: &str,
) {
    let _ = client
        .post(format!(
            "{api_url}/repos/{repository}/pages/deployments/{deployment_id}/cancel"
        ))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .send();
}

fn native_docker_metadata(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> StepExecutionResult {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let images = input_values(&native_input(action, &action_state, "images"));
    let tags_input = native_input(action, &action_state, "tags");
    let mut tags = Vec::new();
    for image in &images {
        for tag in docker_metadata_tags(&action_state, &tags_input) {
            tags.push(format!("{image}:{tag}"));
        }
    }
    let labels = docker_metadata_labels(&action_state).join("\n");
    let tags = tags.join("\n");
    let json = docker_metadata_json(&tags, &labels);
    StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            outputs: [
                ("tags".to_string(), tags.clone()),
                ("labels".to_string(), labels.clone()),
                ("json".to_string(), json.clone()),
            ]
            .into(),
            // Upstream metadata-action also exports DOCKER_METADATA_OUTPUT_*
            // env vars for subsequent steps (used by digest fan-in publish
            // flows: `jq ... <<< "$DOCKER_METADATA_OUTPUT_JSON"`).
            env: [
                ("DOCKER_METADATA_OUTPUT_TAGS".to_string(), tags.clone()),
                ("DOCKER_METADATA_OUTPUT_LABELS".to_string(), labels.clone()),
                ("DOCKER_METADATA_OUTPUT_JSON".to_string(), json),
            ]
            .into(),
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout: format!(
            "{ANSI_CYAN}{ANSI_BOLD}Generated Docker metadata{ANSI_RESET}\n{tags}\n{labels}\n"
        ),
        stderr: String::new(),
    }
}

fn native_github_runtime_export(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> StepExecutionResult {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let mut exported = BTreeMap::new();
    let mut stdout = String::new();
    for (name, value) in action_state.step_env(&[]) {
        if name.starts_with("ACTIONS_") {
            stdout.push_str(&format!("{name}={value}\n"));
            exported.insert(name, value);
        }
    }
    StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            env: exported,
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout,
        stderr: String::new(),
    }
}

fn native_github_script(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    const CONTRACT_OUTPUT: &str = "core.setOutput('docs-xtask', process.env.CONTRACT)";
    const PREPARED_RUNTIME: &str =
        "return await import(process.env.JACKIN_ACTION_RUNTIME).then(({ main }) => main())";
    let action_state = state.with_env(state.resolve_env(&action.env));
    let script = native_input(action, &action_state, "script");
    if script == CONTRACT_OUTPUT {
        let contract = action_state
            .env
            .get("CONTRACT")
            .cloned()
            .unwrap_or_default();
        return Ok(native_success_with_state(StepCommandState {
            outputs: [("docs-xtask".into(), contract)].into(),
            ..StepCommandState::default()
        }));
    }
    if script != PREPARED_RUNTIME {
        bail!("unsupported native github-script body: {script}");
    }
    native_restore_prepared_jackin_tools(&action_state, state)
}

fn native_success_with_state(command_state: StepCommandState) -> StepExecutionResult {
    StepExecutionResult {
        exit_code: 0,
        state: command_state,
        skipped: false,
        failure_ignored: false,
        stdout: String::new(),
        stderr: String::new(),
    }
}

fn native_restore_prepared_jackin_tools(
    action_state: &JobExecutionState,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    use std::os::unix::fs::PermissionsExt;

    let env = &action_state.env;
    let enabled = |name: &str, default: bool| {
        env.get(name)
            .map(|value| value == "true")
            .unwrap_or(default)
    };
    let include_tools = enabled("JACKIN_INCLUDE_TOOLS", false);
    let include_xtask = enabled("JACKIN_INCLUDE_XTASK", true);
    let allow_miss = enabled("JACKIN_ALLOW_MISS", false);
    let mut tools_hit = include_tools && native_cache_step_hit(state, "tools-cache");
    let mut xtask_hit = include_xtask && native_cache_step_hit(state, "xtask-cache");
    let workspace = state
        .workspace_host
        .clone()
        .context("native github-script requires a job workspace mapping")?;
    let tools_dir = workspace.join(".ci-prebuilt-tools");
    let xtask_dir = workspace.join(".ci-prebuilt-xtask");

    if tools_hit && !prepared_jackin_tools_complete(state, &tools_dir) {
        tools_hit = false;
    }

    if include_tools && !tools_hit {
        let artifact = format!(
            "ci-tools-{}-{}-{}",
            required_env(env, "JACKIN_RUNNER_OS")?,
            required_env(env, "JACKIN_RUNNER_ARCH")?,
            required_env(env, "JACKIN_TOOLS_CONTRACT")?
        );
        tools_hit = restore_repository_artifact(state, &artifact, &tools_dir, allow_miss)?;
        if tools_hit && !prepared_jackin_tools_complete(state, &tools_dir) {
            if !allow_miss {
                bail!("prepared CI tools artifact is incomplete: {artifact}");
            }
            tools_hit = false;
        }
    }
    if include_xtask && !xtask_hit {
        let prefix = format!(
            "ci-xtask-{}-{}-",
            required_env(env, "JACKIN_RUNNER_OS")?,
            required_env(env, "JACKIN_RUNNER_ARCH")?
        );
        let primary = format!("{prefix}{}", required_env(env, "JACKIN_XTASK_CONTRACT")?);
        xtask_hit = restore_repository_artifact(state, &primary, &xtask_dir, true)?;
        if !xtask_hit {
            if let Some(fallback) = env
                .get("JACKIN_FALLBACK_XTASK_CONTRACT")
                .filter(|value| !value.is_empty())
            {
                xtask_hit = restore_repository_artifact(
                    state,
                    &format!("{prefix}{fallback}"),
                    &xtask_dir,
                    allow_miss,
                )?;
            } else if !allow_miss {
                bail!("prepared CI xtask artifact not found: {primary}");
            }
        }
    }

    let mut command_state = StepCommandState::default();
    command_state
        .outputs
        .insert("tools-hit".into(), tools_hit.to_string());
    command_state
        .outputs
        .insert("xtask-hit".into(), xtask_hit.to_string());
    command_state
        .env
        .insert("CI_TOOLS_PATH".into(), to_container_path(state, &tools_dir));
    command_state
        .env
        .insert("CI_TOOLS_HIT".into(), tools_hit.to_string());
    if include_tools && tools_hit {
        let tools_root =
            trusted_job_destination(state, &tools_dir)?.open_relative_directory(Path::new(""))?;
        for tool in PREPARED_JACKIN_TOOL_NAMES {
            if let Some(file) = tools_root.open_relative_file_if_exists(Path::new(tool))? {
                file.set_permissions(fs::Permissions::from_mode(0o755))?;
            }
        }
        command_state
            .path
            .push(to_container_path(state, &tools_dir));
        let cargo_fuzz = tools_dir.join("cargo-fuzz");
        if tools_root
            .open_relative_file_if_exists(Path::new("cargo-fuzz"))?
            .is_some()
        {
            command_state.env.insert(
                "CI_CARGO_FUZZ".into(),
                to_container_path(state, &cargo_fuzz),
            );
        }
    }
    if include_xtask {
        command_state
            .env
            .insert("CI_XTASK_HIT".into(), xtask_hit.to_string());
        let xtask = if xtask_hit {
            let prepared = xtask_dir.join("jackin-xtask");
            let xtask_root = trusted_job_destination(state, &xtask_dir)?
                .open_relative_directory(Path::new(""))?;
            let file = xtask_root
                .open_relative_file_if_exists(Path::new("jackin-xtask"))?
                .with_context(|| format!("prepared CI xtask is missing: {}", prepared.display()))?;
            file.set_permissions(fs::Permissions::from_mode(0o755))?;
            prepared
        } else {
            workspace.join("target/debug/jackin-xtask")
        };
        command_state
            .env
            .insert("CI_XTASK".into(), to_container_path(state, &xtask));
        let metadata = xtask_dir.join("workspace-metadata.json");
        let metadata_present = if xtask_hit {
            trusted_job_destination(state, &xtask_dir)?
                .open_relative_directory(Path::new(""))?
                .open_relative_file_if_exists(Path::new("workspace-metadata.json"))?
                .is_some()
        } else {
            false
        };
        if metadata_present {
            command_state
                .env
                .insert("CI_METADATA".into(), to_container_path(state, &metadata));
        }
    }
    Ok(native_success_with_state(command_state))
}

const PREPARED_JACKIN_TOOL_NAMES: &[&str] = &[
    "sccache",
    "cargo-nextest",
    "cargo-deny",
    "cargo-shear",
    "cargo-audit",
    "cargo-dylint",
    "cargo-fuzz",
    "cargo-hack",
    "cargo-hakari",
    "cargo-llvm-cov",
    "cargo-mutants",
    "cargo-zigbuild",
    "dylint-link",
    "weaver",
];

fn prepared_jackin_tools_complete(state: &JobExecutionState, directory: &Path) -> bool {
    let Ok(root) = trusted_job_destination(state, directory)
        .and_then(|scope| scope.open_relative_directory(Path::new("")))
    else {
        return false;
    };
    PREPARED_JACKIN_TOOL_NAMES.iter().all(|tool| {
        root.open_relative_file_if_exists(Path::new(tool))
            .is_ok_and(|file| file.is_some())
    })
}

fn required_env<'a>(env: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str> {
    env.get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("native github-script requires {name}"))
}

fn required_immutable_env<'a>(state: &'a JobExecutionState, name: &str) -> Result<&'a str> {
    state
        .immutable_env
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("native artifact restore requires immutable {name}"))
}

fn workflow_path_from_ref(workflow_ref: &str) -> Result<&str> {
    let path = workflow_ref
        .split_once("/.github/workflows/")
        .and_then(|(_, suffix)| suffix.split_once('@').map(|(path, _)| path))
        .filter(|path| !path.is_empty() && !path.contains(".."))
        .context("immutable GITHUB_WORKFLOW_REF has no normalized workflow path")?;
    Ok(path)
}

fn native_cache_step_hit(state: &JobExecutionState, step_id: &str) -> bool {
    state
        .outputs
        .get(step_id)
        .and_then(|outputs| outputs.get("cache-hit"))
        .is_some_and(|value| value == "true")
}

/// Allows plain-HTTP loopback origins only for test-support builds so the
/// local fixture servers can stand in for the GitHub API.
fn trusted_base_url_allows_loopback(url: &url::Url) -> bool {
    cfg!(feature = "test-support")
        && url.scheme() == "http"
        && url
            .host_str()
            .is_some_and(|host| host == "127.0.0.1" || host == "localhost" || host == "::1")
}

fn trusted_github_api_base(state: &JobExecutionState) -> Result<String> {
    let raw = state
        .immutable_env
        .get("GITHUB_API_URL")
        .map(String::as_str)
        .unwrap_or("https://api.github.com")
        .trim_end_matches('/');
    let url =
        url::Url::parse(raw).with_context(|| format!("invalid immutable GitHub API URL {raw}"))?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.scheme() != "https" && !trusted_base_url_allows_loopback(&url))
    {
        bail!("immutable GitHub API URL must be a credential-free HTTPS origin: {raw}");
    }
    let server_host = state
        .immutable_env
        .get("GITHUB_SERVER_URL")
        .and_then(|server| url::Url::parse(server).ok())
        .and_then(|server| server.host_str().map(str::to_owned));
    let api_host = url.host_str().unwrap_or_default();
    if !trusted_base_url_allows_loopback(&url)
        && api_host != "api.github.com"
        && server_host.as_deref() != Some(api_host)
    {
        bail!("immutable GitHub API URL host is not bound to the GitHub server: {raw}");
    }
    Ok(raw.to_string())
}

fn trusted_github_server_base(state: &JobExecutionState) -> Result<String> {
    let raw = state
        .immutable_env
        .get("GITHUB_SERVER_URL")
        .map(String::as_str)
        .unwrap_or("https://github.com")
        .trim_end_matches('/');
    let url = url::Url::parse(raw)
        .with_context(|| format!("invalid immutable GitHub server URL {raw}"))?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || (url.path() != "/" && !trusted_base_url_allows_loopback(&url))
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.scheme() != "https" && !trusted_base_url_allows_loopback(&url))
    {
        bail!("immutable GitHub server URL must be a credential-free HTTPS origin: {raw}");
    }
    Ok(raw.to_string())
}

fn restore_repository_artifact(
    state: &JobExecutionState,
    name: &str,
    destination: &Path,
    allow_miss: bool,
) -> Result<bool> {
    let token = required_immutable_env(state, "GITHUB_TOKEN")?;
    let repository = required_immutable_env(state, "GITHUB_REPOSITORY")?;
    let current_repository_id = required_immutable_env(state, "GITHUB_REPOSITORY_ID")?;
    let current_sha = required_immutable_env(state, "GITHUB_SHA")?;
    let current_ref = required_immutable_env(state, "GITHUB_REF")?;
    let workflow_path =
        workflow_path_from_ref(required_immutable_env(state, "GITHUB_WORKFLOW_REF")?)?;
    let (owner, repo) = repository
        .split_once('/')
        .context("JACKIN_REPOSITORY must be owner/repo")?;
    let api = trusted_github_api_base(state)?;
    let client = reqwest::blocking::Client::builder()
        .user_agent("velnor-runner")
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let deadline = std::time::Instant::now()
        + if allow_miss {
            std::time::Duration::ZERO
        } else {
            std::time::Duration::from_secs(55)
        };
    loop {
        let list_url =
            format!("{api}/repos/{owner}/{repo}/actions/artifacts?name={name}&per_page=10");
        let body: serde_json::Value = serde_json::from_slice(&repository_artifact_response(
            || {
                client
                    .get(&list_url)
                    .bearer_auth(token)
                    .header("X-GitHub-Api-Version", "2026-03-10")
            },
            "list repository artifacts",
            PREPARED_ARTIFACT_MAX_CONTROL_RESPONSE_BYTES,
        )?)?;
        let mut trusted_artifact = None;
        if let Some(items) = body["artifacts"].as_array() {
            for item in items {
                if !repository_artifact_matches_trusted_producer(
                    item,
                    repository,
                    current_repository_id,
                    current_sha,
                    current_ref,
                ) {
                    continue;
                }
                let Some(run_id) = item["workflow_run"]["id"].as_u64() else {
                    continue;
                };
                let run_url = format!("{api}/repos/{owner}/{repo}/actions/runs/{run_id}");
                let run: serde_json::Value = serde_json::from_slice(&repository_artifact_response(
                    || {
                        client
                            .get(&run_url)
                            .bearer_auth(token)
                            .header("X-GitHub-Api-Version", "2026-03-10")
                    },
                    "inspect prepared CI artifact producer run",
                    PREPARED_ARTIFACT_MAX_CONTROL_RESPONSE_BYTES,
                )?)
                .context("parse prepared CI artifact producer run")?;
                if repository_artifact_run_is_trusted(
                    &run,
                    current_repository_id,
                    current_sha,
                    current_ref,
                    workflow_path,
                ) {
                    let artifact_id = item["id"].as_u64();
                    let digest = item["digest"]
                        .as_str()
                        .filter(|digest| digest.starts_with(PREPARED_ARTIFACT_SHA256_PREFIX))
                        .map(ToOwned::to_owned);
                    let Some((artifact_id, digest)) = artifact_id.zip(digest) else {
                        bail!("prepared CI artifact has no mandatory SHA-256 digest: {name}");
                    };
                    if trusted_artifact.replace((artifact_id, digest)).is_some() {
                        bail!(
                            "multiple trusted prepared CI artifacts matched the exact producer identity: {name}"
                        );
                    }
                }
            }
        }
        if let Some((id, expected_digest)) = trusted_artifact {
            let archive_url = format!("{api}/repos/{owner}/{repo}/actions/artifacts/{id}/zip");
            let temp_host = state
                .temp_host
                .as_deref()
                .context("prepared CI artifact restore requires a runner temp directory")?;
            let private_temp = create_private_artifact_temp_directory(temp_host)?;
            let archive = repository_artifact_archive_response(
                || {
                    client
                        .get(&archive_url)
                        .bearer_auth(token)
                        .header("X-GitHub-Api-Version", "2026-03-10")
                },
                "download repository artifact",
                PREPARED_ARTIFACT_MAX_RESPONSE_BYTES,
                &private_temp.root,
            )?;
            let actual_digest = sha256_opened_file_digest(&archive)?;
            if !github_digest_eq(&expected_digest, &actual_digest) {
                bail!(
                    "prepared CI artifact digest mismatch: expected {expected_digest}, received {actual_digest}"
                );
            }
            let mut validation_file = archive
                .try_clone()
                .context("duplicate downloaded repository artifact for metadata preflight")?;
            let validated_zip = crate::protocol::validate_zip_central_directory_for_restore(
                &mut validation_file,
                name,
            )?;
            let mut zip = zip::ZipArchive::new(archive)?;
            let Some((validated_entries, validated_offset)) = validated_zip else {
                bail!("prepared CI artifact is missing a valid ZIP central directory: {name}");
            };
            if u64::try_from(zip.len()).unwrap_or(u64::MAX) != validated_entries
                || zip.central_directory_start() != validated_offset
            {
                bail!(
                    "prepared CI artifact ZIP parser selected a different central directory: {name}"
                );
            }
            let entries = preflight_prepared_artifact_zip(&mut zip)?;
            let destination_scope = trusted_job_destination(state, destination)?;
            let destination_relative = destination_scope.relative.clone();
            let destination_name = destination_relative
                .file_name()
                .context("prepared CI artifact destination has no final directory name")?
                .to_os_string();
            let destination_parent_relative =
                destination_relative.parent().unwrap_or(Path::new(""));
            let destination_parent_path = destination_scope.root.join(destination_parent_relative);
            let destination_parent =
                crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
                    &destination_scope.root,
                    destination_parent_relative,
                )
                .with_context(|| {
                    format!(
                        "securely open prepared CI artifact destination parent {}",
                        destination_parent_path.display()
                    )
                })?;
            let (staging_directory, staging_root) = create_repository_artifact_staging_directory(
                &private_temp.path,
                &private_temp.root,
            )?;
            for (index, entry) in entries.into_iter().enumerate() {
                let mut file = zip.by_index(index)?;
                if entry.is_directory {
                    staging_root
                        .open_relative_directory(&entry.relative)
                        .with_context(|| {
                            format!(
                                "create prepared CI artifact directory {}",
                                entry.relative.display()
                            )
                        })?;
                } else {
                    staging_root
                        .write_file_from_reader(
                            &mut file,
                            &entry.relative,
                            entry.uncompressed_size,
                            0o644,
                        )
                        .with_context(|| {
                            format!(
                                "stage prepared CI artifact file {}",
                                entry.relative.display()
                            )
                        })?;
                }
            }
            let staging_name = staging_directory.name.as_os_str();
            destination_parent
                .publish_staged_directory_from(&private_temp.root, staging_name, &destination_name)
                .context("publish prepared CI artifact staging tree")?;
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            if allow_miss {
                return Ok(false);
            }
            bail!("prepared CI artifact not found: {name}");
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

#[derive(Debug)]
struct PreparedArtifactEntry {
    relative: PathBuf,
    is_directory: bool,
    uncompressed_size: u64,
}

fn preflight_prepared_artifact_zip<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<Vec<PreparedArtifactEntry>> {
    let member_count = archive.len();
    if member_count > PREPARED_ARTIFACT_MAX_ZIP_MEMBERS {
        bail!(
            "prepared CI artifact contains {member_count} members, exceeding the {}-member limit",
            PREPARED_ARTIFACT_MAX_ZIP_MEMBERS
        );
    }

    let mut entries = Vec::with_capacity(member_count);
    let mut path_bytes = 0_u64;
    let mut total_uncompressed = 0_u64;
    for index in 0..member_count {
        let file = archive
            .by_index(index)
            .with_context(|| format!("inspect prepared CI artifact archive member {index}"))?;
        let relative = file
            .enclosed_name()
            .context("prepared CI artifact archive contains an unsafe path")?
            .to_path_buf();
        if relative.as_os_str().is_empty() {
            bail!("prepared CI artifact archive contains an empty path");
        }
        let depth = relative.components().count();
        if depth > PREPARED_ARTIFACT_MAX_PATH_DEPTH {
            bail!(
                "prepared CI artifact path {} has depth {depth}, exceeding the {}-component limit",
                relative.display(),
                PREPARED_ARTIFACT_MAX_PATH_DEPTH
            );
        }
        path_bytes = path_bytes
            .checked_add(u64::try_from(file.name().len()).unwrap_or(u64::MAX))
            .context("prepared CI artifact path byte count overflowed")?;
        if path_bytes > PREPARED_ARTIFACT_MAX_ZIP_PATH_BYTES {
            bail!(
                "prepared CI artifact paths use {path_bytes} bytes, exceeding the {}-byte limit",
                PREPARED_ARTIFACT_MAX_ZIP_PATH_BYTES
            );
        }
        let is_directory = file.is_dir();
        let uncompressed_size = file.size();
        if uncompressed_size > PREPARED_ARTIFACT_MAX_ZIP_UNCOMPRESSED_BYTES {
            bail!(
                "prepared CI artifact member {} is {} bytes, exceeding the {}-byte limit",
                relative.display(),
                uncompressed_size,
                PREPARED_ARTIFACT_MAX_ZIP_UNCOMPRESSED_BYTES
            );
        }
        total_uncompressed = total_uncompressed
            .checked_add(uncompressed_size)
            .context("prepared CI artifact uncompressed size overflowed")?;
        if total_uncompressed > PREPARED_ARTIFACT_MAX_ZIP_UNCOMPRESSED_BYTES {
            bail!(
                "prepared CI artifact expands to {total_uncompressed} bytes, exceeding the {}-byte limit",
                PREPARED_ARTIFACT_MAX_ZIP_UNCOMPRESSED_BYTES
            );
        }
        entries.push(PreparedArtifactEntry {
            relative,
            is_directory,
            uncompressed_size,
        });
    }

    let mut ordered_entries: Vec<_> = entries.iter().collect();
    ordered_entries.sort_by(|left, right| left.relative.cmp(&right.relative));
    for index in 0..ordered_entries.len() {
        if index > 0 && ordered_entries[index - 1].relative == ordered_entries[index].relative {
            bail!(
                "prepared CI artifact archive contains duplicate path: {}",
                ordered_entries[index].relative.display()
            );
        }
        if ordered_entries[index].is_directory {
            continue;
        }
        for ancestor in ordered_entries[index].relative.ancestors().skip(1) {
            if let Ok(ancestor_index) =
                ordered_entries.binary_search_by(|entry| entry.relative.as_path().cmp(ancestor))
                && !ordered_entries[ancestor_index].is_directory
            {
                bail!(
                    "prepared CI artifact archive has a file ancestor conflict at {}",
                    ancestor.display()
                );
            }
        }
    }
    Ok(entries)
}

fn repository_artifact_archive_response(
    mut request: impl FnMut() -> reqwest::blocking::RequestBuilder,
    operation: &str,
    max_bytes: u64,
    temp_parent: &crate::fs_copy::NoFollowDestinationDir,
) -> Result<fs::File> {
    const MAX_ATTEMPTS: usize = 3;
    for attempt in 1..=MAX_ATTEMPTS {
        let response = match request()
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
        {
            Ok(response) => response,
            Err(error) if attempt < MAX_ATTEMPTS && repository_artifact_retryable(&error) => {
                std::thread::sleep(std::time::Duration::from_secs(attempt as u64));
                continue;
            }
            Err(error) => return Err(error).with_context(|| operation.to_string()),
        };
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes)
        {
            bail!("{operation} response exceeds the {max_bytes}-byte limit");
        }

        let mut file = temp_parent
            .create_unlinked_temporary_file(".velnor-repository-artifact-response")
            .with_context(|| format!("create {operation} response staging file"))?;
        let copied = std::io::copy(&mut response.take(max_bytes.saturating_add(1)), &mut file)
            .with_context(|| format!("read {operation} response"))?;
        if copied > max_bytes {
            bail!("{operation} response exceeds the {max_bytes}-byte limit");
        }
        file.seek(SeekFrom::Start(0))
            .with_context(|| format!("rewind {operation} response"))?;
        return Ok(file);
    }
    unreachable!("repository artifact retry loop always returns")
}

fn repository_artifact_response(
    mut request: impl FnMut() -> reqwest::blocking::RequestBuilder,
    operation: &str,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    const MAX_ATTEMPTS: usize = 3;
    for attempt in 1..=MAX_ATTEMPTS {
        let response = match request()
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
        {
            Ok(response) => response,
            Err(error) if attempt < MAX_ATTEMPTS && repository_artifact_retryable(&error) => {
                std::thread::sleep(std::time::Duration::from_secs(attempt as u64));
                continue;
            }
            Err(error) => return Err(error).with_context(|| operation.to_string()),
        };
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes)
        {
            bail!("{operation} response exceeds the {max_bytes}-byte limit");
        }
        let mut body = Vec::new();
        response
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut body)
            .with_context(|| format!("read {operation} response"))?;
        if u64::try_from(body.len()).unwrap_or(u64::MAX) > max_bytes {
            bail!("{operation} response exceeds the {max_bytes}-byte limit");
        }
        return Ok(body);
    }
    unreachable!("repository artifact retry loop always returns")
}

fn repository_artifact_retryable(error: &reqwest::Error) -> bool {
    error.is_timeout()
        || error.is_connect()
        || error.is_body()
        || error.status().is_some_and(|status| {
            status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
        })
}

fn repository_artifact_matches_trusted_producer(
    artifact: &serde_json::Value,
    repository: &str,
    current_repository_id: &str,
    current_sha: &str,
    current_ref: &str,
) -> bool {
    if artifact["expired"] == true {
        return false;
    }
    let Some(workflow_run) = artifact.get("workflow_run") else {
        return false;
    };
    if workflow_run
        .get("head_sha")
        .and_then(serde_json::Value::as_str)
        != Some(current_sha)
    {
        return false;
    }
    if !json_id_matches(workflow_run.get("repository_id"), current_repository_id)
        || !json_id_matches(
            workflow_run.get("head_repository_id"),
            current_repository_id,
        )
    {
        return false;
    }
    let repository_matches = |value: Option<&serde_json::Value>| {
        value
            .and_then(|value| value.get("full_name"))
            .and_then(serde_json::Value::as_str)
            .is_none_or(|full_name| github_string_eq(full_name, repository))
    };
    if !repository_matches(workflow_run.get("head_repository"))
        || !repository_matches(workflow_run.get("repository"))
    {
        return false;
    }
    let event = workflow_run
        .get("event")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if event.starts_with("pull_request") {
        return false;
    }
    if let Some(branch) = current_ref.strip_prefix("refs/heads/")
        && workflow_run
            .get("head_branch")
            .and_then(serde_json::Value::as_str)
            != Some(branch)
    {
        return false;
    }
    true
}

fn json_id_matches(value: Option<&serde_json::Value>, expected: &str) -> bool {
    match value {
        Some(serde_json::Value::String(value)) => value == expected,
        Some(serde_json::Value::Number(value)) => value.to_string() == expected,
        _ => false,
    }
}

fn repository_artifact_run_is_trusted(
    run: &serde_json::Value,
    current_repository_id: &str,
    current_sha: &str,
    current_ref: &str,
    expected_workflow_path: &str,
) -> bool {
    if run.get("status").and_then(serde_json::Value::as_str) != Some("completed")
        || run.get("conclusion").and_then(serde_json::Value::as_str) != Some("success")
        || run
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
        || run
            .get("event")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|event| event.starts_with("pull_request"))
        || run.get("path").and_then(serde_json::Value::as_str) != Some(expected_workflow_path)
        || !run
            .get("run_attempt")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|attempt| attempt > 0)
    {
        return false;
    }
    if !json_id_matches(run.get("repository_id"), current_repository_id)
        || !json_id_matches(run.get("head_repository_id"), current_repository_id)
        || run.get("head_sha").and_then(serde_json::Value::as_str) != Some(current_sha)
    {
        return false;
    }
    current_ref
        .strip_prefix("refs/heads/")
        .is_none_or(|branch| {
            run.get("head_branch").and_then(serde_json::Value::as_str) == Some(branch)
        })
}

fn to_container_path(state: &JobExecutionState, host: &Path) -> String {
    let workspace_host = state.workspace_host.as_deref().unwrap_or(host);
    let workspace_container = state
        .env
        .get("GITHUB_WORKSPACE")
        .map(String::as_str)
        .unwrap_or("/__w");
    host.strip_prefix(workspace_host)
        .map(|relative| Path::new(workspace_container).join(relative))
        .unwrap_or_else(|_| host.to_path_buf())
        .display()
        .to_string()
}

fn native_input(action: &NativeActionInvocation, state: &JobExecutionState, name: &str) -> String {
    action
        .inputs
        .get(name)
        .or_else(|| action.inputs.get(&name.to_ascii_lowercase()))
        .map(|value| state.resolve_expressions(value))
        .unwrap_or_default()
}

fn native_input_or(
    state: &JobExecutionState,
    action: &NativeActionInvocation,
    name: &str,
    default: &str,
) -> String {
    let value = native_input(action, state, name);
    if value.is_empty() {
        default.to_string()
    } else {
        value
    }
}

fn input_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes"
    )
}

fn artifact_store_dir(state: &JobExecutionState) -> Result<PathBuf> {
    let temp = state
        .temp_host
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("artifact actions require a temp directory"))?;
    let run_key = artifact_run_key(state);
    let run_root = shared_work_root(temp);
    Ok(run_root.join("_velnor_artifacts").join(run_key))
}

/// Resolve the store directory for a cache entry. Trust and repository remain
/// the outer boundary; `version` (a runtime-compatibility segment — see
/// `cache_scope_version`) is appended below `<trust>/caches/<repo>` so the same
/// visible key cannot cross an OS/arch/action-SHA/path boundary. The
/// restore-key prefix scan therefore runs inside the versioned directory.
fn cache_store_dir(state: &JobExecutionState, version: &str) -> Result<PathBuf> {
    let temp = state
        .temp_host
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("cache actions require a temp directory"))?;
    // Daemon-shared (across slots), not per-slot: cold slots must hit the
    // caches their siblings saved (see container::daemon_shared_root).
    let repository = state
        .resolve_context_expression("github.repository")
        .filter(|value| !value.is_empty());
    let Some(repository) = repository else {
        eprintln!(
            "forensics.lifecycle: persistent actions cache refused: missing github.repository"
        );
        return Ok(temp.join("_velnor/ephemeral/caches").join(version));
    };
    let root = crate::storage::cache_class_path(
        &crate::container::daemon_shared_root(shared_work_root(temp)),
        "caches",
        "_velnor_caches",
    );
    Ok(crate::storage::append_legacy_trust(
        root,
        &crate::github_adapter::cargo_target_trust_scope(),
    )
    .join(crate::container::sanitize_store_key(&repository))
    .join(version))
}

fn shared_work_root(temp: &Path) -> PathBuf {
    if temp.file_name().is_some_and(|name| name == "temp") {
        temp.parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| temp.to_path_buf())
    } else {
        temp.to_path_buf()
    }
}

fn artifact_run_key(state: &JobExecutionState) -> String {
    let run_id = state
        .env
        .get("GITHUB_RUN_ID")
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .unwrap_or("local");
    let attempt = state
        .env
        .get("GITHUB_RUN_ATTEMPT")
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .unwrap_or("1");
    sanitize_artifact_name(&format!("{run_id}-{attempt}"))
}

fn artifact_id_for_name(state: &JobExecutionState, name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(artifact_run_key(state).as_bytes());
    hasher.update(b"\0");
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let mut id_bytes = [0u8; 8];
    id_bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(id_bytes).to_string()
}

fn artifact_paths(paths: &str) -> Vec<String> {
    paths
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ResolvedArtifactSource {
    path: PathBuf,
    artifact_path: PathBuf,
}

#[derive(Clone, Copy)]
struct ValidatedArtifactPath<'a> {
    value: &'a str,
}

impl<'a> ValidatedArtifactPath<'a> {
    fn as_str(self) -> &'a str {
        self.value
    }
}

fn validate_artifact_input_path(path: &str) -> Result<ValidatedArtifactPath<'_>> {
    let path = path.trim();
    let has_glob = has_glob_pattern(path);
    if artifact_path_has_parent_component(path)
        || (has_glob && artifact_path_has_parent_component(&normalize_glob_pattern(path)))
    {
        bail!("artifact source path contains parent traversal: {path}");
    }

    let mapped_suffix = [
        "target/",
        "/__w/target/",
        "/github/workspace/target/",
        "/__w/",
        "/github/workspace/",
        "/__t/",
        "/github/runner_temp/",
        "/tmp/",
        "/github/home/",
    ]
    .into_iter()
    .find_map(|prefix| path.strip_prefix(prefix));
    if let Some(suffix) = mapped_suffix {
        let normalized_suffix = has_glob.then(|| normalize_glob_pattern(suffix));
        let suffix = normalized_suffix.as_deref().unwrap_or(suffix);
        if relative_mapped_suffix(suffix).is_none() {
            bail!("artifact source path has absolute mapped suffix: {path}");
        }
    }

    let mapped_absolute_root = matches!(
        path,
        "/__w/target"
            | "/github/workspace/target"
            | "/__w"
            | "/github/workspace"
            | "/__t"
            | "/github/runner_temp"
            | "/tmp"
            | "/github/home"
    ) || mapped_suffix.is_some();
    if Path::new(path).is_absolute() && !mapped_absolute_root {
        bail!("artifact source path has unmapped absolute root: {path}");
    }

    Ok(ValidatedArtifactPath { value: path })
}

fn resolve_artifact_sources(
    state: &JobExecutionState,
    path: ValidatedArtifactPath<'_>,
) -> Result<Vec<ResolvedArtifactSource>> {
    let path = path.as_str();
    let has_glob = has_glob_pattern(path);
    if !has_glob {
        let Some(source) = resolve_host_path(state, path) else {
            return Ok(Vec::new());
        };
        return Ok(if source.exists() {
            let artifact_path = if source.is_dir() {
                PathBuf::new()
            } else {
                PathBuf::from(source.file_name().with_context(|| {
                    format!("artifact source has no file name: {}", source.display())
                })?)
            };
            vec![ResolvedArtifactSource {
                path: source,
                artifact_path,
            }]
        } else {
            Vec::new()
        });
    }

    let Some((mapped_root, mapped_pattern)) = artifact_glob_base_and_pattern(state, path)? else {
        return Ok(Vec::new());
    };
    if !mapped_root.exists() {
        return Ok(Vec::new());
    }
    let mapped_directory =
        crate::fs_copy::NoFollowDir::open_absolute(&mapped_root).with_context(|| {
            format!(
                "securely open mapped artifact root for '{path}': {}",
                mapped_root.display()
            )
        })?;
    let (literal_base, pattern) = artifact_glob_literal_base_and_pattern(&mapped_pattern);
    let base = mapped_root.join(&literal_base);
    let Some(base_source) = mapped_directory.open_source(&literal_base)? else {
        return Ok(Vec::new());
    };
    let base_directory = match base_source {
        crate::fs_copy::NoFollowSource::Directory(directory) => directory,
        crate::fs_copy::NoFollowSource::File(_) => {
            bail!("artifact glob root is not a directory: {}", base.display())
        }
    };
    let matcher = Glob::new(&normalize_glob_pattern(&pattern))?.compile_matcher();
    let mut matches = Vec::new();
    collect_artifact_glob_matches(
        &base_directory,
        &base,
        Path::new(""),
        &matcher,
        &mut matches,
    )?;
    matches.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(matches)
}

fn artifact_path_has_parent_component(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn has_glob_pattern(path: &str) -> bool {
    path.chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
}

fn artifact_glob_base_and_pattern(
    state: &JobExecutionState,
    path: &str,
) -> Result<Option<(PathBuf, String)>> {
    let path = path.trim();
    if artifact_path_has_parent_component(path)
        || artifact_path_has_parent_component(&normalize_glob_pattern(path))
    {
        bail!("artifact source path contains parent traversal: {path}");
    }
    if path == "target" || path == "/__w/target" || path == "/github/workspace/target" {
        return Ok(state
            .cargo_target_host
            .as_ref()
            .map(|base| (base.clone(), String::new())));
    }
    for prefix in ["target/", "/__w/target/", "/github/workspace/target/"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            let rest = mapped_glob_suffix(path, rest)?;
            if let Some(base) = &state.cargo_target_host {
                return Ok(Some((base.clone(), rest)));
            }
        }
    }
    if let Some(rest) = path.strip_prefix("/__w/") {
        let rest = mapped_glob_suffix(path, rest)?;
        return Ok(state
            .workspace_host
            .as_ref()
            .map(|base| (base.clone(), rest)));
    }
    if let Some(rest) = path.strip_prefix("/github/workspace/") {
        let rest = mapped_glob_suffix(path, rest)?;
        return Ok(state
            .workspace_host
            .as_ref()
            .map(|base| (base.clone(), rest)));
    }
    if let Some(rest) = path.strip_prefix("/__t/") {
        let rest = mapped_glob_suffix(path, rest)?;
        return Ok(state.temp_host.as_ref().map(|base| (base.clone(), rest)));
    }
    if let Some(rest) = path.strip_prefix("/github/runner_temp/") {
        let rest = mapped_glob_suffix(path, rest)?;
        return Ok(state.temp_host.as_ref().map(|base| (base.clone(), rest)));
    }
    if let Some(rest) = path.strip_prefix("/tmp/") {
        let rest = mapped_glob_suffix(path, rest)?;
        return Ok(state.temp_host.as_ref().map(|base| (base.clone(), rest)));
    }
    if Path::new(path).is_absolute() {
        bail!("artifact glob has unmapped absolute root: {path}");
    }
    Ok(state
        .workspace_host
        .as_ref()
        .map(|base| (base.clone(), path.to_string())))
}

fn mapped_glob_suffix(path: &str, suffix: &str) -> Result<String> {
    let suffix = normalize_glob_pattern(suffix);
    if relative_mapped_suffix(&suffix).is_none() {
        bail!("artifact source path has absolute mapped suffix: {path}");
    }
    Ok(suffix)
}

fn artifact_glob_literal_base_and_pattern(pattern: &str) -> (PathBuf, String) {
    let pattern = normalize_glob_pattern(pattern);
    let first_glob = pattern
        .char_indices()
        .find_map(|(index, ch)| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}').then_some(index))
        .unwrap_or(pattern.len());
    let literal_end = pattern[..first_glob]
        .rfind('/')
        .map_or(0, |index| index + 1);
    let (literal_base, relative_pattern) = pattern.split_at(literal_end);
    (
        PathBuf::from(literal_base.trim_end_matches('/')),
        relative_pattern.trim_start_matches('/').to_string(),
    )
}

fn normalize_glob_pattern(pattern: &str) -> String {
    pattern.replace('\\', "/")
}

fn collect_artifact_glob_matches(
    current: &crate::fs_copy::NoFollowDir,
    root: &Path,
    current_relative: &Path,
    matcher: &globset::GlobMatcher,
    matches: &mut Vec<ResolvedArtifactSource>,
) -> Result<()> {
    current.for_each_entry_filtered(
        |_| true,
        |entry| {
            let relative = current_relative.join(&entry.name);
            let is_directory =
                matches!(&entry.source, crate::fs_copy::NoFollowSource::Directory(_));
            if matcher.is_match(&relative) {
                matches.push(ResolvedArtifactSource {
                    path: root.join(&relative),
                    artifact_path: relative.clone(),
                });
                if is_directory {
                    return Ok(());
                }
            }
            if let crate::fs_copy::NoFollowSource::Directory(directory) = entry.source {
                collect_artifact_glob_matches(&directory, root, &relative, matcher, matches)?;
            }
            Ok(())
        },
    )
}

fn cache_paths(paths: &str) -> Vec<String> {
    artifact_paths(paths)
}

fn resolve_cache_path(state: &JobExecutionState, path: &str) -> Option<PathBuf> {
    let path = path.trim();
    if path == "~" {
        return state_home_host(state);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return state_home_host(state).and_then(|home| join_mapped_suffix(&home, rest));
    }
    resolve_host_path(state, path)
}

fn state_home_host(state: &JobExecutionState) -> Option<PathBuf> {
    let temp = state.temp_host.as_ref()?;
    if temp.file_name().is_some_and(|name| name == "temp") {
        return temp.parent().map(|job_dir| job_dir.join("home"));
    }
    Some(temp.join("home"))
}

#[derive(Debug)]
struct TrustedJobDestination {
    root: PathBuf,
    relative: PathBuf,
}

impl TrustedJobDestination {
    fn joined_relative(&self, relative: &Path) -> Result<PathBuf> {
        normalized_destination_relative(&self.relative.join(relative))
    }

    fn joined_path(&self, relative: &Path) -> Result<PathBuf> {
        Ok(self.root.join(self.joined_relative(relative)?))
    }

    fn open_relative_directory(
        &self,
        relative: &Path,
    ) -> Result<crate::fs_copy::NoFollowDestinationDir> {
        let rooted_relative = self.joined_relative(relative)?;
        let destination = self.root.join(&rooted_relative);
        crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
            &self.root,
            &rooted_relative,
        )
        .with_context(|| {
            format!(
                "securely open workflow destination without following symlinks: {}",
                destination.display()
            )
        })
    }
}

fn trusted_job_destination(
    state: &JobExecutionState,
    destination: &Path,
) -> Result<TrustedJobDestination> {
    let home = state_home_host(state);
    for root in [
        state.cargo_target_host.as_deref(),
        state.workspace_host.as_deref(),
        home.as_deref(),
        state.temp_host.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let Ok(destination_relative) = destination.strip_prefix(root) else {
            continue;
        };
        let anchor = existing_trusted_destination_anchor(root)?;
        let configured_relative = root.strip_prefix(&anchor).with_context(|| {
            format!(
                "trusted workflow destination root {} is not below existing anchor {}",
                root.display(),
                anchor.display()
            )
        })?;
        return Ok(TrustedJobDestination {
            root: anchor,
            relative: normalized_destination_relative(
                &configured_relative.join(destination_relative),
            )?,
        });
    }
    bail!(
        "workflow destination resolves outside Velnor-mapped job storage: {}",
        destination.display()
    )
}

fn existing_trusted_destination_anchor(configured_root: &Path) -> Result<PathBuf> {
    let mut anchor = configured_root.to_path_buf();
    loop {
        match fs::symlink_metadata(&anchor) {
            Ok(_) => return Ok(anchor),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !anchor.pop() {
                    bail!(
                        "trusted workflow destination root has no existing ancestor: {}",
                        configured_root.display()
                    );
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "inspect trusted workflow destination root {}",
                        configured_root.display()
                    )
                });
            }
        }
    }
}

fn normalized_destination_relative(relative: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(name) => normalized.push(name),
            std::path::Component::RootDir
            | std::path::Component::ParentDir
            | std::path::Component::Prefix(_) => bail!(
                "artifact destination is not a normalized relative path: {}",
                relative.display()
            ),
        }
    }
    Ok(normalized)
}

fn resolve_host_path(state: &JobExecutionState, path: &str) -> Option<PathBuf> {
    let path = path.trim();
    if path.is_empty() || artifact_path_has_parent_component(path) {
        return None;
    }
    if (path == "target" || path == "/__w/target" || path == "/github/workspace/target")
        && let Some(base) = &state.cargo_target_host
    {
        return Some(base.clone());
    }
    for prefix in ["target/", "/__w/target/", "/github/workspace/target/"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            relative_mapped_suffix(rest)?;
            if let Some(base) = &state.cargo_target_host {
                return join_mapped_suffix(base, rest);
            }
        }
    }
    if let Some(rest) = path.strip_prefix("/__w/") {
        return state
            .workspace_host
            .as_deref()
            .and_then(|base| join_mapped_suffix(base, rest));
    }
    if path == "/__w" || path == "/github/workspace" {
        return state.workspace_host.clone();
    }
    if let Some(rest) = path.strip_prefix("/github/workspace/") {
        return state
            .workspace_host
            .as_deref()
            .and_then(|base| join_mapped_suffix(base, rest));
    }
    if let Some(rest) = path.strip_prefix("/__t/") {
        return state
            .temp_host
            .as_deref()
            .and_then(|base| join_mapped_suffix(base, rest));
    }
    if path == "/__t" || path == "/github/runner_temp" {
        return state.temp_host.clone();
    }
    if let Some(rest) = path.strip_prefix("/github/runner_temp/") {
        return state
            .temp_host
            .as_deref()
            .and_then(|base| join_mapped_suffix(base, rest));
    }
    // The job container bind-mounts the host temp dir at /tmp (container.rs),
    // so /tmp paths written by steps (e.g. publish digest exports under
    // /tmp/digests) resolve to the same host directory.
    if path == "/tmp" {
        return state.temp_host.clone();
    }
    if let Some(rest) = path.strip_prefix("/tmp/") {
        return state
            .temp_host
            .as_deref()
            .and_then(|base| join_mapped_suffix(base, rest));
    }
    if path == "/github/home" {
        return state_home_host(state);
    }
    if let Some(rest) = path.strip_prefix("/github/home/") {
        return state_home_host(state).and_then(|home| join_mapped_suffix(&home, rest));
    }
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        // A container-absolute path with no host mapping (e.g. /root/...)
        // exists only inside the job container. Returning the raw path here
        // used to read/write the DAEMON HOST's filesystem (observed live: a
        // workflow cached the host's /root/.cache instead of job output) —
        // never leak host paths; the caller reports the path as unavailable.
        None
    } else {
        state
            .workspace_host
            .as_ref()
            .map(|base| base.join(candidate))
    }
}

fn relative_mapped_suffix(suffix: &str) -> Option<&str> {
    (!suffix.starts_with('/')).then_some(suffix)
}

fn join_mapped_suffix(base: &Path, suffix: &str) -> Option<PathBuf> {
    Some(base.join(relative_mapped_suffix(suffix)?))
}

fn resolve_container_path(state: &JobExecutionState, path: &str) -> String {
    let path = path.trim();
    if path.is_empty() || path == "." {
        return state
            .env
            .get("GITHUB_WORKSPACE")
            .cloned()
            .unwrap_or_else(|| "/__w".to_string());
    }
    if path.starts_with("/__w")
        || path.starts_with("/github/workspace")
        || path.starts_with("/__t")
        || path.starts_with("/github/runner_temp")
        || path.starts_with("/tmp")
    {
        return path.to_string();
    }
    if Path::new(path).is_absolute() {
        path.to_string()
    } else {
        let workspace = state
            .env
            .get("GITHUB_WORKSPACE")
            .map(String::as_str)
            .unwrap_or("/__w");
        format!(
            "{}/{}",
            workspace.trim_end_matches('/'),
            path.trim_start_matches("./")
        )
    }
}

fn artifact_source_is_hidden(state: &JobExecutionState, source: &Path) -> bool {
    let relative = state
        .cargo_target_host
        .as_deref()
        .and_then(|base| source.strip_prefix(base).ok())
        .or_else(|| {
            state
                .workspace_host
                .as_deref()
                .and_then(|base| source.strip_prefix(base).ok())
        })
        .or_else(|| {
            state
                .temp_host
                .as_deref()
                .and_then(|base| source.strip_prefix(base).ok())
        })
        .unwrap_or(source);
    path_has_hidden_component(relative)
}

fn path_has_hidden_component(path: &Path) -> bool {
    path.components().any(|component| match component {
        std::path::Component::Normal(value) => value
            .to_str()
            .is_some_and(|name| name.len() > 1 && name.starts_with('.')),
        _ => false,
    })
}

#[derive(Debug, Default)]
struct ArtifactCopyBudget {
    files: usize,
    total_bytes: u64,
    path_bytes: u64,
}

impl ArtifactCopyBudget {
    fn admit(&mut self, path: &Path, file_bytes: u64) -> Result<()> {
        self.files = self
            .files
            .checked_add(1)
            .context("artifact upload file count overflowed")?;
        if self.files > RESULTS_ARTIFACT_MAX_UPLOAD_FILES {
            bail!(
                "artifact upload contains {} files, exceeding the {}-file upload limit; split it into artifacts with fewer files",
                self.files,
                RESULTS_ARTIFACT_MAX_UPLOAD_FILES
            );
        }
        let depth = path.components().count();
        if depth > RESULTS_ARTIFACT_MAX_UPLOAD_PATH_DEPTH {
            bail!(
                "artifact upload path {} has depth {depth}, exceeding the {}-component limit",
                path.display(),
                RESULTS_ARTIFACT_MAX_UPLOAD_PATH_DEPTH
            );
        }
        self.path_bytes = self
            .path_bytes
            .checked_add(
                u64::try_from(path.as_os_str().as_encoded_bytes().len()).unwrap_or(u64::MAX),
            )
            .context("artifact upload path byte count overflowed")?;
        if self.path_bytes > RESULTS_ARTIFACT_MAX_UPLOAD_PATH_BYTES {
            bail!(
                "artifact upload paths use {} bytes, exceeding the {}-byte limit",
                self.path_bytes,
                RESULTS_ARTIFACT_MAX_UPLOAD_PATH_BYTES
            );
        }
        if file_bytes > RESULTS_ARTIFACT_MAX_UPLOAD_FILE_BYTES {
            bail!(
                "artifact upload file {} is {file_bytes} bytes, exceeding the {}-byte per-file upload limit; split or reduce this file",
                path.display(),
                RESULTS_ARTIFACT_MAX_UPLOAD_FILE_BYTES
            );
        }
        self.total_bytes = self
            .total_bytes
            .checked_add(file_bytes)
            .context("artifact upload source byte count overflowed")?;
        if self.total_bytes > RESULTS_ARTIFACT_MAX_UPLOAD_TOTAL_BYTES {
            bail!(
                "artifact upload sources total {} bytes, exceeding the {}-byte upload limit; split them into smaller artifacts",
                self.total_bytes,
                RESULTS_ARTIFACT_MAX_UPLOAD_TOTAL_BYTES
            );
        }
        Ok(())
    }
}

fn copy_artifact_source(
    source: &ResolvedArtifactSource,
    destination_root: &crate::fs_copy::NoFollowDestinationDir,
    include_hidden: bool,
    budget: &mut ArtifactCopyBudget,
) -> Result<()> {
    let parent = source
        .path
        .parent()
        .with_context(|| format!("artifact source has no parent: {}", source.path.display()))?;
    let file_name = source.path.file_name().with_context(|| {
        format!(
            "artifact source has no file name: {}",
            source.path.display()
        )
    })?;
    let parent = crate::fs_copy::NoFollowDir::open_absolute(parent).with_context(|| {
        format!(
            "securely open artifact source parent for {}",
            source.path.display()
        )
    })?;
    let opened = parent
        .open_source(Path::new(file_name))?
        .with_context(|| format!("artifact source disappeared: {}", source.path.display()))?;
    match opened {
        crate::fs_copy::NoFollowSource::File(file) => {
            budget.admit(&source.artifact_path, file.metadata()?.len())?;
            destination_root
                .clone_or_copy_file(&file, &source.artifact_path)
                .with_context(|| format!("copy artifact file {}", source.path.display()))?;
            Ok(())
        }
        crate::fs_copy::NoFollowSource::Directory(directory) => {
            let destination_directory = destination_root
                .open_relative_directory(&source.artifact_path)
                .with_context(|| {
                    format!(
                        "create artifact directory {}",
                        source.artifact_path.display()
                    )
                })?;
            copy_artifact_dir_contents_filtered(
                &directory,
                &source.path,
                &destination_directory,
                Path::new(""),
                include_hidden,
                budget,
            )
        }
    }
}

fn copy_artifact_dir_contents_filtered(
    source: &crate::fs_copy::NoFollowDir,
    source_path: &Path,
    destination_directory: &crate::fs_copy::NoFollowDestinationDir,
    destination_relative: &Path,
    include_hidden: bool,
    budget: &mut ArtifactCopyBudget,
) -> Result<()> {
    source.for_each_entry_filtered(
        |name| include_hidden || !hidden_file_name(name),
        |entry| {
            let path = source_path.join(&entry.name);
            let target_relative = destination_relative.join(&entry.name);
            match entry.source {
                crate::fs_copy::NoFollowSource::Directory(directory) => {
                    let child_destination = destination_directory
                        .open_relative_directory(Path::new(&entry.name))
                        .with_context(|| {
                            format!("create directory {}", target_relative.display())
                        })?;
                    copy_artifact_dir_contents_filtered(
                        &directory,
                        &path,
                        &child_destination,
                        &target_relative,
                        include_hidden,
                        budget,
                    )?;
                }
                crate::fs_copy::NoFollowSource::File(file) => {
                    budget.admit(&target_relative, file.metadata()?.len())?;
                    destination_directory
                        .clone_or_copy_file(&file, Path::new(&entry.name))
                        .with_context(|| format!("copy {}", path.display()))?;
                }
            }
            Ok(())
        },
    )
}

fn copy_cache_source(source: &Path, destination: &Path) -> Result<()> {
    let parent_path = source
        .parent()
        .with_context(|| format!("cache source has no parent: {}", source.display()))?;
    let file_name = source
        .file_name()
        .with_context(|| format!("cache source has no file name: {}", source.display()))?;
    let parent = crate::fs_copy::NoFollowDir::open_absolute(parent_path)
        .with_context(|| format!("securely open cache source parent for {}", source.display()))?;
    let opened = parent
        .open_source(Path::new(file_name))?
        .with_context(|| format!("cache source disappeared: {}", source.display()))?;
    match opened {
        crate::fs_copy::NoFollowSource::Directory(directory) => {
            let destination_root =
                crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
                    destination,
                    Path::new(""),
                )?;
            copy_dir_contents_filtered(&directory, source, &destination_root, Path::new(""), true)
        }
        crate::fs_copy::NoFollowSource::File(file) => {
            let destination_root =
                crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
                    destination,
                    Path::new(""),
                )?;
            destination_root
                .clone_or_copy_file(&file, Path::new(file_name))
                .with_context(|| format!("copy cache file {}", source.display()))?;
            Ok(())
        }
    }
}

fn restore_cache_dir_contents(
    state: &JobExecutionState,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let source_directory = crate::fs_copy::NoFollowDir::open_absolute(source)
        .with_context(|| format!("securely open cache directory {}", source.display()))?;
    let destination_scope = trusted_job_destination(state, destination)?;
    let destination_directory = destination_scope
        .open_relative_directory(Path::new(""))
        .with_context(|| format!("create cache restore path {}", destination.display()))?;
    copy_cache_restore_dir_contents(
        &source_directory,
        source,
        &destination_directory,
        &destination_scope,
        Path::new(""),
    )
}

fn copy_opened_cache_restore_source(
    source: crate::fs_copy::NoFollowSource,
    source_path: &Path,
    destination_root: &crate::fs_copy::NoFollowDestinationDir,
    destination_scope: &TrustedJobDestination,
    destination_relative: &Path,
) -> Result<()> {
    let destination = destination_scope.joined_path(destination_relative)?;
    match source {
        crate::fs_copy::NoFollowSource::Directory(directory) => {
            let destination_directory = destination_root
                .open_relative_directory(destination_relative)
                .with_context(|| format!("create cache restore path {}", destination.display()))?;
            copy_cache_restore_dir_contents(
                &directory,
                source_path,
                &destination_directory,
                destination_scope,
                destination_relative,
            )
        }
        crate::fs_copy::NoFollowSource::File(file) => {
            destination_root
                .clone_or_copy_file(&file, destination_relative)
                .with_context(|| {
                    format!(
                        "copy cache restore file {} to {}",
                        source_path.display(),
                        destination.display()
                    )
                })?;
            Ok(())
        }
    }
}

fn copy_cache_restore_dir_contents(
    source: &crate::fs_copy::NoFollowDir,
    source_path: &Path,
    destination_directory: &crate::fs_copy::NoFollowDestinationDir,
    destination_scope: &TrustedJobDestination,
    destination_relative: &Path,
) -> Result<()> {
    source.for_each_entry_filtered(
        |_| true,
        |entry| {
            let entry_source_path = source_path.join(&entry.name);
            let entry_destination_relative = destination_relative.join(&entry.name);
            match entry.source {
                crate::fs_copy::NoFollowSource::Directory(directory) => {
                    let child_destination = destination_directory
                        .open_relative_directory(Path::new(&entry.name))
                        .with_context(|| {
                            format!(
                                "create cache restore path {}",
                                entry_destination_relative.display()
                            )
                        })?;
                    copy_cache_restore_dir_contents(
                        &directory,
                        &entry_source_path,
                        &child_destination,
                        destination_scope,
                        &entry_destination_relative,
                    )
                }
                crate::fs_copy::NoFollowSource::File(file) => destination_directory
                    .clone_or_copy_file(&file, Path::new(&entry.name))
                    .with_context(|| {
                        format!(
                            "copy cache restore file {} to {}",
                            entry_source_path.display(),
                            destination_scope
                                .joined_path(&entry_destination_relative)
                                .unwrap_or(entry_destination_relative.clone())
                                .display()
                        )
                    })
                    .map(|_| ()),
            }
        },
    )
}

fn copy_opened_cache_glob_source(
    source: crate::fs_copy::NoFollowSource,
    source_path: &Path,
    destination_root: &crate::fs_copy::NoFollowDestinationDir,
    destination_relative: &Path,
) -> Result<()> {
    match source {
        crate::fs_copy::NoFollowSource::Directory(directory) => {
            let destination_directory = destination_root
                .open_relative_directory(destination_relative)
                .with_context(|| {
                    format!(
                        "create cache glob destination {}",
                        destination_relative.display()
                    )
                })?;
            copy_cache_glob_dir_contents(
                &directory,
                source_path,
                &destination_directory,
                destination_relative,
            )
        }
        crate::fs_copy::NoFollowSource::File(file) => {
            destination_root
                .clone_or_copy_file(&file, destination_relative)
                .with_context(|| format!("copy {}", source_path.display()))?;
            Ok(())
        }
    }
}

fn copy_cache_glob_dir_contents(
    source: &crate::fs_copy::NoFollowDir,
    source_path: &Path,
    destination_directory: &crate::fs_copy::NoFollowDestinationDir,
    destination_relative: &Path,
) -> Result<()> {
    source.for_each_entry_filtered(
        |name| cache_source_entry_is_not_symlink(source_path, name),
        |entry| {
            let entry_source_path = source_path.join(&entry.name);
            let entry_destination_relative = destination_relative.join(&entry.name);
            match entry.source {
                crate::fs_copy::NoFollowSource::Directory(directory) => {
                    let child_destination = destination_directory
                        .open_relative_directory(Path::new(&entry.name))
                        .with_context(|| {
                            format!(
                                "create cache glob destination {}",
                                entry_destination_relative.display()
                            )
                        })?;
                    copy_cache_glob_dir_contents(
                        &directory,
                        &entry_source_path,
                        &child_destination,
                        &entry_destination_relative,
                    )
                }
                crate::fs_copy::NoFollowSource::File(file) => destination_directory
                    .clone_or_copy_file(&file, Path::new(&entry.name))
                    .with_context(|| format!("copy {}", entry_source_path.display()))
                    .map(|_| ()),
            }
        },
    )
}

/// Copy a cache tree in one traversal.
///
/// Each directory entry is either recursively materialized or rejected. The
/// copy primitive already reports a failed file operation, so a second full
/// metadata traversal only adds warm-path latency for large Rust target trees.
fn copy_dir_contents(source: &Path, destination: &Path) -> Result<()> {
    let source_directory = crate::fs_copy::NoFollowDir::open_absolute(source)
        .with_context(|| format!("securely open cache directory {}", source.display()))?;
    let destination_root = crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
        destination,
        Path::new(""),
    )
    .with_context(|| format!("securely open cache destination {}", destination.display()))?;
    copy_dir_contents_filtered(
        &source_directory,
        source,
        &destination_root,
        Path::new(""),
        true,
    )
}

fn copy_artifact_download_contents(
    source: &crate::fs_copy::NoFollowDir,
    source_path: &Path,
    destination_directory: &crate::fs_copy::NoFollowDestinationDir,
    destination_relative: &Path,
) -> Result<()> {
    source.for_each_entry_filtered(
        |_| true,
        |entry| {
            let entry_source_path = source_path.join(&entry.name);
            let entry_destination_relative = destination_relative.join(&entry.name);
            match entry.source {
                crate::fs_copy::NoFollowSource::Directory(directory) => {
                    let child_destination = destination_directory
                        .open_relative_directory(Path::new(&entry.name))
                        .with_context(|| {
                            format!(
                                "create artifact download dir {}",
                                entry_destination_relative.display()
                            )
                        })?;
                    child_destination.set_mode(0o755)?;
                    copy_artifact_download_contents(
                        &directory,
                        &entry_source_path,
                        &child_destination,
                        &entry_destination_relative,
                    )
                }
                crate::fs_copy::NoFollowSource::File(file) => {
                    destination_directory
                        .clone_or_copy_file(&file, Path::new(&entry.name))
                        .with_context(|| {
                            format!(
                                "copy artifact download file {}",
                                entry_source_path.display()
                            )
                        })?;
                    let destination_file = destination_directory
                        .open_relative_file(Path::new(&entry.name))
                        .with_context(|| {
                            format!(
                                "open downloaded artifact file {}",
                                entry_destination_relative.display()
                            )
                        })?;
                    normalize_artifact_file_permissions(
                        &destination_file,
                        &entry_destination_relative,
                    )
                }
            }
        },
    )
}

fn copy_opened_artifact_download_file(
    source: &fs::File,
    destination_root: &crate::fs_copy::NoFollowDestinationDir,
    destination_scope: &TrustedJobDestination,
    destination_relative: &Path,
) -> Result<()> {
    let destination = destination_scope.joined_path(destination_relative)?;
    let parent = destination_relative
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let file_name = destination_relative
        .file_name()
        .context("downloaded artifact path has no file name")?;
    let destination_directory = destination_root
        .open_relative_directory(parent)
        .with_context(|| format!("create artifact directory {}", parent.display()))?;
    destination_directory.set_mode(0o755)?;
    destination_directory
        .clone_or_copy_file(source, Path::new(file_name))
        .with_context(|| format!("copy artifact file {}", destination.display()))?;
    let destination_file = destination_directory
        .open_relative_file(Path::new(file_name))
        .with_context(|| format!("open downloaded artifact file {}", destination.display()))?;
    normalize_artifact_file_permissions(&destination_file, &destination)
}

fn normalize_artifact_file_permissions(file: &fs::File, destination: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        file.set_permissions(fs::Permissions::from_mode(0o644))
            .with_context(|| format!("set artifact file permissions {}", destination.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = (file, destination);
        Ok(())
    }
}

fn copy_dir_contents_filtered(
    source: &crate::fs_copy::NoFollowDir,
    source_path: &Path,
    destination_directory: &crate::fs_copy::NoFollowDestinationDir,
    destination_relative: &Path,
    include_hidden: bool,
) -> Result<()> {
    source.for_each_entry_filtered(
        |name| include_hidden || !hidden_file_name(name),
        |entry| {
            let path = source_path.join(&entry.name);
            let target_relative = destination_relative.join(&entry.name);
            match entry.source {
                crate::fs_copy::NoFollowSource::Directory(directory) => {
                    let child_destination = destination_directory
                        .open_relative_directory(Path::new(&entry.name))
                        .with_context(|| {
                            format!("create directory {}", target_relative.display())
                        })?;
                    copy_dir_contents_filtered(
                        &directory,
                        &path,
                        &child_destination,
                        &target_relative,
                        include_hidden,
                    )?;
                }
                crate::fs_copy::NoFollowSource::File(file) => {
                    destination_directory
                        .clone_or_copy_file(&file, Path::new(&entry.name))
                        .with_context(|| {
                            format!("copy {} to {}", path.display(), target_relative.display())
                        })?;
                }
            }
            Ok(())
        },
    )
}

fn hidden_file_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.len() > 1 && name.starts_with('.'))
}

fn matching_artifacts(store: &Path, name: &str, pattern: &str) -> Result<Vec<(String, PathBuf)>> {
    let mut artifacts = Vec::new();
    if !store.exists() {
        return Ok(artifacts);
    }
    let matcher = if !pattern.is_empty() {
        let mut builder = GlobSetBuilder::new();
        builder.add(Glob::new(pattern)?);
        Some(builder.build().context("build artifact pattern")?)
    } else {
        None
    };
    for entry in fs::read_dir(store).with_context(|| format!("read {}", store.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let artifact_name = entry.file_name().to_string_lossy().to_string();
        let matched = if !name.is_empty() {
            artifact_name == sanitize_artifact_name(name)
        } else if let Some(matcher) = &matcher {
            matcher.is_match(&artifact_name)
        } else {
            true
        };
        if matched {
            artifacts.push((artifact_name, entry.path()));
        }
    }
    artifacts.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(artifacts)
}

fn cache_key_from_metadata(path: &Path) -> Option<String> {
    fs::read_to_string(path.join(".velnor-key"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn hash_artifact_dir(path: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(path, &mut files)?;
    files.sort();
    let digest_results: Vec<_> = files
        .par_iter()
        .enumerate()
        .map(|(index, file)| sha256_file_digest(file).map(|digest| (index, digest)))
        .collect();
    let mut digests: Vec<_> = digest_results.into_iter().collect::<Result<_>>()?;
    digests.sort_by_key(|(index, _)| *index);

    let mut aggregate = Sha256::new();
    for (_, digest) in digests {
        aggregate.update(digest);
    }
    let digest = aggregate.finalize();
    Ok(hex_digest(&digest))
}

fn sha256_file_digest(path: &Path) -> Result<[u8; 32]> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    sha256_opened_file_digest_bytes(&mut file)
}

fn sha256_opened_file_digest(file: &fs::File) -> Result<String> {
    let mut file = file
        .try_clone()
        .context("duplicate file for SHA-256 digest")?;
    let digest = sha256_opened_file_digest_bytes(&mut file)?;
    Ok(format!(
        "{PREPARED_ARTIFACT_SHA256_PREFIX}{}",
        hex_digest(&digest)
    ))
}

fn sha256_opened_file_digest_bytes(file: &mut fs::File) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .context("read opened file for SHA-256 digest")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn github_digest_eq(expected: &str, actual: &str) -> bool {
    let Some(expected_hex) = expected.strip_prefix(PREPARED_ARTIFACT_SHA256_PREFIX) else {
        return false;
    };
    let Some(actual_hex) = actual.strip_prefix(PREPARED_ARTIFACT_SHA256_PREFIX) else {
        return false;
    };
    expected_hex.len() == 64
        && actual_hex.len() == 64
        && expected_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        && expected_hex.eq_ignore_ascii_case(actual_hex)
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn sanitize_artifact_name(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    match mapped.as_str() {
        "" | "." | ".." => "_".to_string(),
        _ => mapped,
    }
}

fn job_scoped_buildx_builder_name(requested: &str, state: &JobExecutionState) -> String {
    format!(
        "{}-{}",
        sanitize_artifact_name(requested),
        job_scope_from_temp(state.temp_host.as_deref())
    )
}

fn buildx_driver_resource_options(resource_options: &[String]) -> Result<Vec<String>> {
    let mut options = Vec::new();
    let (chunks, remainder) = resource_options.as_chunks::<2>();
    for [flag, value] in chunks {
        match flag.as_str() {
            "--memory" => options.push(format!("memory={value}")),
            "--cpus" => {
                let cpus = value
                    .parse::<f64>()
                    .with_context(|| format!("invalid job CPU limit '{value}'"))?;
                if !cpus.is_finite() || cpus <= 0.0 {
                    bail!("invalid job CPU limit '{value}'");
                }
                let quota = (cpus * 100_000.0).round();
                if quota > u64::MAX as f64 {
                    bail!("job CPU limit '{value}' is too large");
                }
                options.push("cpu-period=100000".to_string());
                options.push(format!("cpu-quota={quota:.0}"));
            }
            option => bail!("unsupported BuildKit resource option '{option}'"),
        }
    }
    if !remainder.is_empty() {
        bail!("job resource options must be flag/value pairs");
    }
    Ok(options)
}

fn job_scope_from_temp(temp: Option<&Path>) -> String {
    let scope_path = temp
        .filter(|path| path.file_name().is_some_and(|name| name == "temp"))
        .and_then(Path::parent)
        .or(temp);
    scope_path
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(sanitize_artifact_name)
        .unwrap_or_else(|| "job".to_string())
}

fn pages_url_for_repository(repository: &str) -> String {
    let Some((owner, repo)) = repository.split_once('/') else {
        return String::new();
    };
    format!("https://{owner}.github.io/{repo}/")
}

fn native_command_result(result: CommandResult, state: StepCommandState) -> StepExecutionResult {
    StepExecutionResult {
        exit_code: result.code,
        state,
        skipped: false,
        failure_ignored: false,
        stdout: result.stdout,
        stderr: result.stderr,
    }
}

/// Filter env vars that break Docker CLI plugin discovery when running Docker
/// commands directly on the host (not inside the job container).
/// Docker CLI plugins (buildx, etc.) are stored in ~/.docker/cli-plugins.
/// If HOME is overridden to a container path like /github/home, Docker cannot
/// find the plugins and treats subcommands like `buildx` as unknown commands.
fn host_docker_env(env: Vec<(String, String)>) -> Vec<(String, String)> {
    env.into_iter()
        .filter(|(name, _)| name != "HOME" && name != "USERPROFILE")
        .collect()
}

fn push_arg(args: &mut Vec<String>, name: &str, value: &str) {
    if !value.trim().is_empty() {
        args.push(name.to_string());
        args.push(value.to_string());
    }
}

#[derive(Debug)]
struct BuildSecretFile {
    host_path: PathBuf,
    container_path: String,
}

impl BuildSecretFile {
    fn create(temp_host: &Path, value: &str) -> Result<Self> {
        let relative = PathBuf::from("_velnor")
            .join("build-secrets")
            .join(uuid::Uuid::new_v4().to_string());
        let host_path = temp_host.join(&relative);
        fs::create_dir_all(host_path.parent().expect("secret file has parent"))
            .context("create BuildKit secret directory")?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&host_path)
            .context("create BuildKit secret file")?;
        file.write_all(value.as_bytes())
            .context("write BuildKit secret file")?;
        Ok(Self {
            host_path,
            container_path: format!("/__t/{}", relative.to_string_lossy()),
        })
    }

    fn container_path(&self) -> &str {
        &self.container_path
    }
}

impl Drop for BuildSecretFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.host_path);
    }
}

fn input_values(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Build context as seen from inside the job container (CWD /__w): relative
/// values resolve against the workspace naturally; absolute container paths
/// pass through.
fn container_context_path(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        ".".to_string()
    } else {
        trimmed.to_string()
    }
}

fn host_context_path(state: &JobExecutionState, value: &str) -> String {
    resolve_host_path(state, value)
        .unwrap_or_else(|| PathBuf::from(value))
        .display()
        .to_string()
}

fn docker_metadata_tags(state: &JobExecutionState, input: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for line in input.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let fields = docker_metadata_fields(line);
        if fields
            .get("enable")
            .is_some_and(|value| !input_truthy(value))
        {
            continue;
        }
        match fields.get("type").map(String::as_str) {
            Some("raw") => {
                if let Some(value) = fields.get("value").filter(|value| !value.is_empty()) {
                    tags.push(value.clone());
                }
            }
            Some("sha") => {
                let sha = state
                    .env
                    .get("GITHUB_SHA")
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                let prefix = fields
                    .get("prefix")
                    .cloned()
                    .unwrap_or_else(|| "sha-".to_string());
                let value = if fields.get("format").is_some_and(|value| value == "long") {
                    sha
                } else {
                    sha.chars().take(7).collect()
                };
                tags.push(format!("{prefix}{value}"));
            }
            Some("ref") => {
                let event = fields.get("event").map(String::as_str).unwrap_or("");
                match event {
                    "branch" => {
                        let git_ref = state.env.get("GITHUB_REF").cloned().unwrap_or_default();
                        if let Some(branch) = git_ref.strip_prefix("refs/heads/") {
                            let tag = docker_sanitize_tag(branch);
                            if !tag.is_empty() {
                                tags.push(tag);
                            }
                        }
                    }
                    "tag" => {
                        let git_ref = state.env.get("GITHUB_REF").cloned().unwrap_or_default();
                        if let Some(tag_name) = git_ref.strip_prefix("refs/tags/") {
                            let tag = docker_sanitize_tag(tag_name);
                            if !tag.is_empty() {
                                tags.push(tag);
                            }
                        }
                    }
                    "pr" => {
                        if let Some(pr_number) = state
                            .resolve_context_data_expression("github.event.pull_request.number")
                            .filter(|v| !v.is_empty())
                        {
                            tags.push(format!("pr-{pr_number}"));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if tags.is_empty() {
        let sha = state
            .env
            .get("GITHUB_SHA")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        tags.push(format!("sha-{}", sha.chars().take(7).collect::<String>()));
    }
    tags
}

fn docker_metadata_fields(line: &str) -> BTreeMap<String, String> {
    line.split(',')
        .filter_map(|field| field.split_once('='))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect()
}

fn docker_sanitize_tag(name: &str) -> String {
    // Docker tag chars: [a-zA-Z0-9_.-]; replace others with '-'; max 128 chars.
    // Leading '.' or '-' are invalid — strip them.
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = sanitized.trim_start_matches(['.', '-']);
    trimmed.chars().take(128).collect()
}

fn docker_metadata_labels(state: &JobExecutionState) -> Vec<String> {
    let repository = state
        .env
        .get("GITHUB_REPOSITORY")
        .cloned()
        .unwrap_or_default();
    let server = state
        .env
        .get("GITHUB_SERVER_URL")
        .cloned()
        .unwrap_or_else(|| "https://github.com".to_string());
    let source = if repository.is_empty() {
        server
    } else {
        format!("{}/{}", server.trim_end_matches('/'), repository)
    };
    vec![format!("org.opencontainers.image.source={source}")]
}

fn docker_metadata_json(tags: &str, labels: &str) -> String {
    serde_json::json!({
        "tags": tags.lines().collect::<Vec<_>>(),
        "labels": labels.lines().collect::<Vec<_>>(),
    })
    .to_string()
}

fn parse_paths_filter_rules(filters: &str) -> Result<Vec<(String, Vec<String>)>> {
    let value = serde_yaml::from_str::<serde_yaml::Value>(filters).context("parse filters")?;
    let Some(mapping) = value.as_mapping() else {
        bail!("paths-filter filters input must be a YAML mapping")
    };
    let mut rules = Vec::new();
    for (name, patterns) in mapping {
        let name = name.to_string();
        let patterns = paths_filter_patterns(patterns)
            .with_context(|| format!("parse paths-filter patterns for '{name}'"))?;
        rules.push((name, patterns));
    }
    Ok(rules)
}

fn paths_filter_patterns(value: &serde_yaml::Value) -> Result<Vec<String>> {
    match value {
        serde_yaml::Value::Sequence(sequence) => sequence
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("paths-filter pattern must be a string"))
            })
            .collect(),
        serde_yaml::Value::String(value) => Ok(vec![value.clone()]),
        _ => bail!("paths-filter patterns must be a string or string list"),
    }
}

fn paths_filter_base_ref(state: &JobExecutionState) -> Option<String> {
    state
        .resolve_context_data_expression("github.event.pull_request.base.sha")
        .filter(|value| !value.is_empty())
        .or_else(|| {
            state
                .resolve_context_data_expression("github.event.before")
                .filter(|value| !value.is_empty())
        })
        .or_else(|| state.env.get("GITHUB_BASE_REF").cloned())
        // Current dorny/paths-filter defaults non-PR events (including
        // workflow_dispatch) to repository.default_branch and compares from
        // its merge base. Use the remote-tracking name because checkout is a
        // detached, depth-one SHA and has no local default-branch ref.
        .or_else(|| {
            state
                .resolve_context_data_expression("github.event.repository.default_branch")
                .filter(|value| !value.is_empty())
                .map(|branch| format!("origin/{branch}"))
        })
}

fn paths_filter_head_ref(state: &JobExecutionState) -> Option<String> {
    state
        .resolve_context_data_expression("github.event.pull_request.head.sha")
        .filter(|value| !value.is_empty())
        .or_else(|| state.env.get("GITHUB_SHA").cloned())
}

#[derive(Debug, Default)]
pub(crate) struct JobExecutionState {
    env: BTreeMap<String, String>,
    /// Snapshot of runner-authoritative job values. Action-local env and
    /// workflow command state must never rewrite identity or credentials used
    /// for authenticated artifact operations.
    immutable_env: BTreeMap<String, String>,
    /// Workflow-level env (display: first lines of each `env:` block).
    workflow_env: Vec<(String, String)>,
    /// Env accumulated at runtime via GITHUB_ENV / ::set-env, in set order —
    /// GitHub appends these after the workflow env in the `env:` block.
    dynamic_env: Vec<(String, String)>,
    context_data: BTreeMap<String, Value>,
    workspace_host: Option<PathBuf>,
    temp_host: Option<PathBuf>,
    /// Runner-internal storage fact; deliberately not exported to step env.
    persistent_workspace_target: bool,
    /// Job-local workspace target materialized from a persistent generation.
    cargo_target_host: Option<PathBuf>,
    outputs: BTreeMap<String, BTreeMap<String, String>>,
    action_states: BTreeMap<String, BTreeMap<String, String>>,
    outcomes: BTreeMap<String, StepOutcome>,
    conclusions: BTreeMap<String, StepOutcome>,
    path: Vec<String>,
    masks: Vec<String>,
    composite_stack: Vec<String>,
    composite_conclusion_stack: Vec<CompositeConclusionFrame>,
}

#[derive(Debug, Clone, Default)]
struct CompositeConclusionFrame {
    step_id: String,
    conclusions: BTreeMap<String, StepOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepOutcome {
    Success,
    Failure,
    Skipped,
}

impl StepOutcome {
    fn as_str(self) -> &'static str {
        match self {
            StepOutcome::Success => "success",
            StepOutcome::Failure => "failure",
            StepOutcome::Skipped => "skipped",
        }
    }
}

impl JobExecutionState {
    fn new(base_env: &[(String, String)]) -> Self {
        Self::new_with_context(base_env, &[])
    }

    pub(crate) fn new_with_context(
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
    ) -> Self {
        Self::new_internal(base_env, context_data, None, None)
    }

    fn new_with_workspace(
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        workspace_host: &Path,
        temp_host: &Path,
    ) -> Self {
        Self::new_internal(
            base_env,
            context_data,
            Some(workspace_host.to_path_buf()),
            Some(temp_host.to_path_buf()),
        )
    }

    fn new_internal(
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        workspace_host: Option<PathBuf>,
        temp_host: Option<PathBuf>,
    ) -> Self {
        let initial_env: BTreeMap<_, _> = base_env.iter().cloned().collect();
        let mut state = Self {
            env: initial_env.clone(),
            immutable_env: initial_env,
            workflow_env: Vec::new(),
            dynamic_env: Vec::new(),
            context_data: context_data.iter().cloned().collect(),
            workspace_host,
            temp_host,
            persistent_workspace_target: false,
            cargo_target_host: None,
            outputs: BTreeMap::new(),
            action_states: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            conclusions: BTreeMap::new(),
            path: Vec::new(),
            masks: Vec::new(),
            composite_stack: Vec::new(),
            composite_conclusion_stack: Vec::new(),
        };
        state.env = state
            .env
            .iter()
            .map(|(name, value)| (name.clone(), state.resolve_expressions(value)))
            .collect();
        state.immutable_env = state.env.clone();
        state
    }

    pub(crate) fn step_env(&self, command_file_env: &[(String, String)]) -> Vec<(String, String)> {
        let mut env: Vec<_> = self
            .env
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        env.extend(command_file_env.iter().cloned());
        env
    }

    pub(crate) fn path_prepend(&self) -> &[String] {
        &self.path
    }

    fn secret_masks(&self, initial_masks: &[String]) -> Vec<String> {
        let mut masks = initial_masks.to_vec();
        masks.extend(self.masks.iter().cloned());
        masks
    }

    pub(crate) fn with_step_action(&self, step_id: &str) -> Self {
        let mut state = Self {
            env: self.env.clone(),
            immutable_env: self.immutable_env.clone(),
            workflow_env: self.workflow_env.clone(),
            dynamic_env: self.dynamic_env.clone(),
            context_data: self.context_data.clone(),
            workspace_host: self.workspace_host.clone(),
            temp_host: self.temp_host.clone(),
            persistent_workspace_target: self.persistent_workspace_target,
            cargo_target_host: self.cargo_target_host.clone(),
            outputs: self.outputs.clone(),
            action_states: self.action_states.clone(),
            outcomes: self.outcomes.clone(),
            conclusions: self.conclusions.clone(),
            path: self.path.clone(),
            masks: self.masks.clone(),
            composite_stack: self.composite_stack.clone(),
            composite_conclusion_stack: self.composite_conclusion_stack.clone(),
        };
        state
            .env
            .insert("GITHUB_ACTION".to_string(), step_id.to_string());
        state
    }

    fn with_env(&self, env: Vec<(String, String)>) -> Self {
        let mut state = Self {
            env: self.env.clone(),
            immutable_env: self.immutable_env.clone(),
            workflow_env: self.workflow_env.clone(),
            dynamic_env: self.dynamic_env.clone(),
            context_data: self.context_data.clone(),
            workspace_host: self.workspace_host.clone(),
            temp_host: self.temp_host.clone(),
            persistent_workspace_target: self.persistent_workspace_target,
            cargo_target_host: self.cargo_target_host.clone(),
            outputs: self.outputs.clone(),
            action_states: self.action_states.clone(),
            outcomes: self.outcomes.clone(),
            conclusions: self.conclusions.clone(),
            path: self.path.clone(),
            masks: self.masks.clone(),
            composite_stack: self.composite_stack.clone(),
            composite_conclusion_stack: self.composite_conclusion_stack.clone(),
        };
        for (name, value) in env {
            state.env.insert(name, value);
        }
        state
    }

    fn push_composite(&mut self, step_id: &str) {
        self.composite_stack.push(step_id.to_string());
        self.composite_conclusion_stack
            .push(CompositeConclusionFrame {
                step_id: step_id.to_string(),
                conclusions: BTreeMap::new(),
            });
    }

    fn pop_composite(&mut self, step_id: &str) {
        if self
            .composite_stack
            .last()
            .is_some_and(|scope| scope == step_id)
        {
            self.composite_stack.pop();
        }
        if self
            .composite_conclusion_stack
            .last()
            .is_some_and(|frame| frame.step_id == step_id)
        {
            self.composite_conclusion_stack.pop();
        }
    }

    pub(crate) fn apply(&mut self, step_id: &str, result: &StepExecutionResult) {
        let outcome = if result.skipped {
            StepOutcome::Skipped
        } else if result.exit_code == 0 {
            StepOutcome::Success
        } else {
            StepOutcome::Failure
        };
        let conclusion = if result.failure_ignored && outcome == StepOutcome::Failure {
            StepOutcome::Success
        } else {
            outcome
        };
        self.outcomes.insert(step_id.to_string(), outcome);
        self.conclusions.insert(step_id.to_string(), conclusion);
        if let Some(frame) = self.composite_conclusion_stack.last_mut() {
            frame.conclusions.insert(step_id.to_string(), conclusion);
        }

        if !result.state.outputs.is_empty() {
            self.outputs
                .insert(step_id.to_string(), result.state.outputs.clone());
        }
        if !result.state.state.is_empty() {
            self.action_states
                .entry(step_id.to_string())
                .or_default()
                .extend(result.state.state.clone());
        }
        for (name, value) in &result.state.env {
            self.env.insert(name.clone(), value.clone());
            if let Some(existing) = self
                .dynamic_env
                .iter_mut()
                .find(|(existing_name, _)| existing_name == name)
            {
                existing.1 = value.clone();
            } else {
                self.dynamic_env.push((name.clone(), value.clone()));
            }
        }
        for path in result.state.path.iter().rev() {
            self.path.insert(0, path.clone());
        }
        self.masks.extend(result.state.masks.iter().cloned());
    }

    fn action_state_env(&self, step_id: &str) -> Vec<(String, String)> {
        self.action_states
            .get(step_id)
            .into_iter()
            .flat_map(|state| {
                state
                    .iter()
                    .map(|(name, value)| (format!("STATE_{name}"), value.clone()))
            })
            .collect()
    }

    pub(crate) fn resolve_script_step(&self, step: &ScriptStep) -> ScriptStep {
        ScriptStep {
            id: step.id.clone(),
            display_name: step.display_name.clone(),
            script: self.resolve_expressions(&step.script),
            shell: step.shell,
            working_directory_container: self
                .resolve_expressions(&step.working_directory_container),
            env: self.resolve_env(&step.env),
            condition: step.condition.clone(),
            continue_on_error: step.continue_on_error,
            timeout_minutes: step.timeout_minutes,
        }
    }

    /// The `env:` block GitHub prints in a step's header group: workflow env
    /// first, then runtime-accumulated env (GITHUB_ENV / ::set-env) in set
    /// order, then the step's own env — first position wins, latest value.
    fn prelude_env(&self, step_env: &[(String, String)]) -> Vec<(String, String)> {
        let mut ordered: Vec<(String, String)> = Vec::new();
        let mut upsert = |name: &String, value: &String| {
            if let Some(existing) = ordered.iter_mut().find(|(n, _)| n == name) {
                existing.1 = value.clone();
            } else {
                ordered.push((name.clone(), value.clone()));
            }
        };
        for (name, value) in &self.workflow_env {
            upsert(name, value);
        }
        for (name, value) in &self.dynamic_env {
            upsert(name, value);
        }
        for (name, value) in step_env {
            upsert(name, value);
        }
        self.resolve_env(&ordered)
    }

    pub(crate) fn resolve_env(&self, env: &[(String, String)]) -> Vec<(String, String)> {
        env.iter()
            .map(|(name, value)| (name.clone(), self.resolve_expressions(value)))
            .collect()
    }

    fn evaluate_named_outputs(
        &self,
        outputs: &BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        outputs
            .iter()
            .filter_map(|(name, value)| {
                let value = self.resolve_expressions(value);
                (!value.is_empty()).then(|| (name.clone(), value))
            })
            .collect()
    }

    pub(crate) fn resolve_expressions(&self, value: &str) -> String {
        let mut rendered = String::with_capacity(value.len());
        let mut rest = value;
        while let Some(start) = rest.find("${{") {
            rendered.push_str(&rest[..start]);
            let after_start = &rest[start + 3..];
            let Some(end) = after_start.find("}}") else {
                rendered.push_str(&rest[start..]);
                return rendered;
            };
            let expression = after_start[..end].trim();
            if let Some(value) = self.resolve_expression_value(expression) {
                rendered.push_str(&value);
            } else {
                rendered.push_str(&rest[start..start + 3 + end + 2]);
            }
            rest = &after_start[end + 2..];
        }
        rendered.push_str(rest);
        rendered
    }

    fn resolve_step_output_expression(&self, expression: &str) -> Option<&str> {
        let expression = expression.strip_prefix("steps.")?;
        let (step_id, output) = expression.split_once(".outputs")?;
        let output = if let Some(output) = output.strip_prefix('.') {
            output
        } else {
            output.strip_prefix("['")?.strip_suffix("']")?
        };
        self.outputs
            .get(step_id)
            .and_then(|outputs| outputs.get(output))
            .map(String::as_str)
    }

    fn resolve_expression_value(&self, expression: &str) -> Option<String> {
        let expression = expression.trim();
        if let Some(inner) = strip_wrapping_parentheses(expression) {
            return self.resolve_expression_value(inner);
        }
        if let Some((left, right)) = split_top_level(expression, "||") {
            let left = self.resolve_expression_value(left).unwrap_or_default();
            if expression_truthy(&left) {
                return Some(left);
            }
            return self.resolve_expression_value(right);
        }
        if let Some((left, right)) = split_top_level(expression, "&&") {
            let left = self.resolve_expression_value(left).unwrap_or_default();
            if expression_truthy(&left) {
                return self.resolve_expression_value(right);
            }
            return Some("false".to_string());
        }
        if let Some(inner) = expression.strip_prefix('!') {
            let value = self.resolve_expression_value(inner).unwrap_or_default();
            return Some((!expression_truthy(&value)).to_string());
        }
        if let Some((left, right)) = split_top_level(expression, "!=") {
            return Some(
                (!github_string_eq(
                    &self.resolve_expression_value(left).unwrap_or_default(),
                    &self.resolve_expression_value(right).unwrap_or_default(),
                ))
                .to_string(),
            );
        }
        if let Some((left, right)) = split_top_level(expression, "==") {
            return Some(
                github_string_eq(
                    &self.resolve_expression_value(left).unwrap_or_default(),
                    &self.resolve_expression_value(right).unwrap_or_default(),
                )
                .to_string(),
            );
        }
        if let Some((value, needle)) = parse_contains(expression) {
            return Some(
                self.resolve_expression_value(value)
                    .is_some_and(|value| github_contains(&value, unquote(needle.trim())))
                    .to_string(),
            );
        }
        if is_quoted(expression) {
            return Some(unquote(expression).to_string());
        }
        if expression == "true" || expression == "false" {
            return Some(expression.to_string());
        }
        if expression == "null" {
            return Some(String::new());
        }
        if let Some(output) = self.resolve_step_output_expression(expression) {
            return Some(output.to_string());
        }
        if expression.starts_with("steps.") && expression.contains(".outputs.") {
            return Some(String::new());
        }
        if let Some(context) = parse_to_json(expression) {
            return self
                .resolve_context_data_value(context)
                .and_then(|value| serde_json::to_string(value).ok());
        }
        if let Some(patterns) = parse_hash_files(expression) {
            return self.resolve_hash_files_expression(patterns);
        }
        if expression.trim().starts_with("format(") {
            // Resolve format() args through the full state resolver so that runtime
            // values like `steps.run-output.outputs.output` are available (not just
            // compile-time context_data values from the job message).
            return self.resolve_format_expression(expression.trim());
        }
        self.resolve_context_expression(expression)
    }

    fn resolve_hash_files_expression(&self, patterns: Vec<String>) -> Option<String> {
        let workspace = self.workspace_host.as_ref()?;
        // Match actions/runner's HashFilesFunction: evaluate against the
        // workspace as it exists at this expression call. In particular, an
        // evaluation before checkout may correctly be empty, but must not
        // poison the same expression after checkout has populated the tree.
        Some(hash_files(workspace, &patterns))
    }

    /// Evaluate `format('template', arg0, arg1, ...)` using the full state resolver
    /// so runtime step outputs (`steps.X.outputs.Y`) are accessible for args.
    fn resolve_format_expression(&self, expression: &str) -> Option<String> {
        let inner = expression.strip_prefix("format(")?.strip_suffix(')')?;
        let parts = crate::script_step::split_format_args_pub(inner);
        if parts.is_empty() {
            return None;
        }
        let template = parts[0].trim().strip_prefix('\'')?.strip_suffix('\'')?;
        let placeholder_open = "\x00LBRACE\x00";
        let placeholder_close = "\x00RBRACE\x00";
        let mut result = template
            .replace("''", "'")
            .replace("{{", placeholder_open)
            .replace("}}", placeholder_close);
        for (i, arg) in parts[1..].iter().enumerate() {
            // Use full state resolver so runtime values (step outputs, etc.) work.
            let resolved = self
                .resolve_expression_value(arg.trim())
                .unwrap_or_default();
            result = result.replace(&format!("{{{i}}}"), &resolved);
        }
        result = result
            .replace(placeholder_open, "{")
            .replace(placeholder_close, "}");
        Some(result)
    }

    fn resolve_context_expression(&self, expression: &str) -> Option<String> {
        if let Some(name) = expression.trim().strip_prefix("env.") {
            return self.env.get(name).cloned();
        }
        if expression.trim() == "job.status" {
            return Some(self.job_status().to_string());
        }
        if expression.trim() == "github.action_status" {
            return Some(self.action_status().to_string());
        }

        let env_name = match expression.trim() {
            "github.actor" => "GITHUB_ACTOR",
            "github.actor_id" => "GITHUB_ACTOR_ID",
            "github.action" => "GITHUB_ACTION",
            "github.action_path" => "GITHUB_ACTION_PATH",
            "github.action_ref" => "GITHUB_ACTION_REF",
            "github.action_repository" => "GITHUB_ACTION_REPOSITORY",
            "github.api_url" => "GITHUB_API_URL",
            "github.base_ref" => "GITHUB_BASE_REF",
            "github.event_name" => "GITHUB_EVENT_NAME",
            "github.event_path" => "GITHUB_EVENT_PATH",
            "github.graphql_url" => "GITHUB_GRAPHQL_URL",
            "github.head_ref" => "GITHUB_HEAD_REF",
            "github.ref" => "GITHUB_REF",
            "github.ref_protected" => "GITHUB_REF_PROTECTED",
            "github.ref_type" => "GITHUB_REF_TYPE",
            "github.repository" => "GITHUB_REPOSITORY",
            "github.repository_id" => "GITHUB_REPOSITORY_ID",
            "github.repository_owner" => "GITHUB_REPOSITORY_OWNER",
            "github.repository_owner_id" => "GITHUB_REPOSITORY_OWNER_ID",
            "github.retention_days" => "GITHUB_RETENTION_DAYS",
            "github.run_id" => "GITHUB_RUN_ID",
            "github.run_attempt" => "GITHUB_RUN_ATTEMPT",
            "github.run_number" => "GITHUB_RUN_NUMBER",
            "github.server_url" => "GITHUB_SERVER_URL",
            "github.sha" => "GITHUB_SHA",
            "github.token" => "GITHUB_TOKEN",
            "github.triggering_actor" => "GITHUB_TRIGGERING_ACTOR",
            "github.workflow" => "GITHUB_WORKFLOW",
            "github.workflow_ref" => "GITHUB_WORKFLOW_REF",
            "github.workflow_sha" => "GITHUB_WORKFLOW_SHA",
            "github.ref_name" => "GITHUB_REF_NAME",
            "github.workspace" => "GITHUB_WORKSPACE",
            "runner.arch" => "RUNNER_ARCH",
            "runner.debug" => "RUNNER_DEBUG",
            "runner.environment" => "RUNNER_ENVIRONMENT",
            "runner.name" => "RUNNER_NAME",
            "runner.os" => "RUNNER_OS",
            "runner.temp" => "RUNNER_TEMP",
            "runner.tool_cache" => "RUNNER_TOOL_CACHE",
            "runner.workspace" => "RUNNER_WORKSPACE",
            _ => return self.resolve_context_data_expression(expression),
        };
        self.env
            .get(env_name)
            .cloned()
            .or_else(|| self.resolve_context_data_expression(expression))
    }

    fn job_status(&self) -> &'static str {
        if self
            .conclusions
            .values()
            .any(|outcome| *outcome == StepOutcome::Failure)
        {
            "failure"
        } else {
            "success"
        }
    }

    fn action_status(&self) -> &'static str {
        if self.status_scope_has_failure() {
            "failure"
        } else {
            "success"
        }
    }

    pub(crate) fn evaluate_condition(&self, condition: Option<&str>) -> bool {
        let Some(condition) = condition
            .map(str::trim)
            .filter(|condition| !condition.is_empty())
        else {
            return self.evaluate_condition_expr("success()");
        };
        let expression = strip_expression(condition);
        if condition_has_status_check(expression) {
            return self.evaluate_condition_expr(expression);
        }
        self.evaluate_condition_expr("success()") && self.evaluate_condition_expr(expression)
    }

    fn evaluate_post_condition(&self, condition: Option<&str>) -> bool {
        let Some(condition) = condition
            .map(str::trim)
            .filter(|condition| !condition.is_empty())
        else {
            return true;
        };
        let expression = strip_expression(condition);
        if condition_has_status_check(expression) {
            return self.evaluate_condition_expr(expression);
        }
        self.evaluate_condition_expr("success()") && self.evaluate_condition_expr(expression)
    }

    fn evaluate_condition_expr(&self, expression: &str) -> bool {
        let expression = expression.trim();
        if expression == "always()" {
            return true;
        }
        if expression == "success()" {
            return !self.status_scope_has_failure();
        }
        if expression == "failure()" {
            return self.status_scope_has_failure();
        }
        if expression == "cancelled()" {
            return false;
        }
        if let Some(inner) = strip_wrapping_parentheses(expression) {
            return self.evaluate_condition_expr(inner);
        }
        if let Some((left, right)) = split_top_level(expression, "||") {
            return self.evaluate_condition_expr(left) || self.evaluate_condition_expr(right);
        }
        if let Some((left, right)) = split_top_level(expression, "&&") {
            return self.evaluate_condition_expr(left) && self.evaluate_condition_expr(right);
        }
        if let Some(inner) = expression.strip_prefix('!') {
            return !self.evaluate_condition_expr(inner);
        }
        if let Some((value, needle)) = parse_contains(expression) {
            return self
                .resolve_condition_value(value)
                .is_some_and(|value| github_contains(&value, unquote(needle.trim())));
        }
        if let Some((left, right)) = split_top_level(expression, "!=") {
            let left = self
                .resolve_condition_comparison_value(left)
                .unwrap_or_default();
            let right = self
                .resolve_condition_comparison_value(right)
                .unwrap_or_default();
            return !github_string_eq(&left, &right);
        }
        if let Some((left, right)) = split_top_level(expression, "==") {
            let left = self
                .resolve_condition_comparison_value(left)
                .unwrap_or_default();
            let right = self
                .resolve_condition_comparison_value(right)
                .unwrap_or_default();
            return github_string_eq(&left, &right);
        }
        if expression == "true" {
            return true;
        }
        if expression == "false" {
            return false;
        }
        if let Some(value) = self.resolve_condition_value(expression) {
            return expression_truthy(&value);
        }
        true
    }

    fn status_scope_has_failure(&self) -> bool {
        if let Some(frame) = self.composite_conclusion_stack.last() {
            frame
                .conclusions
                .values()
                .any(|outcome| *outcome == StepOutcome::Failure)
        } else {
            self.conclusions
                .values()
                .any(|outcome| *outcome == StepOutcome::Failure)
        }
    }

    fn resolve_condition_comparison_value(&self, expression: &str) -> Option<String> {
        let expression = expression.trim();
        if condition_returns_bool(expression) {
            return Some(self.evaluate_condition_expr(expression).to_string());
        }
        self.resolve_condition_value(expression)
    }

    fn resolve_condition_value(&self, expression: &str) -> Option<String> {
        let expression = expression.trim();
        if let Some(output) = self.resolve_step_output_expression(expression) {
            return Some(output.to_string());
        }
        if let Some(expression) = expression.strip_prefix("steps.") {
            let (step_id, field) = expression.split_once('.')?;
            if field == "outcome" {
                return self
                    .outcomes
                    .get(step_id)
                    .map(|outcome| outcome.as_str().to_string());
            }
            if field == "conclusion" {
                return self
                    .conclusions
                    .get(step_id)
                    .map(|outcome| outcome.as_str().to_string());
            }
        }
        if expression == "runner.os" {
            return self
                .resolve_context_expression(expression)
                .or_else(|| Some("Linux".to_string()));
        }
        if let Some(value) = self.resolve_context_expression(expression) {
            return Some(value);
        }
        Some(unquote(expression).to_string())
    }

    fn resolve_context_data_expression(&self, expression: &str) -> Option<String> {
        self.resolve_context_data_value(expression)
            .and_then(context_value_string)
            .or_else(|| missing_context_value(expression))
    }

    fn resolve_context_data_value(&self, expression: &str) -> Option<&Value> {
        let mut segments = expression.trim().split('.');
        let root = segments.next()?;
        let mut value = self.context_data.get(root)?;
        for segment in segments {
            value = value.get(segment)?;
        }
        Some(value)
    }
}

/// Return true only when a step condition is provably false from immutable
/// GitHub job context before execution begins. Local composite actions are
/// prepared mid-job by actions/runner, after their parent condition has been
/// evaluated; this narrow preflight proof lets the planner preserve that
/// behavior without treating runtime `steps`, `needs`, or `env` state as
/// known early.
pub(crate) fn condition_is_statically_false(
    condition: Option<&str>,
    base_env: &[(String, String)],
    context_data: &[(String, Value)],
) -> bool {
    let Some(condition) = condition else {
        return false;
    };
    immutable_github_expression_is_false(
        strip_expression(condition),
        &JobExecutionState::new_with_context(base_env, context_data),
    )
}

fn immutable_github_expression_is_false(expression: &str, state: &JobExecutionState) -> bool {
    let expression = expression.trim();
    if let Some(inner) = strip_wrapping_parentheses(expression) {
        return immutable_github_expression_is_false(inner, state);
    }
    // A conjunction is false when any operand is independently provable
    // false. GitHub's broker prefixes ordinary step conditions with
    // `success() &&`; the immutable operand can still prove the whole value.
    if let Some((left, right)) = split_top_level(expression, "&&") {
        return immutable_github_expression_is_false(left, state)
            || immutable_github_expression_is_false(right, state);
    }
    // A disjunction is false only when both operands are independently
    // provable false.
    if let Some((left, right)) = split_top_level(expression, "||") {
        return immutable_github_expression_is_false(left, state)
            && immutable_github_expression_is_false(right, state);
    }
    let lower = expression.to_ascii_lowercase();
    if !lower.contains("github.")
        || [
            "steps.",
            "needs.",
            "env.",
            "job.",
            "matrix.",
            "strategy.",
            "runner.",
            "secrets.",
        ]
        .iter()
        .any(|runtime| lower.contains(runtime))
    {
        return false;
    }
    !state.evaluate_condition(Some(expression))
}

fn missing_context_value(expression: &str) -> Option<String> {
    let expression = expression.trim();
    if expression.starts_with("github.event.") {
        return Some(String::new());
    }
    let root = expression.split('.').next()?;
    matches!(root, "matrix" | "needs" | "inputs" | "vars" | "secrets").then(String::new)
}

fn evaluate_job_outputs(
    job_outputs: Option<&Value>,
    state: &JobExecutionState,
) -> BTreeMap<String, String> {
    job_output_pairs(job_outputs)
        .into_iter()
        .filter_map(|(name, value)| {
            let value = state.resolve_expressions(&value);
            (!value.is_empty()).then_some((name, value))
        })
        .collect()
}

fn evaluate_environment_url(
    environment_url: Option<&Value>,
    state: &JobExecutionState,
) -> Option<String> {
    environment_url
        .and_then(template_string_value)
        .map(|value| state.resolve_expressions(value))
        .filter(|value| !value.is_empty())
}

fn template_string_value(value: &Value) -> Option<&str> {
    if let Some(value) = value.as_str() {
        return Some(value);
    }
    value
        .as_object()
        .and_then(|object| {
            object
                .get("value")
                .or_else(|| object.get("Value"))
                .or_else(|| object.get("expression"))
                .or_else(|| object.get("Expression"))
                .or_else(|| object.get("lit"))
                .or_else(|| object.get("Lit"))
        })
        .and_then(template_string_value)
}

fn job_output_pairs(job_outputs: Option<&Value>) -> Vec<(String, String)> {
    match job_outputs {
        Some(Value::Object(outputs)) => {
            // Template-token mapping objects carry the pairs under "map"
            // (alongside positional metadata like file/line/col, with or
            // without a "type" tag — GitHub's broker omits "type" on the
            // wire). Observed live: JobOutputs = {col,file,line,map:[...]}.
            if let Some(map) = outputs.get("map").or_else(|| outputs.get("Map")) {
                return job_output_pairs(Some(map));
            }
            outputs
                .iter()
                .filter(|(name, _)| {
                    !name.eq_ignore_ascii_case("type")
                        && !name.eq_ignore_ascii_case("file")
                        && !name.eq_ignore_ascii_case("line")
                        && !name.eq_ignore_ascii_case("col")
                })
                .filter_map(|(name, value)| {
                    job_output_expression(value).map(|value| (name.clone(), value))
                })
                .collect()
        }
        Some(Value::Array(outputs)) => outputs.iter().filter_map(job_output_pair_value).collect(),
        _ => Vec::new(),
    }
}

fn job_output_pair_value(value: &Value) -> Option<(String, String)> {
    match value {
        Value::Object(object) => {
            let key = object.get("Key").or_else(|| object.get("key"))?;
            let value = object.get("Value").or_else(|| object.get("value"))?;
            Some((
                job_output_name(key)?.to_string(),
                job_output_expression(value)?,
            ))
        }
        Value::Array(pair) if pair.len() == 2 => Some((
            job_output_name(&pair[0])?.to_string(),
            job_output_expression(&pair[1])?,
        )),
        _ => None,
    }
}

fn job_output_name(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value.as_object().and_then(|object| {
            object
                .get("value")
                .or_else(|| object.get("Value"))
                .or_else(|| object.get("lit"))
                .or_else(|| object.get("Lit"))
                .and_then(job_output_name)
        })
    })
}

fn job_output_expression(value: &Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        return Some(value.to_string());
    }
    let object = value.as_object()?;
    // Bare expression tokens ({"expr": "steps.x.outputs.y", ...}) need the
    // ${{ }} wrapper so resolve_expressions evaluates them lazily — the
    // broker sends job-output values in exactly this shape.
    if let Some(expr) = object
        .get("expr")
        .or_else(|| object.get("Expr"))
        .and_then(Value::as_str)
    {
        return Some(format!("${{{{ {expr} }}}}"));
    }
    object
        .get("value")
        .or_else(|| object.get("Value"))
        .or_else(|| object.get("expression"))
        .or_else(|| object.get("Expression"))
        .or_else(|| object.get("lit"))
        .or_else(|| object.get("Lit"))
        .and_then(job_output_expression)
}

fn reserve_github_post_step_orders(current_order: &mut i32, visible_post_step_count: usize) {
    if visible_post_step_count == 0 {
        return;
    }
    let complete_order = (*current_order * 2) + 1;
    let first_post_order = complete_order - visible_post_step_count as i32;
    *current_order = (*current_order).max(first_post_order - 1);
}

fn step_log(step_id: &str, order: i32, result: &StepExecutionResult) -> StepLog {
    let now = unix_now_rfc3339();
    step_log_with_name(step_id, "", order, &now, &now, result, &[])
}

fn step_log_prelude(step: &ExecutableStep, state: &JobExecutionState) -> Vec<String> {
    match step {
        ExecutableStep::CompositeStart { .. }
        | ExecutableStep::CompositeEnd { .. }
        | ExecutableStep::CompositeOutputs { .. } => Vec::new(),
        ExecutableStep::Checkout(plan) => checkout_log_prelude(plan, state),
        ExecutableStep::Script(step) => script_log_prelude(step, state),
        ExecutableStep::JavaScript { invocation, .. } => {
            action_log_prelude(&invocation.inputs, &invocation.env, state)
        }
        ExecutableStep::Docker { invocation, .. } => {
            action_log_prelude(&invocation.inputs, &invocation.env, state)
        }
        ExecutableStep::Native { invocation, .. } => {
            action_log_prelude(&invocation.inputs, &invocation.env, state)
        }
    }
}

fn native_post_log_prelude(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Vec<String> {
    action_log_prelude(&action.inputs, &action.env, state)
}

fn javascript_post_log_prelude(
    action: &JavaScriptActionInvocation,
    state: &JobExecutionState,
) -> Vec<String> {
    action_log_prelude(&action.inputs, &action.env, state)
}

fn script_log_prelude(step: &ScriptStep, state: &JobExecutionState) -> Vec<String> {
    let mut lines = Vec::new();
    let script = state.resolve_expressions(&step.script);
    if !script.trim().is_empty() {
        // GitHub prints the script body in bold cyan inside the header group
        // (no repeated `Run …` line — the group title already carries it).
        lines.extend(
            script
                .lines()
                .map(|line| format!("\u{1b}[36;1m{line}\u{1b}[0m")),
        );
    }
    lines.push(format!("shell: {}", shell_log_name(step.shell)));
    if step.working_directory_container != "/__w" {
        lines.push(format!(
            "working-directory: {}",
            step.working_directory_container
        ));
    }
    append_env_lines(&mut lines, &state.prelude_env(&step.env));
    lines
}

fn checkout_log_prelude(plan: &CheckoutPlan, state: &JobExecutionState) -> Vec<String> {
    // Input order and default set mirror the GitHub-hosted actions/checkout
    // header (which prints declared defaults for unset inputs too).
    let mut inputs: Vec<(String, String)> = Vec::new();
    inputs.push((
        "repository".to_string(),
        checkout_repository_for_log(&plan.clone_url),
    ));
    if let Some(version) = &plan.version {
        inputs.push(("ref".to_string(), state.resolve_expressions(version)));
    }
    inputs.push(("token".to_string(), "***".to_string()));
    inputs.push(("ssh-strict".to_string(), "true".to_string()));
    inputs.push(("ssh-user".to_string(), "git".to_string()));
    inputs.push((
        "persist-credentials".to_string(),
        plan.persist_credentials.to_string(),
    ));
    inputs.push(("clean".to_string(), plan.clean.to_string()));
    inputs.push(("sparse-checkout-cone-mode".to_string(), "true".to_string()));
    inputs.push((
        "fetch-depth".to_string(),
        plan.fetch_depth
            .map_or_else(|| "0".to_string(), |depth| depth.to_string()),
    ));
    inputs.push(("fetch-tags".to_string(), plan.fetch_tags.to_string()));
    inputs.push(("show-progress".to_string(), "true".to_string()));
    inputs.push(("lfs".to_string(), plan.lfs.to_string()));
    inputs.push(("submodules".to_string(), "false".to_string()));
    inputs.push(("set-safe-directory".to_string(), "true".to_string()));

    let mut lines = Vec::new();
    append_with_pairs(&mut lines, &inputs, state);
    append_env_lines(&mut lines, &state.prelude_env(&[]));
    lines
}

fn append_with_pairs(
    lines: &mut Vec<String>,
    inputs: &[(String, String)],
    state: &JobExecutionState,
) {
    if inputs.is_empty() {
        return;
    }
    lines.push("with:".to_string());
    for (name, value) in inputs {
        lines.push(format!(
            "  {name}: {}",
            redact_log_value(name, &state.resolve_expressions(value))
        ));
    }
}

fn action_log_prelude(
    inputs: &BTreeMap<String, String>,
    env: &[(String, String)],
    state: &JobExecutionState,
) -> Vec<String> {
    let mut lines = Vec::new();
    append_with_lines(&mut lines, inputs, state);
    let explicit_env = env
        .iter()
        .filter(|(name, _)| {
            !name.starts_with("GITHUB_ACTION")
                && !name.starts_with("INPUT_")
                && name != "GITHUB_ACTION_PATH"
                && name != "GITHUB_ACTION_REPOSITORY"
                && name != "GITHUB_ACTION_REF"
        })
        .cloned()
        .collect::<Vec<_>>();
    append_env_lines(&mut lines, &state.prelude_env(&explicit_env));
    lines
}

fn append_with_lines(
    lines: &mut Vec<String>,
    inputs: &BTreeMap<String, String>,
    state: &JobExecutionState,
) {
    if inputs.is_empty() {
        return;
    }
    lines.push("with:".to_string());
    for (name, value) in inputs {
        lines.push(format!(
            "  {name}: {}",
            redact_log_value(name, &state.resolve_expressions(value))
        ));
    }
}

fn append_env_lines(lines: &mut Vec<String>, env: &[(String, String)]) {
    if env.is_empty() {
        return;
    }
    lines.push("env:".to_string());
    for (name, value) in env {
        lines.push(format!("  {name}: {}", redact_log_value(name, value)));
    }
}

fn redact_log_value(name: &str, value: &str) -> String {
    let name = name.to_ascii_lowercase();
    if name.contains("token")
        || name.contains("password")
        || name.contains("secret")
        || name.contains("authorization")
        || name == "ssh-key"
        || name.ends_with("_ssh_key")
        || name.ends_with("-ssh-key")
        || name == "private-key"
        || name.ends_with("_private_key")
        || name.ends_with("-private-key")
        || looks_like_sensitive_value(value)
    {
        "***".to_string()
    } else {
        value.to_string()
    }
}

fn looks_like_sensitive_value(value: &str) -> bool {
    value.contains("-----BEGIN ") && value.contains(" PRIVATE KEY-----")
}

fn shell_log_name(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => "bash --noprofile --norc -e -o pipefail {0}",
        Shell::BashDefault => "bash -e {0}",
        Shell::Sh => "sh -e {0}",
    }
}

fn checkout_repository_for_log(clone_url: &str) -> String {
    clone_url
        .strip_prefix("https://github.com/")
        .and_then(|value| value.strip_suffix(".git"))
        .unwrap_or(clone_url)
        .to_string()
}

fn step_log_with_name(
    step_id: &str,
    display_name: &str,
    order: i32,
    started_at: &str,
    completed_at: &str,
    result: &StepExecutionResult,
    prelude: &[String],
) -> StepLog {
    let lines = step_log_lines(
        display_name,
        &result.stdout,
        &result.stderr,
        result.skipped,
        prelude,
    );
    StepLog {
        step_id: step_id.to_string(),
        display_name: display_name.to_string(),
        order,
        started_at: started_at.to_string(),
        completed_at: completed_at.to_string(),
        lines,
        masks: result.state.masks.clone(),
        annotations: result.state.annotations.clone(),
        telemetry: result.state.telemetry.clone(),
        exit_code: result.exit_code,
        skipped: result.skipped,
        failure_ignored: result.failure_ignored,
        error_count: result.state.error_count,
        warning_count: result.state.warning_count,
        notice_count: result.state.notice_count,
        summary: result.state.summary.clone(),
    }
}

/// Extract workflow backend IDs from the ACTIONS_RUNTIME_TOKEN JWT.
///
/// The token's `scp` claim contains `Actions.Results:{workflowRunBackendId}:{workflowJobRunBackendId}`.
/// These are the IDs needed for Results Service Twirp calls (CreateArtifact etc.).
fn artifact_backend_ids_from_token(token: &str) -> Option<(String, String)> {
    use base64::Engine;
    let parts: Vec<&str> = token.splitn(3, '.').collect();
    let payload_b64 = parts.get(1)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload_b64))
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    let scp = json.get("scp")?.as_str()?;
    for part in scp.split(' ') {
        if let Some(rest) = part.strip_prefix("Actions.Results:") {
            let ids: Vec<&str> = rest.splitn(2, ':').collect();
            if ids.len() == 2 {
                return Some((ids[0].to_string(), ids[1].to_string()));
            }
        }
    }
    None
}

fn unix_now_rfc3339() -> String {
    use time::{format_description, OffsetDateTime};
    // Second-precision RFC3339 for step START/COMPLETION METADATA fields
    // (started_at/completed_at) only. This is NOT a log-line prefix: log-line
    // timestamps live in runner.rs (`blob_log_lines`, 7-digit sub-seconds) —
    // see docs/log-format-contract.md before touching either. A previous
    // version of this comment claimed blob-line prefixes need second
    // precision; that was wrong and caused a UI regression when copied.
    let fmt =
        format_description::parse_borrowed::<1>("[year]-[month]-[day]T[hour]:[minute]:[second]Z")
            .unwrap_or_else(|_| vec![]);
    OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .map(|t| {
            t.format(&fmt)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
        })
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Log lines for a skipped step. A skipped step still posts a log record, and
/// an EMPTY record is indistinguishable from a quiet successful step — that
/// hid the tailrocks/velnor#311 cascade where a daemon restart killed a step
/// mid-flight and every later (export/upload) step silently "succeeded"
/// without running. One explicit, ungrouped line keeps the skip visible in
/// the timeline feed. Skip semantics are unchanged (exit 0, skipped=true);
/// only visibility changes.
fn skipped_step_log_lines() -> Vec<String> {
    vec!["Step skipped: condition evaluated to false (a previous step failed or the if-condition was not met)".to_string()]
}

fn step_log_lines(
    display_name: &str,
    stdout: &str,
    stderr: &str,
    skipped: bool,
    prelude: &[String],
) -> Vec<String> {
    if skipped {
        return skipped_step_log_lines();
    }
    let step_name = if display_name.is_empty() {
        "step"
    } else {
        display_name
    };
    // GitHub's step log groups ONLY the header (command + with:/env:); output
    // stays visible below it, with `::group::`/`::warning::`-style workflow
    // commands converted IN PLACE to `##[...]` markers (actions/runner keeps
    // their position; reordering them to the end breaks user grouping).
    let mut lines = vec![format!("##[group]{step_name}")];
    lines.extend(prelude.iter().cloned());
    lines.push("##[endgroup]".to_string());
    lines.extend(rendered_output_lines(stdout, stderr));
    lines
}

/// Render process output for the uploaded blob the way actions/runner does:
/// grouping/annotation workflow commands become `##[...]` markers at their
/// original position, state-changing commands (set-env, add-mask, …) are
/// consumed invisibly, everything else (including user ANSI) passes verbatim.
fn rendered_output_lines(stdout: &str, stderr: &str) -> Vec<String> {
    stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(rendered_output_line)
        .collect()
}

fn rendered_output_line(line: &str) -> Option<String> {
    let Some(rest) = line.strip_prefix("::") else {
        return Some(line.to_string());
    };
    let Some((command, value)) = rest.split_once("::") else {
        // Not a complete workflow command; GitHub passes such lines through.
        return Some(line.to_string());
    };
    let keyword = command
        .split_once(' ')
        .map_or(command, |(keyword, _)| keyword);
    match keyword {
        "group" | "endgroup" => Some(if keyword == "group" {
            format!("##[group]{value}")
        } else {
            "##[endgroup]".to_string()
        }),
        "error" | "warning" | "notice" | "debug" => Some(format!("##[{keyword}]{value}")),
        // set-env / set-output / add-mask / save-state / add-path / echo /
        // stop-commands…: consumed into state, never shown.
        _ => None,
    }
}

fn parse_workflow_commands_from_output(stdout: &str, stderr: &str) -> StepCommandState {
    let mut state = parse_workflow_commands(stdout);
    state.merge(parse_workflow_commands(stderr));
    state
}

fn prepare_github_event_path(
    temp_host: &Path,
    context_data: &[(String, Value)],
) -> Result<Option<String>> {
    let Some(payload) = github_event_payload(context_data) else {
        return Ok(None);
    };
    let event_dir = temp_host.join("_github_workflow");
    fs::create_dir_all(&event_dir).with_context(|| format!("create {}", event_dir.display()))?;
    let event_path = event_dir.join("event.json");
    fs::write(&event_path, payload).with_context(|| format!("write {}", event_path.display()))?;
    Ok(Some("/github/workflow/event.json".to_string()))
}

fn github_event_payload(context_data: &[(String, Value)]) -> Option<String> {
    let github = context_data
        .iter()
        .find_map(|(name, value)| (name == "github").then_some(value))?;
    let event = github.get("event")?;
    match event {
        Value::String(value) => Some(value.clone()),
        Value::Null => None,
        value => serde_json::to_string(value).ok(),
    }
}

fn context_value_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some(String::new()),
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn parse_contains(expression: &str) -> Option<(&str, &str)> {
    let inner = expression
        .trim()
        .strip_prefix("contains(")?
        .strip_suffix(')')?;
    split_top_level(inner, ",")
}

fn parse_to_json(expression: &str) -> Option<&str> {
    expression
        .trim()
        .strip_prefix("toJSON(")?
        .strip_suffix(')')
        .map(str::trim)
}

fn parse_hash_files(expression: &str) -> Option<Vec<String>> {
    let inner = expression
        .trim()
        .strip_prefix("hashFiles(")?
        .strip_suffix(')')?;
    let mut patterns = Vec::new();
    let mut rest = inner.trim();
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.starts_with(',') {
            rest = rest[1..].trim_start();
            continue;
        }
        let quote = rest.chars().next()?;
        if quote != '\'' && quote != '"' {
            return None;
        }
        let value_start = quote.len_utf8();
        let value_end = rest[value_start..].find(quote)? + value_start;
        patterns.push(rest[value_start..value_end].to_string());
        rest = rest[value_end + quote.len_utf8()..].trim_start();
        if rest.starts_with(',') {
            rest = rest[1..].trim_start();
        } else if !rest.is_empty() {
            return None;
        }
    }
    Some(patterns)
}

fn hash_files(workspace: &Path, patterns: &[String]) -> String {
    let Ok(globs) = build_ordered_globs(patterns) else {
        return String::new();
    };
    let search_roots = hash_file_search_roots(workspace, patterns);
    let mut seen = BTreeSet::new();
    let mut matches = Vec::new();
    // actions/runner's bundled @actions/glob generator preserves its ordered
    // search roots, removes roots covered by another candidate ancestor, and
    // traverses each remaining root lexically with the complete matcher set.
    for root in search_roots {
        let mut files = Vec::new();
        if root.is_file() {
            files.push(root);
        } else {
            collect_workspace_files(&root, &mut files);
            files.sort();
        }
        for path in files {
            let Ok(relative) = path.strip_prefix(workspace) else {
                continue;
            };
            let relative = normalize_path(relative);
            if ordered_globs_match(&globs, &relative) && seen.insert(path.clone()) {
                matches.push(path);
            }
        }
    }
    if matches.is_empty() {
        return String::new();
    }

    let mut digests: Vec<_> = matches
        .par_iter()
        .enumerate()
        .filter_map(|(index, path)| sha256_file_digest(path).ok().map(|digest| (index, digest)))
        .collect();
    digests.sort_by_key(|(index, _)| *index);

    let mut aggregate = Sha256::new();
    for (_, digest) in digests {
        aggregate.update(digest);
    }
    let digest = aggregate.finalize();
    hex_digest(&digest)
}

fn build_ordered_globs(patterns: &[String]) -> Result<Vec<(bool, globset::GlobMatcher)>> {
    let mut globs = Vec::new();
    for pattern in patterns {
        let (negative, pattern) = pattern
            .strip_prefix('!')
            .map_or((false, pattern.as_str()), |pattern| (true, pattern));
        globs.push((
            negative,
            GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()?
                .compile_matcher(),
        ));
    }
    Ok(globs)
}

fn ordered_globs_match(globs: &[(bool, globset::GlobMatcher)], path: &str) -> bool {
    let mut matched = false;
    for (negative, matcher) in globs {
        if matcher.is_match(path) {
            matched = !negative;
        }
    }
    matched
}

fn hash_file_search_roots(workspace: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let candidates = patterns
        .iter()
        .filter_map(|pattern| {
            let pattern = pattern.strip_prefix('!').unwrap_or(pattern);
            if pattern.is_empty() {
                return None;
            }
            let mut root = PathBuf::new();
            for segment in pattern.split('/') {
                if segment.contains(['*', '?', '[', ']', '{', '}']) {
                    break;
                }
                if !segment.is_empty() && segment != "." {
                    root.push(segment);
                }
            }
            Some(workspace.join(root))
        })
        .collect::<Vec<_>>();

    candidates
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            !candidates.iter().enumerate().any(|(other_index, other)| {
                index != &other_index && candidate != &other && candidate.starts_with(other)
            })
        })
        .map(|(_, candidate)| candidate.clone())
        .collect()
}

fn build_globs(patterns: &[String]) -> Result<globset::GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }
    builder.build().context("build glob set")
}

fn collect_workspace_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_workspace_files(&path, files);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        files.push(path);
    }
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn split_top_level<'a>(expression: &'a str, operator: &str) -> Option<(&'a str, &'a str)> {
    let mut depth = 0_i32;
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in expression.char_indices() {
        if let Some(quote_char) = quote {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == quote_char {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth == 0 && expression[index..].starts_with(operator) {
            return Some((
                expression[..index].trim(),
                expression[index + operator.len()..].trim(),
            ));
        }
    }
    None
}

fn strip_wrapping_parentheses(expression: &str) -> Option<&str> {
    let expression = expression.trim();
    let inner = expression.strip_prefix('(')?.strip_suffix(')')?;
    let mut depth = 0_i32;
    for (index, ch) in expression.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && index != expression.len() - 1 {
                    return None;
                }
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    (depth == 0).then_some(inner.trim())
}

fn strip_expression(condition: &str) -> &str {
    condition
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map(str::trim)
        .unwrap_or(condition)
}

fn expression_truthy(value: &str) -> bool {
    let value = value.trim();
    !(value.is_empty()
        || value.eq_ignore_ascii_case("false")
        || value == "0"
        || value.eq_ignore_ascii_case("null"))
}

fn github_string_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn github_contains(value: &str, needle: &str) -> bool {
    value
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn condition_returns_bool(expression: &str) -> bool {
    let expression = expression.trim();
    if matches!(
        expression,
        "always()" | "success()" | "failure()" | "cancelled()" | "true" | "false"
    ) {
        return true;
    }
    if expression.starts_with('!') {
        return true;
    }
    let expression = strip_wrapping_parentheses(expression).unwrap_or(expression);
    split_top_level(expression, "||").is_some()
        || split_top_level(expression, "&&").is_some()
        || split_top_level(expression, "!=").is_some()
        || split_top_level(expression, "==").is_some()
        || parse_contains(expression).is_some()
}

fn condition_has_status_check(expression: &str) -> bool {
    ["always()", "success()", "failure()", "cancelled()"]
        .iter()
        .any(|function| expression.contains(function))
}

fn is_quoted(value: &str) -> bool {
    (value.starts_with('\'') && value.ends_with('\''))
        || (value.starts_with('"') && value.ends_with('"'))
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .or_else(|| {
            value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or(value)
}

fn exit_code(status: ExitStatus) -> Result<i32> {
    status
        .code()
        .ok_or_else(|| anyhow::anyhow!("process terminated by signal"))
}

fn effective_step_timeout(
    step_timeout_minutes: Option<u64>,
    job_timeout_minutes: Option<u64>,
) -> Duration {
    Duration::from_secs(
        step_timeout_minutes
            .or(job_timeout_minutes)
            .unwrap_or(DEFAULT_STEP_TIMEOUT_MINUTES)
            * 60,
    )
}

fn timeout_command_result(stdout: String, mut stderr: String) -> CommandResult {
    if !stderr.is_empty() {
        stderr.push('\n');
    }
    stderr.push_str("##[error]The operation was canceled because it exceeded timeout-minutes.\n");
    CommandResult {
        code: 124,
        stdout,
        stderr,
    }
}

fn spawn_docker_timeout_watchdog(
    program: &str,
    args: &[String],
    child_pid: u32,
    timeout: Duration,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    mpsc::Sender<()>,
    Option<thread::JoinHandle<()>>,
) {
    let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (watchdog_cancel, watchdog_cancelled) = mpsc::channel();
    let container_name = docker_timeout_container_name(program, args);
    let timed_out_thread = std::sync::Arc::clone(&timed_out);
    let watchdog = Some(thread::spawn(move || {
        if watchdog_cancelled.recv_timeout(timeout).is_err() {
            timed_out_thread.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(container_name) = container_name {
                let _ = Command::new("docker")
                    .arg("kill")
                    .arg(container_name)
                    .status();
            }
            let _ = Command::new("/bin/kill")
                .args(["-KILL", &child_pid.to_string()])
                .status();
        }
    }));
    (timed_out, watchdog_cancel, watchdog)
}

fn docker_timeout_container_name(program: &str, args: &[String]) -> Option<String> {
    if program != "docker" {
        return None;
    }
    match args.first().map(String::as_str) {
        Some("exec") => docker_exec_container_name(&args[1..]),
        Some("run") => docker_run_container_name(&args[1..]),
        _ => None,
    }
}

fn docker_exec_container_name(args: &[String]) -> Option<String> {
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-i" | "-t" | "--interactive" | "--tty" | "--privileged" => index += 1,
            "-w" | "--workdir" | "-e" | "--env" | "--env-file" | "-u" | "--user" => index += 2,
            value if value.starts_with('-') => return None,
            _ => return Some(args[index].clone()),
        }
    }
    None
}

fn docker_run_container_name(args: &[String]) -> Option<String> {
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--name" => return args.get(index + 1).cloned(),
            "-v" | "--volume" | "--workdir" | "-w" | "-e" | "--env" | "--env-file"
            | "--network" | "--entrypoint" | "--user" | "-u" => index += 2,
            value if value.starts_with("--name=") => {
                return value.strip_prefix("--name=").map(ToString::to_string);
            }
            _ => index += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn process_runner_forwards_multiline_env_on_all_timed_paths() {
        let env = &[(
            "VELNOR_MULTILINE".to_string(),
            "line-one\nline-two".to_string(),
        )];
        let args = &[
            "-c".to_string(),
            "printf %s \"$VELNOR_MULTILINE\"".to_string(),
        ];
        let mut runner = ProcessCommandRunner;

        let regular = runner
            .run_timeout_with_env("sh", args, env, Duration::from_secs(5))
            .unwrap();
        assert_eq!(regular.stdout, "line-one\nline-two");

        let mut streamed = String::new();
        let mut on_output = |_: CommandStream, line: &str| streamed.push_str(line);
        let streaming = runner
            .run_streaming_timeout_with_env("sh", args, env, Duration::from_secs(5), &mut on_output)
            .unwrap();
        assert_eq!(streaming.stdout, "line-one\nline-two\n");
        assert_eq!(streamed, "line-oneline-two");

        let stdin_args = &[
            "-c".to_string(),
            "read input; printf '%s|%s' \"$VELNOR_MULTILINE\" \"$input\"".to_string(),
        ];
        let with_stdin = runner
            .run_with_stdin_timeout_with_env(
                "sh",
                stdin_args,
                env,
                "payload\n",
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(with_stdin.stdout, "line-one\nline-two|payload");
    }

    #[test]
    fn compiler_cache_setup_scripts_never_download_tools() {
        let sccache = sccache_setup_script();
        let kache = kache_setup_script();
        for script in [&sccache, &kache] {
            assert!(!script.contains("curl"));
            assert!(!script.contains("wget"));
        }
        assert!(sccache.contains("0.16.0"));
        assert!(kache.contains("0.14.2"));
        assert!(!sccache.contains("node"));
        assert!(!kache.contains("node"));
    }

    #[test]
    fn compiler_cache_post_actions_always_run() {
        assert_eq!(
            native_post_condition(NativeActionAdapter::Sccache, None),
            Some("always()")
        );
        assert_eq!(
            native_post_condition(NativeActionAdapter::Kache, None),
            Some("always()")
        );
    }

    #[test]
    fn prepared_jackin_tools_require_the_complete_bundle() {
        let root = temp_dir();
        fs::create_dir_all(&root).unwrap();
        let temp = root.join("temp");
        fs::create_dir_all(&temp).unwrap();
        let state = JobExecutionState::new_with_workspace(&[], &[], &root, &temp);
        fs::write(root.join("cargo-fuzz"), b"tool").unwrap();
        assert!(!prepared_jackin_tools_complete(&state, &root));
        for tool in [
            "sccache",
            "cargo-nextest",
            "cargo-deny",
            "cargo-shear",
            "cargo-audit",
            "cargo-dylint",
            "cargo-hack",
            "cargo-hakari",
            "cargo-llvm-cov",
            "cargo-mutants",
            "cargo-zigbuild",
            "dylint-link",
            "weaver",
        ] {
            fs::write(root.join(tool), b"tool").unwrap();
        }
        assert!(prepared_jackin_tools_complete(&state, &root));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repository_artifact_requires_same_trusted_run_identity() {
        let artifact = serde_json::json!({
            "expired": false,
            "id": 42,
            "workflow_run": {
                "head_branch": "main",
                "head_repository_id": 7,
                "head_sha": "abc123",
                "repository_id": 7,
                "event": "push"
            }
        });
        assert!(repository_artifact_matches_trusted_producer(
            &artifact,
            "tailrocks/velnor",
            "7",
            "abc123",
            "refs/heads/main"
        ));

        for (sha, repository_id, reference, event) in [
            ("other", "7", "refs/heads/main", ""),
            ("abc123", "8", "refs/heads/main", ""),
            ("abc123", "7", "refs/heads/feature", ""),
            ("abc123", "7", "refs/heads/main", "pull_request"),
        ] {
            let mut candidate = artifact.clone();
            candidate["workflow_run"]["event"] = serde_json::Value::String(event.into());
            assert!(!repository_artifact_matches_trusted_producer(
                &candidate,
                "tailrocks/velnor",
                repository_id,
                sha,
                reference
            ));
        }
        let run = serde_json::json!({
            "status": "completed",
            "conclusion": "success",
            "path": ".github/workflows/rust-nextest.yml",
            "event": "push",
            "repository_id": 7,
            "head_repository_id": 7,
            "head_sha": "abc123",
            "head_branch": "main",
            "run_attempt": 1
        });
        assert!(repository_artifact_run_is_trusted(
            &run,
            "7",
            "abc123",
            "refs/heads/main",
            ".github/workflows/rust-nextest.yml"
        ));
        for (status, conclusion, event) in [
            ("in_progress", "success", "push"),
            ("completed", "failure", "push"),
            ("completed", "success", "pull_request"),
        ] {
            let mut candidate = run.clone();
            candidate["status"] = status.into();
            candidate["conclusion"] = conclusion.into();
            candidate["event"] = event.into();
            assert!(!repository_artifact_run_is_trusted(
                &candidate,
                "7",
                "abc123",
                "refs/heads/main",
                ".github/workflows/rust-nextest.yml"
            ));
        }
    }

    #[test]
    fn prepared_jackin_tool_exports_use_container_paths() {
        let root = temp_dir();
        let temp = root.join("temp");
        let tools = root.join(".ci-prebuilt-tools");
        let xtask = root.join(".ci-prebuilt-xtask");
        fs::create_dir_all(&temp).unwrap();
        fs::create_dir_all(&tools).unwrap();
        fs::create_dir_all(&xtask).unwrap();
        for tool in [
            "sccache",
            "cargo-nextest",
            "cargo-deny",
            "cargo-shear",
            "cargo-audit",
            "cargo-dylint",
            "cargo-fuzz",
            "cargo-hack",
            "cargo-hakari",
            "cargo-llvm-cov",
            "cargo-mutants",
            "cargo-zigbuild",
            "dylint-link",
            "weaver",
        ] {
            fs::write(tools.join(tool), b"tool").unwrap();
        }
        fs::write(xtask.join("jackin-xtask"), b"xtask").unwrap();
        fs::write(xtask.join("workspace-metadata.json"), b"{}").unwrap();
        let env = [
            ("JACKIN_WORKSPACE".into(), "/__w".into()),
            ("JACKIN_INCLUDE_TOOLS".into(), "true".into()),
            ("JACKIN_INCLUDE_XTASK".into(), "true".into()),
        ];
        let mut state = JobExecutionState::new_with_workspace(&[], &[], &root, &temp);
        state.outputs.insert(
            "tools-cache".into(),
            [("cache-hit".into(), "true".into())].into(),
        );
        state.outputs.insert(
            "xtask-cache".into(),
            [("cache-hit".into(), "true".into())].into(),
        );
        let action_state = JobExecutionState::new(&env);
        let result = native_restore_prepared_jackin_tools(&action_state, &state).unwrap();
        assert_eq!(result.state.env["CI_TOOLS_PATH"], "/__w/.ci-prebuilt-tools");
        assert_eq!(
            result.state.env["CI_CARGO_FUZZ"],
            "/__w/.ci-prebuilt-tools/cargo-fuzz"
        );
        assert_eq!(
            result.state.env["CI_XTASK"],
            "/__w/.ci-prebuilt-xtask/jackin-xtask"
        );
        assert_eq!(
            result.state.env["CI_METADATA"],
            "/__w/.ci-prebuilt-xtask/workspace-metadata.json"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_mise_emits_bounded_tool_prep_telemetry() {
        let temp = temp_dir();
        let mise_env_dir = temp.join("_velnor");
        fs::create_dir_all(&mise_env_dir).unwrap();
        fs::write(mise_env_dir.join("mise-env.json"), "{}").unwrap();
        fs::write(mise_env_dir.join("mise-env-redacted.json"), "{}").unwrap();

        let sink = Arc::new(
            crate::ops::OpsSink::open(temp.join("state.db"), "test-instance".to_owned()).unwrap(),
        );
        let admission = crate::ops::JobAdmission {
            instance_slug: "test-instance".to_owned(),
            job_uid: "job-tool-prep".to_owned(),
            repository_full_name: "tailrocks/velnor".to_owned(),
            workflow: "control-plane".to_owned(),
            job_name: "tool-prep".to_owned(),
            run_id: Some(42),
            attempt: Some(1),
            head_ref: Some("refs/heads/main".to_owned()),
            head_sha: Some("deadbeef".to_owned()),
            trigger_event: Some("workflow_dispatch".to_owned()),
            queued_at_rfc3339: None,
            slot_name: Some("slot-0".to_owned()),
            runner_name: Some("runner-0".to_owned()),
            trust_scope: Some("trusted".to_owned()),
            resource_policy: Some("standard".to_owned()),
            masks: vec!["secret-marker".to_owned()],
        };
        assert!(sink.record_admission(&admission));

        let steps = vec![ExecutableStep::Native {
            step_id: "mise".to_owned(),
            display_name: "mise".to_owned(),
            invocation: NativeActionInvocation {
                git_ref: "v4".to_owned(),
                adapter: NativeActionAdapter::Mise,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("install".to_owned(), "false".to_owned()),
                    ("cache_save".to_owned(), "false".to_owned()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default())
            .with_job_environment_started(true)
            .with_tool_prep_telemetry(Arc::clone(&sink), admission);

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].exit_code, 0);

        let telemetry = fs::read_to_string(temp.join("state.test-instance.telemetry.jsonl"))
            .expect("tool preparation telemetry");
        let record = telemetry
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|record| record["event"] == "tool_prep")
            .expect("tool_prep event");
        let fields = record["fields"].as_object().expect("tool_prep fields");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields["tool"], "mise");
        assert!(fields["ms"]
            .as_u64()
            .is_some_and(|value| value <= MAX_TOOL_PREP_TELEMETRY_MS));
        assert!(!telemetry.contains("secret-marker"));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_cache_lookup_emits_normalized_telemetry_on_success_and_error() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(&workspace).unwrap();
        let sink = Arc::new(
            crate::ops::OpsSink::open(temp.join("state.db"), "test-instance".to_owned()).unwrap(),
        );
        let admission = crate::ops::JobAdmission {
            instance_slug: "test-instance".to_owned(),
            job_uid: "job-cache-lookup".to_owned(),
            repository_full_name: "tailrocks/velnor".to_owned(),
            workflow: "control-plane".to_owned(),
            job_name: "cache-lookup".to_owned(),
            run_id: Some(42),
            attempt: Some(1),
            head_ref: Some("refs/heads/main".to_owned()),
            head_sha: Some("deadbeef".to_owned()),
            trigger_event: Some("workflow_dispatch".to_owned()),
            queued_at_rfc3339: None,
            slot_name: Some("slot-0".to_owned()),
            runner_name: Some("runner-0".to_owned()),
            trust_scope: Some("trusted".to_owned()),
            resource_policy: Some("standard".to_owned()),
            masks: vec!["secret-marker".to_owned()],
        };
        assert!(sink.record_admission(&admission));

        let action = |adapter, cache_kind, inputs: &[(&str, &str)]| NativeActionInvocation {
            git_ref: "cache-action".to_owned(),
            adapter,
            cache_kind,
            source_path: None,
            inputs: inputs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            env: Vec::new(),
        };
        let state = JobExecutionState::new_with_workspace(&[], &[], &workspace, &temp);
        let job_container = container(&temp);
        let mut executor = DockerJobEngine::new(RecordingRunner::default())
            .with_tool_prep_telemetry(Arc::clone(&sink), admission);

        let root_result = executor
            .execute_native_action_in_started_container(
                &job_container,
                "cache-root",
                &action(
                    NativeActionAdapter::Cache,
                    Some(CacheActionKind::Root),
                    &[("path", "/__w/cache"), ("key", "super-secret-key")],
                ),
                &state,
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(root_result.state.outputs["cache-hit"], "false");

        let restore_error = executor.execute_native_action_in_started_container(
            &job_container,
            "cache-restore",
            &action(
                NativeActionAdapter::Cache,
                Some(CacheActionKind::Restore),
                &[("path", "/__w/[invalid"), ("key", "restore-secret-key")],
            ),
            &state,
            Duration::from_secs(1),
        );
        assert!(restore_error.is_err());

        let rust_result = executor
            .execute_native_action_in_started_container(
                &job_container,
                "rust-cache",
                &action(
                    NativeActionAdapter::RustCache,
                    None,
                    &[
                        ("cache-directories", "/__w/rust-cache"),
                        ("shared-key", "rust-secret-key"),
                    ],
                ),
                &state,
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(rust_result.state.outputs["cache-hit"], "false");

        let telemetry = fs::read_to_string(temp.join("state.test-instance.telemetry.jsonl"))
            .expect("cache lookup telemetry");
        let records: Vec<_> = telemetry
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|record| record["event"] == "cache_lookup")
            .collect();
        assert_eq!(records.len(), 3);

        let root_fields = records[0]["fields"].as_object().unwrap();
        assert_eq!(root_fields["store"], "actions_cache");
        assert_eq!(root_fields["hit"], false);
        assert_eq!(root_fields["miss_reason"], "key_absent");
        assert!(root_fields["lookup_ms"]
            .as_u64()
            .is_some_and(|value| value <= MAX_CACHE_LOOKUP_TELEMETRY_MS));

        let error_fields = records[1]["fields"].as_object().unwrap();
        assert_eq!(error_fields["store"], "actions_cache");
        assert_eq!(error_fields["hit"], false);
        assert_eq!(error_fields["miss_reason"], "lookup_error");

        let rust_fields = records[2]["fields"].as_object().unwrap();
        assert_eq!(rust_fields["store"], "rust_cache");
        assert_eq!(rust_fields["hit"], false);
        assert_eq!(rust_fields["miss_reason"], "key_absent");
        assert!(!telemetry.contains("super-secret-key"));
        assert!(!telemetry.contains("restore-secret-key"));
        assert!(!telemetry.contains("rust-secret-key"));
        assert!(!telemetry.contains("/__w"));
        assert!(!telemetry.contains("invalid"));

        fs::remove_dir_all(temp).unwrap();
    }

    use crate::container::{ServiceContainerSpec, Shell};
    use std::{
        fs,
        path::PathBuf,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[cfg(unix)]
    #[test]
    fn process_command_runner_timeout_kills_sleep_child() {
        let start = std::time::Instant::now();
        let result = ProcessCommandRunner
            .run_timeout("sleep", &["30".into()], std::time::Duration::from_secs(1))
            .unwrap();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "watchdog must kill sleep, elapsed={:?}",
            start.elapsed()
        );
        assert_eq!(result.code, 124);
    }

    #[test]
    fn effective_timeout_prefers_step_over_job_over_default() {
        assert_eq!(
            effective_step_timeout(Some(2), Some(9)),
            Duration::from_secs(120)
        );
        assert_eq!(
            effective_step_timeout(None, Some(9)),
            Duration::from_secs(540)
        );
        assert_eq!(
            effective_step_timeout(None, None),
            Duration::from_secs(DEFAULT_STEP_TIMEOUT_MINUTES * 60)
        );
    }

    #[test]
    fn timeout_step_returns_failure_not_hang() {
        let result = timeout_command_result("stdout\n".into(), "stderr\n".into());

        assert_eq!(result.code, 124);
        assert_eq!(result.stdout, "stdout\n");
        assert!(result.stderr.contains("timeout-minutes"));
        assert!(result.stderr.contains("The operation was canceled"));
    }

    #[test]
    fn docker_timeout_container_name_finds_exec_and_run_targets() {
        let args = vec![
            "exec".to_string(),
            "-i".to_string(),
            "--workdir".to_string(),
            "/__w".to_string(),
            "--env-file".to_string(),
            "/tmp/env".to_string(),
            "-e".to_string(),
            "NAME=value".to_string(),
            "velnor-job-1".to_string(),
            "sh".to_string(),
            "-c".to_string(),
            "sleep 60".to_string(),
        ];

        assert_eq!(
            docker_timeout_container_name("docker", &args).as_deref(),
            Some("velnor-job-1")
        );
        let run_args = vec![
            "run".to_string(),
            "--rm".to_string(),
            "--name".to_string(),
            "velnor-node-action-velnor-job-1".to_string(),
            "node:24-bookworm".to_string(),
        ];
        assert_eq!(
            docker_timeout_container_name("docker", &run_args).as_deref(),
            Some("velnor-node-action-velnor-job-1")
        );
        assert_eq!(docker_timeout_container_name("git", &args), None);
    }

    #[derive(Default)]
    struct RecordingRunner {
        calls: Vec<(String, Vec<String>)>,
        stdin: Vec<String>,
        env: Vec<Vec<(String, String)>>,
        codes: Vec<i32>,
    }

    /// The per-job mise-store seed probe (`docker image inspect`) is
    /// infrastructure noise for call-sequence assertions: drop it unrecorded
    /// with empty stdout, which also short-circuits the seed copy itself.
    fn is_seed_probe(args: &[String]) -> bool {
        args.first().is_some_and(|a| a == "image") && args.get(1).is_some_and(|a| a == "inspect")
    }

    impl CommandRunner for RecordingRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            if is_seed_probe(args) {
                return Ok(CommandResult {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
            self.calls.push((program.to_string(), args.to_vec()));
            self.stdin.push(String::new());
            self.env.push(Vec::new());
            let code = if self.codes.is_empty() {
                0
            } else {
                self.codes.remove(0)
            };
            Ok(CommandResult {
                code,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        fn run_with_env(
            &mut self,
            program: &str,
            args: &[String],
            env: &[(String, String)],
        ) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            self.stdin.push(String::new());
            self.env.push(env.to_vec());
            let code = if self.codes.is_empty() {
                0
            } else {
                self.codes.remove(0)
            };
            Ok(CommandResult {
                code,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        fn run_with_stdin(
            &mut self,
            program: &str,
            args: &[String],
            stdin: &str,
        ) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            self.stdin.push(stdin.to_string());
            self.env.push(Vec::new());
            let code = if self.codes.is_empty() {
                0
            } else {
                self.codes.remove(0)
            };
            Ok(CommandResult {
                code,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct BuildkitCleanupRunner {
        calls: Vec<Vec<String>>,
    }

    impl CommandRunner for BuildkitCleanupRunner {
        fn run(&mut self, _program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push(args.to_vec());
            let stdout = match args.first().map(String::as_str) {
                Some("ps") => {
                    "bk1\tbuildx_buildkit_velnor-builder-job-scope0\tjob\t\tcreated\n\
                     bk2\tbuildx_buildkit_velnor-builder-job-scope0\t\t\tremoving\n"
                }
                Some("volume") if args.get(1).is_some_and(|arg| arg == "ls") => {
                    "buildx_buildkit_velnor-builder-job-scope0_state\n"
                }
                _ => "",
            };
            Ok(CommandResult {
                code: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
            })
        }
    }

    struct ServiceContextRunner;

    impl CommandRunner for ServiceContextRunner {
        fn run(&mut self, _program: &str, args: &[String]) -> Result<CommandResult> {
            let stdout = match args.first().map(String::as_str) {
                Some("inspect") => "container-id\n",
                Some("port") => "5432/tcp -> 0.0.0.0:32768\n5432/tcp -> [::]:32768\n",
                _ => "",
            };
            Ok(CommandResult {
                code: 0,
                stdout: stdout.into(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn service_context_exposes_mapped_ports() {
        let temp = temp_dir();
        let mut job = container(&temp);
        job.services.push(ServiceContainerSpec {
            name: "velnor-service-postgres".into(),
            image: "postgres:16".into(),
            network_alias: "postgres".into(),
            network: "velnor-net".into(),
            env: vec![("POSTGRES_PASSWORD".into(), "postgres".into())],
            ports: vec!["5432".into()],
            options: vec!["--health-cmd".into(), "pg_isready -U postgres".into()],
        });
        let mut executor = DockerJobEngine::new(ServiceContextRunner);
        let context = executor.service_context(&job).unwrap().unwrap();

        assert_eq!(context["postgres"]["id"], "container-id");
        assert_eq!(context["postgres"]["network"], "velnor-net");
        assert_eq!(context["postgres"]["ports"]["5432"], "32768");
        fs::remove_dir_all(temp).ok();
    }

    #[derive(Default)]
    struct GitDiffRunner {
        calls: Vec<(String, Vec<String>)>,
        stdout: String,
        missing_refs: bool,
    }

    impl CommandRunner for GitDiffRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            Ok(CommandResult {
                code: if self.missing_refs && args.iter().any(|arg| arg == "cat-file") {
                    1
                } else {
                    0
                },
                stdout: if program == "git" && args.iter().any(|arg| arg == "diff") {
                    self.stdout.clone()
                } else {
                    String::new()
                },
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct FailingPostRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for FailingPostRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let code = if args.iter().any(|arg| {
                arg.contains("/__a/_actions/sccache/dist/show_stats/index.js")
                    || arg.contains(
                        "/__a/_actions/mozilla-actions_sccache-action/dist/show_stats/index.js",
                    )
            }) {
                1
            } else {
                0
            };
            Ok(CommandResult {
                code,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct CheckoutOutputRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for CheckoutOutputRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let stdout = if program == "docker"
                && args.first().is_some_and(|arg| arg == "exec")
                && args.iter().any(|arg| arg == "/__t/source.sh")
            {
                "::set-output name=sha::def456\n".to_string()
            } else {
                String::new()
            };
            Ok(CommandResult {
                code: 0,
                stdout,
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    /// Step command execution fails by throwing (Err), not by exit code —
    /// the actions/runner exception path. `fail_execs` bounds how many
    /// `docker exec` invocations fail so later steps still run.
    struct ErroringExecRunner {
        calls: Vec<(String, Vec<String>)>,
        fail_execs: usize,
    }

    impl CommandRunner for ErroringExecRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            if is_seed_probe(args) {
                return Ok(CommandResult {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
            self.calls.push((program.to_string(), args.to_vec()));
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        fn run_streaming_timeout_with_env(
            &mut self,
            program: &str,
            args: &[String],
            _env: &[(String, String)],
            _timeout: Duration,
            _on_output: &mut dyn FnMut(CommandStream, &str),
        ) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            if args.first().is_some_and(|arg| arg == "exec") && self.fail_execs > 0 {
                self.fail_execs -= 1;
                anyhow::bail!("docker daemon connection lost");
            }
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct FailingCheckoutRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for FailingCheckoutRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            if is_seed_probe(args) {
                return Ok(CommandResult {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
            self.calls.push((program.to_string(), args.to_vec()));
            Ok(CommandResult {
                code: if program == "git" { 128 } else { 0 },
                stdout: String::new(),
                stderr: if program == "git" {
                    "fatal: couldn't find remote ref missing".into()
                } else {
                    String::new()
                },
            })
        }
    }

    struct OutputWritingRunner {
        calls: Vec<(String, Vec<String>)>,
        temp: PathBuf,
    }

    impl CommandRunner for OutputWritingRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let action_process = program == "docker"
                && (args.first().is_some_and(|arg| arg == "exec")
                    || args.iter().any(|arg| arg.starts_with("node:")));
            if action_process {
                if has_container_env_path(args, "GITHUB_OUTPUT", "producer_output") {
                    fs::write(self.temp.join("producer_output"), "answer=42\n")?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "check-image_output") {
                    fs::write(self.temp.join("check-image_output"), "exists=false\n")?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "paths_output") {
                    fs::write(
                        self.temp.join("paths_output"),
                        "construct=true\nconstruct_count=1\nchanges=[\"construct\"]\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "upload-artifact_output") {
                    fs::write(
                        self.temp.join("upload-artifact_output"),
                        "artifact-id=777\nartifact-url=https://results.actions/artifacts/777\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "deploy-pages_output") {
                    fs::write(
                        self.temp.join("deploy-pages_output"),
                        "page_url=https://jackin-project.github.io/jackin/\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "deployment_output") {
                    fs::write(
                        self.temp.join("deployment_output"),
                        "page_url=https://jackin-project.github.io/jackin/\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "sitemap_output") {
                    fs::write(
                        self.temp.join("sitemap_output"),
                        "url=https://jackin-project.github.io/jackin/sitemap.xml\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_STATE", "cache_state") {
                    fs::write(self.temp.join("cache_state"), "primaryKey=linux-cache\n")?;
                }
                if has_container_env_path(args, "GITHUB_PATH", "pather_path") {
                    fs::write(self.temp.join("pather_path"), "/root/.cargo/bin\n")?;
                }
                if has_container_env_path(args, "GITHUB_PATH", "mise_path") {
                    fs::write(self.temp.join("mise_path"), "/opt/mise/shims\n")?;
                }
                if has_container_env_path(args, "GITHUB_ENV", "toolchain_env") {
                    fs::write(
                        self.temp.join("toolchain_env"),
                        "CARGO_HOME=/github/home/.cargo\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_PATH", "toolchain_path") {
                    fs::write(self.temp.join("toolchain_path"), "/root/.cargo/bin\n")?;
                }
            }
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    struct PhaseStateRunner {
        calls: Vec<(String, Vec<String>)>,
        temp: PathBuf,
    }

    impl CommandRunner for PhaseStateRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let state_file = has_container_env_path(args, "GITHUB_STATE", "wrapped_state")
                .then(|| self.temp.join("wrapped_state"));
            if program == "docker" {
                match (args.last().map(String::as_str), state_file) {
                    (Some("/__a/_actions/wrapped/dist/pre.js"), Some(path)) => {
                        fs::write(path, "preKey=pre-value\n")?;
                    }
                    (Some("/__a/_actions/wrapped/dist/main.js"), Some(path)) => {
                        fs::write(path, "mainKey=main-value\n")?;
                    }
                    _ => {}
                }
            }
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    struct EnvAndFailureRunner {
        calls: Vec<(String, Vec<String>)>,
        temp: PathBuf,
    }

    impl CommandRunner for EnvAndFailureRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            if has_container_env_path(args, "GITHUB_ENV", "enable_env") {
                fs::write(self.temp.join("enable_env"), "CACHE_ON_FAILURE=true\n")?;
            }
            let code = if args.iter().any(|arg| arg == "/__t/fail.sh") {
                1
            } else {
                0
            };
            Ok(CommandResult {
                code,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct StdoutCommandRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for StdoutCommandRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let stdout = if program == "docker" && args.first().is_some_and(|arg| arg == "exec") {
                "::set-output name=answer::42\n::add-path::/opt/tool\n::add-mask::hidden\n::error::broken\nhidden\n"
                    .to_string()
            } else {
                String::new()
            };
            Ok(CommandResult {
                code: 0,
                stdout,
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct StreamingMaskRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for StreamingMaskRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        fn run_streaming(
            &mut self,
            program: &str,
            args: &[String],
            on_output: &mut dyn FnMut(CommandStream, &str),
        ) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            on_output(CommandStream::Stdout, "::add-mask::dynsecret");
            on_output(CommandStream::Stdout, "echo dynsecret");
            Ok(CommandResult {
                code: 0,
                stdout: "::add-mask::dynsecret\necho dynsecret\n".into(),
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct SilentCommandRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for SilentCommandRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct StderrCommandRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for StderrCommandRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let stderr = if program == "docker" && args.first().is_some_and(|arg| arg == "exec") {
                "::set-output name=answer::42\n::add-path::/opt/tool\n::add-mask::hidden\n::warning::slow\nhidden\n"
                    .to_string()
            } else {
                String::new()
            };
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr,
            })
        }
    }

    fn temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_root = std::env::temp_dir();
        let temp_root = temp_root.canonicalize().unwrap_or(temp_root);
        temp_root.join(format!(
            "velnor-executor-test-{}-{nonce}-{sequence}",
            std::process::id()
        ))
    }

    #[test]
    fn stream_reader_survives_invalid_utf8() {
        let bytes = b"first\n\xff\xfebad\nafter\n";
        let (sender, receiver) = mpsc::channel();

        stream_reader(std::io::Cursor::new(bytes), CommandStream::Stdout, sender);

        let lines: Vec<_> = receiver.iter().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines
            .iter()
            .all(|(stream, _)| *stream == CommandStream::Stdout));
        assert_eq!(lines[0].1, "first");
        assert!(lines[1].1.contains('\u{fffd}'));
        assert_eq!(lines[2].1, "after");
    }

    fn assert_uuid(value: &str) {
        uuid::Uuid::parse_str(value).unwrap_or_else(|_| panic!("expected UUID, got {value}"));
    }

    #[test]
    fn mise_setup_persists_binary_and_installs_locked() {
        let script = setup_mise_script(true, "2026.7.7", "", "", "mise-v2", false);

        // Persistent per-version binary store, not a mutation of /opt/mise/bin.
        assert!(script.contains("exact_version='2026.7.7'"));
        assert!(script.contains(r#"store_dir="$VELNOR_MISE_BINARY_STORE/$os_arch/$exact_version""#));
        assert!(script.contains("VELNOR_MISE_BINARY_STORE:=/opt/velnor/mise-binaries"));
        assert!(script.contains("VELNOR_MISE_BOOTSTRAP:=/opt/mise/bin/mise"));
        assert!(script.contains(r#""$staging" self-update "$exact_version" -y"#));
        assert!(script.contains(r#"staging="$staging_dir/mise""#));
        assert!(script.contains("trap 'rm -rf \"$staging_dir\"' EXIT HUP INT TERM"));
        assert!(!script.contains(r#"staging="$store_dir/.mise.staging"#));
        assert!(script.contains(r#"mv -f "$staging" "$mise_bin""#));
        assert!(script.contains("chmod 0555"));
        assert!(script.contains("__VELNOR_MISE_SELF__"));
        // Exactly one exclusive lock covers publish + install + export.
        assert_eq!(script.matches("flock -x 9").count(), 1);
        assert!(script.contains(".velnor-mise.lock"));
        // Integrity: reuse checks recorded version + sha256, fails closed.
        assert!(script.contains("_velnor_sha256"));
        assert!(script.contains("failing closed"));

        // Locked, fail-closed install — never plain `mise install`, never a
        // network mise.run bootstrap or self-update of /opt/mise/bin.
        assert!(script.contains(r#""$mise_bin" install --locked --yes"#));
        assert!(script.contains(r#""$mise_bin" install --locked --yes --force rust"#));
        assert!(!script.contains("rustup component"));
        assert!(!script.contains("rustup toolchain"));
        assert!(script.contains(r#""$mise_bin" exec -- cargo --version"#));
        assert!(script.contains(r#""$mise_bin" exec -- cargo clippy --version"#));
        assert!(script.contains(r#""$mise_bin" exec -- cargo fmt --version"#));
        assert!(script.contains("MISE_LOCKED=1"));
        assert!(script.contains("MISE_LOCKED_VERIFY_PROVENANCE=1"));
        assert!(!script.contains("https://mise.run"));
        assert!(!script.contains("curl"));
        assert!(!script.contains(r#"bin="/opt/mise/bin""#));

        // Existing invariants preserved.
        assert!(script.contains("mise bin-paths"));
        assert!(script.contains("cache_key_prefix='mise-v2'"));
        assert!(script.contains("cache_save_requested=\"\""));
        assert!(script.contains(r#"find "$mise_home/installs" -mindepth 3 -maxdepth 3"#));
        // A cargo- or pipx-backend version dir is valid only with a non-empty
        // bin/ — an interrupted install must never be treated as installed.
        assert!(script.contains(
            r#"for tool_dir in "$mise_home/installs"/cargo-* "$mise_home/installs"/pipx-*; do"#
        ));
        assert!(script.contains(r#"[ ! -d "$version_dir/bin" ]"#));
        assert!(script.contains("dropping incomplete install"));
        assert!(script.contains("__VELNOR_MISE_BIN__"));
        assert!(!script.contains("export CARGO_HOME="));
        assert!(!script.contains("export RUSTUP_HOME="));
        assert!(script.contains("env --redacted --json"));
        assert!(script.contains("env --json"));
    }

    // Fake `mise` binary: `self-update <v>` rewrites itself to deterministically
    // report `<v>` (stable content → stable hash); `install` and `self-update`
    // append markers to $VELNOR_FAKE_LOG so the test can count them.
    #[cfg(unix)]
    const FAKE_MISE: &str = r#"#!/bin/sh
log="$VELNOR_FAKE_LOG"
case "$1" in
  self-update)
    printf 'self-update %s\n' "$2" >> "$log"
    cat > "$0" <<INNER
#!/bin/sh
log="\$VELNOR_FAKE_LOG"
case "\$1" in
  --version) echo "$2 linux-x64 (fake)" ;;
  install) printf 'install %s\n' "\$*" >> "\$log"; echo installed ;;
  self-update) printf 'self-update %s\n' "\$2" >> "\$log" ;;
  bin-paths) : ;;
  trust) : ;;
  env) echo '{}' ;;
  *) : ;;
esac
INNER
    chmod 0755 "$0"
    ;;
  --version) echo "0.0.0 linux-x64 (bootstrap)" ;;
  install) printf 'install %s\n' "$*" >> "$log"; echo installed ;;
  bin-paths) : ;;
  trust) : ;;
  env) echo '{}' ;;
  *) : ;;
esac
"#;

    #[cfg(unix)]
    #[test]
    fn mise_persistent_binary_is_published_once_and_reused_across_jobs() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let base = temp_dir().join(format!("mise-reuse-{}", uuid::Uuid::new_v4()));
        let bootstrap = base.join("baked/mise");
        let store = base.join("persistent-store");
        let log = base.join("fake.log");
        fs::create_dir_all(bootstrap.parent().unwrap()).unwrap();
        fs::create_dir_all(&store).unwrap();
        fs::write(&bootstrap, FAKE_MISE).unwrap();
        fs::set_permissions(&bootstrap, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&log, "").unwrap();
        let bootstrap_before = fs::read(&bootstrap).unwrap();

        // Requested exact version differs from the fake bootstrap (0.0.0).
        let script = setup_mise_script(true, "2026.7.8", "", "", "mise-v2", false);

        let run_job = |job: &str| -> std::process::Output {
            let mise_home = base.join(format!("{job}/mise-home"));
            let env_dir = base.join(format!("{job}/env"));
            fs::create_dir_all(&mise_home).unwrap();
            fs::create_dir_all(&env_dir).unwrap();
            Command::new("/bin/sh")
                .arg("-c")
                .arg(&script)
                .stdin(std::process::Stdio::null()) // closed stdin: zero prompts
                .env("VELNOR_MISE_BOOTSTRAP", &bootstrap)
                .env("VELNOR_MISE_BINARY_STORE", &store)
                .env("VELNOR_MISE_HOME", &mise_home)
                .env("VELNOR_MISE_ENV_DIR", &env_dir)
                .env("VELNOR_FAKE_LOG", &log)
                .output()
                .expect("run mise setup script")
        };

        // Job 1: fresh store → one publish (self-update), one locked install.
        let out1 = run_job("job-1");
        assert!(
            out1.status.success(),
            "job 1 failed: {}",
            String::from_utf8_lossy(&out1.stderr)
        );
        let stdout1 = String::from_utf8_lossy(&out1.stdout);
        assert!(
            stdout1.contains("published persistent 2026.7.8"),
            "{stdout1}"
        );
        assert!(stdout1.contains("__VELNOR_MISE_SELF__"));

        let published = store.join("linux-x64/2026.7.8/mise");
        let published_alt = store.join("linux-arm64/2026.7.8/mise");
        let bin_path = if published.exists() {
            published
        } else {
            published_alt
        };
        assert!(bin_path.exists(), "persistent binary not published");
        let hash1 = fs::read(&bin_path).unwrap();
        let meta = fs::read_to_string(bin_path.parent().unwrap().join("metadata.json")).unwrap();
        assert!(meta.contains("\"version\":\"2026.7.8\""));
        assert!(meta.contains("\"sha256\":\""));
        // chmod 0555 on the published binary.
        let mode = fs::metadata(&bin_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o555, "published binary must be 0555");

        // Job 2: new job dirs, SAME persistent store → reuse, no second update.
        let out2 = run_job("job-2");
        assert!(
            out2.status.success(),
            "job 2 failed: {}",
            String::from_utf8_lossy(&out2.stderr)
        );
        let stdout2 = String::from_utf8_lossy(&out2.stdout);
        assert!(stdout2.contains("reusing persisted 2026.7.8"), "{stdout2}");
        assert!(!stdout2.contains("published persistent"));

        // Identical binary path + hash across both jobs.
        let hash2 = fs::read(&bin_path).unwrap();
        assert_eq!(hash1, hash2, "reused binary content must be identical");

        // Exactly one self-update total, two locked installs total.
        let log_text = fs::read_to_string(&log).unwrap();
        assert_eq!(
            log_text.matches("self-update 2026.7.8").count(),
            1,
            "binary must be updated exactly once across two jobs: {log_text}"
        );
        assert_eq!(
            log_text.matches("install --locked --yes").count(),
            2,
            "each job runs one locked install: {log_text}"
        );

        // No fallback installer or network marker anywhere.
        for text in [&stdout1, &stdout2] {
            assert!(!text.contains("mise.run"));
            assert!(!text.contains("apt-get"));
            assert!(!text.contains("cargo install"));
        }

        // The read-only baked bootstrap was never mutated.
        assert_eq!(
            fs::read(&bootstrap).unwrap(),
            bootstrap_before,
            "the baked bootstrap must not be written"
        );

        fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn mise_persistent_store_fails_closed_on_corruption() {
        use std::process::Command;

        let base = temp_dir().join(format!("mise-corrupt-{}", uuid::Uuid::new_v4()));
        let bootstrap = base.join("baked/mise");
        let store = base.join("persistent-store");
        let log = base.join("fake.log");
        fs::create_dir_all(bootstrap.parent().unwrap()).unwrap();
        fs::create_dir_all(&store).unwrap();
        fs::write(&bootstrap, FAKE_MISE).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bootstrap, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(&log, "").unwrap();
        let script = setup_mise_script(true, "2026.7.8", "", "", "mise-v2", false);

        let mise_home = base.join("mise-home");
        let env_dir = base.join("env");
        fs::create_dir_all(&mise_home).unwrap();
        fs::create_dir_all(&env_dir).unwrap();
        let run = || {
            Command::new("/bin/sh")
                .arg("-c")
                .arg(&script)
                .stdin(std::process::Stdio::null())
                .env("VELNOR_MISE_BOOTSTRAP", &bootstrap)
                .env("VELNOR_MISE_BINARY_STORE", &store)
                .env("VELNOR_MISE_HOME", &mise_home)
                .env("VELNOR_MISE_ENV_DIR", &env_dir)
                .env("VELNOR_FAKE_LOG", &log)
                .output()
                .expect("run mise setup script")
        };

        assert!(run().status.success(), "initial publish must succeed");
        let bin_path = if store.join("linux-x64/2026.7.8/mise").exists() {
            store.join("linux-x64/2026.7.8/mise")
        } else {
            store.join("linux-arm64/2026.7.8/mise")
        };
        // Tamper with the persisted binary; the hash no longer matches metadata.
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(&bin_path, "tampered\n").unwrap();

        let out = run();
        assert!(
            !out.status.success(),
            "a corrupt persisted binary must fail closed"
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("integrity check"));

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn mise_direct_bin_paths_precede_shims_and_ignore_duplicates() {
        let pipx_bin = "/opt/mise/installs/pipx-reuse/6.2.0/bin";
        let path = mise_step_path(&format!(
            "__VELNOR_MISE_BIN__{pipx_bin}\n__VELNOR_MISE_BIN__{pipx_bin}\n"
        ));

        assert_eq!(path.iter().filter(|entry| *entry == pipx_bin).count(), 1);
        assert!(
            path.iter().position(|entry| entry == pipx_bin)
                < path.iter().position(|entry| entry == "/opt/mise/shims")
        );
        assert!(
            path.iter().position(|entry| entry == "/root/.cargo/bin")
                < path.iter().position(|entry| entry == "/opt/mise/shims")
        );
    }

    #[test]
    fn mise_environment_exports_strings_except_path_and_masks_redacted_values() {
        let (env, masks) = parse_mise_environment(
            r#"{"RUSTUP_TOOLCHAIN":"1.97.1","PATH":"ignored","COUNT":2}"#,
            r#"{"TOKEN":"secret","EMPTY":""}"#,
        )
        .unwrap();

        assert_eq!(env.get("RUSTUP_TOOLCHAIN"), Some(&"1.97.1".to_string()));
        assert!(!env.contains_key("PATH"));
        assert!(!env.contains_key("COUNT"));
        assert_eq!(masks, vec!["secret"]);
    }

    fn host_temp_script_path(container_path: &str, temp: &Path) -> PathBuf {
        temp.join(container_path.trim_start_matches("/__t/"))
    }

    fn has_container_env_path(args: &[String], name: &str, file_name: &str) -> bool {
        args.iter().any(|arg| {
            arg == &format!("{name}=/__t/{file_name}")
                || arg == &format!("{name}=/github/file_commands/{file_name}")
        })
    }

    fn runtime_token_with_results_scope(plan_id: &str, job_id: &str) -> String {
        use base64::Engine;

        let payload = serde_json::json!({
            "scp": format!("Actions.Results:{plan_id}:{job_id}")
        });
        format!(
            "e30.{}.sig",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string())
        )
    }

    fn container(temp: &Path) -> JobContainerSpec {
        JobContainerSpec {
            name: "job".into(),
            image: "ubuntu:24.04".into(),
            network: "net".into(),
            workspace_host: temp.join("work"),
            temp_host: temp.to_path_buf(),
            home_host: temp.join("home"),
            actions_host: temp.join("actions"),
            tools_host: temp.join("tools"),
            mount_docker_socket: false,
            env: Vec::new(),
            resource_options: Vec::new(),
            options: Vec::new(),
            services: Vec::new(),
            node_action_image: String::new(),
            docker_cli_host_path: None,
            docker_cli_plugin_host_dir: None,
            docker_host_work_dir: None,
            verify_bind_mounts: false,
            daemon_id: "test-daemon".into(),
            repository: Some("unknown-repository".into()),
            cargo_target_host: None,
            compiler_cache_backend: velnor_cache_service::CompilerCacheBackend::Sccache,
            compiler_cache_trust_class:
                velnor_model::guest_plan::GuestCompilerCacheTrustClass::Trusted,
        }
    }

    fn assert_cleanup_reclaims_job_docker(
        calls: &[(String, Vec<String>)],
        rm_index: usize,
        temp: &Path,
    ) {
        assert_eq!(calls[rm_index].1[0], "rm");
        assert_eq!(
            calls[rm_index + 1].1,
            crate::docker_lease::list_owned_containers_args("job")
        );
        assert_eq!(
            calls[rm_index + 2].1,
            crate::docker_lease::list_owned_networks_args("job")
        );
        assert_eq!(
            calls[rm_index + 3].1,
            crate::docker_lease::list_owned_volumes_args("job")
        );
        assert_eq!(
            calls[rm_index + 4].1,
            crate::docker_lease::list_job_buildkit_format_args()
        );
        let scope = sanitize_artifact_name(temp.file_name().unwrap().to_str().unwrap());
        assert_eq!(calls[rm_index + 5].1[0], "volume");
        assert!(calls[rm_index + 5].1.contains(&format!(
            "name={}{scope}",
            crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX
        )));
    }

    fn expected_network_create_args() -> Vec<String> {
        [
            "network",
            "create",
            "--label",
            "velnor.daemon-id=test-daemon",
            "--label",
            "velnor.job-id=job",
            "net",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    #[test]
    fn verifies_docker_bind_mount_visibility_when_enabled() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let mut spec = container(&temp);
        spec.verify_bind_mounts = true;
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor.start_job_environment_once(&spec).unwrap();

        let runner = executor.runner();
        let calls = &runner.calls;
        assert_eq!(calls[2].1[0], "exec");
        assert!(calls[2]
            .1
            .contains(&format!("test -f /__t/{DOCKER_MOUNT_CHECK_FILE}")));
        assert!(!temp.join(DOCKER_MOUNT_CHECK_FILE).exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn deferred_teardown_keeps_resources_until_completion_boundary() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let spec = container(&temp);
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps_without_cleanup(&spec, &[], &[], &[], None, None, &temp)
            .unwrap();
        assert!(!executor
            .runner()
            .calls
            .iter()
            .any(|(_, args)| args.first().is_some_and(|arg| arg == "rm")));

        executor.cleanup(&spec).unwrap();
        let calls = &executor.runner().calls;
        assert!(calls
            .iter()
            .any(|(_, args)| { args.starts_with(&["rm".into(), "--force".into(), "job".into()]) }));
        assert!(calls
            .iter()
            .any(|(_, args)| args == &crate::docker_lease::list_job_buildkit_format_args()));
        assert!(!calls
            .iter()
            .any(|(_, args)| args == &spec.remove_network_args()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn cleanup_reclaims_labeled_guest_docker_not_just_named_job_objects() {
        struct ReclaimRunner {
            calls: Vec<Vec<String>>,
        }
        impl CommandRunner for ReclaimRunner {
            fn run(&mut self, _program: &str, args: &[String]) -> Result<CommandResult> {
                self.calls.push(args.to_vec());
                let stdout = if args == crate::docker_lease::list_owned_containers_args("job") {
                    "guest-id\tguest-postgres\nbk-id\tbuildx_buildkit_velnor-builder-job0\n".into()
                } else if args == crate::docker_lease::list_owned_networks_args("job") {
                    "guest-net\n".into()
                } else if args == crate::docker_lease::list_owned_volumes_args("job") {
                    "guest-vol\n".into()
                } else {
                    String::new()
                };
                Ok(CommandResult {
                    code: 0,
                    stdout,
                    stderr: String::new(),
                })
            }
        }

        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let spec = container(&temp);
        let mut executor = DockerJobEngine::new(ReclaimRunner { calls: Vec::new() });
        executor.cleanup(&spec).unwrap();
        let calls = &executor.runner().calls;
        assert!(calls
            .iter()
            .any(|args| args == &crate::docker_lease::list_owned_containers_args("job")));
        assert!(calls
            .iter()
            .any(|args| args
                == &crate::docker_lease::force_remove_container_args(&["guest-id".into()])));
        assert!(calls.iter().all(|args| {
            !(args.first().is_some_and(|arg| arg == "rm") && args.iter().any(|arg| arg == "bk-id"))
        }));
        assert!(calls
            .iter()
            .any(|args| args
                == &crate::docker_lease::force_remove_network_args(&["guest-net".into()])));
        assert!(!calls.iter().any(|args| args == &spec.remove_network_args()));
        assert!(calls
            .iter()
            .any(|args| args
                == &crate::docker_lease::force_remove_volume_args(&["guest-vol".into()])));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn cleanup_removes_job_scoped_buildkit_daemons_and_state_volumes() {
        let root = temp_dir();
        let temp = root.join("job-scope").join("temp");
        fs::create_dir_all(&temp).unwrap();
        let spec = container(&temp);
        let mut executor = DockerJobEngine::new(BuildkitCleanupRunner::default());

        executor.cleanup_job_buildkit(&spec).unwrap();
        // Created + removing of this job's builder must both be force-removed,
        // one docker rm per id. Batching those ids deadlocks Engine 29 DELETE.

        for call in &executor.runner().calls {
            if call.first().map(String::as_str) != Some("rm") {
                continue;
            }
            let ids: Vec<_> = call
                .iter()
                .skip(1)
                .filter(|arg| !arg.starts_with('-'))
                .collect();
            assert!(
                ids.len() <= 1,
                "cleanup_job_buildkit batched docker rm {call:?}"
            );
        }

        assert_eq!(
            executor.runner().calls,
            vec![
                crate::docker_lease::list_job_buildkit_format_args(),
                crate::docker_lease::force_remove_one_container_args("bk1"),
                crate::docker_lease::force_remove_one_container_args("bk2"),
                vec![
                    "volume",
                    "ls",
                    "--quiet",
                    "--filter",
                    "name=buildx_buildkit_velnor-builder-job-scope",
                ]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
                vec![
                    "volume",
                    "rm",
                    "--force",
                    "buildx_buildkit_velnor-builder-job-scope0_state",
                ]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_aborts_docker_lease_before_buildkit_rm() {
        use std::path::PathBuf;

        struct LeaseOrderRunner {
            lease_path: PathBuf,
            job_rm_saw_lease: bool,
            buildkit_saw_dead_lease: bool,
        }

        impl CommandRunner for LeaseOrderRunner {
            fn run(&mut self, _program: &str, args: &[String]) -> Result<CommandResult> {
                let lease_live = self.lease_path.exists();
                if args == ["rm".to_string(), "--force".to_string(), "job".to_string()].as_slice() {
                    assert!(
                        lease_live,
                        "lease must stay mounted until the job container is removed"
                    );
                    self.job_rm_saw_lease = true;
                }
                if args == crate::docker_lease::list_job_buildkit_format_args() {
                    assert!(
                        !lease_live,
                        "in-flight Engine Start must be aborted before BuildKit reclaim"
                    );
                    self.buildkit_saw_dead_lease = true;
                }
                Ok(CommandResult {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }

        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let lease_dir = PathBuf::from("/tmp").join(format!(
            "vlc-abort-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&lease_dir).unwrap();
        let lease_path = lease_dir.join("s.sock");
        let lease = crate::docker_lease::DockerLeaseGuard::bind_to(
            lease_path.clone(),
            PathBuf::from("/nonexistent-host-docker.sock"),
            "job".into(),
            "daemon".into(),
        )
        .unwrap();
        assert!(
            lease_path.exists(),
            "lease socket must exist before cleanup"
        );
        let spec = container(&temp);
        let mut executor = DockerJobEngine::new(LeaseOrderRunner {
            lease_path: lease_path.clone(),
            job_rm_saw_lease: false,
            buildkit_saw_dead_lease: false,
        })
        .with_docker_lease(lease);
        executor.cleanup(&spec).unwrap();
        let runner = executor.into_runner();
        assert!(
            runner.job_rm_saw_lease,
            "cleanup must remove the job container"
        );
        assert!(
            runner.buildkit_saw_dead_lease,
            "cleanup must reclaim BuildKit after aborting the lease"
        );
        assert!(!lease_path.exists(), "cleanup must drop the lease socket");
        fs::remove_dir_all(temp).ok();
        fs::remove_dir_all(lease_dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_without_buildkit_aborts_lease_before_deferred_reclaim() {
        use std::path::PathBuf;

        struct SkipBuildkitRunner {
            lease_path: PathBuf,
            job_rm_saw_lease: bool,
            calls: Vec<Vec<String>>,
        }

        impl CommandRunner for SkipBuildkitRunner {
            fn run(&mut self, _program: &str, args: &[String]) -> Result<CommandResult> {
                let lease_live = self.lease_path.exists();
                if args == ["rm".to_string(), "--force".to_string(), "job".to_string()].as_slice() {
                    assert!(
                        lease_live,
                        "lease must stay mounted until the job container is removed"
                    );
                    self.job_rm_saw_lease = true;
                }
                self.calls.push(args.to_vec());
                Ok(CommandResult {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }

        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let lease_dir = PathBuf::from("/tmp").join(format!(
            "vlc-defer-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&lease_dir).unwrap();
        let lease_path = lease_dir.join("s.sock");
        let lease = crate::docker_lease::DockerLeaseGuard::bind_to(
            lease_path.clone(),
            PathBuf::from("/nonexistent-host-docker.sock"),
            "job".into(),
            "daemon".into(),
        )
        .unwrap();
        let spec = container(&temp);
        let mut executor = DockerJobEngine::new(SkipBuildkitRunner {
            lease_path: lease_path.clone(),
            job_rm_saw_lease: false,
            calls: Vec::new(),
        })
        .with_docker_lease(lease);
        executor.cleanup_without_buildkit(&spec).unwrap();
        let runner = executor.into_runner();
        assert!(runner.job_rm_saw_lease);
        assert!(
            runner
                .calls
                .iter()
                .all(|args| args != &crate::docker_lease::list_job_buildkit_format_args()),
            "slot path must not wait on BuildKit rm: {:?}",
            runner.calls
        );
        assert!(
            !lease_path.exists(),
            "deferred BuildKit worker must not inherit a live Engine Start"
        );
        fs::remove_dir_all(temp).ok();
        fs::remove_dir_all(lease_dir).ok();
    }

    #[test]
    fn buildkit_resource_limits_are_derived_and_fail_closed() {
        assert_eq!(
            buildx_driver_resource_options(&[
                "--cpus".into(),
                "2.5".into(),
                "--memory".into(),
                "12g".into(),
            ])
            .unwrap(),
            ["cpu-period=100000", "cpu-quota=250000", "memory=12g"]
        );
        assert!(buildx_driver_resource_options(&["--cpus".into(), "0".into()]).is_err());
        assert!(buildx_driver_resource_options(&["--pids-limit".into(), "512".into()]).is_err());
        assert!(buildx_driver_resource_options(&["--memory".into()]).is_err());
    }

    #[test]
    fn precreated_environment_skips_lazy_container_start() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let spec = container(&temp);
        let mut executor =
            DockerJobEngine::new(RecordingRunner::default()).with_job_environment_started(true);

        executor
            .execute_ordered_steps_without_cleanup(&spec, &[], &[], &[], None, None, &temp)
            .unwrap();

        assert!(
            !executor
                .runner()
                .calls
                .iter()
                .any(|(_, args)| args == &expected_network_create_args()),
            "lazy startup unexpectedly recreated the network: {:?}",
            executor.runner().calls
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn bind_mount_preflight_reports_invisible_daemon_paths() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let mut spec = container(&temp);
        spec.verify_bind_mounts = true;
        let mut executor = DockerJobEngine::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 1],
        });

        let error = executor.start_job_environment_once(&spec).unwrap_err();

        assert!(error.to_string().contains("Docker daemon cannot see"));
        assert!(!temp.join(DOCKER_MOUNT_CHECK_FILE).exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn selects_node_action_image_from_runtime() {
        assert_eq!(
            node_action_image("node24", "velnor/job-ubuntu:26.04"),
            "velnor/job-ubuntu:26.04"
        );
        assert_eq!(node_action_image("node20", ""), "node:20-bookworm");
        assert_eq!(node_action_image("node24", ""), "node:24-bookworm");
        assert_eq!(node_action_image("node16", ""), "node:16");
        assert_eq!(node_action_image("bogus", ""), "");
    }

    #[test]
    fn executes_script_step_and_collects_state() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let step = ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
            script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let result = executor
            .execute_step(&container(&temp), &step, &temp)
            .unwrap();

        assert_eq!(result.exit_code, 0);
        let runner = executor.runner();
        let calls = &runner.calls;
        assert_eq!(calls[0], ("docker".into(), expected_network_create_args()));
        assert_eq!(calls[1].1[0], "run");
        assert_eq!(calls[2].1[0], "exec");
        assert!(calls[2]
            .1
            .contains(&"GITHUB_OUTPUT=/__t/step1_output".into()));
        assert_cleanup_reclaims_job_docker(calls, 3, &temp);

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn recognized_test_command_emits_bounded_completion_telemetry() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let sink = Arc::new(
            crate::ops::OpsSink::open(temp.join("state.db"), "test-instance".to_owned()).unwrap(),
        );
        let admission = crate::ops::JobAdmission {
            instance_slug: "test-instance".to_owned(),
            job_uid: "job-test-end".to_owned(),
            repository_full_name: "tailrocks/velnor".to_owned(),
            workflow: "control-plane".to_owned(),
            job_name: "test-end".to_owned(),
            run_id: Some(45),
            attempt: Some(1),
            head_ref: Some("refs/heads/main".to_owned()),
            head_sha: Some("deadbeef".to_owned()),
            trigger_event: Some("workflow_dispatch".to_owned()),
            queued_at_rfc3339: None,
            slot_name: Some("slot-0".to_owned()),
            runner_name: Some("runner-0".to_owned()),
            trust_scope: Some("trusted".to_owned()),
            resource_policy: Some("standard".to_owned()),
            masks: vec!["secret-marker".to_owned()],
        };
        assert!(sink.record_admission(&admission));
        let step = ScriptStep {
            id: "tests".into(),
            display_name: "Tests".into(),
            script: "cargo nextest run -p private --token secret-marker".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };

        let result = DockerJobEngine::new(RecordingRunner::default())
            .with_tool_prep_telemetry(Arc::clone(&sink), admission)
            .execute_step(&container(&temp), &step, &temp)
            .unwrap();

        assert_eq!(result.exit_code, 0);
        let telemetry = fs::read_to_string(temp.join("state.test-instance.telemetry.jsonl"))
            .expect("test completion telemetry");
        let record = telemetry
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|record| record["event"] == "test_end")
            .expect("test_end event");
        let fields = record["fields"].as_object().expect("test fields");
        assert_eq!(fields["exit_code"], 0);
        assert_eq!(fields["passed"], true);
        assert_eq!(fields["runner"], "cargo_nextest");
        assert!(fields["ms"]
            .as_u64()
            .is_some_and(|value| { value <= MAX_TEST_END_TELEMETRY_MS }));
        assert!(!telemetry.contains("private"));
        assert!(!telemetry.contains("secret-marker"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn compile_command_kind_requires_an_explicit_compile_segment() {
        assert_eq!(
            compile_command_kind("cargo build -p private"),
            Some("cargo")
        );
        assert_eq!(
            compile_command_kind("env RUSTFLAGS=-Dwarnings cargo check"),
            Some("cargo")
        );
        assert_eq!(compile_command_kind("go build ./..."), Some("go"));
        assert_eq!(compile_command_kind("echo cargo build"), None);
        assert_eq!(compile_command_kind("cargo test -p private"), None);
        assert_eq!(link_command_kind("cargo build -p private"), Some("cargo"));
        assert_eq!(link_command_kind("cargo rustc -p private"), Some("cargo"));
        assert_eq!(link_command_kind("go build ./..."), Some("go"));
        assert_eq!(link_command_kind("cargo check -p private"), None);
        assert_eq!(link_command_kind("echo cargo build"), None);
    }

    #[test]
    fn explicit_compile_command_emits_bounded_start_and_end_telemetry() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let sink = Arc::new(
            crate::ops::OpsSink::open(temp.join("state.db"), "test-instance".to_owned()).unwrap(),
        );
        let admission = crate::ops::JobAdmission {
            instance_slug: "test-instance".to_owned(),
            job_uid: "job-compile-boundary".to_owned(),
            repository_full_name: "tailrocks/velnor".to_owned(),
            workflow: "control-plane".to_owned(),
            job_name: "compile-boundary".to_owned(),
            run_id: Some(46),
            attempt: Some(1),
            head_ref: Some("refs/heads/main".to_owned()),
            head_sha: Some("deadbeef".to_owned()),
            trigger_event: Some("workflow_dispatch".to_owned()),
            queued_at_rfc3339: None,
            slot_name: Some("slot-0".to_owned()),
            runner_name: Some("runner-0".to_owned()),
            trust_scope: Some("trusted".to_owned()),
            resource_policy: Some("standard".to_owned()),
            masks: vec!["secret-marker".to_owned()],
        };
        assert!(sink.record_admission(&admission));
        let step = ScriptStep {
            id: "compile".into(),
            display_name: "Compile".into(),
            script: "cargo build -p private --token secret-marker".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };

        let result = DockerJobEngine::new(RecordingRunner::default())
            .with_tool_prep_telemetry(Arc::clone(&sink), admission)
            .execute_step(&container(&temp), &step, &temp)
            .unwrap();

        assert_eq!(result.exit_code, 0);
        let telemetry = fs::read_to_string(temp.join("state.test-instance.telemetry.jsonl"))
            .expect("compile telemetry");
        let records: Vec<_> = telemetry
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|record| {
                matches!(
                    record["event"].as_str(),
                    Some("compile_start" | "compile_end" | "link_end")
                )
            })
            .collect();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["event"], "compile_start");
        assert_eq!(records[1]["event"], "compile_end");
        assert_eq!(records[2]["event"], "link_end");
        for record in &records[..2] {
            let fields = record["fields"].as_object().expect("compile fields");
            assert_eq!(fields["compiler"], "cargo");
            assert_eq!(fields["metrics_known"], false);
            assert_eq!(fields["unit_count"], 0);
            assert_eq!(fields["wrapper_calls"], 0);
            assert_eq!(fields["hits"], 0);
            assert_eq!(fields["misses"], 0);
        }
        let end_fields = records[1]["fields"].as_object().unwrap();
        assert_eq!(end_fields["exit_code"], 0);
        assert!(end_fields["ms"]
            .as_u64()
            .is_some_and(|value| value <= MAX_COMPILE_TELEMETRY_MS));
        let link_fields = records[2]["fields"].as_object().unwrap();
        assert_eq!(link_fields["exit_code"], 0);
        assert_eq!(link_fields["runner_kind"], "cargo");
        assert_eq!(link_fields["success"], true);
        assert!(link_fields["elapsed_ms"]
            .as_u64()
            .is_some_and(|value| value <= MAX_COMPILE_TELEMETRY_MS));
        assert!(!telemetry.contains("private"));
        assert!(!telemetry.contains("secret-marker"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn returns_script_exit_code_and_still_cleans_up() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let step = ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
            script: "exit 7".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let mut executor = DockerJobEngine::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 7, 0, 0, 0, 0],
        });

        let result = executor
            .execute_step(&container(&temp), &step, &temp)
            .unwrap();

        assert_eq!(result.exit_code, 7);
        assert_cleanup_reclaims_job_docker(&executor.runner().calls, 3, &temp);

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn executes_multiple_steps_in_one_container() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ScriptStep {
                id: "step1".into(),
                display_name: String::new(),
                script: "echo one".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ScriptStep {
                id: "step2".into(),
                display_name: String::new(),
                script: "echo two".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let results = executor
            .execute_steps(&container(&temp), &steps, &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        let runner = executor.runner();
        let calls = &runner.calls;
        assert_eq!(calls[0].1[0], "network");
        assert_eq!(calls[1].1[0], "run");
        assert_eq!(calls[2].1[0], "exec");
        assert_eq!(calls[3].1[0], "exec");
        assert_cleanup_reclaims_job_docker(calls, 4, &temp);

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_github_runtime_export_exports_actions_env() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "github-runtime".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::GitHubRuntimeExport,
                cache_kind: None,
                source_path: None,
                inputs: [("github-token".into(), "ghs_token".into())].into(),
                env: vec![("ACTIONS_CUSTOM".into(), "${{ env.CUSTOM_RUNTIME }}".into())],
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[
                    ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
                    (
                        "ACTIONS_RUNTIME_URL".into(),
                        "https://runtime.actions".into(),
                    ),
                    ("CUSTOM_RUNTIME".into(), "custom-value".into()),
                    ("GITHUB_TOKEN".into(), "ghs_token".into()),
                ],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            results[0].state.env["ACTIONS_RUNTIME_TOKEN"],
            "runtime-token"
        );
        assert_eq!(
            results[0].state.env["ACTIONS_RUNTIME_URL"],
            "https://runtime.actions"
        );
        assert_eq!(results[0].state.env["ACTIONS_CUSTOM"], "custom-value");
        assert!(!results[0].state.env.contains_key("GITHUB_TOKEN"));
        assert!(results[0]
            .stdout
            .contains("ACTIONS_RUNTIME_TOKEN=runtime-token"));
        assert_eq!(
            executor
                .runner()
                .calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_github_script_copies_exact_contract_output() {
        let action = NativeActionInvocation {
            git_ref: "3a2844b7e9c422d3c10d287c895573f7108da1b3".into(),
            adapter: NativeActionAdapter::GitHubScript,
            cache_kind: None,
            source_path: None,
            inputs: [(
                "script".into(),
                "core.setOutput('docs-xtask', process.env.CONTRACT)".into(),
            )]
            .into(),
            env: vec![("CONTRACT".into(), "contract-sha".into())],
        };
        let state = JobExecutionState::new(&[]);
        let result = native_github_script(&action, &state).unwrap();
        assert_eq!(result.state.outputs["docs-xtask"], "contract-sha");
    }

    #[test]
    fn repository_artifact_request_retries_server_failure() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for (status, body) in [("500 Internal Server Error", "retry"), ("200 OK", "ready")] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let client = reqwest::blocking::Client::new();

        let body = repository_artifact_response(
            || client.get(&url),
            "test artifact",
            PREPARED_ARTIFACT_MAX_CONTROL_RESPONSE_BYTES,
        )
        .unwrap();

        server.join().unwrap();
        assert_eq!(body, b"ready");
    }

    #[test]
    fn native_paths_filter_outputs_target_shapes() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "filter".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::PathsFilter,
                cache_kind: None,
                source_path: None,
                inputs: [(
                    "filters".into(),
                    "construct:\n  - 'docker/construct/**'\n  - '.github/workflows/construct.yml'\ndocs:\n  - 'docs/**'\n".into(),
                )]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(GitDiffRunner {
            calls: Vec::new(),
            stdout: "docker/construct/Dockerfile\ndocs/index.md\nREADME.md\n".into(),
            missing_refs: true,
        });

        let results = executor
            .execute_ordered_steps_with_context(
                &container(&temp),
                &steps,
                &[("GITHUB_SHA".into(), "head-sha".into())],
                &[(
                    "github".into(),
                    serde_json::json!({
                        "event": {
                            "pull_request": {
                                "base": { "sha": "base-sha" },
                                "head": { "sha": "head-sha" }
                            }
                        }
                    }),
                )],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].state.outputs["construct"], "true");
        assert_eq!(results[0].state.outputs["construct_count"], "1");
        assert_eq!(
            results[0].state.outputs["construct_files"],
            "docker/construct/Dockerfile"
        );
        assert_eq!(results[0].state.outputs["docs"], "true");
        assert_eq!(
            results[0].state.outputs["changes"],
            "[\"construct\",\"docs\"]"
        );
        assert!(results[0].stdout.contains("changed: README.md"));
        assert!(executor.runner().calls.iter().any(|(program, args)| {
            program == "git" && args.contains(&"base-sha...head-sha".into())
        }));
        assert!(executor.runner().calls.iter().any(|(program, args)| {
            program == "git"
                && args.contains(&"fetch".into())
                && args.contains(&"--depth=10".into())
                && args.contains(&"base-sha".into())
                && args.contains(&"head-sha".into())
        }));
        assert_eq!(
            executor
                .runner()
                .calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn paths_filter_dispatch_defaults_to_remote_default_branch() {
        let state = JobExecutionState::new_with_context(
            &[("GITHUB_SHA".into(), "head-sha".into())],
            &[(
                "github".into(),
                serde_json::json!({
                    "event": {"repository": {"default_branch": "main"}}
                }),
            )],
        );

        assert_eq!(
            paths_filter_base_ref(&state).as_deref(),
            Some("origin/main")
        );
        assert_eq!(paths_filter_head_ref(&state).as_deref(), Some("head-sha"));
    }

    #[test]
    fn native_cache_reports_miss_without_node_sidecar() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(workspace.join("crate/src")).unwrap();
        fs::write(
            workspace.join("crate/src/lib.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    (
                        "key".into(),
                        "rust-script-${{ runner.os }}-${{ hashFiles('crate/**/*.rs') }}".into(),
                    ),
                    (
                        "restore-keys".into(),
                        "rust-script-${{ runner.os }}-\n".into(),
                    ),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut expected_hash = Sha256::new();
        expected_hash.update(Sha256::digest(b"pub fn answer() -> u8 { 42 }\n"));
        let digest = expected_hash.finalize();
        let expected_hash = hex_digest(&digest);
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[("RUNNER_OS".into(), "Linux".into())],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].exit_code, 0);
        assert_eq!(results[0].state.outputs["cache-hit"], "false");
        // Root exposes only `cache-hit`; the primary key stays internal state.
        assert!(!results[0].state.outputs.contains_key("cache-primary-key"));
        assert!(!results[0].state.outputs.contains_key("cache-matched-key"));
        assert_eq!(
            results[0].state.state["primaryKey"],
            format!("rust-script-Linux-{expected_hash}")
        );
        assert!(results[0]
            .stdout
            .contains("Cache not found for input keys: rust-script-Linux-"));
        assert!(results[0]
            .stdout
            .contains("Cache path: ~/.cache/rust-script"));
        assert!(results[1]
            .stderr
            .contains("Cache not saved because no paths exist"));
        assert_eq!(
            executor
                .runner()
                .calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn sanitize_artifact_name_neutralizes_traversal() {
        assert_eq!(sanitize_artifact_name(".."), "_");
        assert_eq!(sanitize_artifact_name("."), "_");
        assert_eq!(sanitize_artifact_name(""), "_");
        assert_eq!(sanitize_artifact_name("normal-key.v2"), "normal-key.v2");
    }

    /// Versioned store directory a cache test uses to pre-seed or assert
    /// entries, mirroring `cache_store_dir` + `cache_scope_version` for the
    /// common case of an empty action ref and no runner os/arch in the env.
    fn cache_scope_store_dir(root: &Path, repo_key: &str, path: &str) -> PathBuf {
        root.join("_velnor_caches")
            .join("trusted")
            .join(repo_key)
            .join(cache_scope_version("", "", "", path))
    }

    #[test]
    fn cache_store_dir_is_scoped_by_trust_repo_and_version() {
        let root = temp_dir();
        let temp = root.join("job/temp");
        fs::create_dir_all(&temp).unwrap();
        let state = JobExecutionState::new_internal(
            &[("GITHUB_REPOSITORY".into(), "Org/Repo.Name".into())],
            &[],
            None,
            Some(temp.clone()),
        );

        let version = "cv1-abc123";
        let store = cache_store_dir(&state, version).unwrap();

        // Trust/repo remain the outer boundary; the version segment sits below.
        assert!(store.ends_with("_velnor_caches/trusted/Org_Repo.Name/cv1-abc123"));
        assert!(store.starts_with(root.join("_velnor_caches")));
        assert_eq!(
            store.parent().unwrap().file_name().unwrap(),
            "Org_Repo.Name"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_store_refuses_missing_repository_identity() {
        let root = temp_dir();
        let temp = root.join("job/temp");
        fs::create_dir_all(&temp).unwrap();
        let state = JobExecutionState::new_internal(&[], &[], None, Some(temp.clone()));
        assert_eq!(
            cache_store_dir(&state, "cv1-abc123").unwrap(),
            temp.join("_velnor/ephemeral/caches/cv1-abc123")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_hit_output_matches_actions_cache() {
        let root = temp_dir();
        let store = cache_scope_store_dir(&root, "Test_Repo", "~/.cache/rust-script");
        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let exact_cache = store.join("linux-rust-exact");
        let partial_cache = store.join("linux-rust-prefix-new");
        fs::create_dir_all(exact_cache.join("0")).unwrap();
        fs::create_dir_all(partial_cache.join("0")).unwrap();
        fs::write(exact_cache.join(".velnor-key"), "linux-rust-exact").unwrap();
        fs::write(exact_cache.join(".velnor-created"), "1").unwrap();
        fs::write(exact_cache.join(".velnor-complete-v1"), "complete\n").unwrap();
        fs::write(exact_cache.join("0/state.bin"), "exact\n").unwrap();
        fs::write(partial_cache.join(".velnor-key"), "linux-rust-prefix-new").unwrap();
        fs::write(partial_cache.join(".velnor-created"), "2").unwrap();
        fs::write(partial_cache.join(".velnor-complete-v1"), "complete\n").unwrap();
        fs::write(partial_cache.join("0/state.bin"), "partial\n").unwrap();

        let cache_step = |key: &str, restore_keys: &str| {
            vec![ExecutableStep::Native {
                step_id: "cache".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::Cache,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("path".into(), "~/.cache/rust-script".into()),
                        ("key".into(), key.into()),
                        ("restore-keys".into(), restore_keys.into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }]
        };

        let exact_temp = root.join("exact-job/temp");
        let partial_temp = root.join("partial-job/temp");
        let miss_temp = root.join("miss-job/temp");
        fs::create_dir_all(root.join("exact-job/home")).unwrap();
        fs::create_dir_all(root.join("partial-job/home")).unwrap();
        fs::create_dir_all(root.join("miss-job/home")).unwrap();

        let exact_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&exact_temp),
                &cache_step("linux-rust-exact", ""),
                &env,
                &exact_temp,
            )
            .unwrap();
        let partial_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&partial_temp),
                &cache_step("linux-rust-prefix-miss", "linux-rust-prefix-\n"),
                &env,
                &partial_temp,
            )
            .unwrap();
        let miss_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&miss_temp),
                &cache_step("linux-rust-total-miss", "linux-rust-missing-\n"),
                &env,
                &miss_temp,
            )
            .unwrap();

        assert_eq!(exact_results[0].state.outputs["cache-hit"], "true");
        assert_eq!(partial_results[0].state.outputs["cache-hit"], "false");
        assert_eq!(miss_results[0].state.outputs["cache-hit"], "false");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_treats_rustup_paths_as_velnor_provided() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    (
                        "path".into(),
                        "~/.rustup/toolchains\n~/.rustup/update-hashes\n".into(),
                    ),
                    ("key".into(), "rustup-Linux-X64-lock".into()),
                    ("restore-keys".into(), "rustup-Linux-X64-\n".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].exit_code, 0);
        assert_eq!(results[0].state.outputs["cache-hit"], "false");
        assert!(results[0]
            .stdout
            .contains("Cache paths live on Velnor host-persistent storage (always warm)"));
        assert!(!results[0].stdout.contains("Cache not found"));
        assert!(results[0]
            .state
            .summary
            .contains("host-persistent store — restore/save skipped"));
        assert!(results[1].stdout.contains("nothing to save"));
        assert!(results[1]
            .state
            .summary
            .contains("host-persistent store — restore/save skipped"));
        assert!(results[1].stderr.is_empty());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_cache_treats_root_rustup_path_as_velnor_provided() {
        assert!(velnor_static_persistent_cache_path(
            "/root/.rustup/toolchains"
        ));
        assert!(velnor_static_persistent_cache_path(
            "/root/.rustup/update-hashes"
        ));
    }

    #[test]
    fn workspace_target_cache_paths_include_relative_workflow_inputs() {
        assert!(workspace_target_cache_path("target"));
        assert!(workspace_target_cache_path("./target/debug"));
        assert!(workspace_target_cache_path("/__w/target/release"));
        // Persistent target materialization owns only the workspace-root
        // target. Wildcard targets stay keyed and use concrete manifests.
        assert!(!workspace_target_cache_path("**/target"));
        assert!(!workspace_target_cache_path("nested/target"));
    }

    #[test]
    fn native_cache_globs_round_trip_target_and_node_modules() {
        let root = temp_dir();
        let save_temp = root.join("save/temp");
        let restore_temp = root.join("restore/temp");
        let save_workspace = save_temp.join("work");
        let restore_workspace = restore_temp.join("work");
        fs::create_dir_all(save_workspace.join("target")).unwrap();
        fs::create_dir_all(save_workspace.join("packages/app/target")).unwrap();
        fs::create_dir_all(save_workspace.join("node_modules")).unwrap();
        fs::create_dir_all(save_workspace.join("packages/app/node_modules")).unwrap();
        fs::write(save_workspace.join("target/root.bin"), "root target\n").unwrap();
        fs::write(
            save_workspace.join("packages/app/target/nested.bin"),
            "nested target\n",
        )
        .unwrap();
        fs::write(
            save_workspace.join("node_modules/root.js"),
            "root modules\n",
        )
        .unwrap();
        fs::write(
            save_workspace.join("packages/app/node_modules/nested.js"),
            "nested modules\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            let outside = root.join("outside");
            fs::create_dir_all(outside.join("target")).unwrap();
            fs::create_dir_all(outside.join("modules")).unwrap();
            fs::write(outside.join("target/secret.bin"), "outside target\n").unwrap();
            fs::write(outside.join("modules/secret.js"), "outside modules\n").unwrap();
            std::os::unix::fs::symlink(
                "nested.js",
                save_workspace.join("packages/app/node_modules/link.js"),
            )
            .unwrap();
            std::os::unix::fs::symlink(&outside, save_workspace.join("linked-outside")).unwrap();
            std::os::unix::fs::symlink(
                outside.join("modules"),
                save_workspace.join("packages/app/node_modules/escape"),
            )
            .unwrap();
        }
        fs::create_dir_all(&restore_workspace).unwrap();

        let env = [("GITHUB_REPOSITORY", "Test/Repo")];
        let paths = "**/target\n**/node_modules";
        let save = vec![native_cache_step(
            Some(CacheActionKind::Save),
            Some("save"),
            &[("path", paths), ("key", "glob-round-trip")],
        )];
        let save_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&save_temp),
                &save,
                &env.iter()
                    .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                    .collect::<Vec<_>>(),
                &save_temp,
            )
            .unwrap();
        assert!(save_results[0]
            .stdout
            .contains("Saved cache 'glob-round-trip'"));

        let cache_entry = cache_scope_store_dir(&root, "Test_Repo", paths).join("glob-round-trip");
        assert!(cache_entry_complete_for_paths(&cache_entry, paths));
        let target_manifest: CacheGlobManifest = serde_json::from_slice(
            &fs::read(cache_entry.join("0").join(CACHE_GLOB_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        let node_manifest: CacheGlobManifest = serde_json::from_slice(
            &fs::read(cache_entry.join("1").join(CACHE_GLOB_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        assert!(target_manifest
            .entries
            .iter()
            .any(|entry| entry.relative == "target"));
        assert!(target_manifest
            .entries
            .iter()
            .any(|entry| entry.relative == "packages/app/target"));
        assert!(node_manifest
            .entries
            .iter()
            .any(|entry| entry.relative == "node_modules"));
        assert!(node_manifest
            .entries
            .iter()
            .any(|entry| entry.relative == "packages/app/node_modules"));
        #[cfg(unix)]
        assert!(!target_manifest
            .entries
            .iter()
            .any(|entry| entry.relative.starts_with("linked-outside")));

        let restore = vec![native_cache_step(
            Some(CacheActionKind::Restore),
            Some("restore"),
            &[("path", paths), ("key", "glob-round-trip")],
        )];
        let restore_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&restore_temp),
                &restore,
                &env.iter()
                    .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                    .collect::<Vec<_>>(),
                &restore_temp,
            )
            .unwrap();
        assert_eq!(restore_results[0].state.outputs["cache-hit"], "true");
        assert_eq!(
            fs::read_to_string(restore_workspace.join("target/root.bin")).unwrap(),
            "root target\n"
        );
        assert_eq!(
            fs::read_to_string(restore_workspace.join("packages/app/target/nested.bin")).unwrap(),
            "nested target\n"
        );
        assert_eq!(
            fs::read_to_string(restore_workspace.join("node_modules/root.js")).unwrap(),
            "root modules\n"
        );
        assert_eq!(
            fs::read_to_string(restore_workspace.join("packages/app/node_modules/nested.js"))
                .unwrap(),
            "nested modules\n"
        );
        assert!(!restore_workspace
            .join("packages/app/node_modules/link.js")
            .exists());
        #[cfg(unix)]
        assert!(!restore_workspace
            .join("packages/app/node_modules/escape")
            .exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cache_restore_rejects_direct_and_glob_destination_symlinks() {
        use std::os::unix::fs::symlink;

        let root = temp_dir();
        let direct_source = root.join("direct-cache");
        let direct_destination = root.join("direct-work");
        let direct_outside = root.join("direct-outside");
        fs::create_dir_all(direct_source.join("nested")).unwrap();
        fs::create_dir_all(&direct_destination).unwrap();
        fs::create_dir_all(&direct_outside).unwrap();
        fs::write(direct_source.join("nested/state.bin"), "cached\n").unwrap();
        symlink(&direct_outside, direct_destination.join("nested")).unwrap();
        let direct_state =
            JobExecutionState::new_with_workspace(&[], &[], &direct_destination, &root);

        let direct_error =
            restore_cache_dir_contents(&direct_state, &direct_source, &direct_destination)
                .unwrap_err();

        assert!(
            format!("{direct_error:#}").contains("without following links")
                || format!("{direct_error:#}").contains("symlink"),
            "{direct_error:#}"
        );
        assert!(!direct_outside.join("state.bin").exists());

        let workspace = root.join("glob-work");
        let temp = root.join("glob-job/temp");
        let cache_entry = root.join("glob-cache");
        let glob_outside = root.join("glob-outside");
        fs::create_dir_all(workspace.join("packages/app")).unwrap();
        fs::create_dir_all(&temp).unwrap();
        fs::create_dir_all(cache_entry.join("0/payload/packages/app/target")).unwrap();
        fs::create_dir_all(&glob_outside).unwrap();
        fs::write(
            cache_entry.join("0/payload/packages/app/target/state.bin"),
            "cached\n",
        )
        .unwrap();
        fs::write(
            cache_entry.join("0").join(CACHE_GLOB_MANIFEST_FILE),
            r#"{"version":1,"entries":[{"relative":"packages/app/target","kind":"directory"}]}"#,
        )
        .unwrap();
        symlink(&glob_outside, workspace.join("packages/app/target")).unwrap();
        let state = JobExecutionState::new_with_workspace(&[], &[], &workspace, &temp);
        let paths = vec!["**/target".to_string()];

        let glob_error =
            restore_cache_glob_path(&state, &cache_entry, 0, "**/target", &paths).unwrap_err();

        assert!(format!("{glob_error:#}").contains("symlink"));
        assert!(!glob_outside.join("state.bin").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_glob_restore_handles_large_manifests_without_retaining_sources() {
        const ENTRY_COUNT: usize = 512;

        let root = temp_dir();
        let workspace = root.join("work");
        let temp = root.join("job/temp");
        let cache_entry = root.join("glob-cache");
        let payload = cache_entry.join("0/payload");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&temp).unwrap();
        fs::create_dir_all(&payload).unwrap();

        let mut entries = Vec::with_capacity(ENTRY_COUNT);
        for index in 0..ENTRY_COUNT {
            let relative = format!("entry-{index:04}.bin");
            fs::write(payload.join(&relative), index.to_string()).unwrap();
            entries.push(CacheGlobManifestEntry {
                relative,
                kind: CacheGlobEntryKind::File,
            });
        }
        fs::write(
            cache_entry.join("0").join(CACHE_GLOB_MANIFEST_FILE),
            serde_json::to_vec(&CacheGlobManifest {
                version: 1,
                entries,
            })
            .unwrap(),
        )
        .unwrap();

        let state = JobExecutionState::new_with_workspace(&[], &[], &workspace, &temp);
        let paths = vec!["**/*.bin".to_string()];
        restore_cache_glob_path(&state, &cache_entry, 0, "**/*.bin", &paths).unwrap();

        assert_eq!(
            fs::read_to_string(workspace.join("entry-0000.bin")).unwrap(),
            "0"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("entry-0511.bin")).unwrap(),
            "511"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_keeps_empty_glob_manifest_with_other_paths() {
        let root = temp_dir();
        let save_temp = root.join("save/temp");
        let restore_temp = root.join("restore/temp");
        fs::create_dir_all(save_temp.join("work/cache")).unwrap();
        fs::create_dir_all(&restore_temp).unwrap();
        fs::write(save_temp.join("work/cache/state.bin"), "state\n").unwrap();

        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let paths = "cache\n**/target";
        let save = vec![native_cache_step(
            Some(CacheActionKind::Save),
            Some("save"),
            &[("path", paths), ("key", "empty-glob")],
        )];
        DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &env, &save_temp)
            .unwrap();

        let entry = cache_scope_store_dir(&root, "Test_Repo", paths).join("empty-glob");
        assert!(entry.join("1").join(CACHE_GLOB_MANIFEST_FILE).is_file());
        let manifest: CacheGlobManifest = serde_json::from_slice(
            &fs::read(entry.join("1").join(CACHE_GLOB_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        assert!(manifest.entries.is_empty());
        assert!(cache_entry_complete_for_paths(&entry, paths));

        let restore = vec![native_cache_step(
            Some(CacheActionKind::Restore),
            Some("restore"),
            &[("path", paths), ("key", "empty-glob")],
        )];
        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&restore_temp), &restore, &env, &restore_temp)
            .unwrap();
        assert_eq!(results[0].state.outputs["cache-hit"], "true");
        assert_eq!(
            fs::read_to_string(restore_temp.join("work/cache/state.bin")).unwrap(),
            "state\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_glob_deduplicates_persistent_workspace_target() {
        let root = temp_dir();
        let temp = root.join("job/temp");
        let workspace = temp.join("work");
        let target_store = root.join("target-store");
        fs::create_dir_all(workspace.join("target")).unwrap();
        fs::create_dir_all(workspace.join("packages/app/target")).unwrap();
        fs::write(workspace.join("target/root.bin"), "persistent root\n").unwrap();
        fs::write(
            workspace.join("packages/app/target/nested.bin"),
            "keyed nested\n",
        )
        .unwrap();

        let mut state = JobExecutionState::new_with_workspace(
            &[("GITHUB_REPOSITORY".into(), "Test/Repo".into())],
            &[],
            &workspace,
            &temp,
        );
        state.persistent_workspace_target = true;
        state.cargo_target_host = Some(target_store);
        let paths = "target\n**/target";
        let version = cache_scope_version("", "", "", paths);
        save_cache_result(&state, "glob-dedup", paths, false, &version).unwrap();

        let cache_entry = cache_scope_store_dir(&root, "Test_Repo", paths).join("glob-dedup");
        let manifest: CacheGlobManifest = serde_json::from_slice(
            &fs::read(cache_entry.join("1").join(CACHE_GLOB_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        assert!(!manifest
            .entries
            .iter()
            .any(|entry| entry.relative == "target"));
        assert!(manifest
            .entries
            .iter()
            .any(|entry| entry.relative == "packages/app/target"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_glob_manifest_rejects_traversal_and_unmapped_roots() {
        for path in ["../escape", "nested/../../escape", "/absolute", "./target"] {
            assert!(
                validate_cache_glob_relative(path).is_err(),
                "manifest path unexpectedly accepted: {path}"
            );
        }
        assert!(validate_cache_glob_relative("packages/app/target").is_ok());

        let root = temp_dir();
        let temp = root.join("job/temp");
        let workspace = temp.join("work");
        fs::create_dir_all(&workspace).unwrap();
        let state = JobExecutionState::new_with_workspace(&[], &[], &workspace, &temp);
        assert!(cache_glob_base_and_pattern(&state, "/etc/**").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_cache_glob_is_rejected_before_store_creation() {
        let root = temp_dir();
        let temp = root.join("job/temp");
        fs::create_dir_all(temp.join("work")).unwrap();
        let step = native_cache_step(
            None,
            Some("invalid"),
            &[("path", "/etc/**"), ("key", "invalid-glob")],
        );
        let error = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&temp),
                &[step],
                &[("GITHUB_REPOSITORY".into(), "Test/Repo".into())],
                &temp,
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("outside Velnor-mapped job storage"));
        assert!(!root.join("_velnor_caches").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_actions_cache_save_glob_mutates_no_store() {
        let root = temp_dir();
        let temp = root.join("job/temp");
        fs::create_dir_all(temp.join("work/cache")).unwrap();
        fs::write(temp.join("work/cache/state.bin"), "state\n").unwrap();
        let paths = "cache/[unterminated";
        let step = native_cache_step(
            Some(CacheActionKind::Save),
            Some("save"),
            &[("path", paths), ("key", "malformed-actions-cache")],
        );

        let error = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&temp),
                &[step],
                &[("GITHUB_REPOSITORY".into(), "Test/Repo".into())],
                &temp,
            )
            .unwrap_err();

        assert!(error.to_string().contains("invalid cache glob syntax"));
        assert!(!root.join("_velnor_caches").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_rust_cache_save_glob_mutates_no_store() {
        let root = temp_dir();
        let temp = root.join("job/temp");
        let workspace = temp.join("work");
        fs::create_dir_all(workspace.join("cache")).unwrap();
        fs::write(workspace.join("cache/state.bin"), "state\n").unwrap();
        let state = JobExecutionState::new_with_workspace(
            &[("GITHUB_REPOSITORY".into(), "Test/Repo".into())],
            &[],
            &workspace,
            &temp,
        );
        let action = NativeActionInvocation {
            git_ref: String::new(),
            adapter: NativeActionAdapter::RustCache,
            cache_kind: None,
            source_path: None,
            inputs: [
                ("shared-key".into(), "malformed-rust-cache".into()),
                ("cache-directories".into(), "cache/[unterminated".into()),
            ]
            .into(),
            env: Vec::new(),
        };

        let error = native_rust_cache_save("rust-cache", &action, &state).unwrap_err();

        assert!(error.to_string().contains("invalid cache glob syntax"));
        assert!(!root.join("_velnor_caches").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn canonical_fleet_cache_aliases_are_host_persistent() {
        for path in [
            ".cache/mise",
            ".cache/mise/downloads",
            ".local/share/mise/installs",
            ".local/share/mise/installs/rust/1.85.0",
            ".cargo/registry",
            ".cargo/registry/cache",
            ".cargo/git",
            ".cargo/git/db",
        ] {
            assert!(
                velnor_static_persistent_cache_path(path),
                "canonical cache alias was not recognized: {path}"
            );
        }
        // Matching is component-aware: similarly named workspace paths must
        // remain eligible for ordinary keyed cache storage.
        for path in [
            ".cargo/registry-old",
            ".cache/mise-old",
            ".cargo/config.toml",
        ] {
            assert!(
                !velnor_static_persistent_cache_path(path),
                "non-canonical path was incorrectly treated as persistent: {path}"
            );
        }
    }

    #[test]
    fn mixed_cache_paths_report_host_persistent_subset() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), ".cargo/git\n.gradle/caches\n".into()),
                    ("key".into(), "dependencies-Linux-X64-lock".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert!(results[0]
            .stdout
            .contains("1/2 cache path(s) live on Velnor host-persistent storage"));
        assert!(!results[0].stdout.contains("Cache not found for input keys"));
        assert!(results[0]
            .state
            .summary
            .contains("keyed miss; 1/2 paths host-persistent"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_cache_skips_relative_target_when_native_target_is_persistent() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let mut spec = container(&temp);
        spec.cargo_target_host = Some(temp.join("target-store"));
        let steps = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), "target".into()),
                    ("key".into(), "build-output-Linux-X64-lock".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&spec, &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result
            .state
            .summary
            .contains("host-persistent store — restore/save skipped")));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_cache_globs_round_trip_with_concrete_manifest() {
        let root = temp_dir();
        let save_temp = root.join("save-job/temp");
        let restore_temp = root.join("restore-job/temp");
        let save_workspace = save_temp.join("work");
        fs::create_dir_all(save_workspace.join("target/debug")).unwrap();
        fs::write(save_workspace.join("target/debug/app"), "target\n").unwrap();
        fs::create_dir_all(save_workspace.join("packages/app/node_modules/vendor/node_modules"))
            .unwrap();
        fs::write(
            save_workspace.join("packages/app/node_modules/package.json"),
            "node\n",
        )
        .unwrap();
        fs::write(
            save_workspace.join("packages/app/node_modules/vendor/node_modules/nested.js"),
            "nested\n",
        )
        .unwrap();
        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let paths = "**/target\n**/node_modules";
        let save = vec![native_cache_step(
            Some(CacheActionKind::Save),
            Some("save"),
            &[("path", paths), ("key", "glob-round-trip")],
        )];
        let save_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &env, &save_temp)
            .unwrap();
        assert!(save_results[0]
            .stdout
            .contains("Saved cache 'glob-round-trip'"));

        let entry = cache_scope_store_dir(&root, "Test_Repo", paths).join("glob-round-trip");
        let target_manifest: CacheGlobManifest = serde_json::from_slice(
            &fs::read(entry.join("0").join(CACHE_GLOB_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        let node_manifest: CacheGlobManifest = serde_json::from_slice(
            &fs::read(entry.join("1").join(CACHE_GLOB_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(target_manifest.version, 1);
        assert_eq!(node_manifest.version, 1);
        assert_eq!(
            target_manifest
                .entries
                .iter()
                .map(|entry| entry.relative.as_str())
                .collect::<Vec<_>>(),
            vec!["target"]
        );
        assert_eq!(
            node_manifest
                .entries
                .iter()
                .map(|entry| entry.relative.as_str())
                .collect::<Vec<_>>(),
            vec!["packages/app/node_modules"]
        );

        let restore = vec![native_cache_step(
            Some(CacheActionKind::Restore),
            Some("restore"),
            &[("path", paths), ("key", "glob-round-trip")],
        )];
        let restore_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&restore_temp), &restore, &env, &restore_temp)
            .unwrap();
        assert_eq!(restore_results[0].state.outputs["cache-hit"], "true");
        assert_eq!(
            fs::read_to_string(restore_temp.join("work/target/debug/app")).unwrap(),
            "target\n"
        );
        assert_eq!(
            fs::read_to_string(restore_temp.join("work/packages/app/node_modules/package.json"))
                .unwrap(),
            "node\n"
        );
        assert_eq!(
            fs::read_to_string(
                restore_temp.join("work/packages/app/node_modules/vendor/node_modules/nested.js")
            )
            .unwrap(),
            "nested\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_glob_deduplicates_persistent_target_root() {
        let root = temp_dir();
        let save_temp = root.join("save-job/temp");
        let restore_temp = root.join("restore-job/temp");
        let save_workspace = save_temp.join("work");
        fs::create_dir_all(save_workspace.join("target")).unwrap();
        fs::write(save_workspace.join("target/root.txt"), "root\n").unwrap();
        fs::create_dir_all(save_workspace.join("packages/core/target")).unwrap();
        fs::write(
            save_workspace.join("packages/core/target/nested.txt"),
            "nested\n",
        )
        .unwrap();
        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let paths = "target\n**/target";
        let mut save_container = container(&save_temp);
        save_container.cargo_target_host = Some(save_temp.join("target-store"));
        let save = vec![native_cache_step(
            Some(CacheActionKind::Save),
            Some("save"),
            &[("path", paths), ("key", "target-glob-dedup")],
        )];
        let save_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&save_container, &save, &env, &save_temp)
            .unwrap();
        assert!(save_results[0]
            .stdout
            .contains("Saved cache 'target-glob-dedup'"));

        let entry = cache_scope_store_dir(&root, "Test_Repo", paths).join("target-glob-dedup");
        assert!(!entry.join("0").exists());
        let manifest: CacheGlobManifest = serde_json::from_slice(
            &fs::read(entry.join("1").join(CACHE_GLOB_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest
                .entries
                .iter()
                .map(|entry| entry.relative.as_str())
                .collect::<Vec<_>>(),
            vec!["packages/core/target"]
        );

        let mut restore_container = container(&restore_temp);
        restore_container.cargo_target_host = Some(restore_temp.join("target-store"));
        let restore = vec![native_cache_step(
            Some(CacheActionKind::Restore),
            Some("restore"),
            &[("path", paths), ("key", "target-glob-dedup")],
        )];
        let restore_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&restore_container, &restore, &env, &restore_temp)
            .unwrap();
        assert_eq!(restore_results[0].state.outputs["cache-hit"], "true");
        assert!(!restore_temp.join("work/target/root.txt").exists());
        assert_eq!(
            fs::read_to_string(restore_temp.join("work/packages/core/target/nested.txt")).unwrap(),
            "nested\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_legacy_glob_entry_is_replaced_on_save() {
        let root = temp_dir();
        let temp = root.join("job/temp");
        let workspace = temp.join("work");
        fs::create_dir_all(workspace.join("target")).unwrap();
        fs::write(workspace.join("target/app"), "v1\n").unwrap();
        let state = JobExecutionState::new_with_workspace(
            &[("GITHUB_REPOSITORY".into(), "Test/Repo".into())],
            &[],
            &workspace,
            &temp,
        );
        let paths = "**/target";
        let version = cache_scope_version("", "", "", paths);
        save_cache_result(&state, "legacy-glob", paths, false, &version).unwrap();

        let entry = cache_scope_store_dir(&root, "Test_Repo", paths).join("legacy-glob");
        fs::remove_file(entry.join("0").join(CACHE_GLOB_MANIFEST_FILE)).unwrap();
        fs::write(workspace.join("target/app"), "v2\n").unwrap();
        save_cache_result(&state, "legacy-glob", paths, false, &version).unwrap();
        assert!(cache_entry_complete_for_paths(&entry, paths));
        assert!(entry.join("0").join(CACHE_GLOB_MANIFEST_FILE).is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_glob_manifest_rejects_traversal_before_restore() {
        assert!(validate_cache_glob_relative("../escape").is_err());
        assert!(validate_cache_glob_relative("/absolute").is_err());
        assert!(validate_cache_glob_relative("nested\\escape").is_err());
        assert_eq!(
            validate_cache_glob_relative("packages/app/node_modules").unwrap(),
            PathBuf::from("packages/app/node_modules")
        );

        let root = temp_dir();
        let save_temp = root.join("save-job/temp");
        let restore_temp = root.join("restore-job/temp");
        let save_workspace = save_temp.join("work");
        fs::create_dir_all(save_workspace.join("packages/app/node_modules")).unwrap();
        fs::write(
            save_workspace.join("packages/app/node_modules/package.json"),
            "node\n",
        )
        .unwrap();
        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let paths = "**/node_modules";
        let save = vec![native_cache_step(
            Some(CacheActionKind::Save),
            Some("save"),
            &[("path", paths), ("key", "glob-traversal")],
        )];
        DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &env, &save_temp)
            .unwrap();
        let entry = cache_scope_store_dir(&root, "Test_Repo", paths).join("glob-traversal");
        fs::write(
            entry.join("0").join(CACHE_GLOB_MANIFEST_FILE),
            r#"{"version":1,"entries":[{"relative":"../escape","kind":"directory"}]}"#,
        )
        .unwrap();
        let restore = vec![native_cache_step(
            Some(CacheActionKind::Restore),
            Some("restore"),
            &[("path", paths), ("key", "glob-traversal")],
        )];
        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&restore_temp), &restore, &env, &restore_temp)
            .unwrap();
        assert_eq!(results[0].state.outputs["cache-hit"], "false");
        assert!(!root.join("restore-job/escape").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_rust_cache_treats_persistent_cargo_target_as_warm() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let mut spec = container(&temp);
        spec.cargo_target_host = Some(temp.join("target-store"));
        let steps = vec![ExecutableStep::Native {
            step_id: "rust-cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::RustCache,
                cache_kind: None,
                source_path: None,
                inputs: [("shared-key".into(), "ci-default-dev-workspace-v2".into())].into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&spec, &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].exit_code, 0);
        assert_eq!(results[0].state.outputs["cache-hit"], "true");
        assert!(results[0]
            .stdout
            .contains("Rust cache paths live on Velnor host-persistent storage"));
        assert!(!results[0].stdout.contains("Rust cache miss"));
        assert!(results[0]
            .state
            .summary
            .contains("Backend: rust-cache (native)"));
        assert!(results[1].stdout.contains("Cache hit occurred"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_rust_cache_sees_container_persistent_cargo_target() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let mut spec = container(&temp);
        spec.cargo_target_host = Some(temp.join("target-store"));
        let steps = vec![ExecutableStep::Native {
            step_id: "rust-cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::RustCache,
                cache_kind: None,
                source_path: None,
                inputs: [("shared-key".into(), "ci-default-dev-workspace-v2".into())].into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&spec, &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].state.outputs["cache-hit"], "true");
        assert!(results[0]
            .stdout
            .contains("Rust cache paths live on Velnor host-persistent storage"));
        assert!(!results[0].stdout.contains("Rust cache miss"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_rust_cache_treats_persistent_cache_directories_as_warm() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "rust-cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::RustCache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("shared-key".into(), "ci-custom-dir".into()),
                    (
                        "cache-directories".into(),
                        "/__w/target\n/var/cache/sccache\n".into(),
                    ),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let mut spec = container(&temp);
        spec.cargo_target_host = Some(temp.join("target-store"));
        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&spec, &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].state.outputs["cache-hit"], "true");
        assert!(results[0]
            .stdout
            .contains("Rust cache paths live on Velnor host-persistent storage"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_cache_can_fail_on_miss() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), "target".into()),
                    ("key".into(), "linux-cache".into()),
                    ("fail-on-cache-miss".into(), "true".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].exit_code, 1);
        assert_eq!(results[0].state.outputs["cache-hit"], "false");
        assert!(results[0].stderr.contains("fail-on-cache-miss"));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_cache_fail_on_miss_is_quiet_on_hit() {
        let root = temp_dir();
        let save_temp = root.join("save-job/temp");
        let restore_temp = root.join("restore-job/temp");
        fs::create_dir_all(root.join("save-job/home/.cache/rust-script")).unwrap();
        fs::create_dir_all(root.join("restore-job/home")).unwrap();
        fs::write(
            root.join("save-job/home/.cache/rust-script/state.bin"),
            "cached\n",
        )
        .unwrap();
        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let save = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-strict".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &env, &save_temp)
            .unwrap();

        let restore = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-strict".into()),
                    ("fail-on-cache-miss".into(), "true".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let restore_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&restore_temp), &restore, &env, &restore_temp)
            .unwrap();

        assert_eq!(restore_results[0].exit_code, 0);
        assert_eq!(restore_results[0].state.outputs["cache-hit"], "true");
        assert!(restore_results[0].stderr.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_trims_folded_yaml_primary_key() {
        let root = temp_dir();
        let save_temp = root.join("save-job/temp");
        let restore_temp = root.join("restore-job/temp");
        fs::create_dir_all(root.join("save-job/home/.cache/rust-script")).unwrap();
        fs::create_dir_all(root.join("restore-job/home")).unwrap();
        fs::write(
            root.join("save-job/home/.cache/rust-script/state.bin"),
            "cached\n",
        )
        .unwrap();
        let folded_key = "rust-script-Linux-deadbeef\n";
        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let save = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), folded_key.into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let save_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &env, &save_temp)
            .unwrap();

        // Root exposes only `cache-hit`; the trimmed key is proven by the
        // internal primaryKey state and the store entry name below.
        assert!(!save_results[0]
            .state
            .outputs
            .contains_key("cache-primary-key"));
        assert_eq!(
            save_results[0].state.state["primaryKey"],
            "rust-script-Linux-deadbeef"
        );
        assert!(
            cache_scope_store_dir(&root, "Test_Repo", "~/.cache/rust-script")
                .join("rust-script-Linux-deadbeef/0/state.bin")
                .exists()
        );

        let restore = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "rust-script-Linux-deadbeef".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let restore_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&restore_temp), &restore, &env, &restore_temp)
            .unwrap();

        assert_eq!(restore_results[0].state.outputs["cache-hit"], "true");
        assert_eq!(
            fs::read_to_string(root.join("restore-job/home/.cache/rust-script/state.bin")).unwrap(),
            "cached\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_staging_dirs_are_unique_per_save() {
        let first = cache_staging_name("linux-rust-script-abc");
        let second = cache_staging_name("linux-rust-script-abc");

        assert_ne!(first, second);
        assert!(first.starts_with("linux-rust-script-abc.staging-"));
        assert!(second.starts_with("linux-rust-script-abc.staging-"));
    }

    #[test]
    fn incomplete_cache_generation_is_never_a_hit() {
        let root = temp_dir();
        let restore_temp = root.join("restore-job/temp");
        let version = "cv1-testscope";
        let entry = root.join(format!(
            "_velnor_caches/trusted/Test_Repo/{version}/rustup-v2-linux-key"
        ));
        fs::create_dir_all(entry.join("0/toolchains/1.97.0/bin")).unwrap();
        fs::create_dir_all(root.join("restore-job/home")).unwrap();
        fs::write(entry.join(".velnor-key"), "rustup-v2-linux-key").unwrap();
        fs::write(entry.join("0/toolchains/1.97.0/bin/cargo"), "partial").unwrap();
        let state = JobExecutionState::default()
            .with_env(vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())]);
        let mut state = state;
        state.temp_host = Some(restore_temp);

        assert_eq!(
            find_cache_match(
                &state,
                "rustup-v2-linux-key",
                "rustup-v2-linux-",
                version,
                "",
            )
            .unwrap(),
            None
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cache_copy_rejects_symlink_traversal() {
        let root = temp_dir();
        let source = root.join("source");
        let destination = root.join("destination");
        let outside = root.join("outside");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), "outside\n").unwrap();
        std::os::unix::fs::symlink(&outside, source.join("escape")).unwrap();

        let error = copy_dir_contents(&source, &destination).unwrap_err();
        assert!(error.to_string().contains("is a symlink"), "{error:#}");
        assert!(!destination.join("escape/secret").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_target_is_job_local_and_published_as_complete_generation() {
        let root = temp_dir();
        let temp = root.join("job/temp");
        fs::create_dir_all(&temp).unwrap();
        let mut spec = container(&temp);
        let store = root.join("target-store");
        fs::create_dir_all(store.join("data/debug")).unwrap();
        fs::write(store.join(".velnor-target-complete-v1"), "complete\n").unwrap();
        fs::write(store.join(TARGET_SOURCE_REVISION_MARKER), "old-revision\n").unwrap();
        fs::write(store.join("data/debug/seed"), "warm\n").unwrap();
        fs::create_dir_all(&spec.workspace_host).unwrap();
        fs::write(spec.workspace_host.join("Cargo.toml"), "[workspace]\n").unwrap();
        let stale_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
        fs::File::options()
            .append(true)
            .open(spec.workspace_host.join("Cargo.toml"))
            .unwrap()
            .set_modified(stale_time)
            .unwrap();
        spec.cargo_target_host = Some(store.clone());
        assert!(!spec
            .start_args()
            .iter()
            .any(|arg| arg.contains(":/__w/target")));

        materialize_persistent_target(&spec, Some("new-revision")).unwrap();
        let target = spec.workspace_host.join("target");
        assert!(!target.exists());
        assert_eq!(
            fs::metadata(spec.workspace_host.join("Cargo.toml"))
                .unwrap()
                .modified()
                .unwrap(),
            stale_time
        );

        // Both paths remain inside the workspace mount, so the workflow's
        // ordinary atomic promotion cannot fail with EXDEV.
        fs::create_dir_all(target.join("debug")).unwrap();
        fs::write(target.join("debug/seed"), "compiled\n").unwrap();
        fs::create_dir_all(spec.workspace_host.join(".ci-target-cache")).unwrap();
        fs::rename(
            target.join("debug/seed"),
            spec.workspace_host.join(".ci-target-cache/target.tar.zst"),
        )
        .unwrap();
        fs::write(target.join("new-output"), "compiled\n").unwrap();
        publish_persistent_target(&spec, Some("new-revision")).unwrap();

        assert!(store.join(".velnor-target-complete-v1").is_file());
        assert_eq!(
            fs::read_to_string(store.join(TARGET_SOURCE_REVISION_MARKER)).unwrap(),
            "new-revision\n"
        );
        assert_eq!(
            fs::read_to_string(store.join("data/new-output")).unwrap(),
            "compiled\n"
        );
        fs::File::options()
            .append(true)
            .open(spec.workspace_host.join("Cargo.toml"))
            .unwrap()
            .set_modified(stale_time)
            .unwrap();
        materialize_persistent_target(&spec, Some("new-revision")).unwrap();
        assert_eq!(
            fs::metadata(spec.workspace_host.join("Cargo.toml"))
                .unwrap()
                .modified()
                .unwrap(),
            stale_time
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_target_publishes_only_after_successful_steps() {
        let result = |exit_code, skipped| StepExecutionResult {
            exit_code,
            state: StepCommandState::default(),
            skipped,
            failure_ignored: false,
            stdout: String::new(),
            stderr: String::new(),
        };

        assert!(persistent_target_results_publishable(&[
            result(0, false),
            result(0, true),
        ]));
        assert!(!persistent_target_results_publishable(&[
            result(0, false),
            result(1, false),
        ]));
        assert!(!persistent_target_results_publishable(&[
            StepExecutionResult {
                failure_ignored: true,
                ..result(1, false)
            },
        ]));
    }

    #[test]
    fn native_cache_saves_and_restores_from_shared_workdir() {
        let root = temp_dir();
        let save_temp = root.join("save-job/temp");
        let restore_temp = root.join("restore-job/temp");
        fs::create_dir_all(root.join("save-job/home/.cache/rust-script")).unwrap();
        fs::create_dir_all(root.join("restore-job/home")).unwrap();
        fs::write(
            root.join("save-job/home/.cache/rust-script/state.bin"),
            "cached\n",
        )
        .unwrap();
        let env = vec![
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_REPOSITORY".into(), "Test/Repo".into()),
        ];
        let save = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-abc".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let save_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &env, &save_temp)
            .unwrap();

        assert_eq!(save_results[0].state.outputs["cache-hit"], "false");
        assert!(save_results[1]
            .stdout
            .contains("Saved cache 'linux-rust-script-abc'"));
        assert!(
            cache_scope_store_dir(&root, "Test_Repo", "~/.cache/rust-script")
                .join("linux-rust-script-abc/0/state.bin")
                .exists()
        );

        let restore = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-abc".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let restore_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&restore_temp), &restore, &env, &restore_temp)
            .unwrap();

        assert_eq!(restore_results[0].state.outputs["cache-hit"], "true");
        // Root exposes only `cache-hit`, not `cache-matched-key`.
        assert!(!restore_results[0]
            .state
            .outputs
            .contains_key("cache-matched-key"));
        assert_eq!(
            fs::read_to_string(root.join("restore-job/home/.cache/rust-script/state.bin")).unwrap(),
            "cached\n"
        );
        assert!(restore_results[1]
            .stdout
            .contains("Cache hit occurred on primary key"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_lookup_only_does_not_restore_paths() {
        let root = temp_dir();
        let save_temp = root.join("save-job/temp");
        let lookup_temp = root.join("lookup-job/temp");
        fs::create_dir_all(root.join("save-job/home/.cache/rust-script")).unwrap();
        fs::create_dir_all(root.join("lookup-job/home")).unwrap();
        fs::write(
            root.join("save-job/home/.cache/rust-script/state.bin"),
            "cached\n",
        )
        .unwrap();
        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let save = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-lookup".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &env, &save_temp)
            .unwrap();

        let lookup = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-lookup".into()),
                    ("lookup-only".into(), "true".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let lookup_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&lookup_temp), &lookup, &env, &lookup_temp)
            .unwrap();

        assert_eq!(lookup_results[0].exit_code, 0);
        assert_eq!(lookup_results[0].state.outputs["cache-hit"], "true");
        assert!(lookup_results[0].stdout.contains("Cache lookup found key"));
        assert!(!root
            .join("lookup-job/home/.cache/rust-script/state.bin")
            .exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_restore_key_uses_newest_prefix_match() {
        let root = temp_dir();
        let restore_temp = root.join("restore-job/temp");
        let store = cache_scope_store_dir(&root, "Test_Repo", "~/.cache/rust-script");
        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let old_cache = store.join("rust-linux-a-old");
        let new_cache = store.join("rust-linux-z-new");
        fs::create_dir_all(old_cache.join("0")).unwrap();
        fs::create_dir_all(new_cache.join("0")).unwrap();
        fs::create_dir_all(root.join("restore-job/home")).unwrap();
        fs::write(old_cache.join(".velnor-key"), "rust-linux-a-old").unwrap();
        fs::write(old_cache.join(".velnor-created"), "1").unwrap();
        fs::write(old_cache.join(".velnor-complete-v1"), "complete\n").unwrap();
        fs::write(old_cache.join("0/state.bin"), "old\n").unwrap();
        fs::write(new_cache.join(".velnor-key"), "rust-linux-z-new").unwrap();
        fs::write(new_cache.join(".velnor-created"), "2").unwrap();
        fs::write(new_cache.join(".velnor-complete-v1"), "complete\n").unwrap();
        fs::write(new_cache.join("0/state.bin"), "new\n").unwrap();

        let restore = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                // `/restore` exposes cache-matched-key, which this test asserts.
                cache_kind: Some(CacheActionKind::Restore),
                source_path: Some("restore".into()),
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "rust-linux-exact-miss".into()),
                    ("restore-keys".into(), "rust-linux-\n".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let restore_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&restore_temp), &restore, &env, &restore_temp)
            .unwrap();

        assert_eq!(restore_results[0].state.outputs["cache-hit"], "false");
        assert_eq!(
            restore_results[0].state.outputs["cache-matched-key"],
            "rust-linux-z-new"
        );
        // `/restore` performs no post save.
        assert_eq!(restore_results.len(), 1);
        assert_eq!(
            fs::read_to_string(root.join("restore-job/home/.cache/rust-script/state.bin")).unwrap(),
            "new\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn native_cache_step(
        cache_kind: Option<CacheActionKind>,
        source_path: Option<&str>,
        inputs: &[(&str, &str)],
    ) -> ExecutableStep {
        ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Cache,
                cache_kind,
                source_path: source_path.map(str::to_string),
                inputs: inputs
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }
    }

    #[test]
    fn native_cache_restore_only_produces_one_result_and_never_saves() {
        let root = temp_dir();
        let temp = root.join("restore-job/temp");
        fs::create_dir_all(root.join("restore-job/home/.cache/rust-script")).unwrap();
        fs::write(
            root.join("restore-job/home/.cache/rust-script/state.bin"),
            "local\n",
        )
        .unwrap();
        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let steps = vec![native_cache_step(
            Some(CacheActionKind::Restore),
            Some("restore"),
            &[("path", "~/.cache/rust-script"), ("key", "linux-miss")],
        )];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &steps, &env, &temp)
            .unwrap();

        // `/restore` registers no post action.
        assert_eq!(results.len(), 1);
        // `/restore` exposes all three outputs.
        assert_eq!(results[0].state.outputs["cache-hit"], "false");
        assert!(results[0].state.outputs.contains_key("cache-primary-key"));
        assert!(results[0].state.outputs.contains_key("cache-matched-key"));
        // A restore never writes an entry, even when the workspace path exists.
        let store = cache_scope_store_dir(&root, "Test_Repo", "~/.cache/rust-script");
        assert!(!store.join("linux-miss").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_save_only_persists_without_prior_restore_and_reports_timing() {
        let root = temp_dir();
        let temp = root.join("save-job/temp");
        let home = root.join("save-job/home/.cache/rust-script");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("state.bin"), "fresh\n").unwrap();
        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let steps = vec![native_cache_step(
            Some(CacheActionKind::Save),
            Some("save"),
            &[("path", "~/.cache/rust-script"), ("key", "linux-save-only")],
        )];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &steps, &env, &temp)
            .unwrap();

        // `/save` registers no post action and exposes no outputs.
        assert_eq!(results.len(), 1);
        assert!(results[0].state.outputs.is_empty());
        assert!(results[0].stdout.contains("Saved cache 'linux-save-only'"));
        // Timing fields are recorded in the summary.
        assert!(results[0].state.summary.contains("Lock wait:"));
        assert!(results[0].state.summary.contains("Copy:"));
        assert!(results[0].state.summary.contains("Total:"));
        assert!(results[0].state.summary.contains("Result: saved"));
        let store = cache_scope_store_dir(&root, "Test_Repo", "~/.cache/rust-script");
        assert!(store.join("linux-save-only/.velnor-complete-v1").is_file());
        assert_eq!(
            fs::read_to_string(store.join("linux-save-only/0/state.bin")).unwrap(),
            "fresh\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_save_only_does_not_materialize_existing_entry() {
        let root = temp_dir();
        let temp = root.join("save-job/temp");
        let home = root.join("save-job/home/.cache/rust-script");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("state.bin"), "new\n").unwrap();
        // Pre-seed a complete entry with different (old) content.
        let store = cache_scope_store_dir(&root, "Test_Repo", "~/.cache/rust-script");
        let entry = store.join("linux-existing");
        fs::create_dir_all(entry.join("0")).unwrap();
        fs::write(entry.join(".velnor-key"), "linux-existing").unwrap();
        fs::write(entry.join(".velnor-created"), "1").unwrap();
        fs::write(entry.join(".velnor-complete-v1"), "complete\n").unwrap();
        fs::write(entry.join("0/state.bin"), "old\n").unwrap();
        let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
        let steps = vec![native_cache_step(
            Some(CacheActionKind::Save),
            Some("save"),
            &[("path", "~/.cache/rust-script"), ("key", "linux-existing")],
        )];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &steps, &env, &temp)
            .unwrap();

        assert_eq!(results.len(), 1);
        // Existing key → already exists; never restored into the workspace and
        // the stored generation is left untouched.
        assert!(results[0].state.summary.contains("already exists"));
        assert!(!results[0].state.summary.contains("completed"));
        assert_eq!(fs::read_to_string(home.join("state.bin")).unwrap(), "new\n");
        assert_eq!(
            fs::read_to_string(entry.join("0/state.bin")).unwrap(),
            "old\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_output_sets_match_upstream_by_kind() {
        let output_keys = |cache_kind, source_path| {
            let root = temp_dir();
            let temp = root.join("job/temp");
            fs::create_dir_all(root.join("job/home")).unwrap();
            let env = vec![("GITHUB_REPOSITORY".into(), "Test/Repo".into())];
            let steps = vec![native_cache_step(
                cache_kind,
                source_path,
                &[("path", "~/.cache/rust-script"), ("key", "some-key")],
            )];
            let results = DockerJobEngine::new(RecordingRunner::default())
                .execute_ordered_steps(&container(&temp), &steps, &env, &temp)
                .unwrap();
            let keys: std::collections::BTreeSet<String> =
                results[0].state.outputs.keys().cloned().collect();
            fs::remove_dir_all(&root).ok();
            keys
        };

        let expect = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<std::collections::BTreeSet<String>>()
        };

        assert_eq!(
            output_keys(Some(CacheActionKind::Root), None),
            expect(&["cache-hit"])
        );
        assert_eq!(
            output_keys(Some(CacheActionKind::Restore), Some("restore")),
            expect(&["cache-hit", "cache-primary-key", "cache-matched-key"])
        );
        assert!(output_keys(Some(CacheActionKind::Save), Some("save")).is_empty());
    }

    #[test]
    fn cache_scope_version_isolates_each_compatibility_dimension() {
        let base = cache_scope_version("sha1", "Linux", "X64", "~/.cache/a");
        // Stable for identical inputs.
        assert_eq!(
            base,
            cache_scope_version("sha1", "Linux", "X64", "~/.cache/a")
        );
        // Distinct across every runtime-compatibility dimension.
        assert_ne!(
            base,
            cache_scope_version("sha2", "Linux", "X64", "~/.cache/a")
        );
        assert_ne!(
            base,
            cache_scope_version("sha1", "Windows", "X64", "~/.cache/a")
        );
        assert_ne!(
            base,
            cache_scope_version("sha1", "Linux", "ARM64", "~/.cache/a")
        );
        assert_ne!(
            base,
            cache_scope_version("sha1", "Linux", "X64", "~/.cache/b")
        );
        // Paths are normalized (order/whitespace insensitive).
        assert_eq!(
            cache_scope_version("sha1", "Linux", "X64", "a\nb"),
            cache_scope_version("sha1", "Linux", "X64", " b \n a ")
        );
        assert!(base.starts_with("cv1-"));
    }

    #[test]
    fn native_cache_scope_does_not_cross_runner_os() {
        let root = temp_dir();
        let repo = ("GITHUB_REPOSITORY".to_string(), "Test/Repo".to_string());
        let save_temp = root.join("save/temp");
        fs::create_dir_all(root.join("save/home/.cache/rust-script")).unwrap();
        fs::write(
            root.join("save/home/.cache/rust-script/state.bin"),
            "linux\n",
        )
        .unwrap();
        let save_env = vec![repo.clone(), ("RUNNER_OS".into(), "Linux".into())];
        let save = vec![native_cache_step(
            Some(CacheActionKind::Save),
            Some("save"),
            &[("path", "~/.cache/rust-script"), ("key", "shared-key")],
        )];
        DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &save_env, &save_temp)
            .unwrap();

        let restore = vec![native_cache_step(
            Some(CacheActionKind::Restore),
            Some("restore"),
            &[("path", "~/.cache/rust-script"), ("key", "shared-key")],
        )];

        // A different RUNNER_OS lands in a different version namespace → miss.
        let win_temp = root.join("win/temp");
        fs::create_dir_all(root.join("win/home")).unwrap();
        let win_env = vec![repo.clone(), ("RUNNER_OS".into(), "Windows".into())];
        let win_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&win_temp), &restore, &win_env, &win_temp)
            .unwrap();
        assert_eq!(win_results[0].state.outputs["cache-hit"], "false");
        assert!(!root.join("win/home/.cache/rust-script/state.bin").exists());

        // Same RUNNER_OS restores the entry.
        let lin_temp = root.join("lin/temp");
        fs::create_dir_all(root.join("lin/home")).unwrap();
        let lin_env = vec![repo, ("RUNNER_OS".into(), "Linux".into())];
        let lin_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&lin_temp), &restore, &lin_env, &lin_temp)
            .unwrap();
        assert_eq!(lin_results[0].state.outputs["cache-hit"], "true");
        assert_eq!(
            fs::read_to_string(root.join("lin/home/.cache/rust-script/state.bin")).unwrap(),
            "linux\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_save_two_writers_yield_single_generation() {
        use std::sync::{Arc, Barrier};
        let root = temp_dir();
        let temp = root.join("job/temp");
        fs::create_dir_all(root.join("job/home/.cache/rust-script")).unwrap();
        fs::write(
            root.join("job/home/.cache/rust-script/state.bin"),
            "payload\n",
        )
        .unwrap();
        let env = vec![("GITHUB_REPOSITORY".to_string(), "Test/Repo".to_string())];
        let version = cache_scope_version("", "", "", "~/.cache/rust-script");

        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let env = env.clone();
                let temp = temp.clone();
                let version = version.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let state = JobExecutionState::new_internal(&env, &[], None, Some(temp));
                    barrier.wait();
                    save_cache_result(
                        &state,
                        "contended-key",
                        "~/.cache/rust-script",
                        false,
                        &version,
                    )
                    .unwrap()
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        // Exactly one writer persists; the loser reports a non-persisting
        // outcome (already-exists or declared contention), never "completed".
        let saved = results
            .iter()
            .filter(|result| result.stdout.contains("Saved cache 'contended-key'"))
            .count();
        let not_saved = results
            .iter()
            .filter(|result| {
                result.state.summary.contains("already exists")
                    || result.state.summary.contains("contention")
            })
            .count();
        assert_eq!(saved, 1, "exactly one writer persists");
        assert_eq!(
            not_saved, 1,
            "the loser reports already-exists or contention"
        );
        assert!(results
            .iter()
            .all(|result| !result.state.summary.contains("completed")));

        // Both observe exactly one complete generation.
        let store = cache_scope_store_dir(&root, "Test_Repo", "~/.cache/rust-script");
        let entries: Vec<String> = fs::read_dir(&store)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name != ".velnor-locks")
            .collect();
        assert_eq!(entries, vec!["contended-key".to_string()]);
        assert!(store.join("contended-key/.velnor-complete-v1").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_docker_adapters_invoke_docker_cli_without_node_sidecars() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work/backend-rust/bitcoin-processor-app")).unwrap();
        fs::write(
            temp.join("work/backend-rust/bitcoin-processor-app/Dockerfile"),
            "FROM scratch\n",
        )
        .unwrap();
        fs::write(
            temp.join("work/backend-rust/docker-bake.hcl"),
            "target \"app\" {}\n",
        )
        .unwrap();
        let steps = vec![
            ExecutableStep::Native {
                step_id: "buildx".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::DockerSetupBuildx,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("name".into(), "velnor-builder".into()),
                        ("driver".into(), "docker-container".into()),
                        ("install".into(), "true".into()),
                        (
                            "buildkitd-config-inline".into(),
                            "[registry.\"docker.io\"]\n  mirrors = [\"mirror.gcr.io\"]\n".into(),
                        ),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Native {
                step_id: "login".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::DockerLogin,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("username".into(), "docker-user".into()),
                        ("password".into(), "${{ secrets.DOCKER_TOKEN }}".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Native {
                step_id: "meta".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::DockerMetadata,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("images".into(), "chainargos/rust-bitcoin-processor".into()),
                        (
                            "tags".into(),
                            "type=raw,value=latest,enable=false\n\
type=sha,format=long,prefix=,enable=true"
                                .into(),
                        ),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Native {
                step_id: "build-push".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::DockerBuildPush,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("context".into(), ".".into()),
                        (
                            "file".into(),
                            "backend-rust/bitcoin-processor-app/Dockerfile".into(),
                        ),
                        ("platforms".into(), "linux/amd64".into()),
                        ("push".into(), "false".into()),
                        ("tags".into(), "${{ steps.meta.outputs.tags }}".into()),
                        ("labels".into(), "${{ steps.meta.outputs.labels }}".into()),
                        (
                            "secrets".into(),
                            "github_token=${{ secrets.DOCKER_TOKEN }}".into(),
                        ),
                        (
                            "cache-from".into(),
                            "type=gha,scope=bitcoin-processor-app-pr".into(),
                        ),
                        (
                            "cache-to".into(),
                            "type=gha,scope=bitcoin-processor-app-pr,mode=max".into(),
                        ),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Native {
                step_id: "bake".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::DockerBake,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("files".into(), "backend-rust/docker-bake.hcl".into()),
                        ("targets".into(), "bitcoin-processor-app".into()),
                        (
                            "set".into(),
                            "*.cache-from=type=gha,scope=rust-workspace\n\
*.cache-to=type=gha,scope=rust-workspace,mode=max"
                                .into(),
                        ),
                    ]
                    .into(),
                    env: [
                        (
                            "PUSH".into(),
                            "${{ github.event_name != 'pull_request' && 'true' || 'false' }}"
                                .into(),
                        ),
                        ("SHA".into(), "${{ github.sha }}".into()),
                        (
                            "PR_NUMBER".into(),
                            "${{ github.event.pull_request.number }}".into(),
                        ),
                    ]
                    .into(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];
        let mut executor = DockerJobEngine::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 1],
        });
        let mut spec = container(&temp);
        spec.resource_options = vec!["--cpus".into(), "4".into(), "--memory".into(), "12g".into()];

        let results = executor
            .execute_ordered_steps_with_context(
                &spec,
                &steps,
                &[
                    (
                        "GITHUB_REPOSITORY".into(),
                        "ChainArgos/java-monorepo".into(),
                    ),
                    ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
                    ("GITHUB_EVENT_NAME".into(), "pull_request".into()),
                    ("GITHUB_SHA".into(), "abcdef1234567890".into()),
                    ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
                    ("ACTIONS_CACHE_URL".into(), "https://cache.actions".into()),
                ],
                &[
                    (
                        "secrets".into(),
                        serde_json::json!({ "DOCKER_TOKEN": "docker-token" }),
                    ),
                    (
                        "github".into(),
                        serde_json::json!({ "event": { "pull_request": { "number": 42 } } }),
                    ),
                ],
                &temp,
            )
            .unwrap();

        // 5 main steps + the login-logout and buildx-rm posts (GitHub parity).
        assert_eq!(results.len(), 7);
        assert_eq!(
            results[2].state.outputs["tags"],
            "chainargos/rust-bitcoin-processor:abcdef1234567890"
        );
        assert!(results[2].state.outputs["labels"].contains(
            "org.opencontainers.image.source=https://github.com/ChainArgos/java-monorepo"
        ));
        let runner = executor.runner();
        let calls = docker_call_strings(&runner.calls);
        let builder = format!(
            "velnor-builder-{}",
            sanitize_artifact_name(temp.file_name().unwrap().to_str().unwrap())
        );
        assert!(calls.iter().any(|c| c
            .contains(&format!("'buildx' 'create' '--name' '{builder}'"))
            && c.contains("'--driver-opt' 'cpu-period=100000,cpu-quota=400000,memory=12g'")
            && c.contains(&format!(
                "'--config' '/__t/buildkitd-config-{builder}.toml'"
            ))));
        assert_eq!(
            fs::read_to_string(temp.join(format!("buildkitd-config-{builder}.toml"))).unwrap(),
            "[registry.\"docker.io\"]\n  mirrors = [\"mirror.gcr.io\"]\n"
        );
        let login_call = runner.calls.iter().position(|(program, args)| {
            program == "docker"
                && args.join(" ").contains(
                    "'login' 'https://index.docker.io/v1/' '--username' 'docker-user' '--password-stdin'",
                )
        });
        assert!(login_call.is_some());
        assert_eq!(runner.stdin[login_call.unwrap()], "docker-token");
        let build_call = calls.iter().position(|c| {
            c.contains("'buildx' 'build'")
                && !c.contains("'--load'")
                && !c.contains("'--push'")
                && c.contains("'--tag' 'chainargos/rust-bitcoin-processor:abcdef1234567890'")
        });
        assert!(build_call.is_some());
        // The builder is job-scoped and removed after completion, so external
        // cache import/export must survive to make the next isolated job hot.
        assert!(calls[build_call.unwrap()]
            .contains("'--cache-from' 'type=gha,scope=bitcoin-processor-app-pr'"));
        assert!(calls[build_call.unwrap()]
            .contains("'--cache-to' 'type=gha,scope=bitcoin-processor-app-pr,mode=max'"));
        // Non-secret runtime env remains inline, while credentials use a
        // mode-0600 env file and never occur in the process argument vector.
        let build_invocation = &calls[build_call.unwrap()];
        assert!(!build_invocation.contains("ACTIONS_RUNTIME_TOKEN=runtime-token"));
        assert!(!build_invocation.contains("docker-token"));
        assert!(build_invocation
            .contains("'--secret' 'id=github_token,src=/__t/_velnor/build-secrets/"));
        assert!(build_invocation.contains("--env-file"));
        assert!(build_invocation.contains("ACTIONS_CACHE_URL=https://cache.actions"));
        assert_eq!(
            fs::read_dir(temp.join("_velnor/build-secrets"))
                .unwrap()
                .count(),
            0
        );
        let bake_call = calls
            .iter()
            .position(|c| c.contains("'buildx' 'bake'") && c.contains("'bitcoin-processor-app'"));
        assert!(bake_call.is_some());
        assert!(calls[bake_call.unwrap()]
            .contains("'--set' '*.cache-from=type=gha,scope=rust-workspace'"));
        assert!(calls[bake_call.unwrap()]
            .contains("'--set' '*.cache-to=type=gha,scope=rust-workspace,mode=max'"));
        let bake_invocation = &calls[bake_call.unwrap()];
        assert!(bake_invocation.contains("PUSH=false"));
        assert!(bake_invocation.contains("SHA=abcdef1234567890"));
        assert!(bake_invocation.contains("PR_NUMBER=42"));
        assert_eq!(
            runner
                .calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn docker_login_refused_outside_trusted_scope() {
        let temp = temp_dir();
        let mut executor =
            DockerJobEngine::new(RecordingRunner::default()).with_trust_scope("public-forks");
        let error = executor
            .native_docker_login(
                &container(&temp),
                &NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::DockerLogin,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("username".into(), "docker-user".into()),
                        ("password".into(), "registry-secret".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                &JobExecutionState::default(),
                DEFAULT_STEP_TIMEOUT,
            )
            .unwrap_err();

        assert!(error.to_string().contains("public-forks"));
        assert!(error.to_string().contains("accepted trust scope: trusted"));
        assert!(executor.runner().calls.is_empty());
    }

    #[test]
    fn configure_pages_outputs_match_upstream_url_surface() {
        let outputs = pages_site_outputs("https://octocat.github.io/example/").unwrap();
        assert_eq!(outputs["base_url"], "https://octocat.github.io/example");
        assert_eq!(outputs["origin"], "https://octocat.github.io");
        assert_eq!(outputs["host"], "octocat.github.io");
        assert_eq!(outputs["base_path"], "/example");
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn configure_pages_fetches_site_and_exports_environment() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("GET /repos/octocat/example/pages HTTP/1.1"));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-token"));
            let body = r#"{"html_url":"https://octocat.github.io/example/"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let state = JobExecutionState::new(&[
            ("GITHUB_REPOSITORY".into(), "octocat/example".into()),
            ("GITHUB_API_URL".into(), format!("http://{address}")),
            ("GITHUB_TOKEN".into(), "test-token".into()),
        ]);
        let result = native_configure_pages(
            &NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::ConfigurePages,
                cache_kind: None,
                source_path: None,
                inputs: BTreeMap::new(),
                env: Vec::new(),
            },
            &state,
        )
        .unwrap();
        server.join().unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.state.outputs["base_path"], "/example");
        assert_eq!(result.state.env["GITHUB_PAGES"], "true");
    }

    #[test]
    fn configure_pages_adapter_is_registered() {
        assert_eq!(
            crate::action::native_action_adapter("actions/configure-pages"),
            Some(NativeActionAdapter::ConfigurePages)
        );
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn deploy_pages_runs_artifact_oidc_create_and_status_loop() {
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let responses = [
            r#"{"artifacts":[{"name":"github-pages","workflow_run_backend_id":"plan-1","workflow_job_run_backend_id":"job-1","database_id":"42","size":123,"digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000"}]}"#,
            r#"{"value":"oidc-token"}"#,
            r#"{"id":7,"status_url":"status/7","page_url":"https://initial.example/"}"#,
            r#"{"status":"queued"}"#,
            r#"{"status":"succeed","page_url":"https://deployed.example/"}"#,
        ];
        let server = std::thread::spawn(move || {
            for body in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&request);
                    let header_end = text.find("\r\n\r\n");
                    let content_length = text
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if header_end.is_some_and(|end| request.len() >= end + 4 + content_length) {
                        break;
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&request).to_string());
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        let base = format!("http://{address}");
        let runtime_token = runtime_token_with_results_scope("plan-1", "job-1");
        let state = JobExecutionState::new(&[
            ("GITHUB_REPOSITORY".into(), "octocat/example".into()),
            ("GITHUB_SHA".into(), "abc123".into()),
            ("GITHUB_TOKEN".into(), "github-token".into()),
            ("GITHUB_API_URL".into(), base.clone()),
            ("ACTIONS_RESULTS_URL".into(), base.clone()),
            ("ACTIONS_RUNTIME_TOKEN".into(), runtime_token),
            (
                "ACTIONS_ID_TOKEN_REQUEST_URL".into(),
                format!("{base}/oidc"),
            ),
            (
                "ACTIONS_ID_TOKEN_REQUEST_TOKEN".into(),
                "oidc-request-token".into(),
            ),
        ]);
        let result = native_deploy_pages(
            &NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::DeployPages,
                cache_kind: None,
                source_path: None,
                inputs: [("reporting_interval".into(), "0".into())].into(),
                env: Vec::new(),
            },
            &state,
        )
        .unwrap();
        server.join().unwrap();

        assert_eq!(
            result.state.outputs["page_url"],
            "https://deployed.example/"
        );
        let requests = requests.lock().unwrap();
        assert!(requests[0].starts_with(
            "POST /twirp/github.actions.results.api.v1.ArtifactService/ListArtifacts HTTP/1.1"
        ));
        assert!(requests[0].contains("\"workflow_run_backend_id\":\"plan-1\""));
        assert!(!requests[0].contains("name_filter"));
        assert!(requests[1].starts_with("GET /oidc HTTP/1.1"));
        assert!(requests[2].starts_with("POST /repos/octocat/example/pages/deployments HTTP/1.1"));
        assert!(requests[2].contains("\"artifact_id\":42"));
        assert!(requests[2].contains("\"oidc_token\":\"oidc-token\""));
        assert!(requests[3].starts_with("GET /repos/octocat/example/pages/deployments/7 HTTP/1.1"));
        assert!(requests[4].starts_with("GET /repos/octocat/example/pages/deployments/7 HTTP/1.1"));
    }

    #[test]
    fn native_docker_build_push_honors_load_input_separately() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "build-push".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::DockerBuildPush,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("context".into(), ".".into()),
                    ("push".into(), "false".into()),
                    ("load".into(), "true".into()),
                    ("tags".into(), "example/app:test".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let calls = docker_call_strings(&executor.runner().calls);
        assert!(calls.iter().any(|c| {
            c.contains("'buildx' 'build'") && c.contains("'--load'") && !c.contains("'--push'")
        }));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn build_secret_file_is_private_and_ephemeral() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();

        let secret = BuildSecretFile::create(&temp, "runtime-token").unwrap();
        let host_path = secret.host_path.clone();
        assert_eq!(fs::read_to_string(&host_path).unwrap(), "runtime-token");
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&host_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(secret
            .container_path()
            .starts_with("/__t/_velnor/build-secrets/"));

        drop(secret);
        assert!(!host_path.exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_docker_metadata_matches_target_pr_and_publish_tags() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        let tags_input = "type=raw,value=latest,enable=${{ inputs.publish }}\n\
type=sha,format=long,prefix=,enable=${{ inputs.publish }}\n\
type=raw,value=pr-${{ github.event.pull_request.number }},enable=${{ !inputs.publish && github.event_name == 'pull_request' }}";
        let steps = vec![ExecutableStep::Native {
            step_id: "pr-meta".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::DockerMetadata,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("images".into(), "${{ inputs.image }}".into()),
                    ("tags".into(), tags_input.into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps_with_context(
                &container(&temp),
                &steps,
                &[
                    ("GITHUB_EVENT_NAME".into(), "pull_request".into()),
                    ("GITHUB_SHA".into(), "abcdef1234567890".into()),
                    (
                        "GITHUB_REPOSITORY".into(),
                        "ChainArgos/java-monorepo".into(),
                    ),
                ],
                &[
                    (
                        "inputs".into(),
                        serde_json::json!({
                            "image": "chainargos/rust-bitcoin-processor",
                            "publish": false
                        }),
                    ),
                    (
                        "github".into(),
                        serde_json::json!({ "event": { "pull_request": { "number": 42 } } }),
                    ),
                ],
                &temp,
            )
            .unwrap();

        assert_eq!(
            results[0].state.outputs["tags"],
            "chainargos/rust-bitcoin-processor:pr-42"
        );

        let publish_state = JobExecutionState::new_with_context(
            &[
                ("GITHUB_EVENT_NAME".into(), "push".into()),
                ("GITHUB_SHA".into(), "abcdef1234567890".into()),
                (
                    "GITHUB_REPOSITORY".into(),
                    "ChainArgos/java-monorepo".into(),
                ),
            ],
            &[(
                "inputs".into(),
                serde_json::json!({
                    "image": "chainargos/rust-bitcoin-processor",
                    "publish": true
                }),
            )],
        );
        let publish_action = NativeActionInvocation {
            git_ref: String::new(),
            adapter: NativeActionAdapter::DockerMetadata,
            cache_kind: None,
            source_path: None,
            inputs: [
                ("images".into(), "${{ inputs.image }}".into()),
                ("tags".into(), tags_input.into()),
            ]
            .into(),
            env: Vec::new(),
        };
        let publish_result = native_docker_metadata(&publish_action, &publish_state);
        assert_eq!(
            publish_result.state.outputs["tags"],
            "chainargos/rust-bitcoin-processor:latest\nchainargos/rust-bitcoin-processor:abcdef1234567890"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_docker_metadata_ref_branch_generates_branch_tag() {
        let state = JobExecutionState::new_with_context(
            &[
                ("GITHUB_REF".into(), "refs/heads/main".into()),
                ("GITHUB_SHA".into(), "abcdef1234567890".into()),
                ("GITHUB_REPOSITORY".into(), "org/repo".into()),
            ],
            &[],
        );
        let action = NativeActionInvocation {
            git_ref: String::new(),
            adapter: NativeActionAdapter::DockerMetadata,
            cache_kind: None,
            source_path: None,
            inputs: [
                ("images".into(), "ghcr.io/org/repo/fixture".into()),
                (
                    "tags".into(),
                    "type=ref,event=branch\ntype=sha,prefix=sha-".into(),
                ),
            ]
            .into(),
            env: Vec::new(),
        };
        let result = native_docker_metadata(&action, &state);
        assert_eq!(
            result.state.outputs["tags"],
            "ghcr.io/org/repo/fixture:main\nghcr.io/org/repo/fixture:sha-abcdef1"
        );
    }

    #[test]
    fn native_docker_metadata_ref_branch_skipped_for_tag_ref() {
        let state = JobExecutionState::new_with_context(
            &[
                ("GITHUB_REF".into(), "refs/tags/v1.0.0".into()),
                ("GITHUB_SHA".into(), "abcdef1234567890".into()),
                ("GITHUB_REPOSITORY".into(), "org/repo".into()),
            ],
            &[],
        );
        let action = NativeActionInvocation {
            git_ref: String::new(),
            adapter: NativeActionAdapter::DockerMetadata,
            cache_kind: None,
            source_path: None,
            inputs: [
                ("images".into(), "ghcr.io/org/repo/fixture".into()),
                ("tags".into(), "type=ref,event=branch".into()),
            ]
            .into(),
            env: Vec::new(),
        };
        let result = native_docker_metadata(&action, &state);
        // no branch ref → falls back to sha default
        assert!(result.state.outputs["tags"].starts_with("ghcr.io/org/repo/fixture:sha-"));
    }

    #[test]
    fn native_tool_adapters_use_job_container_without_node_sidecars() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        fs::create_dir_all(temp.join("_velnor")).unwrap();
        fs::write(
            temp.join("_velnor/mise-env.json"),
            r#"{"RUSTUP_TOOLCHAIN":"1.97.1"}"#,
        )
        .unwrap();
        fs::write(temp.join("_velnor/mise-env-redacted.json"), "{}").unwrap();
        let steps = vec![
            ExecutableStep::Native {
                step_id: "mise".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::Mise,
                    cache_kind: None,
                    source_path: None,
                    inputs: [("install".into(), "false".into())].into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Native {
                step_id: "setup-mold".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::SetupMold,
                    cache_kind: None,
                    source_path: None,
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Native {
                step_id: "sccache".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::Sccache,
                    cache_kind: None,
                    source_path: None,
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Native {
                step_id: "setup-just".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::SetupJust,
                    cache_kind: None,
                    source_path: None,
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Native {
                step_id: "rust-cache".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::RustCache,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("shared-key".into(), "kestra-rust-build-cache".into()),
                        ("cache-on-failure".into(), "true".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[
                    (
                        "GITHUB_REPOSITORY".into(),
                        "ChainArgos/java-monorepo".into(),
                    ),
                    ("GITHUB_WORKSPACE".into(), "/__w".into()),
                    ("RUNNER_TEMP".into(), "/__t".into()),
                ],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 7); // 5 main + sccache-post + rust-cache-post
        assert!(results[0].state.path.contains(&"/opt/mise/shims".into()));
        let mise_shims = results[0]
            .state
            .path
            .iter()
            .position(|path| path == "/opt/mise/shims")
            .unwrap();
        let baked_rustup = results[0]
            .state
            .path
            .iter()
            .position(|path| path == "/root/.cargo/bin")
            .unwrap();
        assert!(baked_rustup < mise_shims);
        assert_eq!(results[0].state.env["RUSTUP_TOOLCHAIN"], "1.97.1");
        // just is now a locked mise tool exposed via the mise shims dir.
        assert!(results[3].state.path.contains(&"/opt/mise/shims".into()));
        assert_eq!(results[4].state.outputs["cache-hit"], "false");
        assert_eq!(results[4].state.env["CACHE_ON_FAILURE"], "true");
        let docker_exec_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(program, args)| {
                program == "docker" && args.first().is_some_and(|arg| arg == "exec")
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        // mise, mold, just (main steps) + sccache start + sccache post (show-stats + stop)
        assert_eq!(docker_exec_calls.len(), 5);
        // Plan 008: no runtime mise.run bootstrap, no apt/Cargo installers —
        // mold and just are locked mise installs, one toolchain contract (N8).
        assert!(!docker_exec_calls
            .iter()
            .any(|args| args.iter().any(|arg| arg.contains("https://mise.run"))));
        assert!(!docker_exec_calls
            .iter()
            .any(|args| args.iter().any(|arg| arg.contains("apt-get"))));
        assert!(!docker_exec_calls
            .iter()
            .any(|args| args.iter().any(|arg| arg.contains("cargo install"))));
        assert!(docker_exec_calls.iter().any(|args| args
            .iter()
            .any(|arg| arg.contains("mise install --locked --yes 'mold'"))));
        assert!(docker_exec_calls.iter().any(|args| args
            .iter()
            .any(|arg| arg.contains("mise install --locked --yes 'just'"))));
        assert!(docker_exec_calls
            .iter()
            .any(|args| args.iter().any(|arg| arg.contains("sccache --show-stats"))));
        assert_eq!(
            executor
                .runner()
                .calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_kache_exports_compile_environment_to_later_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "kache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Kache,
                cache_kind: None,
                source_path: None,
                inputs: BTreeMap::new(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());
        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].state.env["RUSTC_WRAPPER"], "kache");
        assert_eq!(results[0].state.env["KACHE_CACHE_DIR"], "/var/cache/kache");
        assert_eq!(results[0].state.env["KACHE_MAX_SIZE"], "20GiB");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn starts_and_waits_for_service_before_job_container() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let mut container = container(&temp);
        container.services.push(ServiceContainerSpec {
            name: "svc".into(),
            image: "postgres:16".into(),
            network_alias: "postgres".into(),
            network: "net".into(),
            env: Vec::new(),
            ports: Vec::new(),
            options: Vec::new(),
        });
        let step = ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
            script: "echo ok".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor.execute_step(&container, &step, &temp).unwrap();

        let calls = &executor.runner().calls;
        assert_eq!(calls[0].1, expected_network_create_args());
        assert_eq!(calls[1].1[0], "run");
        assert!(calls[1].1.windows(2).any(|pair| pair == ["--name", "svc"]));
        assert_eq!(calls[2].1[0], "inspect");
        assert_eq!(calls[3].1[0], "run");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn service_cleanup_is_separate_from_deferred_job_teardown() {
        let temp = temp_dir();
        let mut spec = container(&temp);
        spec.services.push(ServiceContainerSpec {
            name: "svc".into(),
            image: "postgres:16".into(),
            network_alias: "postgres".into(),
            network: "net".into(),
            env: Vec::new(),
            ports: Vec::new(),
            options: Vec::new(),
        });
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor.cleanup_services(&spec).unwrap();

        assert_eq!(executor.runner().calls.len(), 1);
        assert_eq!(executor.runner().calls[0].1, vec!["rm", "--force", "svc"]);
    }

    struct ContainerRemovalRaceRunner;

    impl CommandRunner for ContainerRemovalRaceRunner {
        fn run(&mut self, _program: &str, _args: &[String]) -> Result<CommandResult> {
            Ok(CommandResult {
                code: 1,
                stdout: String::new(),
                stderr:
                    "Error response from daemon: removal of container svc is already in progress"
                        .into(),
            })
        }
    }

    #[test]
    fn service_cleanup_treats_in_progress_removal_as_success() {
        let temp = temp_dir();
        let mut spec = container(&temp);
        spec.services.push(ServiceContainerSpec {
            name: "svc".into(),
            image: "postgres:16".into(),
            network_alias: "postgres".into(),
            network: "net".into(),
            env: Vec::new(),
            ports: Vec::new(),
            options: Vec::new(),
        });

        DockerJobEngine::new(ContainerRemovalRaceRunner)
            .cleanup_services(&spec)
            .unwrap();
    }

    #[derive(Default)]
    struct StaleServiceCleanupRunner {
        calls: Vec<Vec<String>>,
    }

    impl CommandRunner for StaleServiceCleanupRunner {
        fn run(&mut self, _program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push(args.to_vec());
            let running_service_refusal =
                args.len() == 2 && args[0] == "rm" && args[1] == "running-service";
            Ok(CommandResult {
                code: i32::from(running_service_refusal),
                stdout: String::new(),
                stderr: if running_service_refusal {
                    "cannot remove a running container".into()
                } else {
                    String::new()
                },
            })
        }
    }

    #[test]
    fn stale_cleanup_removes_stopped_services_without_force_escalation() {
        let temp = temp_dir();
        let mut spec = container(&temp);
        spec.services.push(ServiceContainerSpec {
            name: "stopped-service".into(),
            image: "postgres:16".into(),
            network_alias: "stopped".into(),
            network: "net".into(),
            env: Vec::new(),
            ports: Vec::new(),
            options: Vec::new(),
        });
        spec.services.push(ServiceContainerSpec {
            name: "running-service".into(),
            image: "redis:7".into(),
            network_alias: "running".into(),
            network: "net".into(),
            env: Vec::new(),
            ports: Vec::new(),
            options: Vec::new(),
        });
        let mut executor = DockerJobEngine::new(StaleServiceCleanupRunner::default());

        executor.cleanup_stale(&spec);

        let calls = &executor.runner().calls;
        assert_eq!(calls[0], vec!["rm", "running-service"]);
        assert_eq!(calls[1], vec!["rm", "stopped-service"]);
        assert!(calls[..2]
            .iter()
            .all(|args| !args.iter().any(|arg| arg == "--force")));
        assert_eq!(
            calls[2],
            crate::docker_lease::list_owned_containers_state_args("job")
        );
        assert_eq!(
            calls[3],
            crate::docker_lease::list_owned_containers_state_args("job")
        );
        assert_eq!(
            calls[4],
            crate::docker_lease::list_owned_networks_args("job")
        );
        assert_eq!(
            calls
                .iter()
                .filter(|args| args.first().is_some_and(|arg| arg == "rm"))
                .count(),
            2
        );
        fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn retries_start_after_removing_stale_job_resources() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
            script: "echo one".into(),
            shell: Shell::Bash,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        });

        let results = executor
            .execute_steps(&container(&temp), &steps, &temp)
            .unwrap();

        assert_eq!(results.len(), 1);
        let calls = &executor.runner().calls;
        assert_eq!(calls[0].1, expected_network_create_args());
        assert_eq!(
            calls[1].1,
            crate::docker_lease::list_owned_containers_state_args("job")
        );
        assert_eq!(
            calls[2].1,
            crate::docker_lease::list_owned_containers_state_args("job")
        );
        assert_eq!(
            calls[3].1,
            crate::docker_lease::list_owned_networks_args("job")
        );
        assert_eq!(
            calls[4].1,
            crate::docker_lease::list_owned_volumes_args("job")
        );
        // Retry cleanup removes the job network before the retry recreates it.
        assert_eq!(
            calls[5].1,
            crate::docker_lease::force_remove_network_args(&["net".to_string()])
        );
        assert_eq!(calls[6].1, expected_network_create_args());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn start_job_double_failure_cleans_up_retry_resources() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let mut executor = DockerJobEngine::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0],
        });

        let error = executor
            .start_job_environment(&container(&temp))
            .unwrap_err();

        assert!(error.to_string().contains("docker run"));
        let calls = &executor.runner().calls;
        assert_eq!(calls[0].1, expected_network_create_args());
        assert_eq!(
            calls[1].1,
            crate::docker_lease::list_owned_containers_state_args("job")
        );
        assert_eq!(
            calls[2].1,
            crate::docker_lease::list_owned_containers_state_args("job")
        );
        assert_eq!(
            calls[3].1,
            crate::docker_lease::list_owned_networks_args("job")
        );
        assert_eq!(
            calls[4].1,
            crate::docker_lease::list_owned_volumes_args("job")
        );
        assert_eq!(
            calls[5].1,
            crate::docker_lease::force_remove_network_args(&["net".to_string()])
        );
        assert_eq!(calls[6].1, expected_network_create_args());
        assert_eq!(calls[7].1[0], "run");
        assert_eq!(
            calls[8].1,
            crate::docker_lease::list_owned_containers_state_args("job")
        );
        assert_eq!(
            calls[9].1,
            crate::docker_lease::list_owned_containers_state_args("job")
        );
        assert_eq!(
            calls[10].1,
            crate::docker_lease::list_owned_networks_args("job")
        );
        assert_eq!(
            calls[11].1,
            crate::docker_lease::list_owned_volumes_args("job")
        );
        assert_eq!(
            calls[12].1,
            crate::docker_lease::force_remove_network_args(&["net".to_string()])
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn job_state_flows_env_and_path_to_later_steps() {
        let mut state = JobExecutionState::default();
        state.apply(
            "producer",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState {
                    outputs: [("answer".to_string(), "42".to_string())].into(),
                    env: [("NAME".to_string(), "value".to_string())].into(),
                    path: vec!["/opt/tool".to_string()],
                    masks: vec!["secret".to_string()],
                    ..Default::default()
                },
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let env = state.step_env(&[("GITHUB_OUTPUT".into(), "/__t/out".into())]);

        assert!(env.contains(&("NAME".into(), "value".into())));
        assert!(env.contains(&("GITHUB_OUTPUT".into(), "/__t/out".into())));
        assert!(!env.iter().any(|(name, _)| name == "PATH"));
        assert_eq!(state.path, vec!["/opt/tool"]);
        assert_eq!(state.masks, vec!["secret"]);
        assert_eq!(
            state.resolve_expressions("value=${{ steps.producer.outputs.answer }}"),
            "value=42"
        );
        assert_eq!(
            state.resolve_expressions("value=${{ steps.producer.outputs['answer'] }}"),
            "value=42"
        );
        assert_eq!(
            state.resolve_expressions("keep=${{ github.ref }}"),
            "keep=${{ github.ref }}"
        );
    }

    #[test]
    fn checkout_uses_prior_step_output_token_instead_of_planning_fallback() {
        let mut state = JobExecutionState::default();
        state.apply(
            "app-token",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState {
                    outputs: [("token".to_string(), "installation-token".to_string())].into(),
                    ..Default::default()
                },
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        let plan = CheckoutPlan {
            step_id: "checkout".into(),
            display_name: "Checkout".into(),
            clone_url: "https://github.com/acme/repo.git".into(),
            version: Some("abc123".into()),
            destination: PathBuf::from("/tmp/work"),
            token: Some("${{ steps.app-token.outputs.token }}".into()),
            fetch_depth: Some(1),
            fetch_tags: false,
            persist_credentials: true,
            clean: true,
            lfs: false,
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };

        let resolved = resolve_checkout_plan_expressions(&plan, &state).unwrap();

        assert_eq!(resolved.token.as_deref(), Some("installation-token"));
    }

    #[test]
    fn runtime_checkout_resolves_ref_from_prior_step_output() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "source".into(),
                display_name: String::new(),
                script: "echo source".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Checkout(CheckoutPlan {
                step_id: "checkout2".into(),
                display_name: String::new(),
                clone_url: "https://github.com/jackin-project/jackin.git".into(),
                version: Some("${{ steps.source.outputs.sha }}".into()),
                destination: temp.join("workspace"),
                token: None,
                fetch_depth: None,
                fetch_tags: false,
                persist_credentials: true,
                clean: true,
                lfs: false,
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(CheckoutOutputRunner::default());

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        let fetch = executor
            .runner()
            .calls
            .iter()
            .find(|(program, args)| {
                program == "git"
                    && args.contains(&"fetch".to_string())
                    && !args.contains(&"+refs/*:refs/*".to_string())
            })
            .unwrap();
        assert!(fetch.1.contains(&"def456".to_string()));
        assert!(!fetch
            .1
            .iter()
            .any(|arg| arg.contains("steps.source.outputs.sha")));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn resolves_step_outputs_in_later_action_env() {
        let mut state = JobExecutionState::default();
        state.apply(
            "meta",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState {
                    outputs: [("tags".to_string(), "image:latest".to_string())].into(),
                    ..Default::default()
                },
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let env =
            state.resolve_env(&[("INPUT_TAGS".into(), "${{ steps.meta.outputs.tags }}".into())]);

        assert_eq!(env, vec![("INPUT_TAGS".into(), "image:latest".into())]);
    }

    #[test]
    fn resolves_github_and_runner_context_expressions() {
        let state = JobExecutionState::new(&[
            ("GITHUB_ACTION".into(), "setup".into()),
            (
                "GITHUB_ACTION_PATH".into(),
                "/__a/actions_setup-node/v4".into(),
            ),
            ("GITHUB_ACTION_REF".into(), "v4".into()),
            (
                "GITHUB_ACTION_REPOSITORY".into(),
                "actions/setup-node".into(),
            ),
            (
                "GITHUB_EVENT_PATH".into(),
                "/github/workflow/event.json".into(),
            ),
            ("GITHUB_REF".into(), "refs/heads/main".into()),
            ("GITHUB_REPOSITORY_OWNER".into(), "acme".into()),
            ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
            ("GITHUB_SHA".into(), "abc123".into()),
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("DOCS_SITE_URL".into(), "https://docs.example".into()),
            ("RUNNER_OS".into(), "Linux".into()),
            ("RUNNER_NAME".into(), "velnor".into()),
            ("RUNNER_ENVIRONMENT".into(), "self-hosted".into()),
            ("RUNNER_WORKSPACE".into(), "/__w".into()),
        ]);

        assert_eq!(
            state.resolve_expressions("${{ github.ref }} ${{ github.sha }}"),
            "refs/heads/main abc123"
        );
        assert_eq!(
            state.resolve_env(&[
                ("INPUT_TOKEN".into(), "${{ github.token }}".into()),
                ("ACTION".into(), "${{ github.action }}".into()),
                ("ACTION_PATH".into(), "${{ github.action_path }}".into()),
                ("ACTION_REF".into(), "${{ github.action_ref }}".into()),
                (
                    "ACTION_REPOSITORY".into(),
                    "${{ github.action_repository }}".into(),
                ),
                ("EVENT_PATH".into(), "${{ github.event_path }}".into()),
                ("OWNER".into(), "${{ github.repository_owner }}".into()),
                ("SERVER_URL".into(), "${{ github.server_url }}".into()),
                ("DOCS_SITE_URL".into(), "${{ env.DOCS_SITE_URL }}".into()),
                ("WORKSPACE".into(), "${{ github.workspace }}".into()),
                ("OS".into(), "${{ runner.os }}".into()),
                ("RUNNER_NAME".into(), "${{ runner.name }}".into()),
                (
                    "RUNNER_ENVIRONMENT".into(),
                    "${{ runner.environment }}".into()
                ),
                ("RUNNER_WORKSPACE".into(), "${{ runner.workspace }}".into()),
                ("ACTION_STATUS".into(), "${{ github.action_status }}".into()),
            ]),
            vec![
                ("INPUT_TOKEN".into(), "ghs_token".into()),
                ("ACTION".into(), "setup".into()),
                ("ACTION_PATH".into(), "/__a/actions_setup-node/v4".into()),
                ("ACTION_REF".into(), "v4".into()),
                ("ACTION_REPOSITORY".into(), "actions/setup-node".into()),
                ("EVENT_PATH".into(), "/github/workflow/event.json".into()),
                ("OWNER".into(), "acme".into()),
                ("SERVER_URL".into(), "https://github.com".into()),
                ("DOCS_SITE_URL".into(), "https://docs.example".into()),
                ("WORKSPACE".into(), "/__w".into()),
                ("OS".into(), "Linux".into()),
                ("RUNNER_NAME".into(), "velnor".into()),
                ("RUNNER_ENVIRONMENT".into(), "self-hosted".into()),
                ("RUNNER_WORKSPACE".into(), "/__w".into()),
                ("ACTION_STATUS".into(), "success".into()),
            ]
        );
    }

    #[test]
    fn resolves_job_context_data_expressions_and_conditions() {
        let state = JobExecutionState::new_with_context(
            &[
                ("GITHUB_EVENT_NAME".into(), "workflow_dispatch".into()),
                ("GITHUB_REF".into(), "refs/heads/main".into()),
            ],
            &[
                (
                    "matrix".into(),
                    serde_json::json!({
                        "target": "x86_64-apple-darwin",
                        "zigbuild": true
                    }),
                ),
                (
                    "needs".into(),
                    serde_json::json!({
                        "changes": {
                            "outputs": {
                                "bitcoin-processor": "false",
                                "bake-targets": "bitcoin-processor-app"
                            },
                            "result": "success"
                        },
                        "test-bitcoin-processor": {
                            "result": "failure"
                        }
                    }),
                ),
                (
                    "inputs".into(),
                    serde_json::json!({ "packages": "bitcoin-processor-app" }),
                ),
                (
                    "secrets".into(),
                    serde_json::json!({ "DOCKERHUB_TOKEN": "docker_secret" }),
                ),
                (
                    "github".into(),
                    serde_json::json!({
                        "repository": "jackin-project/jackin",
                        "event": {
                            "pull_request": { "number": 42 },
                            "workflow_run": {
                                "conclusion": "success",
                                "event": "push",
                                "head_sha": "def456",
                                "head_branch": "main",
                                "head_repository": {
                                    "full_name": "jackin-project/jackin"
                                }
                            }
                        }
                    }),
                ),
            ],
        );

        assert_eq!(
            state.resolve_expressions("target=${{ matrix.target }}"),
            "target=x86_64-apple-darwin"
        );
        assert_eq!(
            state.resolve_expressions("needs=${{ toJSON(needs.changes.outputs) }}"),
            r#"needs={"bake-targets":"bitcoin-processor-app","bitcoin-processor":"false"}"#
        );
        assert_eq!(
            state.resolve_expressions(
                "tool=${{ matrix.zigbuild && 'rust zig cargo:cargo-zigbuild' || 'rust' }}"
            ),
            "tool=rust zig cargo:cargo-zigbuild"
        );
        assert_eq!(
            state.resolve_expressions("literal=${{ 'a || b && c == d' }}"),
            "literal=a || b && c == d"
        );
        assert_eq!(
            state.resolve_expressions(
                "push=${{ (github.event_name == 'push' && needs.changes.outputs.bitcoin-processor == 'true') || (github.event_name == 'workflow_dispatch' && inputs.packages) }}"
            ),
            "push=bitcoin-processor-app"
        );
        assert_eq!(
            state.resolve_expressions(
                "selected=${{ contains(inputs.packages, 'BITCOIN-PROCESSOR-APP') }}"
            ),
            "selected=true"
        );
        assert_eq!(
            state.resolve_expressions("comma=${{ contains('alpha,beta', 'BETA') }}"),
            "comma=true"
        );
        assert_eq!(
            state.resolve_expressions(
                "fallback=${{ steps.dispatch.outputs.docs || needs.changes.outputs.bake-targets }}"
            ),
            "fallback=bitcoin-processor-app"
        );
        assert_eq!(
            state.resolve_expressions(
                "enabled=${{ !inputs.publish && github.event_name == 'workflow_dispatch' }}"
            ),
            "enabled=true"
        );
        assert_eq!(
            state.resolve_expressions("event=${{ github.event_name == 'WORKFLOW_DISPATCH' }}"),
            "event=true"
        );
        assert_eq!(
            state.resolve_expressions("pr=${{ github.event.pull_request.number }}"),
            "pr=42"
        );
        assert_eq!(
            state.resolve_expressions("head=${{ github.event.workflow_run.head_sha }}"),
            "head=def456"
        );
        assert_eq!(
            state.resolve_expressions(
                "same=${{ github.event.workflow_run.head_repository.full_name == github.repository }}"
            ),
            "same=true"
        );
        assert_eq!(
            state.resolve_expressions("token=${{ secrets.DOCKERHUB_TOKEN }}"),
            "token=docker_secret"
        );
        assert_eq!(
            state.resolve_expressions("missing=${{ github.event.issue.number }}"),
            "missing="
        );
        assert_eq!(
            state.resolve_expressions("missing-input=${{ inputs.publish }}"),
            "missing-input="
        );
        assert!(state.evaluate_condition(Some("matrix.zigbuild")));
        assert!(state.evaluate_condition(Some("matrix.target")));
        assert!(!state.evaluate_condition(Some("inputs.publish")));
        assert!(!state.evaluate_condition(Some("secrets.MISSING_TOKEN")));
        assert!(state.evaluate_condition(Some("contains(matrix.target, 'apple')")));
        assert!(state.evaluate_condition(Some("needs.test-bitcoin-processor.result == 'failure'")));
        assert!(state.evaluate_condition(Some(
            "needs.changes.outputs.bitcoin-processor == 'true' || (github.event_name == 'workflow_dispatch' && (inputs.packages == '' || contains(inputs.packages, 'bitcoin-processor-app')))"
        )));
        assert!(
            state.evaluate_condition(Some("contains(inputs.packages, 'BITCOIN-PROCESSOR-APP')"))
        );
        assert!(state.evaluate_condition(Some("needs.changes.outputs.bake-targets != ''")));
        assert!(
            !state.evaluate_condition(Some("(github.event_name == 'workflow_dispatch') == false"))
        );
        assert!(
            state.evaluate_condition(Some("(github.event_name == 'workflow_dispatch') == true"))
        );

        let false_state = JobExecutionState::new_with_context(
            &[],
            &[(
                "matrix".into(),
                serde_json::json!({
                    "zigbuild": false
                }),
            )],
        );
        assert!(!false_state.evaluate_condition(Some("matrix.zigbuild")));
    }

    #[test]
    fn target_workflow_run_preview_gate_matches_jackin_shape() {
        let state = JobExecutionState::new_with_context(
            &[
                ("GITHUB_EVENT_NAME".into(), "workflow_run".into()),
                ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
            ],
            &[(
                "github".into(),
                serde_json::json!({
                    "repository": "jackin-project/jackin",
                    "event": {
                        "workflow_run": {
                            "conclusion": "success",
                            "event": "push",
                            "head_repository": {
                                "full_name": "jackin-project/jackin"
                            },
                            "head_branch": "main",
                            "head_sha": "def456"
                        }
                    }
                }),
            )],
        );

        assert!(state.evaluate_condition(Some(
            "github.event_name == 'workflow_run' && \
             github.event.workflow_run.conclusion == 'success' && \
             github.event.workflow_run.event == 'push' && \
             github.event.workflow_run.head_repository.full_name == github.repository && \
             github.event.workflow_run.head_branch == 'main'"
        )));
        assert_eq!(
            state.resolve_expressions("sha=${{ github.event.workflow_run.head_sha }}"),
            "sha=def456"
        );
    }

    #[test]
    fn target_workflow_expressions_use_supported_subset() {
        let workflow_roots = [
            Path::new("/tmp/velnor-targets/jackin/.github/workflows"),
            Path::new("/tmp/velnor-targets/java-monorepo/.github/workflows"),
        ];
        if workflow_roots.iter().all(|root| !root.exists()) {
            return;
        }

        let workspace = std::env::current_dir().unwrap();
        let state = JobExecutionState::new_with_workspace(
            &[
                ("GITHUB_EVENT_NAME".into(), "workflow_dispatch".into()),
                ("GITHUB_REF".into(), "refs/heads/main".into()),
                ("GITHUB_REF_NAME".into(), "main".into()),
                ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
                ("GITHUB_RUN_ID".into(), "123456".into()),
                ("GITHUB_RUN_NUMBER".into(), "42".into()),
                ("GITHUB_SHA".into(), "abc123".into()),
                ("GITHUB_TOKEN".into(), "ghs_token".into()),
                ("GITHUB_WORKFLOW".into(), "CI".into()),
                ("GITHUB_WORKSPACE".into(), "/__w".into()),
                ("RUNNER_OS".into(), "Linux".into()),
                ("DOCS_SITE_URL".into(), "https://docs.example".into()),
                (
                    "JACKIN_REPO_EDIT_URL".into(),
                    "https://github.com/jackin-project/jackin/edit/main".into(),
                ),
                (
                    "JACKIN_REPO_BLOB_URL".into(),
                    "https://github.com/jackin-project/jackin/blob/main".into(),
                ),
                (
                    "REGISTRY_IMAGE".into(),
                    "docker.io/jackinproject/jackin".into(),
                ),
                ("DIGEST_DIR".into(), "/tmp/digests".into()),
                ("BUILDX_BUILDER".into(), "velnor-builder".into()),
            ],
            &target_expression_context(),
            &workspace,
            &workspace,
        );

        for root in workflow_roots.into_iter().filter(|root| root.exists()) {
            for path in workflow_files(root) {
                let contents = fs::read_to_string(&path).unwrap();
                let yaml = serde_yaml::from_str::<serde_yaml::Value>(&contents).unwrap();
                let mut strings = Vec::new();
                collect_yaml_strings(&yaml, &mut strings);
                for value in strings {
                    if value.contains("${{") {
                        let rendered = state.resolve_expressions(value);
                        assert!(
                            !rendered.contains("${{"),
                            "{} left unresolved expression in {value:?}: {rendered:?}",
                            path.display()
                        );
                    }
                }

                let mut conditions = Vec::new();
                collect_yaml_key_strings(&yaml, "if", &mut conditions);
                for condition in conditions {
                    state.evaluate_condition(Some(condition));
                }
            }
        }
    }

    #[test]
    fn cached_target_action_metadata_expressions_use_supported_subset() {
        let action_roots = [
            Path::new("/tmp/velnor-actions"),
            Path::new("/tmp/velnor-targets/jackin/.github/actions"),
        ];
        if action_roots.iter().all(|root| !root.exists()) {
            return;
        }

        let workspace = std::env::current_dir().unwrap();
        let state = JobExecutionState::new_with_workspace(
            &[
                ("GITHUB_ACTION".into(), "setup".into()),
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__a/_actions/acme_action/v1".into(),
                ),
                ("CACHE_HIT".into(), "false".into()),
                ("CACHE_KEY".into(), "node_modules-cache".into()),
                ("GITHUB_EVENT_NAME".into(), "workflow_dispatch".into()),
                ("GITHUB_REF".into(), "refs/heads/main".into()),
                ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
                ("GITHUB_RUN_ID".into(), "123456".into()),
                ("GITHUB_RUN_NUMBER".into(), "42".into()),
                ("GITHUB_SHA".into(), "abc123".into()),
                ("GITHUB_TOKEN".into(), "ghs_token".into()),
                ("GITHUB_WORKSPACE".into(), "/__w".into()),
                ("RUNNER_OS".into(), "Linux".into()),
                ("RUNNER_TEMP".into(), "/__t".into()),
                ("RUNNER_TOOL_CACHE".into(), "/__tool".into()),
            ],
            &target_expression_context(),
            &workspace,
            &workspace,
        );

        for root in action_roots.into_iter().filter(|root| root.exists()) {
            for path in action_metadata_files(root) {
                let contents = fs::read_to_string(&path).unwrap();
                let yaml = serde_yaml::from_str::<serde_yaml::Value>(&contents).unwrap();
                let mut strings = Vec::new();
                collect_yaml_strings(&yaml, &mut strings);
                for value in strings {
                    if value.contains("${{") {
                        let rendered = state.resolve_expressions(value);
                        assert!(
                            !rendered.contains("${{"),
                            "{} left unresolved expression in {value:?}: {rendered:?}",
                            path.display()
                        );
                    }
                }

                let mut conditions = Vec::new();
                collect_yaml_key_strings(&yaml, "if", &mut conditions);
                for condition in conditions {
                    state.evaluate_condition(Some(condition));
                }
            }
        }
    }

    #[test]
    fn injects_resolved_step_environment_into_script_exec() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "env-step".into(),
            display_name: String::new(),
            script: "echo env".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: vec![
                ("MODE".into(), "release".into()),
                ("TOKEN".into(), "${{ github.token }}".into()),
            ],
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        })];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[("GITHUB_TOKEN".into(), "ghs_token".into())],
                &temp,
            )
            .unwrap();

        let exec_args = &executor.runner().calls[2].1;
        assert!(exec_args.contains(&"MODE=release".into()));
        assert!(exec_args.contains(&"TOKEN=ghs_token".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn initial_environment_expressions_are_resolved() {
        let state = JobExecutionState::new(&[
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            ("GITHUB_SHA".into(), "abc123".into()),
            ("OUTER".into(), "${{ github.token }}".into()),
            ("REVISION".into(), "rev-${{ github.sha }}".into()),
        ]);

        let env = state.step_env(&[]);

        assert!(env.contains(&("OUTER".into(), "ghs_token".into())));
        assert!(env.contains(&("REVISION".into(), "rev-abc123".into())));
    }

    #[test]
    fn target_jackin_release_job_env_resolves_needs_version() {
        let state = JobExecutionState::new_with_context(
            &[(
                "VERSION".into(),
                "${{ needs.check-version.outputs.version }}".into(),
            )],
            &[(
                "needs".into(),
                serde_json::json!({
                    "check-version": {
                        "outputs": {
                            "version": "0.6.0"
                        },
                        "result": "success"
                    }
                }),
            )],
        );

        let env = state.step_env(&[]);

        assert!(env.contains(&("VERSION".into(), "0.6.0".into())));
    }

    #[test]
    fn script_steps_receive_github_action_context() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "build".into(),
            display_name: String::new(),
            script: "echo ${{ github.action }}".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: vec![("ACTION_NAME".into(), "${{ github.action }}".into())],
            condition: Some("${{ github.action == 'build' }}".into()),
            continue_on_error: false,
            timeout_minutes: None,
        })];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let exec_args = &executor.runner().calls[2].1;
        assert!(exec_args.contains(&"GITHUB_ACTION=build".into()));
        assert!(exec_args.contains(&"ACTION_NAME=build".into()));
        let script_path = exec_args
            .iter()
            .position(|arg| arg.ends_with(".sh"))
            .and_then(|index| exec_args.get(index))
            .expect("script path should be present");
        let script = fs::read_to_string(host_temp_script_path(script_path, &temp)).unwrap();
        assert_eq!(script, "echo build\n");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn script_step_environment_is_not_resolved_twice() {
        let mut state = JobExecutionState::new(&[("GITHUB_TOKEN".into(), "ghs_token".into())]);
        state.apply(
            "producer",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState {
                    env: [("OUTER".into(), "${{ github.token }}".into())].into(),
                    ..StepCommandState::default()
                },
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let resolved = state.resolve_env(&[("GENERATED".into(), "${{ env.OUTER }}".into())]);

        assert_eq!(
            resolved,
            vec![("GENERATED".into(), "${{ github.token }}".into())]
        );
    }

    #[test]
    fn writes_github_event_payload_and_injects_event_path() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "event-step".into(),
            display_name: String::new(),
            script: "cat \"$GITHUB_EVENT_PATH\"".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        })];
        let context = vec![(
            "github".to_string(),
            serde_json::json!({
                "event": {
                    "pull_request": { "number": 42 },
                    "workflow_run": { "head_sha": "abc123" }
                }
            }),
        )];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps_with_context(&container(&temp), &steps, &[], &context, &temp)
            .unwrap();

        let exec_args = &executor.runner().calls[2].1;
        assert!(exec_args.contains(&"GITHUB_EVENT_PATH=/github/workflow/event.json".into()));
        assert_eq!(
            fs::read_to_string(temp.join("_github_workflow/event.json")).unwrap(),
            r#"{"pull_request":{"number":42},"workflow_run":{"head_sha":"abc123"}}"#
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn resolves_step_outputs_in_later_script_before_exec() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
                script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
                script: "echo answer=${{ steps.producer.outputs.answer }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(
            fs::read_to_string(temp.join("consumer.sh")).unwrap(),
            "echo answer=42\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_check_image_output_gates_java_monorepo_build_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let condition = "steps.check-image.outputs.exists == 'false'";
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "check-image".into(),
                display_name: String::new(),
                script: r#"if [ "$(just exists ${{ inputs.package }})" = "true" ]; then
  echo "exists=true" >> "$GITHUB_OUTPUT"
else
  echo "exists=false" >> "$GITHUB_OUTPUT"
fi"#
                .into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::JavaScript {
                step_id: "login-docker-hub".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_login-action/v4/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_login-action/v4".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        (
                            "INPUT_USERNAME".into(),
                            "${{ secrets.DOCKERHUB_USERNAME }}".into(),
                        ),
                        (
                            "INPUT_PASSWORD".into(),
                            "${{ secrets.DOCKERHUB_TOKEN }}".into(),
                        ),
                    ],
                },
                condition: Some(condition.into()),
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::JavaScript {
                step_id: "setup-buildx".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path:
                        "/__a/_actions/docker_setup-buildx-action/v4/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_setup-buildx-action/v4".into(),
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: Some(condition.into()),
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "build-docker-image".into(),
                display_name: String::new(),
                script: "just build-${{ inputs.package }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: vec![("GITHUB_TOKEN".into(), "${{ secrets.GITHUB_TOKEN }}".into())],
                condition: Some(condition.into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let context = vec![
            (
                "inputs".into(),
                serde_json::json!({ "package": "bitcoin-processor-app" }),
            ),
            (
                "secrets".into(),
                serde_json::json!({
                    "DOCKERHUB_USERNAME": "docker_user",
                    "DOCKERHUB_TOKEN": "docker_secret",
                    "GITHUB_TOKEN": "ghs_token"
                }),
            ),
        ];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps_with_context(&container(&temp), &steps, &[], &context, &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        assert!(results.iter().all(|result| !result.skipped));
        assert_eq!(results[0].state.outputs["exists"], "false");
        assert_eq!(
            fs::read_to_string(temp.join("build-docker-image.sh")).unwrap(),
            "just build-bitcoin-processor-app\n"
        );
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        assert!(node_calls[0].contains(&"INPUT_USERNAME=docker_user".into()));
        assert!(!node_calls[0].contains(&"INPUT_PASSWORD=docker_secret".into()));
        assert!(node_calls[0].contains(&"--env-file".into()));
        let build_exec = executor
            .runner()
            .calls
            .iter()
            .find(|(_, args)| args.iter().any(|arg| arg == "/__t/build-docker-image.sh"))
            .expect("build script should execute");
        assert!(!build_exec.1.contains(&"GITHUB_TOKEN=ghs_token".into()));
        assert!(build_exec.1.contains(&"--env-file".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn evaluates_job_outputs_from_final_step_state() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "producer".into(),
            display_name: String::new(),
            script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        })];
        let job_outputs = serde_json::json!({
            "answer": "${{ steps.producer.outputs.answer }}",
            "fallback": "${{ steps.missing.outputs.value || 'default' }}",
            "empty": "${{ steps.missing.outputs.value }}"
        });
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let summary = executor
            .execute_ordered_steps_with_job_outputs(
                &container(&temp),
                &steps,
                &[],
                &[],
                Some(&job_outputs),
                &temp,
            )
            .unwrap();

        assert_eq!(summary.step_results.len(), 1);
        assert_eq!(summary.job_outputs["answer"], "42");
        assert_eq!(summary.job_outputs["fallback"], "default");
        assert!(!summary.job_outputs.contains_key("empty"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_docs_environment_url_uses_deployment_step_output() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "deployment".into(),
            display_name: String::new(),
            script: "echo page_url=https://example.com/docs/ >> $GITHUB_OUTPUT".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        })];
        let environment_url = serde_json::json!({
            "type": "String",
            "value": "${{ steps.deployment.outputs.page_url }}"
        });
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let summary = executor
            .execute_ordered_steps_with_completion(
                &container(&temp),
                &steps,
                &[],
                &[],
                None,
                Some(&environment_url),
                &temp,
            )
            .unwrap();

        assert_eq!(
            summary.environment_url.as_deref(),
            Some("https://jackin-project.github.io/jackin/")
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_docs_sitemap_step_receives_deployment_page_url() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "deployment".into(),
                display_name: String::new(),
                script: "echo deploy".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "sitemap".into(),
                display_name: String::new(),
                script: r#"echo "url=${PAGE_URL%/}/sitemap.xml" >> "$GITHUB_OUTPUT""#.into(),
                shell: Shell::Bash,
                working_directory_container: "/__w/repo".into(),
                env: vec![(
                    "PAGE_URL".into(),
                    "${{ steps.deployment.outputs.page_url }}".into(),
                )],
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let sitemap_exec = executor
            .runner()
            .calls
            .iter()
            .find(|(_program, args)| args.iter().any(|arg| arg.ends_with("/sitemap.sh")))
            .expect("sitemap script should be executed");
        assert!(sitemap_exec
            .1
            .contains(&"PAGE_URL=https://jackin-project.github.io/jackin/".into()));
        assert_eq!(
            fs::read_to_string(temp.join("sitemap.sh")).unwrap(),
            "echo \"url=${PAGE_URL%/}/sitemap.xml\" >> \"$GITHUB_OUTPUT\"\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_check_deployed_docs_runtime_inputs_resolve_after_sitemap_step() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "deployment".into(),
                display_name: String::new(),
                script: "echo deploy".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "sitemap".into(),
                display_name: String::new(),
                script: r#"echo "url=${PAGE_URL%/}/sitemap.xml" >> "$GITHUB_OUTPUT""#.into(),
                shell: Shell::Bash,
                working_directory_container: "/__w/repo".into(),
                env: vec![(
                    "PAGE_URL".into(),
                    "${{ steps.deployment.outputs.page_url }}".into(),
                )],
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "check-deployed-1".into(),
                display_name: String::new(),
                script: r#"lychee --dump "${{ inputs.sitemap-url }}""#.into(),
                shell: Shell::Bash,
                working_directory_container: "/__w/repo".into(),
                env: vec![
                    (
                        "SITEMAP_URL".into(),
                        "${{ steps.sitemap.outputs.url }}".into(),
                    ),
                    ("EDIT_URL".into(), "${{ env.JACKIN_REPO_EDIT_URL }}".into()),
                    ("BLOB_URL".into(), "${{ env.JACKIN_REPO_BLOB_URL }}".into()),
                ],
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[
                    (
                        "JACKIN_REPO_EDIT_URL".into(),
                        "https://github.com/jackin-project/jackin/edit/main".into(),
                    ),
                    (
                        "JACKIN_REPO_BLOB_URL".into(),
                        "https://github.com/jackin-project/jackin/blob/main".into(),
                    ),
                ],
                &temp,
            )
            .unwrap();

        let check_exec = executor
            .runner()
            .calls
            .iter()
            .find(|(_program, args)| args.iter().any(|arg| arg.ends_with("/check-deployed-1.sh")))
            .expect("check-deployed script should be executed");
        assert!(check_exec
            .1
            .contains(&("SITEMAP_URL=https://jackin-project.github.io/jackin/sitemap.xml".into())));
        assert!(check_exec
            .1
            .contains(&("EDIT_URL=https://github.com/jackin-project/jackin/edit/main".into())));
        assert!(check_exec
            .1
            .contains(&("BLOB_URL=https://github.com/jackin-project/jackin/blob/main".into())));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn evaluates_typed_job_outputs_from_final_step_state() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "producer".into(),
            display_name: String::new(),
            script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        })];
        let job_outputs = serde_json::json!({
            "type": "map",
            "map": [
                {
                    "Key": { "lit": "answer" },
                    "Value": { "lit": "${{ steps.producer.outputs.answer }}" }
                },
                {
                    "Key": { "lit": "fallback" },
                    "Value": { "value": "${{ steps.missing.outputs.value || 'default' }}" }
                },
                {
                    "Key": { "lit": "empty" },
                    "Value": { "lit": "${{ steps.missing.outputs.value }}" }
                }
            ]
        });
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let summary = executor
            .execute_ordered_steps_with_job_outputs(
                &container(&temp),
                &steps,
                &[],
                &[],
                Some(&job_outputs),
                &temp,
            )
            .unwrap();

        assert_eq!(summary.step_results.len(), 1);
        assert_eq!(summary.job_outputs["answer"], "42");
        assert_eq!(summary.job_outputs["fallback"], "default");
        assert!(!summary.job_outputs.contains_key("empty"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_rust_docker_job_outputs_resolve_after_filter_and_targets_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::CompositeOutputs {
                step_id: "filter".into(),
                outputs: BTreeMap::from([("bitcoin-processor".into(), "false".into())]),
                condition: None,
            },
            ExecutableStep::CompositeOutputs {
                step_id: "targets".into(),
                outputs: BTreeMap::from([("list".into(), "bitcoin-processor-app".into())]),
                condition: None,
            },
        ];
        let job_outputs = serde_json::json!({
            "bitcoin-processor": "${{ github.event_name == 'workflow_dispatch' && 'true' || steps.filter.outputs.bitcoin-processor }}",
            "bake-targets": "${{ steps.targets.outputs.list }}"
        });
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let summary = executor
            .execute_ordered_steps_with_job_outputs(
                &container(&temp),
                &steps,
                &[("GITHUB_EVENT_NAME".into(), "workflow_dispatch".into())],
                &[],
                Some(&job_outputs),
                &temp,
            )
            .unwrap();

        assert_eq!(summary.job_outputs["bitcoin-processor"], "true");
        assert_eq!(summary.job_outputs["bake-targets"], "bitcoin-processor-app");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_jackin_dispatch_or_filter_job_output_uses_runtime_fallback() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::CompositeOutputs {
                step_id: "dispatch".into(),
                outputs: BTreeMap::new(),
                condition: None,
            },
            ExecutableStep::CompositeOutputs {
                step_id: "filter".into(),
                outputs: BTreeMap::from([("docs".into(), "true".into())]),
                condition: None,
            },
        ];
        let job_outputs = serde_json::json!({
            "docs": "${{ steps.dispatch.outputs.docs || steps.filter.outputs.docs }}"
        });
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let summary = executor
            .execute_ordered_steps_with_job_outputs(
                &container(&temp),
                &steps,
                &[],
                &[],
                Some(&job_outputs),
                &temp,
            )
            .unwrap();

        assert_eq!(summary.job_outputs["docs"], "true");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_jackin_release_job_outputs_collect_platform_shas() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::CompositeOutputs {
            step_id: "shas".into(),
            outputs: BTreeMap::from([
                ("arm64_linux".into(), "sha-arm64-linux".into()),
                ("arm64_macos".into(), "sha-arm64-macos".into()),
                ("capsule_arm64_linux".into(), "sha-capsule-arm64".into()),
                ("capsule_x86_linux".into(), "sha-capsule-x86".into()),
                ("x86_linux".into(), "sha-x86-linux".into()),
                ("x86_macos".into(), "sha-x86-macos".into()),
            ]),
            condition: None,
        }];
        let job_outputs = serde_json::json!({
            "arm64_linux": "${{ steps.shas.outputs.arm64_linux }}",
            "arm64_macos": "${{ steps.shas.outputs.arm64_macos }}",
            "capsule_arm64_linux": "${{ steps.shas.outputs.capsule_arm64_linux }}",
            "capsule_x86_linux": "${{ steps.shas.outputs.capsule_x86_linux }}",
            "x86_linux": "${{ steps.shas.outputs.x86_linux }}",
            "x86_macos": "${{ steps.shas.outputs.x86_macos }}"
        });
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let summary = executor
            .execute_ordered_steps_with_job_outputs(
                &container(&temp),
                &steps,
                &[],
                &[],
                Some(&job_outputs),
                &temp,
            )
            .unwrap();

        assert_eq!(summary.job_outputs["arm64_linux"], "sha-arm64-linux");
        assert_eq!(summary.job_outputs["arm64_macos"], "sha-arm64-macos");
        assert_eq!(
            summary.job_outputs["capsule_arm64_linux"],
            "sha-capsule-arm64"
        );
        assert_eq!(summary.job_outputs["capsule_x86_linux"], "sha-capsule-x86");
        assert_eq!(summary.job_outputs["x86_linux"], "sha-x86-linux");
        assert_eq!(summary.job_outputs["x86_macos"], "sha-x86-macos");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn materializes_composite_outputs_as_outer_step_outputs() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
                script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::CompositeOutputs {
                step_id: "composite".into(),
                outputs: [(
                    "artifact-id".to_string(),
                    "${{ steps.producer.outputs.answer }}".to_string(),
                )]
                .into(),
                condition: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
                script: "echo artifact=${{ steps.composite.outputs.artifact-id }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[1].state.outputs["artifact-id"], "42");
        assert_eq!(
            fs::read_to_string(temp.join("consumer.sh")).unwrap(),
            "echo artifact=42\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn composite_status_ignores_prior_job_failure() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "prior-failure".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::CompositeStart {
                step_id: "composite".into(),
                display_name: "Run local composite".into(),
                inputs: BTreeMap::new(),
                env: Vec::new(),
                condition: Some("always()".into()),
            },
            ExecutableStep::Script(ScriptStep {
                id: "composite-first".into(),
                display_name: String::new(),
                script: "echo first".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "composite-success".into(),
                display_name: String::new(),
                script: "echo success".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("success()".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "composite-failure".into(),
                display_name: String::new(),
                script: "echo failure".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("failure()".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::CompositeEnd {
                step_id: "composite".into(),
            },
        ];
        let mut executor = DockerJobEngine::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 1, 0, 0],
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        assert_eq!(results[0].exit_code, 1);
        assert_eq!(results[1].exit_code, 0);
        assert_eq!(results[2].exit_code, 0);
        assert!(results[3].skipped);
        let exec_scripts = executor
            .runner()
            .calls
            .iter()
            .filter_map(|(_, args)| {
                (args.first().is_some_and(|arg| arg == "exec"))
                    .then(|| args.last().cloned())
                    .flatten()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            exec_scripts,
            vec![
                "/__t/prior-failure.sh",
                "/__t/composite-first.sh",
                "/__t/composite-success.sh",
            ]
        );
        assert!(!temp.join("composite-failure.sh").exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn emits_step_started_events_for_real_executable_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
                script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::CompositeOutputs {
                step_id: "composite".into(),
                outputs: [(
                    "artifact-id".to_string(),
                    "${{ steps.producer.outputs.answer }}".to_string(),
                )]
                .into(),
                condition: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
                script: "echo artifact=${{ steps.composite.outputs.artifact-id }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        })
        .with_step_start_sender(sender);

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }

        assert_eq!(events.len(), 2);
        assert_uuid(&events[0].step_id);
        assert_eq!(events[0].display_name, "");
        assert_eq!(events[0].order, 1);
        assert_uuid(&events[1].step_id);
        assert_eq!(events[1].display_name, "");
        assert_eq!(events[1].order, 2);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn emits_step_logs_with_timeline_order_after_each_step() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
                script: "node old-action.js".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
                script: "echo answer=${{ steps.producer.outputs.answer }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut executor =
            DockerJobEngine::new(StdoutCommandRunner::default()).with_step_log_sender(sender);

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();
        let mut logs = Vec::new();
        while let Ok(log) = receiver.try_recv() {
            logs.push(log);
        }

        assert_eq!(logs.len(), 2);
        assert_uuid(&logs[0].step_id);
        assert_eq!(logs[0].order, 1);
        assert_uuid(&logs[1].step_id);
        assert_eq!(logs[1].order, 2);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn live_stream_carries_add_mask_values_after_registration() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "masker".into(),
            display_name: String::new(),
            script: "echo mask".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        })];
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut executor =
            DockerJobEngine::new(StreamingMaskRunner::default()).with_step_log_sender(sender);

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();
        let mut logs = Vec::new();
        while let Ok(log) = receiver.try_recv() {
            logs.push(log);
        }
        let live_logs: Vec<_> = logs
            .iter()
            .filter(|log| log.completed_at.is_empty() && !log.lines.is_empty())
            .collect();

        assert_eq!(live_logs.len(), 2);
        assert_eq!(live_logs[1].lines, vec!["echo dynsecret".to_string()]);
        assert_eq!(live_logs[1].masks, vec!["dynsecret".to_string()]);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn reports_silent_and_skipped_steps_for_completion() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "silent".into(),
                display_name: String::new(),
                script: "true".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "skipped".into(),
                display_name: String::new(),
                script: "echo unreachable".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("${{ false }}".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(SilentCommandRunner::default());

        let summary = executor
            .execute_ordered_steps_with_job_outputs(
                &container(&temp),
                &steps,
                &[],
                &[],
                None,
                &temp,
            )
            .unwrap();

        assert_eq!(summary.step_results.len(), 2);
        assert_eq!(summary.step_logs.len(), 2);
        assert_uuid(&summary.step_logs[0].step_id);
        assert_eq!(summary.step_logs[0].order, 1);
        assert!(
            !summary.step_logs[0].lines.is_empty(),
            "executed steps need a log blob even when the command is silent"
        );
        assert!(!summary.step_logs[0].skipped);
        assert_uuid(&summary.step_logs[1].step_id);
        assert_eq!(summary.step_logs[1].order, 2);
        assert!(
            summary.step_logs[1]
                .lines
                .iter()
                .any(|line| line.contains("Step skipped:")),
            "skipped steps need a visible marker line, got {:?}",
            summary.step_logs[1].lines
        );
        assert!(summary.step_logs[1].skipped);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn workflow_commands_from_stdout_update_later_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
                script: "node old-action.js".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
                script: "echo answer=${{ steps.producer.outputs.answer }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(StdoutCommandRunner::default());

        let summary = executor
            .execute_ordered_steps_with_job_outputs(
                &container(&temp),
                &steps,
                &[],
                &[],
                None,
                &temp,
            )
            .unwrap();

        let results = &summary.step_results;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].state.outputs["answer"], "42");
        assert_eq!(results[0].state.path, vec!["/opt/tool"]);
        assert_eq!(results[0].state.error_count, 1);
        assert_eq!(results[0].state.warning_count, 0);
        assert_eq!(results[0].state.notice_count, 0);
        assert_uuid(&summary.step_logs[0].step_id);
        assert!(summary.step_logs[0].lines.contains(&"hidden".to_string()));
        assert_eq!(summary.step_logs[0].masks, vec!["hidden"]);
        assert_eq!(summary.step_logs[0].error_count, 1);
        assert_eq!(summary.step_logs[0].exit_code, 0);
        assert!(!summary.step_logs[0].skipped);
        assert_eq!(
            fs::read_to_string(temp.join("consumer.sh")).unwrap(),
            "export PATH='/opt/tool':\"$PATH\"\necho answer=42\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn workflow_commands_from_stderr_update_later_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
                script: "node old-action.js".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
                script: "echo answer=${{ steps.producer.outputs.answer }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(StderrCommandRunner::default());

        let summary = executor
            .execute_ordered_steps_with_job_outputs(
                &container(&temp),
                &steps,
                &[],
                &[],
                None,
                &temp,
            )
            .unwrap();

        let results = &summary.step_results;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].state.outputs["answer"], "42");
        assert_eq!(results[0].state.path, vec!["/opt/tool"]);
        assert_eq!(results[0].state.warning_count, 1);
        assert_eq!(summary.step_logs[0].masks, vec!["hidden"]);
        assert_eq!(summary.step_logs[0].warning_count, 1);
        assert_eq!(
            fs::read_to_string(temp.join("consumer.sh")).unwrap(),
            "export PATH='/opt/tool':\"$PATH\"\necho answer=42\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn step_log_includes_step_summary_file_content() {
        let result = StepExecutionResult {
            exit_code: 0,
            state: StepCommandState {
                summary: "### cache stats\nhit rate: 80%\n".into(),
                ..StepCommandState::default()
            },
            skipped: false,
            failure_ignored: false,
            stdout: "done\n".into(),
            stderr: String::new(),
        };

        let log = step_log("sccache", 1, &result);

        let joined = log.lines.join("\n");
        assert!(joined.contains("##[group]step"));
        assert!(joined.contains("done"));
        // The summary renders in the run Summary tab via its own upload —
        // never inlined into the step log (GitHub parity).
        assert!(!joined.contains("### cache stats"));
        assert_eq!(log.summary, "### cache stats\nhit rate: 80%\n");
    }

    #[test]
    fn resolves_hash_files_in_action_env() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(workspace.join("kestra-docker-containers/app/src")).unwrap();
        fs::create_dir_all(workspace.join("kestra-docker-containers/app")).unwrap();
        fs::write(
            workspace.join("kestra-docker-containers/app/src/main.rs"),
            "fn main() {}\n",
        )
        .unwrap();
        fs::write(
            workspace.join("kestra-docker-containers/app/build.toml"),
            "image = 'app'\n",
        )
        .unwrap();
        fs::write(
            workspace.join("kestra-docker-containers/justfile"),
            "build:\n",
        )
        .unwrap();
        fs::write(workspace.join("ignored.txt"), "ignored\n").unwrap();
        let mut expected_hash = Sha256::new();
        // All patterns share the same ancestor search root, so the official
        // globber traverses that root lexically with the combined matcher.
        expected_hash.update(Sha256::digest(b"image = 'app'\n"));
        expected_hash.update(Sha256::digest(b"fn main() {}\n"));
        expected_hash.update(Sha256::digest(b"build:\n"));
        let digest = expected_hash.finalize();
        let expected = hex_digest(&digest);
        let steps = vec![ExecutableStep::JavaScript {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: JavaScriptActionInvocation {
                node: "node24".into(),
                pre_container_path: None,
                pre_condition: None,
                main_container_path: "/__a/_actions/actions_cache/dist/restore/index.js".into(),
                post_container_path: None,
                post_condition: None,
                action_container_path: "/__a/_actions/actions_cache".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                    ("INPUT_PATH".into(), "~/.cache/rust-script".into()),
                    (
                        "INPUT_KEY".into(),
                        "rust-script-${{ runner.os }}-${{ hashFiles('kestra-docker-containers/**/*.rs', 'kestra-docker-containers/**/build.toml', 'kestra-docker-containers/justfile') }}".into(),
                    ),
                    (
                        "INPUT_RESTORE-KEYS".into(),
                        "rust-script-${{ runner.os }}-\n".into(),
                    ),
                ],
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[("RUNNER_OS".into(), "Linux".into())],
                &temp,
            )
            .unwrap();

        assert!(!expected.is_empty());
        let cache_args = &executor.runner().calls[2].1;
        assert!(cache_args.contains(&"INPUT_PATH=~/.cache/rust-script".into()));
        assert!(cache_args.contains(&format!("INPUT_KEY=rust-script-Linux-{expected}")));
        assert!(cache_args.contains(&"INPUT_RESTORE-KEYS=rust-script-Linux-\n".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn hash_files_digest_is_stable_for_sorted_streamed_files() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(workspace.join("nested")).unwrap();
        fs::write(workspace.join("alpha.txt"), "alpha\n").unwrap();
        fs::write(workspace.join("nested/beta.txt"), "beta\n").unwrap();
        fs::write(
            workspace.join("nested/gamma.bin"),
            vec![b'x'; 128 * 1024 + 17],
        )
        .unwrap();

        let digest = hash_files(
            &workspace,
            &["**/*.txt".to_string(), "nested/gamma.bin".to_string()],
        );

        assert_eq!(
            digest,
            "04ffe0aff7d100a890dfec2e0c951e89fa45e2afb6b9f96bafabf15f15ba1c35"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn hash_files_preserves_official_pattern_search_order() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("alpha.txt"), "alpha\n").unwrap();
        fs::write(workspace.join("zeta.txt"), "zeta\n").unwrap();

        let actual = hash_files(
            &workspace,
            &["zeta.txt".to_string(), "alpha.txt".to_string()],
        );
        let mut expected = Sha256::new();
        expected.update(sha256_file_digest(&workspace.join("zeta.txt")).unwrap());
        expected.update(sha256_file_digest(&workspace.join("alpha.txt")).unwrap());

        assert_eq!(actual, hex_digest(&expected.finalize()));
        assert_ne!(
            actual,
            hash_files(
                &workspace,
                &["alpha.txt".to_string(), "zeta.txt".to_string()]
            )
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn hash_files_star_does_not_cross_path_separators() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(workspace.join("crates/direct")).unwrap();
        fs::create_dir_all(workspace.join("crates/direct/fuzz")).unwrap();
        fs::write(workspace.join("crates/direct/Cargo.toml"), "direct\n").unwrap();
        fs::write(workspace.join("crates/direct/fuzz/Cargo.toml"), "nested\n").unwrap();

        let actual = hash_files(&workspace, &["crates/*/Cargo.toml".to_string()]);
        let mut expected = Sha256::new();
        expected.update(sha256_file_digest(&workspace.join("crates/direct/Cargo.toml")).unwrap());

        assert_eq!(actual, hex_digest(&expected.finalize()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn hash_files_resolution_observes_files_created_after_an_empty_evaluation() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(&workspace).unwrap();
        let state = JobExecutionState::new_internal(&[], &[], Some(workspace.clone()), None);

        let expression = "hash=${{ hashFiles('Cargo.toml') }}";
        let first = state.resolve_expressions(expression);
        fs::write(workspace.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        let second = state.resolve_expressions(expression);

        assert_eq!(first, "hash=");
        assert_ne!(second, "hash=");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn skips_steps_when_output_condition_is_false() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
                script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
                script: "echo should-not-run".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("steps.producer.outputs.answer == 'nope'".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[1].skipped);
        assert!(!temp.join("consumer.sh").exists());
        let exec_count = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| args.first().is_some_and(|arg| arg == "exec"))
            .count();
        assert_eq!(exec_count, 1);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_required_job_cancelled_need_condition_fails_and_skips_ok_step() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "check-failed".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some(
                    "needs.check.result == 'failure' || needs.check.result == 'cancelled'".into(),
                ),
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "bitcoin-cancelled".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some(
                    "needs.test-bitcoin-processor.result == 'failure' || \
                     needs.test-bitcoin-processor.result == 'cancelled'"
                        .into(),
                ),
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "ok".into(),
                display_name: String::new(),
                script: "echo OK".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let context = vec![(
            "needs".into(),
            serde_json::json!({
                "check": { "result": "success" },
                "test-bitcoin-processor": { "result": "cancelled" }
            }),
        )];
        let mut executor = DockerJobEngine::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 1],
        });

        let results = executor
            .execute_ordered_steps_with_context(&container(&temp), &steps, &[], &context, &temp)
            .unwrap();

        assert_eq!(results.len(), 3);
        assert!(results[0].skipped);
        assert_eq!(results[1].exit_code, 1);
        assert!(!results[1].failure_ignored);
        assert!(results[2].skipped);
        let exec_scripts = executor
            .runner()
            .calls
            .iter()
            .filter_map(|(_, args)| {
                (args.first().is_some_and(|arg| arg == "exec"))
                    .then(|| args.last().cloned())
                    .flatten()
            })
            .collect::<Vec<_>>();
        assert_eq!(exec_scripts, vec!["/__t/bitcoin-cancelled.sh"]);
        assert!(!temp.join("check-failed.sh").exists());
        assert!(!temp.join("ok.sh").exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn continue_on_error_keeps_failure_outcome_but_runs_later_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "sccache".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/sccache/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/sccache".into(),
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: true,
                timeout_minutes: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "enable-sccache".into(),
                display_name: String::new(),
                script: "echo SCCACHE_GHA_ENABLED=true >> $GITHUB_ENV".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("steps.sccache.outcome == 'success'".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "next".into(),
                display_name: String::new(),
                script: "echo next".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("success()".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 7, 0, 0],
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].exit_code, 7);
        assert!(results[0].failure_ignored);
        assert!(results[1].skipped);
        assert_eq!(results[2].exit_code, 0);
        let exec_count = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "exec")
                    || (args.first().is_some_and(|arg| arg == "run")
                        && args.contains(&"node:20-bookworm".into()))
            })
            .count();
        assert_eq!(exec_count, 2);
        fs::remove_dir_all(temp).unwrap();
    }

    // actions/runner parity (RunStepAsync catch + ExecutionContext.ApplyContinueOnError):
    // a step that fails by THROWING (not exit code) with continue-on-error: true
    // keeps outcome=failure for `steps.<id>.outcome`, reports conclusion=success,
    // and the job runs the remaining steps and finishes green.
    // Regression: jackin-project/jackin Hygiene run 33319391723 ("miri jackin-config").
    #[test]
    fn script_step_execution_error_honors_continue_on_error() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "miri".into(),
                display_name: "miri crate".into(),
                script: "cargo +nightly miri test".into(),
                shell: Shell::Bash,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: true,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "summary".into(),
                display_name: "summary".into(),
                script: "echo summary".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("success()".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "outcome-probe".into(),
                display_name: "probe".into(),
                script: "echo probe".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("steps.miri.outcome == 'failure'".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(ErroringExecRunner {
            calls: Vec::new(),
            fail_execs: 1,
        });

        let summary = executor
            .execute_ordered_steps_with_completion(
                &container(&temp),
                &steps,
                &[],
                &[],
                None,
                None,
                &temp,
            )
            .expect("continue-on-error step error must not fail the job");

        assert_eq!(summary.step_results.len(), 3);
        // outcome stays failure internally...
        assert_eq!(summary.step_results[0].exit_code, 1);
        assert!(!summary.step_results[0].skipped);
        // ...while the reported conclusion is success (failure_ignored).
        assert!(summary.step_results[0].failure_ignored);
        // The job continues: success() is true (conclusion-based)...
        assert_eq!(summary.step_results[1].exit_code, 0);
        assert!(!summary.step_results[1].skipped);
        // ...and steps.miri.outcome still reports failure.
        assert_eq!(summary.step_results[2].exit_code, 0);
        assert!(!summary.step_results[2].skipped);
        // The step record carries the error text but maps to success.
        let log = &summary.step_logs[0];
        assert_eq!(log.display_name, "miri crate");
        assert!(log.failure_ignored);
        assert!(log
            .lines
            .iter()
            .any(|line| line.contains("docker daemon connection lost")));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn script_step_execution_error_without_continue_on_error_fails_job() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "miri".into(),
            display_name: "miri crate".into(),
            script: "cargo +nightly miri test".into(),
            shell: Shell::Bash,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        })];
        let mut executor = DockerJobEngine::new(ErroringExecRunner {
            calls: Vec::new(),
            fail_execs: 1,
        });

        let error = executor
            .execute_ordered_steps_with_completion(
                &container(&temp),
                &steps,
                &[],
                &[],
                None,
                None,
                &temp,
            )
            .expect_err("step execution error without continue-on-error must fail the job");
        assert!(format!("{error:#}").contains("docker daemon connection lost"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn failed_checkout_runs_always_step() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Checkout(CheckoutPlan {
                step_id: "checkout".into(),
                display_name: "Checkout".into(),
                clone_url: "https://github.com/acme/missing.git".into(),
                version: Some("missing".into()),
                destination: temp.join("workspace"),
                token: None,
                fetch_depth: Some(1),
                fetch_tags: false,
                persist_credentials: false,
                clean: true,
                lfs: false,
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "always".into(),
                display_name: "Always".into(),
                script: "echo always".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w".into(),
                env: Vec::new(),
                condition: Some("always()".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(FailingCheckoutRunner::default());

        let summary = executor
            .execute_ordered_steps_with_completion(
                &container(&temp),
                &steps,
                &[],
                &[],
                None,
                None,
                &temp,
            )
            .unwrap();

        assert_eq!(summary.step_results.len(), 2);
        assert_eq!(summary.step_results[0].exit_code, 128);
        assert!(!summary.step_results[0].failure_ignored);
        assert_eq!(summary.step_results[1].exit_code, 0);
        assert_eq!(summary.step_logs.len(), 2);
        assert_eq!(summary.step_logs[0].display_name, "Checkout");
        assert_eq!(summary.step_logs[0].exit_code, 128);
        assert!(summary.step_logs[0]
            .lines
            .iter()
            .any(|line| line.contains("couldn't find remote ref")));
        assert_eq!(summary.step_logs[1].display_name, "Always");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn failed_native_action_honors_continue_on_error() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Native {
                step_id: "upload".into(),
                display_name: "Upload".into(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::UploadArtifact,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("path".into(), "missing-file".into()),
                        ("if-no-files-found".into(), "error".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: true,
                timeout_minutes: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "next".into(),
                display_name: "Next".into(),
                script: "echo next".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let summary = executor
            .execute_ordered_steps_with_completion(
                &container(&temp),
                &steps,
                &[],
                &[],
                None,
                None,
                &temp,
            )
            .unwrap();

        assert_eq!(summary.step_results.len(), 2);
        assert_eq!(summary.step_results[0].exit_code, 1);
        assert!(summary.step_results[0].failure_ignored);
        assert_eq!(summary.step_results[1].exit_code, 0);
        assert_eq!(summary.step_logs.len(), 2);
        assert_eq!(summary.step_logs[0].display_name, "Upload");
        assert_eq!(summary.step_logs[0].exit_code, 1);
        assert!(summary.step_logs[0]
            .lines
            .iter()
            .any(|line| line.contains("No files were found")));
        assert_eq!(summary.step_logs[1].display_name, "Next");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn conditions_without_status_check_require_success() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "fail".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "plain-condition".into(),
                display_name: String::new(),
                script: "echo plain".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("steps.fail.outcome == 'failure'".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "status-condition".into(),
                display_name: String::new(),
                script: "echo status".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("failure() && steps.fail.outcome == 'failure'".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 1, 0, 0, 0],
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 3);
        assert!(results[1].skipped);
        assert_eq!(results[2].exit_code, 0);
        let exec_count = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| args.first().is_some_and(|arg| arg == "exec"))
            .count();
        assert_eq!(exec_count, 2);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn evaluates_step_outcome_conditions() {
        let mut state = JobExecutionState::default();
        state.apply(
            "sccache",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        state.apply(
            "disabled",
            &StepExecutionResult {
                exit_code: 0,
                skipped: true,
                failure_ignored: false,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        assert!(state.evaluate_condition(Some("steps.sccache.outcome == 'success'")));
        assert!(state.evaluate_condition(Some("steps.sccache.conclusion == 'success'")));
        assert!(state.evaluate_condition(Some("steps.disabled.outcome != 'success'")));
        assert!(state.evaluate_condition(Some("steps.disabled.conclusion == 'skipped'")));
        assert!(state.evaluate_condition(Some("runner.os == 'Linux'")));
        assert!(state.evaluate_condition(Some("runner.os == 'linux'")));
        assert!(state.evaluate_condition(Some("runner.os != 'windows'")));
        assert_eq!(state.resolve_expressions("${{ job.status }}"), "success");
        assert_eq!(
            state.resolve_expressions("${{ github.action_status }}"),
            "success"
        );
        assert!(state.evaluate_condition(Some("job.status == 'success'")));
        assert!(state.evaluate_condition(Some("github.action_status == 'success'")));
        assert!(state.evaluate_condition(Some("success()")));
        assert!(!state.evaluate_condition(Some("failure()")));
        assert!(!state.evaluate_condition(Some("cancelled()")));
        assert!(state.evaluate_condition(Some("!cancelled()")));

        state.apply(
            "failed",
            &StepExecutionResult {
                exit_code: 1,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        assert!(!state.evaluate_condition(Some("success()")));
        assert!(state.evaluate_condition(Some("failure()")));
        assert!(state.evaluate_condition(Some("failure() && !cancelled()")));
        assert!(state.evaluate_condition(Some("always() && failure()")));
        assert_eq!(state.resolve_expressions("${{ job.status }}"), "failure");
        assert_eq!(
            state.resolve_expressions("${{ github.action_status }}"),
            "failure"
        );
        assert!(state.evaluate_condition(Some("always() && job.status == 'failure'")));
        assert!(state.evaluate_condition(Some("always() && github.action_status == 'failure'")));

        let mut ignored_state = JobExecutionState::default();
        ignored_state.apply(
            "ignored",
            &StepExecutionResult {
                exit_code: 1,
                skipped: false,
                failure_ignored: true,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        assert!(ignored_state.evaluate_condition(Some("steps.ignored.outcome == 'failure'")));
        assert!(ignored_state.evaluate_condition(Some("steps.ignored.conclusion == 'success'")));
        assert!(ignored_state.evaluate_condition(Some("success()")));
        assert!(!ignored_state.evaluate_condition(Some("failure()")));
        assert_eq!(
            ignored_state.resolve_expressions("${{ job.status }}"),
            "success"
        );
    }

    #[test]
    fn not_precedence_binds_tighter_than_and() {
        let mut state = JobExecutionState::default();
        state.apply(
            "failed",
            &StepExecutionResult {
                exit_code: 1,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        assert!(
            !state.evaluate_condition(Some("!cancelled() && steps.failed.outcome == 'success'"))
        );
        assert!(state.evaluate_condition(Some("!cancelled() && steps.failed.outcome == 'failure'")));
        assert!(state.evaluate_condition(Some("!cancelled()")));
        assert!(state.evaluate_condition(Some("failure() && !cancelled()")));
        assert!(!state.evaluate_condition(Some("!failure()")));
    }

    #[test]
    fn github_action_status_uses_current_composite_scope() {
        let mut state = JobExecutionState::default();
        state.apply(
            "top-level-failure",
            &StepExecutionResult {
                exit_code: 1,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        state.push_composite("composite");

        assert_eq!(state.resolve_expressions("${{ job.status }}"), "failure");
        assert_eq!(
            state.resolve_expressions("${{ github.action_status }}"),
            "success"
        );

        state.apply(
            "composite-child",
            &StepExecutionResult {
                exit_code: 1,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        assert_eq!(
            state.resolve_expressions("${{ github.action_status }}"),
            "failure"
        );
        state.pop_composite("composite");
        assert_eq!(
            state.resolve_expressions("${{ github.action_status }}"),
            "failure"
        );
    }

    #[test]
    fn executes_javascript_action_with_inputs_and_command_files() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let action = JavaScriptActionInvocation {
            node: "node20".into(),
            pre_container_path: None,
            pre_condition: None,
            main_container_path: "/__a/_actions/acme_action/v1/dist/index.js".into(),
            post_container_path: None,
            post_condition: None,
            action_container_path: "/__a/_actions/acme_action/v1".into(),
            inputs: BTreeMap::new(),
            env: vec![
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__a/_actions/acme_action/v1".into(),
                ),
                ("INPUT_NAME".into(), "value".into()),
                (
                    "INPUT_ACTION_PATH".into(),
                    "${{ github.action_path }}".into(),
                ),
            ],
        };
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let result = executor
            .execute_javascript_action(&container(&temp), "action1", &action, &temp)
            .unwrap();

        assert_eq!(result.exit_code, 0);
        let calls = &executor.runner().calls;
        assert_eq!(calls[2].1[0], "run");
        assert!(calls[2].1.contains(&"INPUT_NAME=value".into()));
        assert!(calls[2]
            .1
            .contains(&"INPUT_ACTION_PATH=/__a/_actions/acme_action/v1".into()));
        assert!(calls[2]
            .1
            .contains(&"GITHUB_OUTPUT=/github/file_commands/action1_output".into()));
        assert!(calls[2]
            .1
            .windows(2)
            .any(|pair| pair == ["--entrypoint", "node"]));
        assert!(calls[2].1.ends_with(&[
            "node:20-bookworm".into(),
            "/__a/_actions/acme_action/v1/dist/index.js".into()
        ]));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn executes_docker_action_with_inputs_and_command_files() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let action = DockerActionInvocation {
            image: "alpine:3.20".into(),
            build_context_host: None,
            dockerfile_host: None,
            action_container_path: "/__a/_actions/acme_docker/v1".into(),
            inputs: BTreeMap::new(),
            env: vec![
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__a/_actions/acme_docker/v1".into(),
                ),
                ("INPUT_NAME".into(), "value".into()),
                (
                    "INPUT_ACTION_PATH".into(),
                    "${{ github.action_path }}".into(),
                ),
            ],
            entrypoint: Some("/entrypoint.sh".into()),
            args: vec!["arg1".into()],
        };
        let steps = vec![ExecutableStep::Docker {
            step_id: "docker1".into(),
            display_name: String::new(),
            invocation: action,
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 1);
        let calls = &executor.runner().calls;
        assert_eq!(calls[2].1[0], "run");
        assert!(calls[2]
            .1
            .windows(2)
            .any(|pair| pair == ["--workdir", "/github/workspace"]));
        assert!(calls[2].1.contains(&format!(
            "{}:/github/workspace",
            temp.join("work").display()
        )));
        assert!(calls[2]
            .1
            .contains(&format!("{}:/github/runner_temp", temp.display())));
        assert!(calls[2]
            .1
            .contains(&format!("{}:/github/file_commands", temp.display())));
        assert!(calls[2]
            .1
            .contains(&"GITHUB_WORKSPACE=/github/workspace".into()));
        assert!(calls[2]
            .1
            .contains(&"RUNNER_TEMP=/github/runner_temp".into()));
        assert!(calls[2].1.contains(&"INPUT_NAME=value".into()));
        assert!(calls[2]
            .1
            .contains(&"INPUT_ACTION_PATH=/__a/_actions/acme_docker/v1".into()));
        assert!(calls[2]
            .1
            .contains(&"GITHUB_OUTPUT=/github/file_commands/docker1_output".into()));
        assert!(calls[2]
            .1
            .windows(2)
            .any(|pair| pair == ["--entrypoint", "/entrypoint.sh"]));
        assert!(calls[2].1.ends_with(&["alpine:3.20".into(), "arg1".into()]));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn resolves_docker_action_entrypoint_and_args_at_execution_time() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let action = DockerActionInvocation {
            image: "alpine:3.20".into(),
            build_context_host: None,
            dockerfile_host: None,
            action_container_path: "/__a/_actions/acme_docker/v1".into(),
            inputs: BTreeMap::new(),
            env: Vec::new(),
            entrypoint: Some("${{ env.DOCKER_ENTRYPOINT }}".into()),
            args: vec![
                "pr-${{ github.event.pull_request.number }}".into(),
                "${{ secrets.DOCKER_TOKEN }}".into(),
            ],
        };
        let steps = vec![ExecutableStep::Docker {
            step_id: "docker1".into(),
            display_name: String::new(),
            invocation: action,
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let context = vec![
            (
                "github".into(),
                serde_json::json!({ "event": { "pull_request": { "number": 42 } } }),
            ),
            (
                "secrets".into(),
                serde_json::json!({ "DOCKER_TOKEN": "secret-token" }),
            ),
        ];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps_with_context(
                &container(&temp),
                &steps,
                &[("DOCKER_ENTRYPOINT".into(), "/entrypoint.sh".into())],
                &context,
                &temp,
            )
            .unwrap();

        let calls = &executor.runner().calls;
        assert!(calls[2]
            .1
            .windows(2)
            .any(|pair| pair == ["--entrypoint", "/entrypoint.sh"]));
        assert!(calls[2].1.ends_with(&[
            "alpine:3.20".into(),
            "pr-42".into(),
            "secret-token".into()
        ]));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn builds_local_docker_action_before_running_it() {
        let temp = temp_dir();
        let action_dir = temp.join("actions/acme");
        fs::create_dir_all(&action_dir).unwrap();
        let action = DockerActionInvocation {
            image: "velnor-action-acme-docker-v1-root".into(),
            build_context_host: Some(action_dir.clone()),
            dockerfile_host: Some(action_dir.join("Dockerfile")),
            action_container_path: "/__a/_actions/acme_docker/v1".into(),
            inputs: BTreeMap::new(),
            env: Vec::new(),
            entrypoint: None,
            args: Vec::new(),
        };
        let steps = vec![ExecutableStep::Docker {
            step_id: "docker1".into(),
            display_name: String::new(),
            invocation: action,
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let calls = &executor.runner().calls;
        assert_eq!(calls[2].1[0], "build");
        assert!(calls[2]
            .1
            .contains(&"velnor-action-acme-docker-v1-root".into()));
        assert_eq!(
            executor.runner().env[2],
            vec![(
                "DOCKER_CONFIG".into(),
                temp.join("_velnor/docker-client").display().to_string()
            )]
        );
        assert_eq!(
            fs::metadata(temp.join("_velnor/docker-client"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(calls[3].1[0], "run");
        assert!(calls[3]
            .1
            .contains(&"velnor-action-acme-docker-v1-root".into()));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn executes_ordered_script_and_javascript_steps_in_one_container() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "step1".into(),
                display_name: String::new(),
                script: "echo NAME=value >> $GITHUB_ENV".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::JavaScript {
                step_id: "action1".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/acme_action/v1/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/acme_action/v1".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_NAME".into(), "value".into()),
                        ("TOKEN".into(), "${{ github.token }}".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "step2".into(),
                display_name: String::new(),
                script: "echo done".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[
                    ("GITHUB_REPOSITORY".into(), "acme/repo".into()),
                    ("GITHUB_TOKEN".into(), "ghs_token".into()),
                ],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 3);
        let calls = &executor.runner().calls;
        assert_eq!(calls[0].1[0], "network");
        assert_eq!(calls[1].1[0], "run");
        assert_eq!(calls[2].1[0], "exec");
        assert_eq!(calls[3].1[0], "run");
        assert_eq!(calls[4].1[0], "exec");
        assert_cleanup_reclaims_job_docker(calls, 5, &temp);
        assert!(calls[3].1.contains(&"INPUT_NAME=value".into()));
        assert!(calls[3].1.contains(&"GITHUB_REPOSITORY=acme/repo".into()));
        assert!(calls[3].1.contains(&"TOKEN=ghs_token".into()));
        assert!(calls[3]
            .1
            .contains(&"GITHUB_OUTPUT=/github/file_commands/action1_output".into()));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_cache_and_artifact_actions_receive_runtime_env() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Native {
                step_id: "cache".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::Cache,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("path".into(), "~/.cache/rust-script".into()),
                        (
                            "key".into(),
                            "rust-script-${{ runner.os }}-${{ hashFiles('kestra-docker-containers/**/*.rs', 'kestra-docker-containers/**/build.toml', 'kestra-docker-containers/justfile') }}".into(),
                        ),
                        (
                            "restore-keys".into(),
                            "rust-script-${{ runner.os }}-\n".into(),
                        ),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Native {
                step_id: "upload".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::UploadArtifact,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        (
                            "name".into(),
                            "construct-digest-${{ matrix.platform }}".into(),
                        ),
                        (
                            "path".into(),
                            "${{ env.DIGEST_DIR }}/${{ matrix.platform }}.digest".into(),
                        ),
                        ("if-no-files-found".into(), "error".into()),
                        ("retention-days".into(), "1".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Native {
                step_id: "download".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::DownloadArtifact,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("pattern".into(), "construct-digest-*".into()),
                        ("path".into(), "${{ env.DIGEST_DIR }}".into()),
                        ("merge-multiple".into(), "true".into()),
                    ]
                    .into(),
                    // Keep this broad adapter-contract test offline. Dedicated
                    // protocol tests cover the authenticated Results Service path.
                    env: vec![("ACTIONS_RUNTIME_TOKEN".into(), String::new())],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Native {
                step_id: "github-runtime".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::GitHubRuntimeExport,
                    cache_kind: None,
                    source_path: None,
                    inputs: [("github-token".into(), "ghs_token".into())].into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];
        let runtime_env = vec![
            ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
            ("GITHUB_RETENTION_DAYS".into(), "90".into()),
            ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
            ("RUNNER_OS".into(), "Linux".into()),
            ("DIGEST_DIR".into(), "/__w/digests".into()),
            ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
            (
                "ACTIONS_RUNTIME_URL".into(),
                "https://runtime.actions".into(),
            ),
            (
                "ACTIONS_RESULTS_URL".into(),
                "https://results.actions".into(),
            ),
            ("ACTIONS_CACHE_URL".into(), "https://cache.actions".into()),
            ("ACTIONS_CACHE_SERVICE_V2".into(), "True".into()),
        ];
        let workspace = temp.join("work/kestra-docker-containers");
        fs::create_dir_all(workspace.join("app/src")).unwrap();
        fs::write(workspace.join("app/src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(workspace.join("app/build.toml"), "image = 'app'\n").unwrap();
        fs::write(workspace.join("justfile"), "build:\n").unwrap();
        fs::create_dir_all(temp.join("work/digests")).unwrap();
        fs::write(temp.join("work/digests/linux-amd64.digest"), "sha256:abc\n").unwrap();
        let mut expected_hash = Sha256::new();
        expected_hash.update(Sha256::digest(b"image = 'app'\n"));
        expected_hash.update(Sha256::digest(b"fn main() {}\n"));
        expected_hash.update(Sha256::digest(b"build:\n"));
        let digest = expected_hash.finalize();
        let expected_hash = hex_digest(&digest);
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps_with_context(
                &container(&temp),
                &steps,
                &runtime_env,
                &[(
                    "matrix".into(),
                    serde_json::json!({ "platform": "linux-amd64" }),
                )],
                &temp,
            )
            .unwrap();

        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 0);
        assert_eq!(results[0].state.outputs["cache-hit"], "false");
        // Root exposes only `cache-hit`; the primary key stays internal state.
        assert!(!results[0].state.outputs.contains_key("cache-primary-key"));
        assert_eq!(
            results[0].state.state["primaryKey"],
            format!("rust-script-Linux-{expected_hash}")
        );
        assert!(results[0]
            .stdout
            .contains("Cache path: ~/.cache/rust-script"));
        let expected_artifact_id = artifact_id_for_name(
            &JobExecutionState::new(&runtime_env),
            "construct-digest-linux-amd64",
        );
        assert_eq!(
            results[1].state.outputs["artifact-id"],
            expected_artifact_id
        );
        assert_eq!(
            results[1].state.outputs["artifact-url"],
            format!("https://results.actions/artifacts/{expected_artifact_id}")
        );
        assert_eq!(results[2].state.outputs["download-path"], "/__w/digests");
        assert_eq!(
            results[3].state.env["ACTIONS_RUNTIME_TOKEN"],
            "runtime-token"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_artifacts_are_shared_across_jobs_in_same_run_workdir() {
        let root = temp_dir();
        let upload_temp = root.join("upload-job/temp");
        let download_temp = root.join("download-job/temp");
        fs::create_dir_all(upload_temp.join("work/digests")).unwrap();
        fs::create_dir_all(download_temp.join("work")).unwrap();
        fs::write(
            upload_temp.join("work/digests/linux-amd64.digest"),
            "sha256:abc\n",
        )
        .unwrap();
        let runtime_env = vec![
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
        ];
        let upload = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::UploadArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("name".into(), "construct-digest-linux-amd64".into()),
                    ("path".into(), "/__w/digests/linux-amd64.digest".into()),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let download = vec![ExecutableStep::Native {
            step_id: "download".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::DownloadArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("pattern".into(), "construct-digest-*".into()),
                    ("path".into(), "/__w/downloaded".into()),
                    ("merge-multiple".into(), "true".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&upload_temp),
                &upload,
                &runtime_env,
                &upload_temp,
            )
            .unwrap();
        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&download_temp),
                &download,
                &runtime_env,
                &download_temp,
            )
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            fs::read_to_string(download_temp.join("work/downloaded/linux-amd64.digest")).unwrap(),
            "sha256:abc\n"
        );
        assert!(root
            .join("_velnor_artifacts/123456-1/construct-digest-linux-amd64/linux-amd64.digest")
            .exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_download_artifact_all_mode_uses_named_directories() {
        let root = temp_dir();
        let upload_temp = root.join("upload-job/temp");
        let download_temp = root.join("download-job/temp");
        fs::create_dir_all(upload_temp.join("work")).unwrap();
        fs::create_dir_all(download_temp.join("work")).unwrap();
        fs::write(upload_temp.join("work/linux.txt"), "linux\n").unwrap();
        fs::write(upload_temp.join("work/macos.txt"), "macos\n").unwrap();
        let runtime_env = vec![
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
        ];
        for (name, path) in [
            ("artifact-linux", "/__w/linux.txt"),
            ("artifact-macos", "/__w/macos.txt"),
        ] {
            let upload = vec![ExecutableStep::Native {
                step_id: format!("upload-{name}"),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::UploadArtifact,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("name".into(), name.into()),
                        ("path".into(), path.into()),
                        ("if-no-files-found".into(), "error".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }];
            DockerJobEngine::new(RecordingRunner::default())
                .execute_ordered_steps(
                    &container(&upload_temp),
                    &upload,
                    &runtime_env,
                    &upload_temp,
                )
                .unwrap();
        }
        let download = vec![ExecutableStep::Native {
            step_id: "download".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::DownloadArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [("path".into(), "/__w/downloaded".into())].into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&download_temp),
                &download,
                &runtime_env,
                &download_temp,
            )
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            fs::read_to_string(download_temp.join("work/downloaded/artifact-linux/linux.txt"))
                .unwrap(),
            "linux\n"
        );
        assert_eq!(
            fs::read_to_string(download_temp.join("work/downloaded/artifact-macos/macos.txt"))
                .unwrap(),
            "macos\n"
        );
        assert!(!download_temp.join("work/downloaded/linux.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_download_artifact_rejects_destination_symlink() {
        use std::os::unix::fs::symlink;

        let root = temp_dir();
        let temp = root.join("job/temp");
        let workspace = temp.join("work");
        let outside = root.join("outside");
        let destination = workspace.join("downloaded");
        fs::create_dir_all(&temp).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, destination.join("nested")).unwrap();

        let runtime_env = vec![
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
        ];
        let state = JobExecutionState::new_with_workspace(&runtime_env, &[], &workspace, &temp);
        let artifact_dir = artifact_store_dir(&state).unwrap().join("artifact");
        fs::create_dir_all(artifact_dir.join("nested")).unwrap();
        fs::write(artifact_dir.join("nested/output.txt"), "artifact\n").unwrap();
        let action = NativeActionInvocation {
            git_ref: String::new(),
            adapter: NativeActionAdapter::DownloadArtifact,
            cache_kind: None,
            source_path: None,
            inputs: [
                ("name".into(), "artifact".into()),
                ("path".into(), "/__w/downloaded".into()),
            ]
            .into(),
            env: Vec::new(),
        };

        let error = native_download_artifact(&action, &state).unwrap_err();

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("without following links") || rendered.contains("symlink"),
            "unexpected secure-copy error: {rendered}"
        );
        assert!(!outside.join("output.txt").exists());
        assert!(fs::symlink_metadata(destination.join("nested"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(fs::read_dir(&destination).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .as_encoded_bytes()
                .starts_with(b".velnor-copy-")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_download_artifact_reports_container_download_path() {
        let root = temp_dir();
        let upload_temp = root.join("upload-job/temp");
        let download_temp = root.join("download-job/temp");
        fs::create_dir_all(upload_temp.join("work")).unwrap();
        fs::create_dir_all(download_temp.join("work")).unwrap();
        fs::write(upload_temp.join("work/output.txt"), "artifact\n").unwrap();
        let runtime_env = vec![
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
            ("GITHUB_WORKSPACE".into(), "/github/workspace".into()),
        ];
        let upload = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::UploadArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("name".into(), "output".into()),
                    ("path".into(), "/github/workspace/output.txt".into()),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&upload_temp),
                &upload,
                &runtime_env,
                &upload_temp,
            )
            .unwrap();
        let download = vec![ExecutableStep::Native {
            step_id: "download".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::DownloadArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("name".into(), "output".into()),
                    ("path".into(), "downloads/output".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&download_temp),
                &download,
                &runtime_env,
                &download_temp,
            )
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            results[0].state.outputs["download-path"],
            "/github/workspace/downloads/output"
        );
        assert_eq!(
            fs::read_to_string(download_temp.join("work/downloads/output/output.txt")).unwrap(),
            "artifact\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn native_download_artifact_normalizes_permissions() {
        let root = temp_dir();
        let upload_temp = root.join("upload-job/temp");
        let download_temp = root.join("download-job/temp");
        fs::create_dir_all(upload_temp.join("work/bin")).unwrap();
        fs::create_dir_all(download_temp.join("work")).unwrap();
        let script = upload_temp.join("work/bin/tool.sh");
        fs::write(&script, "#!/usr/bin/env bash\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let runtime_env = vec![
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
        ];
        let upload = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::UploadArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("name".into(), "bin".into()),
                    ("path".into(), "/__w/bin".into()),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&upload_temp),
                &upload,
                &runtime_env,
                &upload_temp,
            )
            .unwrap();
        let download = vec![ExecutableStep::Native {
            step_id: "download".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::DownloadArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("name".into(), "bin".into()),
                    ("path".into(), "/__w/downloaded".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&download_temp),
                &download,
                &runtime_env,
                &download_temp,
            )
            .unwrap();

        let downloaded_dir = download_temp.join("work/downloaded");
        let downloaded_file = downloaded_dir.join("tool.sh");
        assert_eq!(
            fs::metadata(&downloaded_dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(&downloaded_file).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn artifact_upload_sources_rejects_too_many_files_before_reading() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("first.txt"), "").unwrap();
        fs::write(temp.join("second.txt"), "").unwrap();

        let error = artifact_upload_sources(
            "release",
            &temp,
            ArtifactUploadSourceLimits {
                files: 1,
                file_bytes: 10,
                total_bytes: 10,
                path_bytes: u64::MAX,
                path_depth: usize::MAX,
            },
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("artifact 'release' contains 2 files, exceeding the 1-file upload limit"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn artifact_upload_sources_reports_directory_enumeration_failure() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let missing = temp.join("missing");

        let error =
            artifact_upload_sources("release", &missing, RESULTS_ARTIFACT_UPLOAD_SOURCE_LIMITS)
                .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("enumerate local files"));
        assert!(message.contains("Results Service artifact 'release'"));
        assert!(message.contains("missing"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn artifact_upload_sources_reports_non_utf8_filename() {
        use std::os::unix::ffi::OsStringExt;

        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let filename = std::ffi::OsString::from_vec(b"bad-\xff.txt".to_vec());
        if fs::write(temp.join(&filename), "content").is_err() {
            fs::remove_dir_all(temp).unwrap();
            return;
        }

        let error =
            artifact_upload_sources("release", &temp, RESULTS_ARTIFACT_UPLOAD_SOURCE_LIMITS)
                .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("non-UTF-8 or unsafe relative file name"));
        assert!(message.contains("Results Service artifact 'release'"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn artifact_upload_sources_rejects_symlink_traversal_and_admission() {
        let source_root = temp_dir();
        fs::create_dir_all(&source_root).unwrap();
        let outside = source_root.join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        std::os::unix::fs::symlink(&outside, source_root.join("linked.txt")).unwrap();
        let error = artifact_upload_sources(
            "release",
            &source_root,
            RESULTS_ARTIFACT_UPLOAD_SOURCE_LIMITS,
        )
        .unwrap_err();
        assert!(error.to_string().contains("is a symlink"), "{error:#}");
        fs::remove_dir_all(source_root).unwrap();

        let traversal_root = temp_dir();
        fs::create_dir_all(&traversal_root).unwrap();
        let nested = traversal_root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("inside.txt"), "inside").unwrap();
        std::os::unix::fs::symlink(&nested, traversal_root.join("linked-dir")).unwrap();
        let error = artifact_upload_sources(
            "release",
            &traversal_root,
            RESULTS_ARTIFACT_UPLOAD_SOURCE_LIMITS,
        )
        .unwrap_err();
        assert!(error.to_string().contains("is a symlink"), "{error:#}");
        fs::remove_dir_all(traversal_root).unwrap();
    }

    #[test]
    fn artifact_upload_sources_rejects_oversized_file_before_reading() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("release.tar"), "four").unwrap();

        let error = artifact_upload_sources(
            "release",
            &temp,
            ArtifactUploadSourceLimits {
                files: 1,
                file_bytes: 3,
                total_bytes: 10,
                path_bytes: u64::MAX,
                path_depth: usize::MAX,
            },
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("release.tar"));
        assert!(message.contains("4 bytes, exceeding the 3-byte per-file upload limit"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn artifact_upload_sources_rejects_oversized_total_before_reading() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("first.txt"), "aa").unwrap();
        fs::write(temp.join("second.txt"), "bb").unwrap();

        let error = artifact_upload_sources(
            "release",
            &temp,
            ArtifactUploadSourceLimits {
                files: 2,
                file_bytes: 2,
                total_bytes: 3,
                path_bytes: u64::MAX,
                path_depth: usize::MAX,
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains(
            "artifact 'release' sources total 4 bytes, exceeding the 3-byte upload limit"
        ));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn artifact_upload_source_read_rechecks_growth_after_admission() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let source = temp.join("release.tar");
        fs::write(&source, "aa").unwrap();
        let limits = ArtifactUploadSourceLimits {
            files: 1,
            file_bytes: 3,
            total_bytes: 3,
            path_bytes: u64::MAX,
            path_depth: usize::MAX,
        };
        let sources = artifact_upload_sources("release", &temp, limits).unwrap();
        fs::write(&source, "four").unwrap();

        let error = read_artifact_upload_sources("release", sources, limits).unwrap_err();

        assert!(error.to_string().contains(
            "release.tar grew from 2 to 4 bytes after admission, exceeding the 3-byte per-file upload limit"
        ));
        fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn resolve_artifact_sources_preserves_explicit_mapped_globs() {
        let root = temp_dir();
        let workspace = root.join("work");
        let target = root.join("target-store");
        fs::create_dir_all(workspace.join("dist")).unwrap();
        fs::create_dir_all(root.join("logs")).unwrap();
        fs::create_dir_all(target.join("release")).unwrap();
        fs::write(workspace.join("dist/a.txt"), "a\n").unwrap();
        fs::write(workspace.join("dist/workspace.txt"), "workspace\n").unwrap();
        fs::write(workspace.join("dist/z.txt"), "z\n").unwrap();
        fs::write(root.join("logs/temp.txt"), "temp\n").unwrap();
        fs::write(target.join("release/target.bin"), "target\n").unwrap();
        std::os::unix::fs::symlink(root.join("logs/temp.txt"), workspace.join("unrelated-link"))
            .unwrap();

        let mut state = JobExecutionState::new_with_workspace(&[], &[], &workspace, &root);
        state.cargo_target_host = Some(target.clone());
        let workspace_matches = vec![
            workspace.join("dist/a.txt"),
            workspace.join("dist/workspace.txt"),
            workspace.join("dist/z.txt"),
        ];
        let cases = [
            ("dist/*.txt", workspace_matches.clone()),
            ("/__w/dist/*.txt", workspace_matches.clone()),
            ("/github/workspace/dist/*.txt", workspace_matches),
            ("/__t/logs/*.txt", vec![root.join("logs/temp.txt")]),
            (
                "/github/runner_temp/logs/*.txt",
                vec![root.join("logs/temp.txt")],
            ),
            ("/tmp/logs/*.txt", vec![root.join("logs/temp.txt")]),
            (
                "target/release/*.bin",
                vec![target.join("release/target.bin")],
            ),
            (
                "/__w/target/release/*.bin",
                vec![target.join("release/target.bin")],
            ),
            (
                "/github/workspace/target/release/*.bin",
                vec![target.join("release/target.bin")],
            ),
        ];

        for (pattern, expected) in cases {
            let pattern = validate_artifact_input_path(pattern).unwrap();
            assert_eq!(
                resolve_artifact_sources(&state, pattern)
                    .unwrap()
                    .into_iter()
                    .map(|source| source.path)
                    .collect::<Vec<_>>(),
                expected,
                "mapped glob changed: {}",
                pattern.as_str()
            );
        }
        let missing = validate_artifact_input_path("missing/*.txt").unwrap();
        assert!(resolve_artifact_sources(&state, missing)
            .unwrap()
            .is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn artifact_glob_literal_base_keeps_directory_pattern_relative() {
        assert_eq!(
            artifact_glob_literal_base_and_pattern("dist/*.txt"),
            (PathBuf::from("dist"), "*.txt".to_string())
        );
    }

    #[test]
    fn native_upload_artifact_expands_target_release_globs() {
        assert!(artifact_store_uncompressed("0"));
        assert!(!artifact_store_uncompressed(""));
        assert_eq!(artifact_retention_days("14", Some("7")), Some(7));
        assert_eq!(artifact_retention_days("7", Some("90")), Some(7));
        assert_eq!(artifact_retention_days("", Some("90")), None);

        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        fs::write(temp.join("work/jackin-1.2.3-x86_64.tar.gz"), "archive\n").unwrap();
        fs::write(
            temp.join("work/jackin-1.2.3-x86_64.tar.gz.sha256"),
            "hash\n",
        )
        .unwrap();
        fs::write(temp.join("work/ignore.txt"), "no\n").unwrap();
        let sink = Arc::new(
            crate::ops::OpsSink::open(temp.join("state.db"), "test-instance".to_owned()).unwrap(),
        );
        let admission = crate::ops::JobAdmission {
            instance_slug: "test-instance".to_owned(),
            job_uid: "job-artifact-materialize".to_owned(),
            repository_full_name: "tailrocks/velnor".to_owned(),
            workflow: "control-plane".to_owned(),
            job_name: "artifact-materialize".to_owned(),
            run_id: Some(44),
            attempt: Some(1),
            head_ref: Some("refs/heads/main".to_owned()),
            head_sha: Some("deadbeef".to_owned()),
            trigger_event: Some("workflow_dispatch".to_owned()),
            queued_at_rfc3339: None,
            slot_name: Some("slot-0".to_owned()),
            runner_name: Some("runner-0".to_owned()),
            trust_scope: Some("trusted".to_owned()),
            resource_policy: Some("standard".to_owned()),
            masks: vec!["secret-marker".to_owned()],
        };
        assert!(sink.record_admission(&admission));
        let steps = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::UploadArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("name".into(), "jackin-x86_64-unknown-linux-gnu".into()),
                    (
                        "path".into(),
                        "jackin-*.tar.gz\njackin-*.tar.gz.sha256".into(),
                    ),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .with_tool_prep_telemetry(Arc::clone(&sink), admission)
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        let telemetry = fs::read_to_string(temp.join("state.test-instance.telemetry.jsonl"))
            .expect("artifact materialization telemetry");
        let record = telemetry
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|record| record["event"] == "artifact_materialize")
            .expect("artifact_materialize event");
        let fields = record["fields"].as_object().expect("artifact fields");
        assert_eq!(
            fields["digest"],
            results[0].state.outputs["artifact-digest"]
        );
        assert_eq!(fields["subset"], "artifact");
        assert!(fields["ms"]
            .as_u64()
            .is_some_and(|value| { value <= MAX_ARTIFACT_MATERIALIZE_TELEMETRY_MS }));
        assert!(!telemetry.contains("jackin-x86_64-unknown-linux-gnu"));
        assert!(!telemetry.contains("/work"));
        assert_eq!(
            fs::read_to_string(
                temp.join("_velnor_artifacts/local-1/jackin-x86_64-unknown-linux-gnu/jackin-1.2.3-x86_64.tar.gz")
            )
            .unwrap(),
            "archive\n"
        );
        assert_eq!(
            fs::read_to_string(
                temp.join("_velnor_artifacts/local-1/jackin-x86_64-unknown-linux-gnu/jackin-1.2.3-x86_64.tar.gz.sha256")
            )
            .unwrap(),
            "hash\n"
        );
        assert!(!temp
            .join("_velnor_artifacts/local-1/jackin-x86_64-unknown-linux-gnu/ignore.txt")
            .exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_upload_artifact_preserves_nested_glob_structure_and_other_path_semantics() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(workspace.join("directory")).unwrap();
        fs::create_dir_all(workspace.join("glob/alpha")).unwrap();
        fs::create_dir_all(workspace.join("glob/beta")).unwrap();
        fs::write(workspace.join("direct.txt"), "direct\n").unwrap();
        fs::write(workspace.join("directory/child.txt"), "directory\n").unwrap();
        fs::write(workspace.join("glob/alpha/result.txt"), "alpha\n").unwrap();
        fs::write(workspace.join("glob/beta/result.txt"), "beta\n").unwrap();
        let action = NativeActionInvocation {
            git_ref: String::new(),
            adapter: NativeActionAdapter::UploadArtifact,
            cache_kind: None,
            source_path: None,
            inputs: [
                ("name".into(), "path-semantics".into()),
                ("path".into(), "direct.txt\ndirectory\nglob/**/*.txt".into()),
                ("if-no-files-found".into(), "error".into()),
            ]
            .into(),
            env: Vec::new(),
        };
        let state = JobExecutionState::new_with_workspace(&[], &[], &workspace, &temp);

        let result = native_upload_artifact(&action, &state).unwrap();

        assert_eq!(result.exit_code, 0);
        let artifact = temp.join("_velnor_artifacts/local-1/path-semantics");
        assert_eq!(
            fs::read_to_string(artifact.join("direct.txt")).unwrap(),
            "direct\n"
        );
        assert_eq!(
            fs::read_to_string(artifact.join("child.txt")).unwrap(),
            "directory\n"
        );
        assert_eq!(
            fs::read_to_string(artifact.join("alpha/result.txt")).unwrap(),
            "alpha\n"
        );
        assert_eq!(
            fs::read_to_string(artifact.join("beta/result.txt")).unwrap(),
            "beta\n"
        );
        assert!(!artifact.join("directory/child.txt").exists());
        assert!(!artifact.join("result.txt").exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_upload_artifact_reads_job_local_persistent_workspace_target() {
        let temp = temp_dir();
        let mut spec = container(&temp);
        let target = spec.workspace_host.join("target");
        fs::create_dir_all(target.join("cargo-timings")).unwrap();
        fs::write(target.join("cargo-timings/cargo-timing.html"), "timing\n").unwrap();
        fs::write(target.join("sccache-check.txt"), "stats\n").unwrap();
        fs::write(target.join(".private"), "hidden\n").unwrap();
        spec.cargo_target_host = Some(temp.join("persistent-target-generation"));
        let steps = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::UploadArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("name".into(), "cargo-cache-evidence".into()),
                    (
                        "path".into(),
                        "target/cargo-timings/*.html\n/__w/target/sccache-check.txt".into(),
                    ),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&spec, &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        let artifact = temp.join("_velnor_artifacts/local-1/cargo-cache-evidence");
        assert_eq!(
            fs::read_to_string(artifact.join("cargo-timing.html")).unwrap(),
            "timing\n"
        );
        assert_eq!(
            fs::read_to_string(artifact.join("sccache-check.txt")).unwrap(),
            "stats\n"
        );
        assert!(!artifact.join(".private").exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_upload_artifact_excludes_hidden_files_by_default() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work/dist/.well-known")).unwrap();
        fs::write(temp.join("work/dist/app.js"), "app\n").unwrap();
        fs::write(temp.join("work/dist/.env"), "secret\n").unwrap();
        fs::write(temp.join("work/dist/.well-known/assetlinks.json"), "{}\n").unwrap();
        let upload = |include_hidden_files: Option<&str>, name: &str| {
            let mut inputs: BTreeMap<String, String> = [
                ("name".into(), name.into()),
                ("path".into(), "dist".into()),
                ("if-no-files-found".into(), "error".into()),
            ]
            .into();
            if let Some(value) = include_hidden_files {
                inputs.insert("include-hidden-files".into(), value.into());
            }
            vec![ExecutableStep::Native {
                step_id: format!("upload-{name}"),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::UploadArtifact,
                    cache_kind: None,
                    source_path: None,
                    inputs,
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }]
        };

        let default_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &upload(None, "default"), &[], &temp)
            .unwrap();
        let explicit_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&temp),
                &upload(Some("true"), "explicit"),
                &[],
                &temp,
            )
            .unwrap();

        assert_eq!(default_results[0].exit_code, 0);
        assert_eq!(explicit_results[0].exit_code, 0);
        assert!(temp
            .join("_velnor_artifacts/local-1/default/app.js")
            .exists());
        assert!(!temp.join("_velnor_artifacts/local-1/default/.env").exists());
        assert!(!temp
            .join("_velnor_artifacts/local-1/default/.well-known")
            .exists());
        assert!(temp
            .join("_velnor_artifacts/local-1/explicit/.env")
            .exists());
        assert!(temp
            .join("_velnor_artifacts/local-1/explicit/.well-known/assetlinks.json")
            .exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_upload_artifact_rejects_unconfined_sources_before_create() {
        use std::io::{ErrorKind, Read, Write};
        use std::net::TcpListener;
        use std::os::unix::fs::symlink;
        use std::sync::mpsc;
        use std::time::Duration;

        let root = temp_dir();
        let workspace = root.join("work");
        fs::create_dir_all(workspace.join("normal-dir/nested")).unwrap();
        fs::write(workspace.join("normal.txt"), "normal file\n").unwrap();
        fs::write(
            workspace.join("normal-dir/nested/child.txt"),
            "normal directory\n",
        )
        .unwrap();
        fs::create_dir_all(workspace.join("glob-root")).unwrap();
        fs::write(workspace.join("glob-root/inside.txt"), "inside\n").unwrap();
        fs::create_dir_all(root.join("outside-dir")).unwrap();
        fs::write(root.join("outside-dir/secret.txt"), "secret\n").unwrap();
        fs::write(root.join("outside.txt"), "outside\n").unwrap();

        let upload = |path: &str, name: &str| NativeActionInvocation {
            git_ref: String::new(),
            adapter: NativeActionAdapter::UploadArtifact,
            cache_kind: None,
            source_path: None,
            inputs: [
                ("name".into(), name.into()),
                ("path".into(), path.into()),
                ("if-no-files-found".into(), "error".into()),
            ]
            .into(),
            env: Vec::new(),
        };
        let normal_state = JobExecutionState::new_with_workspace(&[], &[], &workspace, &root);
        native_upload_artifact(&upload("normal.txt", "normal-file"), &normal_state).unwrap();
        native_upload_artifact(&upload("normal-dir", "normal-directory"), &normal_state).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("_velnor_artifacts/local-1/normal-file/normal.txt"))
                .unwrap(),
            "normal file\n"
        );
        assert_eq!(
            fs::read_to_string(
                root.join("_velnor_artifacts/local-1/normal-directory/nested/child.txt")
            )
            .unwrap(),
            "normal directory\n"
        );

        symlink("normal.txt", workspace.join("file-link.txt")).unwrap();
        symlink("normal-dir", workspace.join("directory-link")).unwrap();
        symlink("normal-dir", workspace.join("ancestor-link")).unwrap();
        symlink(root.join("outside-dir"), workspace.join("glob-root/escape")).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let results_url = format!("http://{}", listener.local_addr().unwrap());
        let (stop_tx, stop_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut requests = 0;
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        requests += 1;
                        let mut request = [0_u8; 4096];
                        let _ = stream.read(&mut request);
                        write!(
                            stream,
                            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .unwrap();
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        if stop_rx.recv_timeout(Duration::from_millis(10)).is_ok() {
                            break;
                        }
                    }
                    Err(error) => panic!("artifact test server failed: {error}"),
                }
            }
            requests
        });
        let secure_state = JobExecutionState::new_with_workspace(
            &[
                ("ACTIONS_RESULTS_URL".into(), results_url),
                (
                    "ACTIONS_RUNTIME_TOKEN".into(),
                    runtime_token_with_results_scope("plan-1", "job-1"),
                ),
            ],
            &[],
            &workspace,
            &root,
        );

        let cases = [
            ("file-link.txt", "file-symlink", "symlink"),
            ("directory-link", "directory-symlink", "symlink"),
            (
                "ancestor-link/nested/child.txt",
                "directory-symlink-ancestor",
                "symlink",
            ),
            (
                "ancestor-link/**/*.txt",
                "glob-directory-symlink-ancestor",
                "symlink",
            ),
            ("glob-root/**/*.txt", "glob-directory-symlink", "symlink"),
            (
                "glob-root/escape/**/*.txt\n/etc/passwd",
                "preflight-before-enumeration",
                "unmapped absolute root",
            ),
            (
                "/etc/passwd",
                "unmapped-absolute-file",
                "unmapped absolute root",
            ),
            (
                "/__w//etc/passwd",
                "absolute-mapped-suffix",
                "absolute mapped suffix",
            ),
            ("../outside.txt", "parent-traversal", "parent traversal"),
            ("/etc/*", "unmapped-absolute-glob", "unmapped absolute root"),
        ];
        let mut errors = Vec::new();
        let mut staging_empty = Vec::new();
        for (path, name, expected_error) in cases {
            let error = match native_upload_artifact(&upload(path, name), &secure_state) {
                Ok(result) => format!(
                    "unexpected upload result: exit={}, stderr={}",
                    result.exit_code, result.stderr
                ),
                Err(error) => format!("{error:#}"),
            };
            errors.push((path, expected_error, error));
            let artifact_dir = root.join("_velnor_artifacts/local-1").join(name);
            staging_empty.push(
                !artifact_dir.exists() || fs::read_dir(artifact_dir).unwrap().next().is_none(),
            );
        }

        stop_tx.send(()).unwrap();
        let create_requests = server.join().unwrap();
        fs::remove_dir_all(&root).unwrap();

        for (path, expected, error) in &errors {
            assert!(
                error.contains(expected),
                "unexpected error for {path}: {error}"
            );
        }
        assert!(staging_empty.iter().all(|empty| *empty));
        assert_eq!(create_requests, 0, "CreateArtifact must not be called");
    }

    #[test]
    fn native_upload_pages_artifact_uploads_single_dereferenced_tar() {
        let temp = temp_dir();
        let site = temp.join("work/site");
        fs::create_dir_all(site.join("assets")).unwrap();
        fs::create_dir_all(site.join(".github")).unwrap();
        fs::create_dir_all(site.join(".well-known")).unwrap();
        fs::write(site.join("index.html"), "pages\n").unwrap();
        fs::write(site.join("assets/app.js"), "app\n").unwrap();
        fs::write(site.join(".github/workflow.yml"), "excluded\n").unwrap();
        fs::write(site.join(".well-known/security.txt"), "hidden\n").unwrap();
        fs::hard_link(site.join("index.html"), site.join("hard-linked.html")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("index.html", site.join("linked.html")).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "pages".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::UploadPagesArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [("path".into(), "site".into())].into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            results[0].state.outputs["artifact_id"],
            results[0].state.outputs["artifact-id"]
        );
        let artifact_dir = temp.join("_velnor_artifacts/local-1/github-pages");
        let files = fs::read_dir(&artifact_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(files, vec![std::ffi::OsString::from("artifact.tar")]);

        let file = fs::File::open(artifact_dir.join("artifact.tar")).unwrap();
        let mut archive = tar::Archive::new(file);
        let mut archived = BTreeMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.header().entry_type().is_file() {
                let mut content = String::new();
                entry.read_to_string(&mut content).unwrap();
                archived.insert(entry.path().unwrap().into_owned(), content);
            }
        }
        assert_eq!(archived[Path::new("index.html")], "pages\n");
        assert_eq!(archived[Path::new("assets/app.js")], "app\n");
        assert_eq!(archived[Path::new("hard-linked.html")], "pages\n");
        #[cfg(unix)]
        assert_eq!(archived[Path::new("linked.html")], "pages\n");
        assert!(!archived.contains_key(Path::new(".github/workflow.yml")));
        assert!(!archived.contains_key(Path::new(".well-known/security.txt")));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn create_pages_archive_streams_large_directory() {
        const FILE_COUNT: usize = 1_024;

        let temp = temp_dir();
        let site = temp.join("site");
        let archive_path = temp.join("artifact.tar");
        fs::create_dir_all(&site).unwrap();
        for index in 0..FILE_COUNT {
            fs::write(site.join(format!("page-{index:04}.html")), "page\n").unwrap();
        }

        create_pages_archive(&site, &temp, Path::new("artifact.tar")).unwrap();

        let file = fs::File::open(&archive_path).unwrap();
        let mut archive = tar::Archive::new(file);
        let archived_files = archive
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.header().entry_type().is_file())
            .count();
        assert_eq!(archived_files, FILE_COUNT);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn pages_archive_staging_files_are_unique_siblings() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let parent = crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
            &temp,
            Path::new(""),
        )
        .unwrap();

        let (first_file, first_path) = create_pages_archive_staging_file(&temp, &parent).unwrap();
        let (second_file, second_path) = create_pages_archive_staging_file(&temp, &parent).unwrap();

        assert_ne!(first_path.name, second_path.name);
        drop(first_file);
        drop(second_file);
        drop(first_path);
        drop(second_path);
        fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn create_pages_archive_rejects_destination_symlink() {
        use std::os::unix::fs::symlink;

        let temp = temp_dir();
        let site = temp.join("site");
        let archive_path = temp.join("artifact.tar");
        let outside = temp.join("outside.tar");
        fs::create_dir_all(&site).unwrap();
        fs::write(site.join("index.html"), "pages\n").unwrap();
        fs::write(&outside, "outside\n").unwrap();
        symlink(&outside, &archive_path).unwrap();

        let error = create_pages_archive(&site, &temp, Path::new("artifact.tar")).unwrap_err();

        assert!(format!("{error:#}").contains("destination is a symlink"));
        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside\n");
        assert!(fs::symlink_metadata(&archive_path)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn create_pages_archive_failure_does_not_publish_partial_archive() {
        use std::os::unix::fs::symlink;

        let temp = temp_dir();
        let site = temp.join("site");
        let archive_path = temp.join("artifact.tar");
        fs::create_dir_all(&site).unwrap();
        fs::write(site.join("a.html"), "written before failure\n").unwrap();
        symlink(".", site.join("z-cycle")).unwrap();
        fs::write(&archive_path, "previous archive\n").unwrap();

        let error = create_pages_archive(&site, &temp, Path::new("artifact.tar")).unwrap_err();

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("symlink"),
            "unexpected Pages error: {rendered}"
        );
        assert_eq!(
            fs::read_to_string(&archive_path).unwrap(),
            "previous archive\n"
        );
        let staging_files = fs::read_dir(&temp)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(".velnor-pages-archive-"))
            .collect::<Vec<_>>();
        assert!(staging_files.is_empty(), "{staging_files:?}");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn create_pages_archive_orders_sources_deterministically() {
        let temp = temp_dir();
        let site = temp.join("site");
        let archive_path = temp.join("artifact.tar");
        fs::create_dir_all(site.join("z-dir")).unwrap();
        fs::create_dir_all(site.join("a-dir")).unwrap();
        fs::write(site.join("z-last.html"), "z\n").unwrap();
        fs::write(site.join("middle.html"), "middle\n").unwrap();
        fs::write(site.join("a-dir/z.html"), "z\n").unwrap();
        fs::write(site.join("a-dir/a.html"), "a\n").unwrap();
        fs::write(site.join("z-dir/a.html"), "a\n").unwrap();

        create_pages_archive(&site, &temp, Path::new("artifact.tar")).unwrap();

        let file = fs::File::open(&archive_path).unwrap();
        let mut archive = tar::Archive::new(file);
        let paths = archive
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.header().entry_type().is_file())
            .map(|entry| entry.path().unwrap().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                "a-dir/a.html",
                "a-dir/z.html",
                "middle.html",
                "z-dir/a.html",
                "z-last.html",
            ]
            .map(PathBuf::from)
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_upload_artifact_requires_overwrite_for_duplicate_name() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        fs::write(temp.join("work/first.txt"), "first\n").unwrap();
        fs::write(temp.join("work/second.txt"), "second\n").unwrap();
        let upload = |path: &str, overwrite: Option<&str>| {
            let mut inputs: BTreeMap<String, String> = [
                ("name".into(), "duplicate".into()),
                ("path".into(), path.into()),
                ("if-no-files-found".into(), "error".into()),
            ]
            .into();
            if let Some(value) = overwrite {
                inputs.insert("overwrite".into(), value.into());
            }
            vec![ExecutableStep::Native {
                step_id: format!("upload-{}", path.replace('.', "-")),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::UploadArtifact,
                    cache_kind: None,
                    source_path: None,
                    inputs,
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }]
        };

        let first_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &upload("first.txt", None), &[], &temp)
            .unwrap();
        let duplicate_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &upload("second.txt", None), &[], &temp)
            .unwrap();

        assert_eq!(first_results[0].exit_code, 0);
        // Velnor always overwrites on duplicate (re-runs on same slot reuse artifact store).
        assert_eq!(duplicate_results[0].exit_code, 0);
        assert_eq!(
            fs::read_to_string(temp.join("_velnor_artifacts/local-1/duplicate/second.txt"))
                .unwrap(),
            "second\n"
        );
        let overwrite_results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&temp),
                &upload("second.txt", Some("true")),
                &[],
                &temp,
            )
            .unwrap();
        assert_eq!(overwrite_results[0].exit_code, 0);
        assert!(!temp
            .join("_velnor_artifacts/local-1/duplicate/first.txt")
            .exists());
        assert_eq!(
            fs::read_to_string(temp.join("_velnor_artifacts/local-1/duplicate/second.txt"))
                .unwrap(),
            "second\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_upload_artifact_maps_container_tmp_to_host_temp() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("jackin-construct-digests")).unwrap();
        fs::write(
            temp.join("jackin-construct-digests/amd64.digest"),
            "sha256:abc\n",
        )
        .unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::UploadArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("name".into(), "construct-digest-amd64".into()),
                    (
                        "path".into(),
                        "/tmp/jackin-construct-digests/amd64.digest".into(),
                    ),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            fs::read_to_string(
                temp.join("_velnor_artifacts/local-1/construct-digest-amd64/amd64.digest")
            )
            .unwrap(),
            "sha256:abc\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_upload_artifact_fails_when_results_service_upload_fails() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        fs::write(temp.join("work/result.json"), "{\"ok\":true}\n").unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::UploadArtifact,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("name".into(), "result-velnor-app".into()),
                    ("path".into(), "result.json".into()),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let runtime_env = [
            ("ACTIONS_RESULTS_URL".into(), "http://127.0.0.1:9".into()),
            (
                "ACTIONS_RUNTIME_TOKEN".into(),
                runtime_token_with_results_scope("plan-1", "job-1"),
            ),
        ];

        let results = DockerJobEngine::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &steps, &runtime_env, &temp)
            .unwrap();

        assert_eq!(results[0].exit_code, 1);
        assert!(results[0]
            .stdout
            .contains("Saved local artifact 'result-velnor-app'"));
        assert!(results[0]
            .stderr
            .contains("Results Service artifact upload failed"));
        assert_eq!(
            fs::read_to_string(
                temp.join("_velnor_artifacts/local-1/result-velnor-app/result.json")
            )
            .unwrap(),
            "{\"ok\":true}\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_artifact_ids_are_stable_per_run_and_name() {
        let state = JobExecutionState::new(&[
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
        ]);

        let first = artifact_id_for_name(&state, "construct-digest-linux-amd64");
        let repeat = artifact_id_for_name(&state, "construct-digest-linux-amd64");
        let second = artifact_id_for_name(&state, "construct-digest-linux-arm64");

        assert_eq!(first, repeat);
        assert_ne!(first, second);
        assert!(first.parse::<u64>().is_ok());
    }

    #[test]
    fn target_paths_filter_receives_event_context_and_outputs_gate_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "paths".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/dorny_paths-filter/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/dorny_paths-filter".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_TOKEN".into(), "${{ github.token }}".into()),
                        ("INPUT_BASE".into(), "main".into()),
                        (
                            "INPUT_FILTERS".into(),
                            "construct:\n  - 'construct/**'\n  - '.github/workflows/construct.yml'"
                                .into(),
                        ),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "consume".into(),
                display_name: String::new(),
                script: "echo construct changed".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w".into(),
                env: Vec::new(),
                condition: Some("steps.paths.outputs.construct == 'true'".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let context_data = vec![(
            "github".into(),
            serde_json::json!({
                "event": {
                    "pull_request": {
                        "number": 42,
                        "base": { "sha": "base-sha" }
                    },
                    "repository": { "default_branch": "main" }
                }
            }),
        )];
        let base_env = vec![
            ("GITHUB_EVENT_NAME".into(), "pull_request".into()),
            ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
            ("GITHUB_SHA".into(), "head-sha".into()),
            ("GITHUB_REF".into(), "refs/pull/42/merge".into()),
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
        ];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps_with_context(
                &container(&temp),
                &steps,
                &base_env,
                &context_data,
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].state.outputs["construct"], "true");
        assert_eq!(results[0].state.outputs["construct_count"], "1");
        assert!(!results[1].skipped);
        let node_call = executor
            .runner()
            .calls
            .iter()
            .find(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .unwrap();
        assert!(!node_call.contains(&"INPUT_TOKEN=ghs_token".into()));
        assert!(node_call.contains(&"--env-file".into()));
        assert!(node_call.contains(&"INPUT_BASE=main".into()));
        assert!(node_call
            .iter()
            .any(|arg| arg.starts_with("INPUT_FILTERS=construct:\n")));
        assert!(node_call.contains(&"GITHUB_EVENT_NAME=pull_request".into()));
        assert!(node_call.contains(&"GITHUB_EVENT_PATH=/github/workflow/event.json".into()));
        assert!(node_call.contains(&"GITHUB_REPOSITORY=jackin-project/jackin".into()));
        assert!(node_call.contains(&"GITHUB_WORKSPACE=/__w".into()));
        assert!(executor.runner().calls.iter().any(|(_, args)| {
            args.first().is_some_and(|arg| arg == "exec")
                && args.iter().any(|arg| arg == "/__t/consume.sh")
        }));
        let event = fs::read_to_string(temp.join("_github_workflow/event.json")).unwrap();
        assert!(event.contains("\"pull_request\""));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_pages_actions_receive_runtime_env_and_outputs() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let outputs = [(
            "artifact_id".to_string(),
            "${{ steps.upload-artifact.outputs.artifact-id }}".to_string(),
        )]
        .into();
        let steps = vec![
            ExecutableStep::CompositeStart {
                step_id: "pages-artifact".into(),
                display_name: "Run actions/upload-pages-artifact@v4".into(),
                inputs: BTreeMap::new(),
                env: Vec::new(),
                condition: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "pages-artifact-1".into(),
                display_name: String::new(),
                script: "tar --directory \"$INPUT_PATH\" -cvf \"$RUNNER_TEMP/artifact.tar\" ."
                    .into(),
                shell: Shell::Sh,
                working_directory_container: "/__w".into(),
                env: vec![("INPUT_PATH".into(), "docs/dist".into())],
                condition: Some("runner.os == 'Linux'".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Native {
                step_id: "upload-artifact".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::UploadArtifact,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("name".into(), "github-pages".into()),
                        ("path".into(), "${{ runner.temp }}/artifact.tar".into()),
                        ("retention-days".into(), "1".into()),
                        ("if-no-files-found".into(), "error".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::CompositeOutputs {
                step_id: "pages-artifact".into(),
                outputs,
                condition: None,
            },
            ExecutableStep::CompositeEnd {
                step_id: "pages-artifact".into(),
            },
            ExecutableStep::Native {
                step_id: "deploy-pages".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::DeployPages,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("token".into(), "${{ github.token }}".into()),
                        ("artifact_name".into(), "github-pages".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                // The full network-backed deployment is covered by
                // deploy_pages_runs_artifact_oidc_create_and_status_loop.
                condition: Some("false".into()),
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];
        let runtime_env = vec![
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_NUMBER".into(), "12".into()),
            ("GITHUB_SHA".into(), "abc123".into()),
            ("GITHUB_ACTOR".into(), "donbeave".into()),
            ("GITHUB_ACTION".into(), "deploy-pages".into()),
            ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
            ("GITHUB_API_URL".into(), "https://api.github.com".into()),
            ("GITHUB_RETENTION_DAYS".into(), "90".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
            ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
            (
                "ACTIONS_RESULTS_URL".into(),
                "https://results.actions".into(),
            ),
            (
                "ACTIONS_ID_TOKEN_REQUEST_URL".into(),
                "https://oidc.actions/token".into(),
            ),
            (
                "ACTIONS_ID_TOKEN_REQUEST_TOKEN".into(),
                "id-token-request-token".into(),
            ),
        ];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });
        fs::write(temp.join("artifact.tar"), "pages archive\n").unwrap();

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &runtime_env, &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        let expected_artifact_id =
            artifact_id_for_name(&JobExecutionState::new(&runtime_env), "github-pages");
        assert_eq!(
            results[2].state.outputs["artifact_id"],
            expected_artifact_id
        );
        assert!(results[3].skipped);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 0);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_docker_javascript_actions_receive_socket_and_cli_mounts() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "buildx".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_setup-buildx-action/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_setup-buildx-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![("INPUT_INSTALL".into(), "true".into())],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::JavaScript {
                step_id: "login".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_login-action/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_login-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_USERNAME".into(), "docker-user".into()),
                        ("INPUT_PASSWORD".into(), "docker-token".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::JavaScript {
                step_id: "metadata".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_metadata-action/dist/index.cjs"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_metadata-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_IMAGES".into(), "ghcr.io/chainargos/app".into()),
                        ("INPUT_TAGS".into(), "type=sha".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::JavaScript {
                step_id: "build-push".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_build-push-action/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_build-push-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_CONTEXT".into(), ".".into()),
                        ("INPUT_PUSH".into(), "false".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::JavaScript {
                step_id: "bake".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_bake-action/dist/index.cjs".into(),
                    post_container_path: Some(
                        "/__a/_actions/docker_bake-action/dist/index.cjs".into(),
                    ),
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_bake-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_FILES".into(), "docker-bake.hcl".into()),
                        ("INPUT_TARGETS".into(), "app".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];
        let mut spec = container(&temp);
        spec.mount_docker_socket = true;
        spec.docker_cli_host_path = Some("/usr/bin/docker".into());
        spec.docker_cli_plugin_host_dir = Some("/usr/libexec/docker/cli-plugins".into());
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(
                &spec,
                &steps,
                &[
                    (
                        "GITHUB_REPOSITORY".into(),
                        "ChainArgos/java-monorepo".into(),
                    ),
                    ("GITHUB_TOKEN".into(), "ghs_token".into()),
                    ("RUNNER_TEMP".into(), "/__t".into()),
                ],
                &temp,
            )
            .unwrap();

        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 6);
        for call in &node_calls {
            assert!(
                call.iter().any(|arg| {
                    arg.contains("vdl-")
                        && arg.contains(".sock:/var/run/docker.sock")
                        && !arg.starts_with("/tmp/vdl-")
                        && !arg.starts_with("/var/run/docker.sock:")
                }),
                "guest Docker must use the job lease socket, got {call:?}"
            );
            assert!(call.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
            assert!(call.contains(
                &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
            ));
            assert!(!call.contains(&"GITHUB_TOKEN=ghs_token".into()));
            assert!(call.contains(&"--env-file".into()));
            assert!(call.contains(&"GITHUB_REPOSITORY=ChainArgos/java-monorepo".into()));
            assert!(call.contains(&"RUNNER_TEMP=/__t".into()));
        }
        assert!(node_calls[0].contains(&"INPUT_INSTALL=true".into()));
        assert!(node_calls[1].contains(&"INPUT_USERNAME=docker-user".into()));
        assert!(node_calls[2].contains(&"INPUT_IMAGES=ghcr.io/chainargos/app".into()));
        assert!(node_calls[3].contains(&"INPUT_CONTEXT=.".into()));
        assert!(node_calls[4].contains(&"INPUT_FILES=docker-bake.hcl".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_setup_buildx_reuses_existing_builder() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "buildx".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::DockerSetupBuildx,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("name".into(), "jackin-construct".into()),
                    ("driver".into(), "docker-container".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let builder = format!(
            "jackin-construct-{}",
            sanitize_artifact_name(temp.file_name().unwrap().to_str().unwrap())
        );
        assert_eq!(results[0].exit_code, 0);
        assert_eq!(results[0].state.outputs["name"], builder);
        assert_eq!(results[0].state.env["BUILDX_BUILDER"], builder);
        let calls = docker_call_strings(&executor.runner().calls);
        let inspect_call = calls
            .iter()
            .position(|c| c.contains(&format!("'buildx' 'inspect' '{builder}'")))
            .unwrap();
        let use_call = calls
            .iter()
            .position(|c| c.contains(&format!("'buildx' 'use' '{builder}'")))
            .unwrap();
        assert!(inspect_call < use_call);
        assert!(
            calls[inspect_call].starts_with("exec "),
            "buildx state lives in the job container: {}",
            calls[inspect_call]
        );
        assert!(!calls.iter().any(|c| c.contains("'buildx' 'create'")));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_setup_buildx_honors_cleanup_controls() {
        for (cleanup, keep_state, expected_rm) in [
            ("false", "false", None),
            ("true", "false", Some("docker buildx rm builder")),
            (
                "true",
                "true",
                Some("docker buildx rm --keep-state builder"),
            ),
        ] {
            let temp = temp_dir();
            fs::create_dir_all(&temp).unwrap();
            let steps = vec![ExecutableStep::Native {
                step_id: "buildx".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::DockerSetupBuildx,
                    cache_kind: None,
                    source_path: None,
                    inputs: [
                        ("name".into(), "builder".into()),
                        ("cleanup".into(), cleanup.into()),
                        ("keep-state".into(), keep_state.into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }];
            let mut executor = DockerJobEngine::new(RecordingRunner::default());

            executor
                .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
                .unwrap();

            let calls = executor
                .runner()
                .calls
                .iter()
                .map(|(program, args)| format!("{program} {}", args.join(" ")))
                .collect::<Vec<_>>();
            match expected_rm {
                Some(expected) => assert!(calls.iter().any(|call| call.contains(expected))),
                None => assert!(!calls.iter().any(|call| call.contains("buildx rm"))),
            }
            fs::remove_dir_all(temp).unwrap();
        }
    }

    #[test]
    fn target_docker_action_inputs_match_current_workflows() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "buildx-reusable".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_setup-buildx-action/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_setup-buildx-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        (
                            "INPUT_NAME".into(),
                            "chainargos-${{ inputs.app }}-builder".into(),
                        ),
                        ("INPUT_DRIVER".into(), "docker-container".into()),
                        ("INPUT_CLEANUP".into(), "false".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::JavaScript {
                step_id: "buildx-jackin".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_setup-buildx-action/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_setup-buildx-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_NAME".into(), "${{ env.BUILDX_BUILDER }}".into()),
                        ("INPUT_DRIVER".into(), "docker-container".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::JavaScript {
                step_id: "metadata".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_metadata-action/dist/index.cjs"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_metadata-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_IMAGES".into(), "${{ inputs.image }}".into()),
                        (
                            "INPUT_TAGS".into(),
                            "type=raw,value=latest,enable=${{ inputs.publish }}\n\
type=sha,format=long,prefix=,enable=${{ inputs.publish }}\n\
type=raw,value=pr-${{ github.event.pull_request.number }},enable=${{ !inputs.publish && github.event_name == 'pull_request' }}"
                                .into(),
                        ),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::CompositeOutputs {
                step_id: "meta".into(),
                outputs: BTreeMap::from([
                    (
                        "tags".into(),
                        "chainargos/rust-bitcoin-processor:pr-42".into(),
                    ),
                    (
                        "labels".into(),
                        "org.opencontainers.image.source=https://github.com/ChainArgos/java-monorepo"
                            .into(),
                    ),
                ]),
                condition: None,
            },
            ExecutableStep::CompositeOutputs {
                step_id: "cache".into(),
                outputs: BTreeMap::from([
                    ("from".into(), "type=gha,scope=bitcoin-processor-app-pr".into()),
                    (
                        "to".into(),
                        "type=gha,scope=bitcoin-processor-app-pr,mode=max".into(),
                    ),
                ]),
                condition: None,
            },
            ExecutableStep::JavaScript {
                step_id: "build-push".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_build-push-action/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_build-push-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_CONTEXT".into(), ".".into()),
                        (
                            "INPUT_FILE".into(),
                            "backend-rust/${{ inputs.app }}/Dockerfile".into(),
                        ),
                        ("INPUT_PLATFORMS".into(), "linux/amd64".into()),
                        ("INPUT_PUSH".into(), "${{ inputs.publish }}".into()),
                        ("INPUT_TAGS".into(), "${{ steps.meta.outputs.tags }}".into()),
                        ("INPUT_LABELS".into(), "${{ steps.meta.outputs.labels }}".into()),
                        ("INPUT_CACHE-FROM".into(), "${{ steps.cache.outputs.from }}".into()),
                        ("INPUT_CACHE-TO".into(), "${{ steps.cache.outputs.to }}".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::JavaScript {
                step_id: "bake".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_bake-action/dist/index.cjs".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_bake-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_FILES".into(), "backend-rust/docker-bake.hcl".into()),
                        ("INPUT_TARGETS".into(), "${{ needs.changes.outputs.bake-targets }}".into()),
                        (
                            "INPUT_SET".into(),
                            "*.cache-from=type=gha,scope=rust-workspace\n\
*.cache-to=type=gha,scope=rust-workspace,mode=max\n\
bitcoin-processor-app.push=${{ (github.event_name == 'push' && needs.changes.outputs.bitcoin-processor == 'true') || (github.event_name == 'workflow_dispatch' && inputs.push) }}"
                                .into(),
                        ),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];
        let mut spec = container(&temp);
        spec.mount_docker_socket = true;
        spec.docker_cli_host_path = Some("/usr/bin/docker".into());
        spec.docker_cli_plugin_host_dir = Some("/usr/libexec/docker/cli-plugins".into());
        let context = vec![
            (
                "inputs".into(),
                serde_json::json!({
                    "app": "bitcoin-processor-app",
                    "image": "chainargos/rust-bitcoin-processor",
                    "publish": false,
                    "push": true
                }),
            ),
            (
                "github".into(),
                serde_json::json!({ "event": { "pull_request": { "number": 42 } } }),
            ),
            (
                "needs".into(),
                serde_json::json!({
                    "changes": {
                        "outputs": {
                            "bake-targets": "bitcoin-processor-app",
                            "bitcoin-processor": "true"
                        }
                    }
                }),
            ),
        ];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps_with_context(
                &spec,
                &steps,
                &[
                    ("GITHUB_EVENT_NAME".into(), "workflow_dispatch".into()),
                    (
                        "GITHUB_REPOSITORY".into(),
                        "ChainArgos/java-monorepo".into(),
                    ),
                    ("BUILDX_BUILDER".into(), "jackin-construct".into()),
                    ("RUNNER_TEMP".into(), "/__t".into()),
                ],
                &context,
                &temp,
            )
            .unwrap();

        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 5);
        for call in &node_calls {
            assert!(
                call.iter().any(|arg| {
                    arg.contains("vdl-")
                        && arg.contains(".sock:/var/run/docker.sock")
                        && !arg.starts_with("/tmp/vdl-")
                        && !arg.starts_with("/var/run/docker.sock:")
                }),
                "guest Docker must use the job lease socket, got {call:?}"
            );
            assert!(call.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
            assert!(call.contains(
                &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
            ));
        }
        assert!(
            node_calls[0].contains(&"INPUT_NAME=chainargos-bitcoin-processor-app-builder".into())
        );
        assert!(node_calls[0].contains(&"INPUT_DRIVER=docker-container".into()));
        assert!(node_calls[0].contains(&"INPUT_CLEANUP=false".into()));
        assert!(node_calls[1].contains(&"INPUT_NAME=jackin-construct".into()));
        assert!(node_calls[2].contains(&"INPUT_IMAGES=chainargos/rust-bitcoin-processor".into()));
        assert!(node_calls[2].contains(
            &("INPUT_TAGS=type=raw,value=latest,enable=false\n\
type=sha,format=long,prefix=,enable=false\n\
type=raw,value=pr-42,enable=false")
                .into()
        ));
        assert!(node_calls[3]
            .contains(&"INPUT_FILE=backend-rust/bitcoin-processor-app/Dockerfile".into()));
        assert!(node_calls[3].contains(&"INPUT_PUSH=false".into()));
        assert!(
            node_calls[3].contains(&"INPUT_TAGS=chainargos/rust-bitcoin-processor:pr-42".into())
        );
        assert!(node_calls[3].contains(
            &"INPUT_LABELS=org.opencontainers.image.source=https://github.com/ChainArgos/java-monorepo".into()
        ));
        assert!(node_calls[3]
            .contains(&"INPUT_CACHE-FROM=type=gha,scope=bitcoin-processor-app-pr".into()));
        assert!(node_calls[3]
            .contains(&"INPUT_CACHE-TO=type=gha,scope=bitcoin-processor-app-pr,mode=max".into()));
        assert!(node_calls[4].contains(&"INPUT_FILES=backend-rust/docker-bake.hcl".into()));
        assert!(node_calls[4].contains(&"INPUT_TARGETS=bitcoin-processor-app".into()));
        assert!(node_calls[4].contains(
            &("INPUT_SET=*.cache-from=type=gha,scope=rust-workspace\n\
*.cache-to=type=gha,scope=rust-workspace,mode=max\n\
bitcoin-processor-app.push=true")
                .into()
        ));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_renovate_action_receives_docker_cli_socket_and_env() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "renovate".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::Renovate,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("token".into(), "${{ secrets.RENOVATE_TOKEN }}".into()),
                    ("renovate-version".into(), "43".into()),
                ]
                .into(),
                env: vec![
                    (
                        "RENOVATE_REPOSITORIES".into(),
                        "${{ github.repository }}".into(),
                    ),
                    ("RENOVATE_ONBOARDING".into(), "false".into()),
                    ("LOG_LEVEL".into(), "debug".into()),
                ],
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut spec = container(&temp);
        spec.mount_docker_socket = true;
        spec.docker_cli_host_path = Some("/usr/bin/docker".into());
        let context = vec![(
            "secrets".into(),
            serde_json::json!({ "RENOVATE_TOKEN": "renovate-token" }),
        )];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps_with_context(
                &spec,
                &steps,
                &[(
                    "GITHUB_REPOSITORY".into(),
                    "ChainArgos/java-monorepo".into(),
                )],
                &context,
                &temp,
            )
            .unwrap();

        let node_call = executor
            .runner()
            .calls
            .iter()
            .find(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"ghcr.io/renovatebot/renovate:43".into())
            })
            .map(|(_, args)| args)
            .unwrap();
        assert!(
            node_call.iter().any(|arg| {
                arg.contains("vdl-")
                    && arg.contains(".sock:/var/run/docker.sock")
                    && !arg.starts_with("/tmp/vdl-")
                    && !arg.starts_with("/var/run/docker.sock:")
            }),
            "guest Docker must use the job lease socket, got {node_call:?}"
        );
        assert!(node_call.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(!node_call.contains(&"INPUT_TOKEN=renovate-token".into()));
        assert!(!node_call.contains(&"RENOVATE_TOKEN=renovate-token".into()));
        assert!(node_call.contains(&"--env-file".into()));
        assert!(node_call.contains(&"RENOVATE_REPOSITORIES=ChainArgos/java-monorepo".into()));
        assert!(node_call.contains(&"RENOVATE_ONBOARDING=false".into()));
        assert!(node_call.contains(&"LOG_LEVEL=debug".into()));
        assert!(node_call.contains(&"GITHUB_REPOSITORY=ChainArgos/java-monorepo".into()));
        assert!(node_call.ends_with(&["ghcr.io/renovatebot/renovate:43".into()]));
        assert_eq!(
            executor
                .runner()
                .calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn javascript_actions_receive_prior_github_path_entries() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "pather".into(),
                display_name: String::new(),
                script: "echo /root/.cargo/bin >> $GITHUB_PATH".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::JavaScript {
                step_id: "cargo-install".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/cargo-install/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/cargo-install".into(),
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let node_call = executor
            .runner()
            .calls
            .iter()
            .find(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .map(|(_, args)| args)
            .unwrap();
        assert!(node_call.contains(
            &"PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
                .into()
        ));
        assert!(node_call.ends_with(&[
            "node:20-bookworm".into(),
            "/__a/_actions/cargo-install/dist/index.js".into()
        ]));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_setup_actions_share_home_toolcache_and_path() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "mise".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/jdx_mise-action/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/jdx_mise-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_INSTALL".into(), "false".into()),
                        ("INPUT_CACHE".into(), "false".into()),
                        ("INPUT_GITHUB_TOKEN".into(), "${{ github.token }}".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::JavaScript {
                step_id: "setup-python".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/actions_setup-python/dist/setup/index.js"
                        .into(),
                    post_container_path: Some(
                        "/__a/_actions/actions_setup-python/dist/cache-save/index.js".into(),
                    ),
                    post_condition: Some("success()".into()),
                    action_container_path: "/__a/_actions/actions_setup-python".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_PYTHON-VERSION".into(), "3.13".into()),
                        (
                            "INPUT_TOKEN".into(),
                            "${{ github.server_url == 'https://github.com' && github.token || '' }}"
                                .into(),
                        ),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];
        let base_env = vec![
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
            (
                "GITHUB_REPOSITORY".into(),
                "ChainArgos/java-monorepo".into(),
            ),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
        ];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        executor
            .execute_ordered_steps(&container(&temp), &steps, &base_env, &temp)
            .unwrap();

        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 3);
        for call in &node_calls {
            assert!(call.contains(&"HOME=/github/home".into()));
            assert!(call.contains(&"RUNNER_TOOL_CACHE=/__tool".into()));
            assert!(call.contains(&"AGENT_TOOLSDIRECTORY=/__tool".into()));
            assert!(call.contains(&"GITHUB_REPOSITORY=ChainArgos/java-monorepo".into()));
            assert!(call.contains(&"GITHUB_WORKSPACE=/__w".into()));
            assert!(call.contains(&"RUNNER_TEMP=/__t".into()));
        }
        assert!(!node_calls[0].contains(&"INPUT_GITHUB_TOKEN=ghs_token".into()));
        assert!(node_calls[0].contains(&"--env-file".into()));
        assert!(node_calls[0].contains(&"GITHUB_PATH=/github/file_commands/mise_path".into()));
        assert!(node_calls[1].contains(&"INPUT_PYTHON-VERSION=3.13".into()));
        assert!(!node_calls[1].contains(&"INPUT_TOKEN=ghs_token".into()));
        assert!(node_calls[1].contains(&"--env-file".into()));
        assert!(
            node_calls[1].contains(&"GITHUB_PATH=/github/file_commands/setup-python_path".into())
        );
        assert!(node_calls[1].contains(
            &"PATH=/opt/mise/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
                .into()
        ));
        assert!(node_calls[2].contains(
            &"PATH=/opt/mise/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
                .into()
        ));
        assert!(node_calls[2].ends_with(&[
            "node:24-bookworm".into(),
            "/__a/_actions/actions_setup-python/dist/cache-save/index.js".into()
        ]));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_rust_tool_installers_share_cargo_home_path_and_cache_env() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "toolchain".into(),
                display_name: String::new(),
                script: "echo CARGO_HOME=/github/home/.cargo >> \"$GITHUB_ENV\"\necho /root/.cargo/bin >> \"$GITHUB_PATH\"".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/kestra-docker-containers".into(),
                env: Vec::new(),
                condition: Some("runner.os != 'Windows'".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "setup-mold".into(),
                display_name: String::new(),
                script: "echo mold 2.41.0".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/kestra-docker-containers".into(),
                env: Vec::new(),
                condition: Some("runner.os == 'Linux'".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::JavaScript {
                step_id: "setup-just".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/extractions_setup-crate/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/extractions_setup-crate".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_REPO".into(), "casey/just".into()),
                        ("INPUT_GITHUB-TOKEN".into(), "${{ github.token }}".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::JavaScript {
                step_id: "cargo-install".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/baptiste0928_cargo-install/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/baptiste0928_cargo-install".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        ("INPUT_CRATE".into(), "cargo-binstall".into()),
                        ("INPUT_VERSION".into(), "latest".into()),
                        ("INPUT_LOCKED".into(), "true".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];
        let runtime_env = vec![
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            (
                "GITHUB_REPOSITORY".into(),
                "ChainArgos/java-monorepo".into(),
            ),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
            ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
            ("ACTIONS_CACHE_URL".into(), "https://cache.actions".into()),
            ("ACTIONS_CACHE_SERVICE_V2".into(), "True".into()),
            ("RUNNER_OS".into(), "Linux".into()),
        ];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &runtime_env, &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        assert!(!results[0].skipped);
        assert!(!results[1].skipped);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        for call in &node_calls {
            assert!(call.contains(&"HOME=/github/home".into()));
            assert!(call.contains(&"CARGO_HOME=/github/home/.cargo".into()));
            assert!(call.contains(&"GITHUB_REPOSITORY=ChainArgos/java-monorepo".into()));
            assert!(call.contains(&"GITHUB_WORKSPACE=/__w".into()));
            assert!(call.contains(&"RUNNER_TEMP=/__t".into()));
            assert!(call.contains(&"PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()));
        }
        assert!(node_calls[0].contains(&"INPUT_REPO=casey/just".into()));
        assert!(node_calls[0].contains(&"INPUT_GITHUB-TOKEN=ghs_token".into()));
        assert!(node_calls[1].contains(&"INPUT_CRATE=cargo-binstall".into()));
        assert!(node_calls[1].contains(&"INPUT_VERSION=latest".into()));
        assert!(node_calls[1].contains(&"INPUT_LOCKED=true".into()));
        assert!(!node_calls[1].contains(&"ACTIONS_RUNTIME_TOKEN=runtime-token".into()));
        assert!(node_calls[1].contains(&"--env-file".into()));
        assert!(node_calls[1].contains(&"ACTIONS_CACHE_URL=https://cache.actions".into()));
        assert!(node_calls[1].contains(&"ACTIONS_CACHE_SERVICE_V2=True".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn executes_javascript_post_actions_in_reverse_order() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "cache".into(),
                display_name: "Run actions/cache@v4".into(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/cache/dist/restore.js".into(),
                    post_container_path: Some("/__a/_actions/cache/dist/save.js".into()),
                    post_condition: None,
                    action_container_path: "/__a/_actions/cache".into(),
                    inputs: BTreeMap::new(),
                    env: vec![("INPUT_KEY".into(), "linux-cache".into())],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::JavaScript {
                step_id: "login".into(),
                display_name: "Run docker/login-action@v3".into(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_login/dist/main.js".into(),
                    post_container_path: Some("/__a/_actions/docker_login/dist/post.js".into()),
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_login".into(),
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        })
        .with_initial_order(1)
        .with_step_log_sender(sender);

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        let exec_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert!(exec_calls[0].ends_with(&[
            "node:20-bookworm".into(),
            "/__a/_actions/cache/dist/restore.js".into()
        ]));
        assert!(exec_calls[1].ends_with(&[
            "node:20-bookworm".into(),
            "/__a/_actions/docker_login/dist/main.js".into()
        ]));
        assert!(exec_calls[2].ends_with(&[
            "node:20-bookworm".into(),
            "/__a/_actions/docker_login/dist/post.js".into()
        ]));
        assert!(exec_calls[3].ends_with(&[
            "node:20-bookworm".into(),
            "/__a/_actions/cache/dist/save.js".into()
        ]));
        assert!(exec_calls[3].contains(&"STATE_primaryKey=linux-cache".into()));

        let mut logs = Vec::new();
        while let Ok(log) = receiver.try_recv() {
            logs.push(log);
        }
        assert_eq!(
            logs.iter()
                .map(|log| (log.order, log.display_name.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (2, "Run actions/cache@v4"),
                (3, "Run docker/login-action@v3"),
                (5, "Post Run docker/login-action@v3"),
                (6, "Post Run actions/cache@v4"),
            ]
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn executes_javascript_pre_action_before_main() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::JavaScript {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: JavaScriptActionInvocation {
                node: "node20".into(),
                pre_container_path: Some("/__a/_actions/cache/dist/pre.js".into()),
                pre_condition: Some("always()".into()),
                main_container_path: "/__a/_actions/cache/dist/restore.js".into(),
                post_container_path: None,
                post_condition: None,
                action_container_path: "/__a/_actions/cache".into(),
                inputs: BTreeMap::new(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert!(node_calls[0].ends_with(&[
            "node:20-bookworm".into(),
            "/__a/_actions/cache/dist/pre.js".into()
        ]));
        assert!(node_calls[1].ends_with(&[
            "node:20-bookworm".into(),
            "/__a/_actions/cache/dist/restore.js".into()
        ]));
        assert!(node_calls[1].contains(&"STATE_primaryKey=linux-cache".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn javascript_action_state_accumulates_across_pre_and_main_for_post() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::JavaScript {
            step_id: "wrapped".into(),
            display_name: String::new(),
            invocation: JavaScriptActionInvocation {
                node: "node20".into(),
                pre_container_path: Some("/__a/_actions/wrapped/dist/pre.js".into()),
                pre_condition: None,
                main_container_path: "/__a/_actions/wrapped/dist/main.js".into(),
                post_container_path: Some("/__a/_actions/wrapped/dist/post.js".into()),
                post_condition: None,
                action_container_path: "/__a/_actions/wrapped".into(),
                inputs: BTreeMap::new(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(PhaseStateRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let post_call = executor
            .runner()
            .calls
            .iter()
            .find(|(_, args)| {
                args.iter()
                    .any(|arg| arg == "/__a/_actions/wrapped/dist/post.js")
            })
            .unwrap();
        assert!(post_call.1.contains(&"STATE_preKey=pre-value".into()));
        assert!(post_call.1.contains(&"STATE_mainKey=main-value".into()));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn javascript_pre_action_registers_post_before_main() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::JavaScript {
            step_id: "wrapped".into(),
            display_name: String::new(),
            invocation: JavaScriptActionInvocation {
                node: "node20".into(),
                pre_container_path: Some("/__a/_actions/wrapped/dist/pre.js".into()),
                pre_condition: None,
                main_container_path: "/__a/_actions/wrapped/dist/main.js".into(),
                post_container_path: Some("/__a/_actions/wrapped/dist/post.js".into()),
                post_condition: None,
                action_container_path: "/__a/_actions/wrapped".into(),
                inputs: BTreeMap::new(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 9, 0, 0],
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].exit_code, 9);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        assert!(node_calls[0].ends_with(&[
            "node:20-bookworm".into(),
            "/__a/_actions/wrapped/dist/pre.js".into()
        ]));
        assert!(node_calls[1].ends_with(&[
            "node:20-bookworm".into(),
            "/__a/_actions/wrapped/dist/post.js".into()
        ]));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn skips_javascript_post_action_when_post_if_is_false() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "cache".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/cache/dist/restore.js".into(),
                    post_container_path: Some("/__a/_actions/cache/dist/save.js".into()),
                    post_condition: Some("success()".into()),
                    action_container_path: "/__a/_actions/cache".into(),
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "fail".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 0, 1, 0, 0],
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[1].exit_code, 1);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .count();
        assert_eq!(node_calls, 1);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn javascript_post_action_inherits_continue_on_error() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::JavaScript {
            step_id: "sccache".into(),
            display_name: String::new(),
            invocation: JavaScriptActionInvocation {
                node: "node20".into(),
                pre_container_path: None,
                pre_condition: None,
                main_container_path: "/__a/_actions/sccache/dist/setup/index.js".into(),
                post_container_path: Some("/__a/_actions/sccache/dist/show_stats/index.js".into()),
                post_condition: None,
                action_container_path: "/__a/_actions/sccache".into(),
                inputs: BTreeMap::new(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: true,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(FailingPostRunner { calls: Vec::new() });

        let results = executor
            .execute_ordered_steps_with_context(&container(&temp), &steps, &[], &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].exit_code, 0);
        assert!(!results[0].failure_ignored);
        assert_eq!(results[1].exit_code, 1);
        assert!(results[1].failure_ignored);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_sccache_action_soft_fails_and_gates_wrapper_step() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "sccache".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path:
                        "/__a/_actions/mozilla-actions_sccache-action/dist/setup/index.js".into(),
                    post_container_path: Some(
                        "/__a/_actions/mozilla-actions_sccache-action/dist/show_stats/index.js"
                            .into(),
                    ),
                    post_condition: None,
                    action_container_path: "/__a/_actions/mozilla-actions_sccache-action".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        (
                            "INPUT_TOKEN".into(),
                            "${{ github.server_url == 'https://github.com' && github.token || '' }}"
                                .into(),
                        ),
                        ("INPUT_DISABLE_ANNOTATIONS".into(), "false".into()),
                    ],
                },
                condition: None,
                continue_on_error: true,
                timeout_minutes: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "enable-sccache".into(),
                display_name: String::new(),
                script: "echo RUSTC_WRAPPER=sccache >> \"$GITHUB_ENV\"".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w".into(),
                env: Vec::new(),
                condition: Some("steps.sccache.outcome == 'success'".into()),
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(FailingPostRunner { calls: Vec::new() });

        let results = executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[
                    ("GITHUB_TOKEN".into(), "ghs_token".into()),
                    ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
                    ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
                    ("RUNNER_TEMP".into(), "/__t".into()),
                ],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].exit_code, 0);
        assert!(!results[1].skipped);
        assert_eq!(results[2].exit_code, 1);
        assert!(results[2].failure_ignored);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        for call in &node_calls {
            assert!(!call.contains(&"INPUT_TOKEN=ghs_token".into()));
            assert!(call.contains(&"--env-file".into()));
            assert!(call.contains(&"INPUT_DISABLE_ANNOTATIONS=false".into()));
            assert!(call.contains(&"GITHUB_REPOSITORY=jackin-project/jackin".into()));
            assert!(call.contains(&"RUNNER_TEMP=/__t".into()));
        }
        assert!(node_calls[0].ends_with(&[
            "node:24-bookworm".into(),
            "/__a/_actions/mozilla-actions_sccache-action/dist/setup/index.js".into()
        ]));
        assert!(node_calls[1].ends_with(&[
            "node:24-bookworm".into(),
            "/__a/_actions/mozilla-actions_sccache-action/dist/show_stats/index.js".into()
        ]));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn javascript_post_if_reads_env_after_failure() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "rust-cache".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/rust-cache/dist/restore/index.js".into(),
                    post_container_path: Some("/__a/_actions/rust-cache/dist/save/index.js".into()),
                    post_condition: Some("success() || env.CACHE_ON_FAILURE == 'true'".into()),
                    action_container_path: "/__a/_actions/rust-cache".into(),
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "enable".into(),
                display_name: String::new(),
                script: "echo CACHE_ON_FAILURE=true >> $GITHUB_ENV".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "fail".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let mut executor = DockerJobEngine::new(EnvAndFailureRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        assert_eq!(results[2].exit_code, 1);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        assert!(node_calls[1].ends_with(&[
            "node:20-bookworm".into(),
            "/__a/_actions/rust-cache/dist/save/index.js".into()
        ]));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_rust_cache_receives_runtime_env_and_posts_on_failure() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "rust-cache".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/Swatinem_rust-cache/dist/restore/index.js"
                        .into(),
                    post_container_path: Some(
                        "/__a/_actions/Swatinem_rust-cache/dist/save/index.js".into(),
                    ),
                    post_condition: Some("success() || env.CACHE_ON_FAILURE == 'true'".into()),
                    action_container_path: "/__a/_actions/Swatinem_rust-cache".into(),
                    inputs: BTreeMap::new(),
                    env: vec![
                        (
                            "INPUT_CACHE-DIRECTORIES".into(),
                            "~/.cache/rust-script".into(),
                        ),
                        ("INPUT_SHARED-KEY".into(), "kestra-rust-build-cache".into()),
                        ("INPUT_CACHE-ON-FAILURE".into(), "true".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "enable".into(),
                display_name: String::new(),
                script: "echo CACHE_ON_FAILURE=true >> $GITHUB_ENV".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/kestra-docker-containers".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "fail".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/kestra-docker-containers".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }),
        ];
        let runtime_env = vec![
            (
                "GITHUB_REPOSITORY".into(),
                "ChainArgos/java-monorepo".into(),
            ),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
            ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
            ("ACTIONS_CACHE_URL".into(), "https://cache.actions".into()),
            ("ACTIONS_CACHE_SERVICE_V2".into(), "True".into()),
        ];
        let mut executor = DockerJobEngine::new(EnvAndFailureRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &runtime_env, &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        assert_eq!(results[2].exit_code, 1);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        for call in &node_calls {
            assert!(call.contains(&"HOME=/github/home".into()));
            assert!(call.contains(&"GITHUB_REPOSITORY=ChainArgos/java-monorepo".into()));
            assert!(call.contains(&"GITHUB_WORKSPACE=/__w".into()));
            assert!(call.contains(&"RUNNER_TEMP=/__t".into()));
            assert!(!call.contains(&"ACTIONS_RUNTIME_TOKEN=runtime-token".into()));
            assert!(call.contains(&"--env-file".into()));
            assert!(call.contains(&"ACTIONS_CACHE_URL=https://cache.actions".into()));
            assert!(call.contains(&"ACTIONS_CACHE_SERVICE_V2=True".into()));
        }
        assert!(node_calls[0].contains(&"INPUT_SHARED-KEY=kestra-rust-build-cache".into()));
        assert!(node_calls[0].contains(&"INPUT_CACHE-ON-FAILURE=true".into()));
        assert!(node_calls[1].contains(&"CACHE_ON_FAILURE=true".into()));
        assert!(node_calls[1].ends_with(&[
            "node:24-bookworm".into(),
            "/__a/_actions/Swatinem_rust-cache/dist/save/index.js".into()
        ]));
        fs::remove_dir_all(temp).unwrap();
    }

    fn target_expression_context() -> Vec<(String, Value)> {
        vec![
            (
                "github".into(),
                serde_json::json!({
                    "repository": "jackin-project/jackin",
                    "event": {
                        "pull_request": { "number": 42 },
                        "workflow_run": {
                            "conclusion": "success",
                            "event": "push",
                            "head_branch": "main",
                            "head_sha": "def456",
                            "head_repository": {
                                "full_name": "jackin-project/jackin"
                            }
                        }
                    }
                }),
            ),
            (
                "inputs".into(),
                serde_json::json!({
                    "app": "bitcoin-processor-app",
                    "image": "docker.io/chainargos/bitcoin-processor-app",
                    "package": "prod",
                    "packages": "bitcoin-processor-app",
                    "publish": false,
                    "push": true,
                    "targets": "bitcoin-processor-app"
                }),
            ),
            (
                "matrix".into(),
                serde_json::json!({
                    "arch": "x86_64",
                    "os": "ubuntu-latest",
                    "platform": "linux-amd64",
                    "runner": "ubuntu-latest",
                    "target": "x86_64-unknown-linux-gnu",
                    "zigbuild": true,
                    "zigbuild_target": "x86_64-unknown-linux-gnu"
                }),
            ),
            (
                "needs".into(),
                serde_json::json!({
                    "changes": {
                        "outputs": {
                            "bake-targets": "bitcoin-processor-app",
                            "bitcoin-processor": "true",
                            "blockchain-explorer": "true",
                            "coingecko-pricing": "true",
                            "construct": "true",
                            "docs": "true",
                            "eth-grpc-server": "true",
                            "eth-processor": "true",
                            "is_publish": "false",
                            "legacy-grpc-server": "true",
                            "rust": "true",
                            "tron-grpc-server": "true",
                            "tron-processor": "true"
                        },
                        "result": "success"
                    },
                    "check": { "result": "success" },
                    "check-version": {
                        "outputs": { "version": "1.2.3" },
                        "result": "success"
                    },
                    "docker-bake": { "result": "success" },
                    "release": {
                        "outputs": {
                            "arm64_linux": "sha",
                            "arm64_macos": "sha",
                            "capsule_arm64_linux": "sha",
                            "capsule_x86_linux": "sha",
                            "x86_linux": "sha",
                            "x86_macos": "sha"
                        },
                        "result": "success"
                    },
                    "source-changed": {
                        "outputs": {
                            "sha": "abc123",
                            "source": "true"
                        },
                        "result": "success"
                    },
                    "test-bitcoin-processor": { "result": "success" },
                    "test-blockchain-explorer": { "result": "success" },
                    "test-coingecko-pricing": { "result": "success" },
                    "test-eth-grpc-server": { "result": "success" },
                    "test-eth-processor": { "result": "success" },
                    "test-legacy-grpc-server": { "result": "success" },
                    "test-tron-grpc-server": { "result": "success" },
                    "test-tron-processor": { "result": "success" }
                }),
            ),
            (
                "secrets".into(),
                serde_json::json!({
                    "DOCKERHUB_TOKEN": "secret",
                    "DOCKERHUB_USERNAME": "user",
                    "GH_READONLY_TOKEN": "secret",
                    "GITHUB_TOKEN": "secret",
                    "HOMEBREW_TAP_TOKEN": "secret",
                    "RENOVATE_TOKEN": "secret"
                }),
            ),
        ]
    }

    fn workflow_files(root: &Path) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(root) else {
            return Vec::new();
        };
        let mut files = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| matches!(extension, "yml" | "yaml"))
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    fn action_metadata_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_action_metadata_files(root, &mut files);
        files.sort();
        files
    }

    fn collect_action_metadata_files(root: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_action_metadata_files(&path, files);
                continue;
            }
            if path
                .file_name()
                .and_then(|file_name| file_name.to_str())
                .is_some_and(|file_name| matches!(file_name, "action.yml" | "action.yaml"))
            {
                files.push(path);
            }
        }
    }

    fn collect_yaml_strings<'a>(value: &'a serde_yaml::Value, strings: &mut Vec<&'a str>) {
        match value {
            serde_yaml::Value::String(value) => strings.push(value),
            serde_yaml::Value::Sequence(sequence) => {
                for value in sequence {
                    collect_yaml_strings(value, strings);
                }
            }
            serde_yaml::Value::Mapping(map) => {
                for (key, value) in map {
                    strings.push(key);
                    collect_yaml_strings(value, strings);
                }
            }
            _ => {}
        }
    }

    fn collect_yaml_key_strings<'a>(
        value: &'a serde_yaml::Value,
        name: &str,
        strings: &mut Vec<&'a str>,
    ) {
        match value {
            serde_yaml::Value::Mapping(map) => {
                for (key, value) in map {
                    if key == name
                        && let Some(value) = value.as_str()
                    {
                        strings.push(value);
                    }
                    collect_yaml_key_strings(value, name, strings);
                }
            }
            serde_yaml::Value::Sequence(sequence) => {
                for value in sequence {
                    collect_yaml_key_strings(value, name, strings);
                }
            }
            _ => {}
        }
    }

    // ── timestamp format ──────────────────────────────────────────────────

    #[test]
    fn unix_now_rfc3339_produces_second_precision_no_subseconds() {
        // Must match YYYY-MM-DDTHH:MM:SSZ — GitHub's frontend timestamp regex
        // requires no sub-second component to render timestamps in the separate
        // timestamp column rather than as plain content text.
        let ts = unix_now_rfc3339();
        assert!(
            !ts.contains('.'),
            "timestamp must not contain sub-seconds: {ts}"
        );
        assert!(ts.ends_with('Z'), "timestamp must end with Z: {ts}");
        assert_eq!(
            ts.len(),
            20,
            "expected YYYY-MM-DDTHH:MM:SSZ (20 chars): {ts}"
        );
    }

    // ── step_log_lines ────────────────────────────────────────────────────

    #[test]
    fn step_log_lines_renders_workflow_commands_in_place() {
        // `::group::`/`::endgroup::` convert to ##[group] markers AT THEIR
        // ORIGINAL POSITION (GitHub keeps user grouping placement); state
        // commands like ::set-output are consumed invisibly.
        let stdout =
            "::group::Build phase\nsome build output\n::endgroup::\n::set-output name=x::42\n";
        let stderr = "";
        let lines = step_log_lines("Run tests", stdout, stderr, false, &[]);

        let group_at = lines
            .iter()
            .position(|l| l == "##[group]Build phase")
            .expect("group marker in place");
        let output_at = lines
            .iter()
            .position(|l| l == "some build output")
            .expect("output present");
        let endgroup_at = lines
            .iter()
            .rposition(|l| l == "##[endgroup]")
            .expect("endgroup marker in place");
        assert!(
            group_at < output_at && output_at < endgroup_at,
            "group markers must wrap the output they grouped: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("::")),
            "raw workflow command lines must not leak: {lines:?}"
        );
    }

    #[test]
    fn step_log_lines_passes_through_normal_output() {
        let stdout = "cargo test passed\nall 42 tests passed\n";
        let stderr = "warning: unused import\n";
        let lines = step_log_lines("Run cargo test", stdout, stderr, false, &[]);
        assert!(lines.iter().any(|l| l.contains("cargo test passed")));
        assert!(lines.iter().any(|l| l.contains("all 42 tests passed")));
        assert!(lines.iter().any(|l| l.contains("warning: unused import")));
        assert!(lines.iter().any(|l| l == "##[group]Run cargo test"));
        // GitHub closes the header group BEFORE the output: output lines stay
        // visible without expanding a group, and no "Finishing:" line exists.
        assert_eq!(lines[1], "##[endgroup]");
        assert!(!lines.iter().any(|l| l.starts_with("Finishing:")));
    }

    #[test]
    fn step_log_lines_includes_github_style_metadata_prelude() {
        let prelude = vec![
            "with:".to_string(),
            "  token: ***".to_string(),
            "env:".to_string(),
            "  RUN_TESTS: true".to_string(),
        ];
        let lines = step_log_lines("Run action", "done\n", "", false, &prelude);
        let joined = lines.join("\n");
        assert!(joined.contains("with:\n  token: ***"));
        assert!(joined.contains("env:\n  RUN_TESTS: true"));
        // Header group closes before output: done is OUTSIDE the group.
        assert!(joined.contains("##[endgroup]\ndone"));
    }

    #[test]
    fn prelude_env_orders_workflow_then_dynamic_then_step() {
        let mut state = JobExecutionState::new(&[]);
        state.workflow_env = vec![
            ("SCCACHE_GHA_ENABLED".into(), "true".into()),
            ("RUSTC_WRAPPER".into(), "sccache".into()),
        ];
        let mut result = StepExecutionResult {
            exit_code: 0,
            state: StepCommandState::default(),
            skipped: false,
            failure_ignored: false,
            stdout: String::new(),
            stderr: String::new(),
        };
        result
            .state
            .env
            .insert("SCCACHE_PATH".into(), "/usr/bin/sccache".into());
        result
            .state
            .env
            .insert("RUSTC_WRAPPER".into(), "sccache2".into());
        state.apply("s1", &result);

        let env = state.prelude_env(&[("STEP_ONLY".into(), "1".into())]);
        let names: Vec<&str> = env.iter().map(|(n, _)| n.as_str()).collect();
        // Workflow env keeps first position even when re-set dynamically;
        // dynamic additions follow in set order; step env last.
        assert_eq!(
            names,
            vec![
                "SCCACHE_GHA_ENABLED",
                "RUSTC_WRAPPER",
                "SCCACHE_PATH",
                "STEP_ONLY"
            ]
        );
        let rustc_wrapper = env
            .iter()
            .find(|(n, _)| n == "RUSTC_WRAPPER")
            .map(|(_, v)| v.as_str());
        assert_eq!(rustc_wrapper, Some("sccache2"));
    }

    #[test]
    fn explicit_bash_shell_runs_and_displays_pipefail_form() {
        // GitHub's explicit `shell: bash` is
        // `bash --noprofile --norc -e -o pipefail {0}` — omitting pipefail
        // masks pipeline failures the hosted lane would catch.
        assert_eq!(
            shell_log_name(Shell::Bash),
            "bash --noprofile --norc -e -o pipefail {0}"
        );
        assert_eq!(shell_log_name(Shell::BashDefault), "bash -e {0}");
        assert_eq!(shell_log_name(Shell::Sh), "sh -e {0}");
    }

    #[test]
    fn action_log_prelude_redacts_secret_like_inputs() {
        let state = JobExecutionState::new(&[]);
        let inputs = BTreeMap::from([
            ("username".to_string(), "chainargos".to_string()),
            ("password".to_string(), "secret-value".to_string()),
            (
                "ssh-key".to_string(),
                "-----BEGIN OPENSSH PRIVATE KEY-----".to_string(),
            ),
        ]);
        let lines = action_log_prelude(&inputs, &[], &state);
        let joined = lines.join("\n");
        assert!(joined.contains("username: chainargos"));
        assert!(joined.contains("password: ***"));
        assert!(joined.contains("ssh-key: ***"));
        assert!(!joined.contains("secret-value"));
        assert!(!joined.contains("BEGIN OPENSSH PRIVATE KEY"));
    }

    #[test]
    fn action_log_prelude_keeps_non_secret_diagnostics_visible() {
        let state = JobExecutionState::new(&[]);
        let inputs = BTreeMap::from([
            (
                "key".to_string(),
                "rust-script-Velnor-Linux-abcdef".to_string(),
            ),
            (
                "shared-key".to_string(),
                "kestra-rust-build-cache-Velnor".to_string(),
            ),
            ("persist-credentials".to_string(), "true".to_string()),
        ]);
        let lines = action_log_prelude(&inputs, &[], &state);
        let joined = lines.join("\n");
        assert!(joined.contains("key: rust-script-Velnor-Linux-abcdef"));
        assert!(joined.contains("shared-key: kestra-rust-build-cache-Velnor"));
        assert!(joined.contains("persist-credentials: true"));
    }

    #[test]
    fn step_log_lines_keeps_summary_out_of_the_log() {
        // GITHUB_STEP_SUMMARY renders in the run Summary tab via its own
        // Results Service upload — GitHub never inlines it into the step log.
        let stdout = "output line\n";
        let lines = step_log_lines("Summarize", stdout, "", false, &[]);
        let joined = lines.join("\n");
        assert!(!joined.contains("Step summary:"));
        assert!(joined.contains("output line"));
    }

    #[test]
    fn step_log_lines_keeps_silent_executed_steps_expandable() {
        let lines = step_log_lines("Silent action", "", "", false, &[]);
        assert_eq!(
            lines,
            vec![
                "##[group]Silent action".to_string(),
                "##[endgroup]".to_string(),
            ]
        );
    }

    #[test]
    fn step_log_lines_marks_skipped_steps_visibly() {
        let lines = step_log_lines("Skipped action", "", "", true, &[]);
        assert_eq!(lines, skipped_step_log_lines());
        assert!(
            lines.iter().any(|line| line.contains("Step skipped:")),
            "a skipped step must carry an explicit marker, got {lines:?}"
        );
    }

    // ── hadolint_script ───────────────────────────────────────────────────

    fn docker_call_strings(calls: &[(String, Vec<String>)]) -> Vec<String> {
        calls
            .iter()
            .filter(|(program, _)| program == "docker")
            .map(|(_, args)| args.join(" "))
            .collect()
    }

    fn hadolint_inputs_defaults() -> HadolintInputs {
        HadolintInputs {
            dockerfile: "Dockerfile".into(),
            config: String::new(),
            recursive: "false".into(),
            output_file: "/dev/stdout".into(),
            no_color: "false".into(),
            no_fail: "false".into(),
            verbose: "false".into(),
            format: "tty".into(),
            failure_threshold: "info".into(),
            override_error: String::new(),
            override_warning: String::new(),
            override_info: String::new(),
            override_style: String::new(),
            ignore: String::new(),
            trusted_registries: String::new(),
        }
    }

    #[test]
    fn hadolint_script_default_invocation() {
        let script = hadolint_script(&hadolint_inputs_defaults());
        assert!(script.contains("export HADOLINT_FAILURE_THRESHOLD='info'"));
        assert!(script.contains("out=$(hadolint -- 'Dockerfile')"));
        assert!(
            !script.contains("HADOLINT_TRUSTED_REGISTRIES"),
            "empty trusted-registries must stay unset: {script}"
        );
        assert!(
            !script.contains("saved to"),
            "default output goes to stdout only"
        );
        assert!(script.ends_with("exit $code\n"));
    }

    #[test]
    fn hadolint_script_maps_inputs() {
        let mut inputs = hadolint_inputs_defaults();
        inputs.dockerfile = "./Dockerfile".into();
        inputs.config = "./.hadolint.yaml".into();
        inputs.ignore = "DL3004".into();
        inputs.failure_threshold = "error".into();
        inputs.recursive = "true".into();
        inputs.trusted_registries = "docker.io".into();
        let script = hadolint_script(&inputs);
        assert!(script.contains("-c './.hadolint.yaml'"));
        assert!(script.contains("export HADOLINT_IGNORE='DL3004'"));
        assert!(script.contains("export HADOLINT_FAILURE_THRESHOLD='error'"));
        assert!(script.contains("export HADOLINT_TRUSTED_REGISTRIES='docker.io'"));
        assert!(script.contains("find . -type f -name './Dockerfile'"));
        assert!(script.contains("-exec hadolint -c './.hadolint.yaml' -- {} +"));
    }

    #[test]
    fn hadolint_script_output_file_writes_and_notes_on_stderr() {
        let mut inputs = hadolint_inputs_defaults();
        inputs.output_file = "hadolint.txt".into();
        let script = hadolint_script(&inputs);
        assert!(script.contains("> 'hadolint.txt'"));
        assert!(script.contains("Hadolint output saved to: "));
        assert!(script.contains(">&2"));
    }

    #[test]
    fn job_output_pairs_parse_broker_template_token_mapping() {
        // Exact wire shape captured live from a velnor job dump
        // (publish / validate-and-config on ChainArgos/jackin-agent-brown):
        // a mapping token with positional metadata and NO top-level "type".
        let job_outputs = serde_json::json!({
            "col": 7, "file": 2, "line": 39,
            "map": [
                {
                    "Key": {"col": 7, "file": 2, "line": 39, "lit": "image", "type": 0},
                    "Value": {"col": 14, "expr": "steps.config.outputs.image", "file": 2, "line": 39, "type": 3}
                },
                {
                    "Key": {"col": 7, "file": 2, "line": 40, "lit": "construct_version", "type": 0},
                    "Value": {"col": 26, "expr": "steps.config.outputs.construct_version", "file": 2, "line": 40, "type": 3}
                }
            ]
        });
        let pairs = job_output_pairs(Some(&job_outputs));
        assert_eq!(
            pairs,
            vec![
                (
                    "image".to_string(),
                    "${{ steps.config.outputs.image }}".to_string()
                ),
                (
                    "construct_version".to_string(),
                    "${{ steps.config.outputs.construct_version }}".to_string()
                ),
            ]
        );
    }

    #[test]
    fn resolve_host_path_maps_container_tmp_to_temp_host() {
        // The job container bind-mounts the host temp dir at /tmp; paths a
        // step writes under /tmp (e.g. publish digest exports) must resolve
        // for native adapters like upload-artifact. Observed live: brown's
        // publish digest upload failed with if-no-files-found: error.
        let state = JobExecutionState::new_with_workspace(
            &[],
            &[],
            Path::new("/host/work"),
            Path::new("/host/temp"),
        );
        assert_eq!(
            resolve_host_path(&state, "/tmp/digests"),
            Some(PathBuf::from("/host/temp/digests"))
        );
        assert_eq!(
            resolve_host_path(&state, "/tmp"),
            Some(PathBuf::from("/host/temp"))
        );
        assert_eq!(
            resolve_host_path(&state, "/__t/x"),
            Some(PathBuf::from("/host/temp/x"))
        );
        assert_eq!(resolve_host_path(&state, "../outside"), None);
        assert_eq!(resolve_host_path(&state, "/__w/../outside"), None);
    }

    #[test]
    fn mapped_paths_reject_absolute_suffixes() {
        let mut state = JobExecutionState::new_with_workspace(
            &[],
            &[],
            Path::new("/host/work"),
            Path::new("/host/temp"),
        );
        state.cargo_target_host = Some(PathBuf::from("/host/target"));

        for path in [
            "target//escape",
            "/__w/target//escape",
            "/github/workspace/target//escape",
            "/__w//escape",
            "/github/workspace//escape",
            "/__t//escape",
            "/github/runner_temp//escape",
            "/tmp//escape",
            "/github/home//escape",
        ] {
            assert_eq!(
                resolve_host_path(&state, path),
                None,
                "mapped path escaped: {path}"
            );
        }
        assert_eq!(resolve_cache_path(&state, "~//escape"), None);

        for path in [
            "target//*.txt",
            "/__w/target//*.txt",
            "/github/workspace/target//*.txt",
            "/__w//*.txt",
            "/github/workspace//*.txt",
            "/__t//*.txt",
            "/github/runner_temp//*.txt",
            "/tmp//*.txt",
        ] {
            assert!(
                artifact_glob_base_and_pattern(&state, path).is_err(),
                "mapped glob accepted absolute suffix: {path}"
            );
        }
    }

    #[test]
    fn docker_build_push_outputs_input_maps_to_buildx_output() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "build".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::DockerBuildPush,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("context".into(), ".".into()),
                    (
                        "outputs".into(),
                        "type=image,push-by-digest=true,name=example/app,push=true".into(),
                    ),
                    ("push".into(), "true".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let calls = docker_call_strings(&executor.runner().calls);
        let invocation = calls
            .iter()
            .find(|c| c.contains("'buildx' 'build'"))
            .expect("buildx build invoked");
        assert!(
            invocation.starts_with("exec "),
            "runs in the job container: {invocation}"
        );
        assert!(invocation.contains("'type=image,push-by-digest=true,name=example/app,push=true'"));
        assert!(invocation.contains("'--output'"));
        assert!(!invocation.contains("'--push'"));
        assert!(invocation.contains("'--metadata-file'"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn docker_build_push_keeps_explicit_dockerfile_workspace_relative() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "build".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                git_ref: String::new(),
                adapter: NativeActionAdapter::DockerBuildPush,
                cache_kind: None,
                source_path: None,
                inputs: [
                    ("context".into(), "docker".into()),
                    ("file".into(), "docker/job-ubuntu.Dockerfile".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }];
        let mut executor = DockerJobEngine::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let calls = docker_call_strings(&executor.runner().calls);
        let invocation = calls
            .iter()
            .find(|call| call.contains("'buildx' 'build'"))
            .expect("buildx build invoked");
        assert!(invocation.contains("'--file' 'docker/job-ubuntu.Dockerfile'"));
        assert!(!invocation.contains("docker/docker/job-ubuntu.Dockerfile"));
        assert!(invocation.contains("'docker'"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn cosign_installer_uses_locked_mise_and_rejects_other_releases() {
        // Requesting the locked version installs via mise, links install-dir,
        // verifies the version, and never downloads over curl.
        let script = cosign_installer_script("v3.1.3", "$HOME/.cosign");
        assert!(script.contains("mise install --locked --yes 'cosign'"));
        assert!(script.contains("MISE_LOCKED=1"));
        assert!(script.contains("WANT='3.1.3'"));
        assert!(script.contains("LOCKED='3.1.3'"));
        assert!(script.contains("DIR=\"$HOME/.cosign\""));
        assert!(script.contains("command -v cosign"));
        assert!(script.contains("__VELNOR_COSIGN_DIR__"));
        assert!(!script.contains("curl"));
        assert!(!script.contains("releases/download"));

        // A different requested release fails closed (no fallback download).
        let mismatch = cosign_installer_script("v3.1.1", "$HOME/.cosign");
        assert!(mismatch.contains("WANT='3.1.1'"));
        assert!(mismatch.contains("refusing a non-mise download"));
        assert!(!mismatch.contains("curl"));
    }

    #[test]
    fn cosign_installer_adapter_is_registered() {
        assert_eq!(
            crate::action::native_action_adapter("sigstore/cosign-installer"),
            Some(NativeActionAdapter::CosignInstaller)
        );
    }

    #[test]
    fn setup_qemu_adapter_is_registered() {
        assert_eq!(
            crate::action::native_action_adapter("docker/setup-qemu-action"),
            Some(NativeActionAdapter::SetupQemu)
        );
    }

    #[test]
    fn setup_qemu_uses_pinned_image() {
        let mut executor = DockerJobEngine::new(RecordingRunner::default());
        let mut inputs = BTreeMap::new();
        inputs.insert(
            "image".to_string(),
            "evil.example/binfmt:latest".to_string(),
        );
        inputs.insert("platforms".to_string(), "arm64".to_string());
        let result = executor
            .native_setup_qemu(
                &NativeActionInvocation {
                    git_ref: String::new(),
                    adapter: NativeActionAdapter::SetupQemu,
                    cache_kind: None,
                    source_path: None,
                    inputs,
                    env: Vec::new(),
                },
                &JobExecutionState::default(),
                DEFAULT_STEP_TIMEOUT,
            )
            .unwrap();

        assert_eq!(result.exit_code, 0);
        let calls = &executor.runner().calls;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "docker");
        assert!(calls[0].1.contains(&"--privileged".to_string()));
        assert!(calls[0].1.contains(&SETUP_QEMU_BINFMT_IMAGE.to_string()));
        assert!(!calls[0]
            .1
            .contains(&"evil.example/binfmt:latest".to_string()));
    }

    #[test]
    fn hadolint_adapter_is_registered() {
        assert_eq!(
            crate::action::native_action_adapter("hadolint/hadolint-action"),
            Some(NativeActionAdapter::Hadolint)
        );
    }

    // ── setup_mold_script ─────────────────────────────────────────────────

    #[test]
    fn setup_mold_script_installs_locked_mise_and_wires_gcc_linker() {
        let script = setup_mold_script();
        // Plan 008 N8: mold is a locked mise install, never apt.
        assert!(script.contains("mise install --locked --yes 'mold'"));
        assert!(script.contains("MISE_LOCKED=1"));
        assert!(!script.contains("apt-get"));
        assert!(script.contains("mold --version | grep -F '2.41.0'"));
        // Cargo-linker wiring is unchanged: gcc + -fuse-ld=mold, no clang.
        assert!(
            !script.contains("linker = \"clang\""),
            "script must not require clang as linker: {script}"
        );
        assert!(
            script.contains("fuse-ld=mold"),
            "script must wire mold via -fuse-ld=mold: {script}"
        );
        assert!(script.contains("x86_64-unknown-linux-gnu"));
        assert!(script.contains("aarch64-unknown-linux-gnu"));
    }

    #[test]
    fn setup_just_script_installs_locked_mise_without_apt_or_cargo() {
        let script = setup_just_script();
        assert!(script.contains("mise install --locked --yes 'just'"));
        assert!(script.contains("MISE_LOCKED=1"));
        assert!(script.contains("just --version | grep -F '1.58.0'"));
        assert!(!script.contains("apt-get"));
        assert!(!script.contains("cargo install"));
    }

    // ── RunServiceStepResult fields ───────────────────────────────────────
    // (Serialization tests for the newly-added fields)

    #[test]
    fn run_service_step_result_serializes_number_field() {
        use crate::protocol::{RunServiceStepResult, TaskResult, TimelineRecordState};
        let result = RunServiceStepResult {
            external_id: Some("abc-uuid".to_string()),
            number: Some(7),
            name: "Run just test".to_string(),
            status: TimelineRecordState::Completed,
            conclusion: TaskResult::Succeeded,
            started_at: Some("2026-06-04T10:00:00Z".to_string()),
            completed_at: Some("2026-06-04T10:00:05Z".to_string()),
            completed_log_lines: 42,
            annotations: vec![],
        };
        let json = serde_json::to_string(&result).unwrap();
        // number must be present in payload (GitHub uses it for /logs/{n} URL).
        assert!(
            json.contains("\"number\":7"),
            "number field missing: {json}"
        );
        // started_at and completed_at must be present.
        assert!(
            json.contains("\"started_at\":\"2026-06-04T10:00:00Z\""),
            "started_at missing: {json}"
        );
        assert!(
            json.contains("\"completed_at\":\"2026-06-04T10:00:05Z\""),
            "completed_at missing: {json}"
        );
        // external_id present.
        assert!(
            json.contains("\"external_id\":\"abc-uuid\""),
            "external_id missing: {json}"
        );
    }

    #[test]
    fn run_service_step_result_omits_none_fields() {
        use crate::protocol::{RunServiceStepResult, TaskResult, TimelineRecordState};
        let result = RunServiceStepResult {
            external_id: None,
            number: None,
            name: "step".to_string(),
            status: TimelineRecordState::Completed,
            conclusion: TaskResult::Succeeded,
            started_at: None,
            completed_at: None,
            completed_log_lines: 0,
            annotations: vec![],
        };
        let json = serde_json::to_string(&result).unwrap();
        // None fields must be omitted (skip_serializing_if = "Option::is_none").
        assert!(
            !json.contains("number"),
            "absent number must be omitted: {json}"
        );
        assert!(
            !json.contains("started_at"),
            "absent started_at must be omitted: {json}"
        );
        assert!(
            !json.contains("completed_at"),
            "absent completed_at must be omitted: {json}"
        );
        assert!(
            !json.contains("external_id"),
            "absent external_id must be omitted: {json}"
        );
    }

    // ── StepLog.started_at ────────────────────────────────────────────────

    #[test]
    fn step_log_with_name_records_started_at() {
        // started_at must flow from emit_step_started through step_log_with_name
        // into StepLog so RunServiceStepResult can include the actual step start time.
        let started = "2026-06-04T10:00:00Z";
        let completed = "2026-06-04T10:00:05Z";
        let result = StepExecutionResult {
            exit_code: 0,
            state: StepCommandState::default(),
            skipped: false,
            failure_ignored: false,
            stdout: "ok\n".to_string(),
            stderr: String::new(),
        };
        let log = step_log_with_name("step1", "Run tests", 3, started, completed, &result, &[]);
        assert_eq!(
            log.started_at, started,
            "started_at must be stored in StepLog"
        );
        assert_eq!(
            log.completed_at, completed,
            "completed_at must be stored in StepLog"
        );
        assert_eq!(log.order, 3);
        assert_eq!(log.display_name, "Run tests");
    }

    // ── host_docker_env ───────────────────────────────────────────────────

    #[test]
    fn host_docker_env_strips_home_so_docker_cli_plugins_are_found() {
        // When HOME is overridden to a container path (e.g. /github/home), Docker
        // looks for CLI plugins (buildx) in /github/home/.docker/cli-plugins which
        // doesn't exist on the host → `docker buildx` treated as unknown command →
        // `unknown flag: --file` error. Stripping HOME lets Docker inherit the host
        // HOME where the plugins are actually installed (~/.docker/cli-plugins).
        let env = vec![
            ("HOME".to_string(), "/github/home".to_string()),
            (
                "ACTIONS_CACHE_URL".to_string(),
                "https://cache.example.com/".to_string(),
            ),
            ("GITHUB_WORKSPACE".to_string(), "/__w".to_string()),
            (
                "ACTIONS_RUNTIME_TOKEN".to_string(),
                "secret-token".to_string(),
            ),
            ("USERPROFILE".to_string(), "C:\\Users\\runner".to_string()),
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ];
        let filtered = host_docker_env(env);
        // HOME and USERPROFILE must be stripped.
        assert!(
            !filtered.iter().any(|(k, _)| k == "HOME"),
            "HOME must be stripped: {filtered:?}"
        );
        assert!(
            !filtered.iter().any(|(k, _)| k == "USERPROFILE"),
            "USERPROFILE must be stripped: {filtered:?}"
        );
        // ACTIONS_* and other vars must be preserved so GHA cache works.
        assert!(
            filtered
                .iter()
                .any(|(k, v)| k == "ACTIONS_CACHE_URL" && v.contains("cache.example")),
            "ACTIONS_CACHE_URL must be preserved: {filtered:?}"
        );
        assert!(
            filtered.iter().any(|(k, _)| k == "GITHUB_WORKSPACE"),
            "GITHUB_WORKSPACE must be preserved: {filtered:?}"
        );
        assert!(
            filtered.iter().any(|(k, _)| k == "PATH"),
            "PATH must be preserved: {filtered:?}"
        );
    }

    #[test]
    fn docker_start_retry_classifies_only_transient_runtime_failures() {
        for message in [
            "failed to create TTRPC connection: unsupported protocol",
            "error reading from server: EOF",
            "Cannot connect to the Docker daemon. Is the docker daemon running?",
            "rpc error: transport is closing",
        ] {
            assert!(docker_start_error_is_transient(&anyhow::anyhow!(message)));
        }
        for message in [
            "pull access denied for private/image",
            "invalid reference format",
            "network with name job already exists",
            "failed to create task for container: OCI runtime create failed: executable not found",
        ] {
            assert!(!docker_start_error_is_transient(&anyhow::anyhow!(message)));
        }
    }

    #[test]
    fn docker_start_retry_delay_is_bounded() {
        assert_eq!(docker_start_retry_delay(1), Duration::from_secs(1));
        assert_eq!(docker_start_retry_delay(2), Duration::from_secs(2));
        assert_eq!(docker_start_retry_delay(3), Duration::from_secs(4));
        assert_eq!(docker_start_retry_delay(4), Duration::from_secs(8));
        assert_eq!(docker_start_retry_delay(40), Duration::from_secs(8));
    }

    #[test]
    fn github_app_client_id_is_preferred_with_legacy_fallback() {
        assert_eq!(
            github_app_identifier("Iv23client".to_string(), "12345".to_string()),
            "Iv23client"
        );
        assert_eq!(
            github_app_identifier(String::new(), "12345".to_string()),
            "12345"
        );
    }

    #[test]
    fn immutable_github_condition_can_prove_local_action_is_skipped() {
        let context = vec![(
            "github".to_string(),
            serde_json::json!({
                "ref": "refs/heads/perf/subminute-ci",
                "event_name": "workflow_dispatch"
            }),
        )];
        let condition = "github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'workflow_dispatch')";

        assert!(condition_is_statically_false(
            Some(condition),
            &[],
            &context
        ));
        assert!(condition_is_statically_false(
            Some(&format!("success() && ({condition})")),
            &[],
            &context
        ));
        assert!(!condition_is_statically_false(
            Some("steps.changes.outputs.docs == 'true'"),
            &[],
            &context
        ));
        assert!(!condition_is_statically_false(
            Some("success() && github.ref != ''"),
            &[],
            &context
        ));
    }
}

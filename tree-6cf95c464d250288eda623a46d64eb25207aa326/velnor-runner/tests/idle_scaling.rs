//! Deterministic local proof for issue 408's idle resource-scaling gate.

#![cfg(unix)]

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

// The controller's bounded remote budget is 15s and its watchdog reserves an
// equal local margin. Keep the observation deadline within that same 30s
// liveness contract; normal runs still finish after four 2s cycles.
const METRICS_WAIT: Duration = Duration::from_secs(30);
const METRICS_POLL: Duration = Duration::from_millis(20);
const OUTPUT_TAIL_CAP_BYTES: usize = 64 * 1024;
const OUTPUT_READ_CHUNK_BYTES: usize = 8 * 1024;
const CLEANUP_WAIT: Duration = Duration::from_millis(500);
const CLEANUP_POLL: Duration = Duration::from_millis(5);

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn scratch() -> ScratchDir {
    let temp_root = std::env::temp_dir()
        .canonicalize()
        .expect("canonicalize system temporary directory");
    let path = loop {
        let candidate = temp_root.join(format!("velnor-408-idle-scale-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create private measurement directory: {error}"),
        }
    };
    let scratch = ScratchDir { path };
    std::fs::write(
        scratch.path().join("execution.toml"),
        "[execution]\nbackend = \"docker\"\n",
    )
    .expect("write execution configuration");
    scratch
}

struct Controller {
    child: Child,
    pgid: libc::pid_t,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    stdout_capture: TailCapture,
    stderr_capture: TailCapture,
    sanitizer: DiagnosticSanitizer,
    child_reaped: bool,
    exit_status: Option<ExitStatus>,
    shutdown: bool,
}

impl Controller {
    fn new(child: Child, sanitizer: DiagnosticSanitizer) -> Self {
        let pgid = child.id() as libc::pid_t;
        Self {
            child,
            pgid,
            stdout: None,
            stderr: None,
            stdout_capture: TailCapture::with_sanitizer(sanitizer.clone()),
            stderr_capture: TailCapture::with_sanitizer(sanitizer.clone()),
            sanitizer,
            child_reaped: false,
            exit_status: None,
            shutdown: false,
        }
    }

    fn install_output_capture(&mut self) {
        let stdout = self.child.stdout.take().expect("capture controller stdout");
        set_nonblocking(stdout.as_raw_fd()).expect("configure controller stdout");
        self.stdout = Some(stdout);

        let stderr = self.child.stderr.take().expect("capture controller stderr");
        set_nonblocking(stderr.as_raw_fd()).expect("configure controller stderr");
        self.stderr = Some(stderr);
    }

    fn shutdown(&mut self, strict: bool) -> (Option<ExitStatus>, String, String) {
        if self.shutdown {
            return (
                None,
                "<not captured>".to_owned(),
                "<not captured>".to_owned(),
            );
        }

        let mut cleanup_errors = self.terminate_owned_processes();
        if let Some(error) = self.reap_child() {
            cleanup_errors.push(error);
        }
        self.pump_output();
        let output = self.capture_child_output();
        self.shutdown = true;

        if strict && !cleanup_errors.is_empty() {
            panic!("idle controller cleanup failed: {cleanup_errors:?}");
        }
        if !cleanup_errors.is_empty() {
            eprintln!("idle controller cleanup warning: {cleanup_errors:?}");
        }
        (self.exit_status, output.0, output.1)
    }

    fn terminate_owned_processes(&mut self) -> Vec<String> {
        if self.child_reaped {
            return Vec::new();
        }

        let child_pid = self.child.id() as libc::pid_t;
        let (group_owned, mut errors) = match prove_process_group(child_pid, self.pgid) {
            Ok(true) => (true, Vec::new()),
            Ok(false) => (false, Vec::new()),
            Err(error) => (
                false,
                vec![format!(
                    "could not prove controller process-group identity: {error}"
                )],
            ),
        };

        if let Err(error) = self.child.kill() {
            if error.raw_os_error() != Some(libc::ESRCH) {
                errors.push(format!("kill controller: {error}"));
            }
        }

        if group_owned {
            // SAFETY: `prove_process_group` checked that the unreaped Child
            // remains the leader of `pgid`. The child cannot be reaped or have
            // its PID reused before this signal, so this numeric group ID
            // cannot name a reused process group. If proof fails, this method
            // uses only the tracked Child kill above; POSIX has no portable
            // atomic process-group identity-and-signal operation here.
            let result = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
            if result == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    errors.push(format!("kill controller process group: {error}"));
                }
            }
        } else {
            if errors.is_empty() {
                errors.push(format!(
                    "controller process-group identity mismatch: child_pid={child_pid}, pgid={}",
                    self.pgid
                ));
            }
        }
        errors
    }

    fn reap_child(&mut self) -> Option<String> {
        if self.child_reaped {
            return None;
        }

        let deadline = Instant::now() + CLEANUP_WAIT;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.child_reaped = true;
                    self.exit_status = Some(status);
                    return None;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        return Some(format!("controller did not exit within {CLEANUP_WAIT:?}"));
                    }
                    self.pump_output();
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    thread::sleep(CLEANUP_POLL.min(remaining));
                }
                Err(error) => return Some(format!("reap controller: {error}")),
            }
        }
    }

    fn pump_output(&mut self) {
        drain_child_stream(&mut self.stdout, &mut self.stdout_capture);
        drain_child_stream(&mut self.stderr, &mut self.stderr_capture);
    }

    fn capture_child_output(&mut self) -> (String, String) {
        self.pump_output();
        let stdout = std::mem::take(&mut self.stdout_capture).finish();
        let stderr = std::mem::take(&mut self.stderr_capture).finish();
        (stdout, stderr)
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.shutdown(false);
    }
}

#[derive(Clone, Debug, Default)]
struct DiagnosticSanitizer {
    names: Vec<OsString>,
    values: Vec<String>,
}

impl DiagnosticSanitizer {
    fn from_environment() -> Self {
        let mut sanitizer = Self::default();
        for (name, value) in std::env::vars_os() {
            if !is_sensitive_environment_name(&name.to_string_lossy()) {
                continue;
            }
            sanitizer.names.push(name);
            let value = value.to_string_lossy().into_owned();
            if !value.is_empty() && !sanitizer.values.contains(&value) {
                sanitizer.values.push(value);
            }
        }
        sanitizer
            .values
            .sort_by_key(|value| std::cmp::Reverse(value.len()));
        sanitizer
    }

    fn sanitize(&self, raw: &str) -> String {
        bounded_diagnostic(&self.sanitize_unbounded(raw))
    }

    fn sanitize_unbounded(&self, raw: &str) -> String {
        let mut sanitized = raw.to_owned();
        let mut values = self.values.clone();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        for value in values {
            sanitized = sanitized.replace(&value, "[REDACTED]");
        }
        sanitized = redact_credential_tokens(&sanitized);
        let mut output = String::with_capacity(sanitized.len());
        for character in sanitized.chars() {
            output.push(
                if character.is_control() && character != '\n' && character != '\t' {
                    ' '
                } else {
                    character
                },
            );
        }
        output
    }
}

fn is_sensitive_environment_name(name: &str) -> bool {
    let normalized = normalize_credential_name(name);
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "PRIVATE",
        "APIKEY",
        "ACCESSKEY",
        "SIGNINGKEY",
        "ENCRYPTIONKEY",
        "SSHKEY",
        "AUTH",
        "BEARER",
        "COOKIE",
        "CERTIFICATE",
        "KEY",
        "VALUE",
        "AWSACCESSKEYID",
        "DATABASEURL",
    ]
    .into_iter()
    .any(|marker| normalized.contains(marker))
}

fn normalize_credential_name(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_uppercase)
        .collect()
}

fn redact_credential_tokens(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut redact_next = false;
    for token in raw.split_whitespace() {
        if !output.is_empty() {
            output.push(' ');
        }
        if redact_next {
            if token.eq_ignore_ascii_case("bearer") || token.eq_ignore_ascii_case("basic") {
                output.push_str(token);
                redact_next = true;
            } else {
                output.push_str("[REDACTED]");
                redact_next = false;
            }
            continue;
        }

        if token.eq_ignore_ascii_case("bearer") {
            output.push_str(token);
            redact_next = true;
            continue;
        }

        let separator = token.find(['=', ':']);
        if let Some(separator) = separator {
            let key = token[..separator].trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != '_'
            });
            if is_sensitive_environment_name(key) {
                output.push_str(&token[..=separator]);
                output.push_str("[REDACTED]");
                if token[separator + 1..].eq_ignore_ascii_case("bearer")
                    || token[separator + 1..].eq_ignore_ascii_case("basic")
                    || token[separator + 1..].is_empty()
                {
                    redact_next = true;
                }
                continue;
            }
        }
        output.push_str(token);
    }
    output
}

fn bounded_diagnostic(raw: &str) -> String {
    if raw.len() <= OUTPUT_TAIL_CAP_BYTES {
        return raw.to_owned();
    }
    let start = raw.len() - OUTPUT_TAIL_CAP_BYTES;
    format!(
        "<output truncated; showing final {OUTPUT_TAIL_CAP_BYTES} bytes>\n{}",
        String::from_utf8_lossy(&raw.as_bytes()[start..])
    )
}

fn spawn_controller(state_dir: &Path, slots: u32) -> Controller {
    let sanitizer = DiagnosticSanitizer::from_environment();
    let mut command = Command::new(env!("CARGO_BIN_EXE_velnor-runner"));
    command
        .args([
            "controller",
            "--state-dir",
            state_dir.to_str().expect("state directory is utf-8"),
            "--scope",
            "idle-scale",
            "--desired-ready",
            &slots.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in &sanitizer.names {
        command.env_remove(name);
    }
    // Own the controller and its deliberately persistent slot children as a
    // single test process group; no measurement may leak into the next one.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let child = command.spawn().expect("spawn idle controller");
    let mut controller = Controller::new(child, sanitizer);
    // Construct the controller guard before touching either pipe. If any
    // reader setup fails, `Drop` still owns and kills the spawned process.
    controller.install_output_capture();
    controller
}

fn wait_for_metrics(
    controller: &mut Controller,
    state_dir: &Path,
    path: &Path,
    deadline: Instant,
) -> Value {
    while Instant::now() < deadline {
        fail_if_controller_exited(controller, state_dir);
        controller.pump_output();
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(value) = serde_json::from_slice(&bytes) {
                return value;
            }
        }
        sleep_until(deadline);
    }
    panic!("controller did not publish metrics: {}", path.display());
}

fn wait_for_steady_cycles(
    controller: &mut Controller,
    state_dir: &Path,
    path: &Path,
    slots: u32,
) -> (Value, Value) {
    let deadline = Instant::now() + METRICS_WAIT;
    let mut previous = wait_for_metrics(controller, state_dir, path, deadline);
    let mut steady_cycles = 0;
    while Instant::now() < deadline {
        fail_if_controller_exited(controller, state_dir);
        controller.pump_output();
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                let populated = number(&value, &["slot_processes"]) == u64::from(slots);
                let previous_sequence = number(&previous, &["sequence"]);
                // The producer publishes by atomic rename. A polling reader
                // may legitimately miss one or more snapshots under load; a
                // missed sequence is still controller progress, not a stall.
                let advanced = number(&value, &["sequence"]) > previous_sequence;
                if advanced {
                    if populated {
                        steady_cycles += 1;
                    } else {
                        steady_cycles = 0;
                    }
                    if steady_cycles >= 4 {
                        return (previous, value);
                    }
                    previous = value;
                }
            }
        }
        sleep_until(deadline);
    }
    panic!(
        "controller did not publish a second metrics cycle: {}",
        path.display()
    );
}

fn sleep_until(deadline: Instant) {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if !remaining.is_zero() {
        thread::sleep(METRICS_POLL.min(remaining));
    }
}

fn fail_if_controller_exited(controller: &mut Controller, state_dir: &Path) {
    if !child_has_exited(&controller.child).expect("poll idle controller") {
        return;
    }

    let (status, stdout, stderr) = controller.shutdown(false);
    let state_contents = sanitized_state_contents(state_dir, &controller.sanitizer);
    report_controller_exit(status, stdout, stderr, state_contents);
}

fn child_has_exited(child: &Child) -> std::io::Result<bool> {
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }

    // WNOWAIT keeps the child unreaped so the process-group proof and kill can
    // run before `Child::try_wait` releases the PID and its group identity.
    let observed_pid = unsafe { info.si_pid() };
    Ok(observed_pid == child.id() as libc::pid_t)
}

fn prove_process_group(child_pid: libc::pid_t, pgid: libc::pid_t) -> std::io::Result<bool> {
    if child_pid <= 0 || child_pid != pgid {
        return Ok(false);
    }

    let observed_pgid = unsafe { libc::getpgid(child_pid) };
    if observed_pgid == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(observed_pgid == pgid)
}

fn set_nonblocking(fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn drain_child_stream<T: Read>(stream: &mut Option<T>, capture: &mut TailCapture) {
    let mut finished = false;
    if let Some(stream) = stream.as_mut() {
        let mut buffer = [0_u8; OUTPUT_READ_CHUNK_BYTES];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => {
                    finished = true;
                    break;
                }
                Ok(read) => capture.push(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    capture.push(format!("<read diagnostic pipe failed: {error}>").as_bytes());
                    finished = true;
                    break;
                }
            }
        }
    }
    if finished {
        *stream = None;
    }
}

struct TailCapture {
    bytes: VecDeque<u8>,
    truncated: bool,
    sanitizer: DiagnosticSanitizer,
}

impl Default for TailCapture {
    fn default() -> Self {
        Self::with_sanitizer(DiagnosticSanitizer::default())
    }
}

impl TailCapture {
    fn with_sanitizer(sanitizer: DiagnosticSanitizer) -> Self {
        Self {
            bytes: VecDeque::new(),
            truncated: false,
            sanitizer,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let sanitized = self
            .sanitizer
            .sanitize_unbounded(&String::from_utf8_lossy(bytes));
        self.push_sanitized(sanitized.as_bytes());
    }

    fn push_sanitized(&mut self, bytes: &[u8]) {
        if bytes.len() > OUTPUT_TAIL_CAP_BYTES {
            self.bytes.clear();
            self.bytes
                .extend(bytes[bytes.len() - OUTPUT_TAIL_CAP_BYTES..].iter().copied());
            self.truncated = true;
            return;
        }

        let excess = (self.bytes.len() + bytes.len()).saturating_sub(OUTPUT_TAIL_CAP_BYTES);
        for _ in 0..excess {
            self.bytes.pop_front();
        }
        self.truncated |= excess > 0;
        self.bytes.extend(bytes.iter().copied());
    }

    fn finish(self) -> String {
        let bytes: Vec<_> = self.bytes.into_iter().collect();
        let tail = String::from_utf8_lossy(&bytes);
        if self.truncated {
            format!("<output truncated; showing final {OUTPUT_TAIL_CAP_BYTES} bytes>\n{tail}")
        } else {
            tail.into_owned()
        }
    }
}

fn sanitized_state_contents(state_dir: &Path, sanitizer: &DiagnosticSanitizer) -> String {
    let mut entries = Vec::new();
    match std::fs::read_dir(state_dir) {
        Ok(directory) => {
            for entry in directory {
                match entry {
                    Ok(entry) => entries.push(entry.file_name().to_string_lossy().into_owned()),
                    Err(error) => entries.push(format!("<entry read failed: {error}>")),
                }
            }
        }
        Err(error) => entries.push(format!("<directory read failed: {error}>")),
    }
    entries.sort();
    sanitizer.sanitize(&format!("{entries:?}"))
}

fn report_controller_exit(
    status: Option<ExitStatus>,
    stdout: String,
    stderr: String,
    state_contents: String,
) -> ! {
    panic!(
        "controller exited while waiting for metrics: status={status:?}, code={:?}, signal={:?}, stdout={stdout:?}, stderr={stderr:?}, state_dir_entries={state_contents}",
        status.as_ref().and_then(ExitStatus::code),
        status.as_ref().and_then(ExitStatusExt::signal),
    );
}

fn number(metrics: &Value, path: &[&str]) -> u64 {
    path.iter()
        .try_fold(metrics, |value, key| value.get(*key))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("missing numeric metric {path:?}: {metrics}"))
}

fn cpu_us(metrics: &Value) -> u64 {
    [
        "journal",
        "filesystem",
        "github",
        "broker",
        "child_supervision",
    ]
    .into_iter()
    .map(|phase| {
        number(metrics, &["cpu", phase, "user_us"])
            .saturating_add(number(metrics, &["cpu", phase, "system_us"]))
    })
    .sum()
}

fn stop_process_group(controller: &mut Controller) {
    let _ = controller.shutdown(true);
}

#[derive(Debug)]
struct Measurement {
    slots: u32,
    slot_processes: u64,
    job_processes: u64,
    waiter_processes: u64,
    reconcile_p95_ms: u64,
    controller_cpu_us: u64,
    journal_transactions: u64,
    wal_bytes: u64,
}

#[test]
fn idle_resource_scaling_from_one_to_sixteen_slots_is_bounded() {
    let mut measurements = Vec::new();
    for slots in [1, 2, 4, 8, 16] {
        let state_dir = scratch();
        let metrics_path = state_dir.path().join("controller-metrics.json");
        let mut controller = spawn_controller(state_dir.path(), slots);
        let (first_metrics, metrics) =
            wait_for_steady_cycles(&mut controller, state_dir.path(), &metrics_path, slots);
        let measurement = Measurement {
            slots,
            slot_processes: number(&metrics, &["slot_processes"]),
            job_processes: number(&metrics, &["job_processes"]),
            waiter_processes: number(&metrics, &["waiter_processes"]),
            reconcile_p95_ms: number(&metrics, &["reconcile_duration_ms", "p95"]),
            controller_cpu_us: cpu_us(&metrics).saturating_sub(cpu_us(&first_metrics)),
            journal_transactions: number(&metrics, &["journal", "transactions"])
                .saturating_sub(number(&first_metrics, &["journal", "transactions"])),
            wal_bytes: number(&metrics, &["journal", "wal_bytes"]),
        };
        stop_process_group(&mut controller);

        assert_eq!(measurement.slots, slots);
        assert_eq!(measurement.slot_processes, u64::from(slots));
        assert_eq!(measurement.job_processes, 0, "idle jobs: {measurement:?}");
        assert_eq!(
            measurement.waiter_processes, 0,
            "idle waiters: {measurement:?}"
        );
        assert!(
            measurement.reconcile_p95_ms > 0,
            "controller must publish a non-zero reconcile duration: {measurement:?}"
        );
        assert!(
            measurement.journal_transactions > 0,
            "controller must publish journal telemetry: {measurement:?}"
        );
        assert!(
            measurement.wal_bytes <= 4 * 1024 * 1024,
            "startup WAL must remain bounded: {measurement:?}"
        );
        println!(
            "idle_scaling slots={} slot_processes={} job_processes={} waiter_processes={} reconcile_p95_ms={} controller_cpu_us={} journal_transactions={} wal_bytes={}",
            measurement.slots,
            measurement.slot_processes,
            measurement.job_processes,
            measurement.waiter_processes,
            measurement.reconcile_p95_ms,
            measurement.controller_cpu_us,
            measurement.journal_transactions,
            measurement.wal_bytes,
        );
        measurements.push(measurement);
    }

    let baseline = measurements.first().expect("one-slot measurement");
    let largest = measurements.last().expect("sixteen-slot measurement");
    // The first cycle includes deterministic slot-process creation. CPU
    // attribution is the steady control-resource gate; duration remains in
    // the exact report for diagnosing startup work separately.
    assert!(
        largest.controller_cpu_us <= baseline.controller_cpu_us.saturating_mul(2) + 1_000,
        "controller CPU exceeded 2x: {measurements:#?}"
    );

    println!("idle scaling measurements: {measurements:#?}");
}

#[test]
fn diagnostics_keep_only_a_bounded_tail_and_mark_truncation() {
    let tail_canary = "tail-secret-canary";
    let raw = format!(
        "{} Authorization=Bearer {tail_canary}",
        "x".repeat(OUTPUT_TAIL_CAP_BYTES)
    );
    let mut capture = TailCapture::default();
    capture.push(raw.as_bytes());
    let output = capture.finish();

    let truncation_marker =
        format!("<output truncated; showing final {OUTPUT_TAIL_CAP_BYTES} bytes>\n");
    assert!(output.starts_with(&truncation_marker));
    assert!(!output.contains(tail_canary));
    assert!(!output.contains("Authorization=Bearer"));
    assert!(output.contains("Authorization=[REDACTED] [REDACTED]"));
}

#[test]
fn diagnostics_redact_credential_names_and_repeated_authorization_values() {
    let canaries = [
        ("token", "token-canary"),
        ("password", "password-canary"),
        ("key", "key-canary"),
        ("value", "value-canary"),
        ("AWS_ACCESS_KEY_ID", "aws-access-canary"),
        ("SIGNING_KEY", "signing-canary"),
        ("ENCRYPTION_KEY", "encryption-canary"),
        ("SSH_KEY", "ssh-canary"),
        ("DATABASE_URL", "database-canary"),
    ];
    let first_basic_canary = "first-basic-canary";
    let second_bearer_canary = "second-bearer-canary";
    let sanitizer = DiagnosticSanitizer {
        names: Vec::new(),
        values: vec![canaries[0].1.to_owned()],
    };
    let output = sanitizer.sanitize(&format!(
        "token={} Authorization: Basic {first_basic_canary} Authorization=Bearer {second_bearer_canary} password={} key={} value={} AWS_ACCESS_KEY_ID={} SIGNING_KEY={} ENCRYPTION_KEY={} SSH_KEY={} DATABASE_URL={}",
        canaries[0].1,
        canaries[1].1,
        canaries[2].1,
        canaries[3].1,
        canaries[4].1,
        canaries[5].1,
        canaries[6].1,
        canaries[7].1,
        canaries[8].1,
    ));

    for (_, canary) in canaries {
        assert!(!output.contains(canary), "canary leaked: {canary}");
    }
    assert!(!output.contains(first_basic_canary));
    assert!(!output.contains(second_bearer_canary));
    assert!(!output.contains("Authorization: Basic"));
    assert!(!output.contains("Authorization=Bearer"));
}

#[test]
fn diagnostics_redact_secret_crossing_raw_tail_boundary_before_truncation() {
    let boundary_canary = "raw-tail-boundary-secret";
    let split = boundary_canary.len() / 2;
    let raw = format!(
        "{}{}{}{}",
        "x".repeat(OUTPUT_TAIL_CAP_BYTES - split),
        &boundary_canary[..split],
        &boundary_canary[split..],
        "y".repeat(boundary_canary.len()),
    );
    let sanitizer = DiagnosticSanitizer {
        names: Vec::new(),
        values: vec![boundary_canary.to_owned()],
    };
    let mut capture = TailCapture::with_sanitizer(sanitizer);
    capture.push(raw.as_bytes());
    let output = capture.finish();

    assert!(!output.contains(boundary_canary));
    assert!(!output.contains(&boundary_canary[split..]));
}

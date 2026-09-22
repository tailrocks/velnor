//! Bounded execution for one CI project command, shared by both runtimes.
//!
//! Two independent bounds replace the old output-silence kill:
//!
//! - `stall_warn`: no stdout/stderr byte for this long emits a diagnostic
//!   warning and resets. Silence is evidence of nothing: buffered compilers
//!   whose captured subprocesses hold output in pipes stay quiet for many
//!   minutes while healthy, so silence must never fail a command by itself.
//! - `wall`: an absolute deadline from spawn. A chatty hang, a live child
//!   with closed streams, or any descendant that outlives its parent is
//!   terminated as one process group (SIGTERM, bounded grace, SIGKILL) and
//!   reported as a wall-deadline failure.
//!
//! The child always runs in its own process group on Unix, so termination
//! reaches grandchildren. Group signaling validates the group first and
//! falls back to the direct child when validation fails. Every wait is
//! bounded; there is no blocking `wait()` on any path.
//!
//! Errors are plain messages: schema 1 and schema 2 carry distinct private
//! `GeneratorError` types, so each caller wraps the message in its own.

use std::env;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Typed execution policy for one project command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunLimits {
    /// Silence interval that emits a diagnostic warning and resets.
    pub(crate) stall_warn: Duration,
    /// Absolute deadline from spawn; exceeding it terminates the group.
    pub(crate) wall: Duration,
}

/// Default silence-warning interval: healthy commands usually stream within
/// minutes, so ten quiet minutes is worth a diagnostic line, never a kill.
pub(crate) const DEFAULT_STALL_WARN_SECS: u64 = 600;
/// Default absolute deadline: one hour bounds a hung native build without
/// touching healthy long compiles (the slowest observed honest native
/// command finished in ~17 minutes).
pub(crate) const DEFAULT_WALL_SECS: u64 = 3600;
/// Kept knob name: previously a fatal stall budget, now the warning interval.
const STALL_ENV: &str = "VELNOR_RUN_CMD_STALL_SECS";
const WALL_ENV: &str = "VELNOR_RUN_CMD_WALL_SECS";
/// Quantum between child-exit polls while waiting for output: a silent
/// command that already exited (or whose pipes grandchildren hold open) is
/// noticed within this long instead of at a deadline.
const EXIT_POLL_QUANTUM: Duration = Duration::from_secs(1);
/// Total cap on the post-exit pipe drain: detached pumps forward the tail,
/// but grandchildren holding the pipes open must not hang completion.
const DRAIN_GRACE: Duration = Duration::from_secs(10);
/// Idle window inside the drain: each forwarded chunk resets it, so a large
/// honest tail gets the time it needs while an idle squatter-held pipe
/// costs at most this long.
const DRAIN_IDLE_GRACE: Duration = Duration::from_secs(1);
/// Grace after SIGTERM before SIGKILL, and the bound on each reap poll.
const SIGNAL_GRACE: Duration = Duration::from_secs(10);

/// Parse one limit override in seconds; missing, unparsable, or zero values
/// fall back to the default.
pub(crate) fn parse_limit_secs(raw: Option<&str>, default: u64) -> Duration {
    let seconds = raw
        .map(str::trim)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0);
    Duration::from_secs(seconds.unwrap_or(default))
}

impl RunLimits {
    /// Policy from the environment, with defaults for absent/invalid knobs.
    pub(crate) fn from_env() -> Self {
        Self {
            stall_warn: parse_limit_secs(
                env::var(STALL_ENV).ok().as_deref(),
                DEFAULT_STALL_WARN_SECS,
            ),
            wall: parse_limit_secs(env::var(WALL_ENV).ok().as_deref(), DEFAULT_WALL_SECS),
        }
    }
}

/// One output chunk forwarded by a child-stream pump, or that stream's EOF.
enum PumpEvent {
    Output,
    Eof,
}

/// Forward one child pipe to the matching process stream, reporting every
/// chunk as [`PumpEvent::Output`]. Runs detached: it exits on pipe EOF (or
/// when the supervisor drops the receiver), so a wedged grandchild holding
/// the pipe cannot hang supervision.
fn pump_child_stream<R, W>(mut reader: R, mut writer: W, sender: &mpsc::Sender<PumpEvent>)
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let _ = writer.write_all(&buffer[..read]);
                let _ = writer.flush();
                if sender.send(PumpEvent::Output).is_err() {
                    break;
                }
            }
        }
    }
    let _ = sender.send(PumpEvent::Eof);
}

/// Spawn through bash in its own process group, so termination signals reach
/// every descendant. The group call is the safe std API; no `unsafe` needed.
fn spawn_grouped_command(root: &Path, command: &str) -> std::io::Result<Child> {
    let mut spawned = Command::new("bash");
    spawned
        .args(["-euo", "pipefail", "-c", command])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        spawned.process_group(0);
    }
    spawned.spawn()
}

/// Poll for the child's exit until `grace` elapses; `None` on timeout.
/// Reaps through `wait()`: `try_wait` only peeks (waitid/WNOWAIT) and a
/// peeked-but-unreaped child would linger as a zombie.
fn poll_exit(child: &mut Child, grace: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait().ok(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(
                    EXIT_POLL_QUANTUM.min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(_) => return None,
        }
    }
}

/// Resolve the child's process group after validating it is really the
/// child's own group and not ours; `None` means signal the direct child.
#[cfg(unix)]
fn process_group_of(child: &mut Child) -> Option<rustix::process::Pid> {
    let child_pid = rustix::process::Pid::from_child(child);
    if child_pid.is_init() {
        return None;
    }
    let group = rustix::process::getpgid(Some(child_pid)).ok()?;
    // The child was spawned as a group leader, so its group must equal
    // its PID; anything else (or our own group) is not safe to signal.
    if group != child_pid {
        return None;
    }
    if group == rustix::process::getpgid(None).ok()? {
        return None;
    }
    Some(group)
}

/// Signal a previously resolved group, falling back to the direct child
/// when there is no group or the group signal fails.
#[cfg(unix)]
fn signal_group(
    child: &mut Child,
    group: Option<rustix::process::Pid>,
    signal: rustix::process::Signal,
) {
    match group {
        Some(group) => {
            if rustix::process::kill_process_group(group, signal).is_err() {
                let _ = child.kill();
            }
        }
        None => {
            let _ = child.kill();
        }
    }
}

/// Without process groups there is only the direct child to kill.
#[cfg(not(unix))]
fn signal_tree(child: &mut Child, _signal: ()) {
    let _ = child.kill();
}

/// Terminate the whole tree: SIGTERM, bounded grace, SIGKILL, bounded reap.
/// Always returns a reap description; never blocks past two graces.
#[cfg(unix)]
fn terminate_tree(child: &mut Child) -> String {
    use rustix::process::Signal;
    // Preserve the group before signaling: once the leader exits and is
    // reaped, its PID no longer resolves via getpgid, but TERM-ignoring
    // descendants still live in that group and need SIGKILL.
    let group = process_group_of(child);
    signal_group(child, group, Signal::TERM);
    if let Some(status) = poll_exit(child, SIGNAL_GRACE) {
        // Leader exited, but descendants that trapped SIGTERM survive:
        // SIGKILL the preserved group so no survivor outlives the wall
        // failure. ESRCH (group already empty) is a harmless no-op.
        if let Some(group) = group {
            let _ = rustix::process::kill_process_group(group, Signal::KILL);
        }
        return format!("reaped with {status}");
    }
    signal_group(child, group, Signal::KILL);
    match poll_exit(child, SIGNAL_GRACE) {
        Some(status) => format!("reaped with {status}"),
        None => String::from("still alive after SIGKILL grace; abandoned"),
    }
}

/// Terminate without process groups: kill, bounded reap.
#[cfg(not(unix))]
fn terminate_tree(child: &mut Child) -> String {
    signal_tree(child, ());
    match poll_exit(child, SIGNAL_GRACE) {
        Some(status) => format!("reaped with {status}"),
        None => String::from("still alive after kill grace; abandoned"),
    }
}

/// Exit status decides success, unchanged from the pre-guard contract.
fn check_unit_command_status(unit_id: &str, status: ExitStatus) -> Result<(), String> {
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "CI command failed for unit {unit_id} with {status}"
        ))
    }
}

/// Drain pump events until both streams report EOF, the pipes sit idle for
/// [`DRAIN_IDLE_GRACE`], or the total [`DRAIN_GRACE`] cap elapses. The child
/// has already exited when this runs: its bytes are all in the pipe buffer
/// (or already forwarded), so an idle wait means squatter grandchildren,
/// not a slow tail — while every forwarded chunk resets the idle window.
fn drain_pumps(receiver: &mpsc::Receiver<PumpEvent>, eofs: &mut usize, expected_eof: usize) {
    let cap = Instant::now() + DRAIN_GRACE;
    let mut idle_deadline = Instant::now() + DRAIN_IDLE_GRACE;
    while *eofs < expected_eof && Instant::now() < cap {
        let now = Instant::now();
        let quantum = idle_deadline
            .saturating_duration_since(now)
            .min(cap.saturating_duration_since(now));
        match receiver.recv_timeout(quantum) {
            Ok(PumpEvent::Output) => {
                idle_deadline = Instant::now() + DRAIN_IDLE_GRACE;
            }
            Ok(PumpEvent::Eof) => {
                *eofs += 1;
            }
            // Either the idle window or the total cap elapsed: both stop.
            Err(_) => break,
        }
    }
}

/// Run one project command under `limits`.
///
/// Silence emits a stderr diagnostic and resets its timer; only the wall
/// deadline kills. A silent exit is completion, noticed within one poll
/// quantum even when grandchildren hold the pipes open. Reaping after EOF
/// is bounded by the same wall deadline, so a live child with closed
/// streams cannot hang supervision.
pub(crate) fn run_command(
    root: &Path,
    unit_id: &str,
    command: &str,
    limits: &RunLimits,
) -> Result<(), String> {
    let started = Instant::now();
    let wall_deadline = started + limits.wall;
    let mut child = spawn_grouped_command(root, command)
        .map_err(|error| format!("run CI command {unit_id}: {error}"))?;
    let (sender, receiver) = mpsc::channel();
    let mut expected_eof = 0;
    if let Some(stdout) = child.stdout.take() {
        expected_eof += 1;
        let sender = sender.clone();
        thread::spawn(move || pump_child_stream(stdout, std::io::stdout(), &sender));
    }
    if let Some(stderr) = child.stderr.take() {
        expected_eof += 1;
        let sender = sender.clone();
        thread::spawn(move || pump_child_stream(stderr, std::io::stderr(), &sender));
    }
    drop(sender);
    let pid = child.id();
    let mut stall_deadline = Instant::now() + limits.stall_warn;
    let mut eofs = 0;
    loop {
        let quantum = EXIT_POLL_QUANTUM
            .min(stall_deadline.saturating_duration_since(Instant::now()))
            .min(wall_deadline.saturating_duration_since(Instant::now()));
        match receiver.recv_timeout(quantum) {
            Ok(PumpEvent::Output) => {
                stall_deadline = Instant::now() + limits.stall_warn;
                if Instant::now() >= wall_deadline {
                    let reaped = terminate_tree(&mut child);
                    return Err(wall_message(
                        unit_id, command, limits, started, pid, &reaped,
                    ));
                }
            }
            Ok(PumpEvent::Eof) => {
                eofs += 1;
                if eofs >= expected_eof {
                    break;
                }
            }
            Err(_) => {
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                let now = Instant::now();
                if now >= wall_deadline {
                    let reaped = terminate_tree(&mut child);
                    return Err(wall_message(
                        unit_id, command, limits, started, pid, &reaped,
                    ));
                }
                if now >= stall_deadline {
                    eprintln!(
                        "velnor: CI command for unit {unit_id} produced no stdout/stderr output for {}s (pid {pid}); still running under a {}s wall deadline: {command}",
                        limits.stall_warn.as_secs(),
                        limits.wall.as_secs(),
                    );
                    stall_deadline = now + limits.stall_warn;
                }
            }
        }
    }
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                // Reap now: `try_wait` only peeks, and the exit status from
                // the reaping `wait()` is the one that counts.
                let status = child
                    .wait()
                    .map_err(|error| format!("run CI command {unit_id}: reap failed: {error}"))?;
                drain_pumps(&receiver, &mut eofs, expected_eof);
                return check_unit_command_status(unit_id, status);
            }
            Ok(None) => {
                if Instant::now() >= wall_deadline {
                    let reaped = terminate_tree(&mut child);
                    return Err(wall_message(
                        unit_id, command, limits, started, pid, &reaped,
                    ));
                }
                thread::sleep(
                    EXIT_POLL_QUANTUM.min(wall_deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(error) => {
                return Err(format!("run CI command {unit_id}: reap failed: {error}"));
            }
        }
    }
}

/// Wall-deadline failure message: names the unit, budget, elapsed time, pid,
/// reap outcome, and command.
fn wall_message(
    unit_id: &str,
    command: &str,
    limits: &RunLimits,
    started: Instant,
    pid: u32,
    reaped: &str,
) -> String {
    format!(
        "CI command exceeded its wall deadline for unit {unit_id}: no completion within {}s ({}s elapsed); terminated pid {pid} ({reaped}); command: {command}",
        limits.wall.as_secs(),
        started.elapsed().as_secs(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        parse_limit_secs, run_command, RunLimits, DEFAULT_STALL_WARN_SECS, DEFAULT_WALL_SECS,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    fn limits(stall_warn: Duration, wall: Duration) -> RunLimits {
        RunLimits { stall_warn, wall }
    }

    fn failed_message(result: Result<(), String>) -> String {
        match result {
            Ok(()) => String::from("<unexpected success>"),
            Err(message) => message,
        }
    }

    #[test]
    fn limit_parses_override_and_falls_back() {
        assert_eq!(parse_limit_secs(None, 600), Duration::from_mins(10));
        assert_eq!(parse_limit_secs(Some("30"), 600), Duration::from_secs(30));
        assert_eq!(parse_limit_secs(Some(" 120 "), 600), Duration::from_mins(2));
        for invalid in ["", "0", "-5", "ten", "1.5"] {
            assert_eq!(
                parse_limit_secs(Some(invalid), 600),
                Duration::from_mins(10),
                "invalid override {invalid:?} must fall back to the default",
            );
        }
        assert_eq!(DEFAULT_STALL_WARN_SECS, 600);
        assert_eq!(DEFAULT_WALL_SECS, 3600);
    }

    #[test]
    fn quiet_command_succeeds() {
        let result = run_command(
            &std::env::temp_dir(),
            "test-unit",
            "echo hello",
            &limits(Duration::from_mins(1), Duration::from_mins(1)),
        );
        assert!(
            result.is_ok(),
            "quiet command must succeed, got: {}",
            failed_message(result)
        );
    }

    #[test]
    fn silent_past_stall_warn_still_succeeds() {
        // 2s of silence against a 200ms warning interval: warnings fire,
        // nothing dies, the exit status decides.
        let result = run_command(
            &std::env::temp_dir(),
            "test-unit",
            "exec sleep 2",
            &limits(Duration::from_millis(200), Duration::from_mins(1)),
        );
        assert!(
            result.is_ok(),
            "silent command must succeed, got: {}",
            failed_message(result)
        );
    }

    #[test]
    fn chatty_hang_hits_wall_deadline() {
        let started = Instant::now();
        let result = run_command(
            &std::env::temp_dir(),
            "test-unit",
            "while true; do echo tick; sleep 0.05; done",
            &limits(Duration::from_mins(1), Duration::from_millis(500)),
        );
        let message = failed_message(result);
        assert!(
            message.contains("wall deadline"),
            "chatty hang must fail naming the wall deadline, got: {message}"
        );
        assert!(
            message.contains("test-unit") && message.contains("while true"),
            "wall error must name unit and command, got: {message}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "wall kill must return promptly"
        );
    }

    #[test]
    fn closed_streams_live_child_bounded_by_wall() {
        // Both pipes EOF immediately while the child lives on: supervision
        // must fail at the wall deadline, never block in `wait()` forever.
        let started = Instant::now();
        let result = run_command(
            &std::env::temp_dir(),
            "test-unit",
            "exec 1>&- 2>&-; exec sleep 30",
            &limits(Duration::from_mins(1), Duration::from_millis(500)),
        );
        let message = failed_message(result);
        assert!(
            message.contains("wall deadline"),
            "closed-stream live child must fail at the wall, got: {message}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "closed-stream reap must stay bounded"
        );
    }

    #[test]
    fn exited_child_with_piped_grandchild_completes() {
        // The child exits while a grandchild holds the pipes open: the exit
        // poll notices completion without waiting out the grandchild.
        let started = Instant::now();
        let result = run_command(
            &std::env::temp_dir(),
            "test-unit",
            "(sleep 5 &)",
            &limits(Duration::from_mins(1), Duration::from_mins(1)),
        );
        assert!(
            result.is_ok(),
            "exited child must complete, got: {}",
            failed_message(result)
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "completion must not wait out the grandchild"
        );
    }

    #[test]
    fn nonzero_status_preserved() {
        let message = failed_message(run_command(
            &std::env::temp_dir(),
            "test-unit",
            "exit 3",
            &limits(Duration::from_mins(1), Duration::from_mins(1)),
        ));
        assert!(
            message.contains("CI command failed for unit test-unit"),
            "exit-status failure must keep its error, got: {message}"
        );
    }

    #[test]
    fn output_without_trailing_newline_succeeds() {
        let result = run_command(
            &std::env::temp_dir(),
            "test-unit",
            "printf 'partial'",
            &limits(Duration::from_mins(1), Duration::from_mins(1)),
        );
        assert!(
            result.is_ok(),
            "no-newline output must succeed, got: {}",
            failed_message(result)
        );
    }

    #[cfg(unix)]
    #[test]
    fn descendant_killed_with_group() {
        // A background grandchild must die with the group at the wall: after
        // the failure, `kill -0` on its recorded PID must fail.
        let pid_file =
            std::env::temp_dir().join(format!("velnor-exec-group-{}.pid", crate::unique_suffix()));
        let script = format!(
            "sleep 30 & echo $! > '{}'; exec sleep 30",
            pid_file.display()
        );
        let message = failed_message(run_command(
            &std::env::temp_dir(),
            "test-unit",
            &script,
            &limits(Duration::from_mins(1), Duration::from_secs(1)),
        ));
        assert!(
            message.contains("wall deadline"),
            "group sleeper must fail at the wall, got: {message}"
        );
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap_or_default()
            .trim()
            .to_owned();
        assert!(!pid.is_empty(), "fixture must record the grandchild PID");
        let deadline = Instant::now() + Duration::from_secs(10);
        let gone = loop {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid])
                .output()
                .is_ok_and(|output| output.status.success());
            if !alive {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(50));
        };
        let _ = std::fs::remove_file(&pid_file);
        assert!(gone, "grandchild {pid} must die with its process group");
    }

    #[cfg(unix)]
    #[test]
    fn term_ignoring_descendant_killed_with_group() {
        // A grandchild that traps SIGTERM must still die at the wall: the
        // leader exits on SIGTERM, but the preserved group must still get
        // SIGKILL so no survivor outlives the wall failure.
        let pid_file = std::env::temp_dir().join(format!(
            "velnor-exec-term-ignore-{}.pid",
            crate::unique_suffix()
        ));
        let script = format!(
            "(trap '' TERM; exec sleep 30) & echo $! > '{}'; exec sleep 30",
            pid_file.display()
        );
        let message = failed_message(run_command(
            &std::env::temp_dir(),
            "test-unit",
            &script,
            &limits(Duration::from_mins(1), Duration::from_secs(1)),
        ));
        assert!(
            message.contains("wall deadline"),
            "TERM-ignoring sleeper must fail at the wall, got: {message}"
        );
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap_or_default()
            .trim()
            .to_owned();
        assert!(!pid.is_empty(), "fixture must record the grandchild PID");
        let deadline = Instant::now() + Duration::from_secs(10);
        let gone = loop {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid])
                .output()
                .is_ok_and(|output| output.status.success());
            if !alive {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(50));
        };
        if !gone {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid])
                .output();
        }
        let _ = std::fs::remove_file(&pid_file);
        assert!(
            gone,
            "TERM-ignoring grandchild {pid} must die with its process group"
        );
    }
}

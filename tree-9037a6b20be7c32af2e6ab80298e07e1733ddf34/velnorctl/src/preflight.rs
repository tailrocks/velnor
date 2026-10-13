//! Bounded local command capture used by the operator diagnostics bundle.
//!
//! The bundle invokes the existing CLI surfaces in a child process so its
//! evidence cannot drift from the commands an operator actually runs. The
//! child receives no credential environment variables and both output streams
//! are drained into bounded in-memory buffers before the process is waited on.

use std::{
    env,
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::{host, CommandError, BIN_NAME};

const MAX_CAPTURE_BYTES: usize = 256 * 1024;
const OUTPUT_TRUNCATION_MARKER: &[u8] = b"\n[output truncated]\n";
pub(crate) const CREDENTIAL_ENV: [&str; 6] = [
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "VELNOR_PAT",
    "ACTIONS_RUNTIME_TOKEN",
    "RUNNER_TOKEN",
    "VELNOR_GITHUB_TOKEN",
];

/// One bounded invocation of an existing operator command.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandEvidence {
    pub command: Vec<String>,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub error: Option<String>,
}

impl CommandEvidence {
    /// Whether the captured command completed successfully.
    #[must_use]
    pub(crate) fn succeeded(&self) -> bool {
        self.status == Some(0) && !self.timed_out && self.error.is_none()
    }
}

/// Resolve the same per-host configuration directory used by `host start`.
pub(crate) fn resolve_config_dir(instance: Option<&str>) -> Result<PathBuf, CommandError> {
    host::ensure_dev_canonical_storage()?;
    let name = instance
        .map(str::to_owned)
        .unwrap_or_else(host::default_host_name);
    host::resolve_default_host_config_dir(&name)
}

/// Capture several invocations concurrently under one shared deadline.
pub(crate) fn capture_parallel(
    commands: Vec<(String, Vec<OsString>)>,
    timeout: Duration,
) -> Vec<(String, CommandEvidence)> {
    let executable = match env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            return commands
                .into_iter()
                .map(|(name, args)| {
                    let command = command_evidence(BIN_NAME, &args);
                    (
                        name,
                        failed(command, format!("resolve {BIN_NAME}: {error}")),
                    )
                })
                .collect();
        }
    };
    capture_parallel_executable(&executable, BIN_NAME, commands, timeout)
}

fn capture_parallel_executable(
    executable: &Path,
    display_name: &str,
    commands: Vec<(String, Vec<OsString>)>,
    timeout: Duration,
) -> Vec<(String, CommandEvidence)> {
    let deadline = deadline_after(timeout);
    thread::scope(|scope| {
        let handles = commands
            .into_iter()
            .map(|(name, args)| {
                let name_for_panic = name.clone();
                let handle = scope.spawn(move || {
                    (
                        name,
                        capture_executable(executable, display_name, &args, deadline),
                    )
                });
                (name_for_panic, handle)
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|(name, handle)| {
                handle.join().unwrap_or_else(|_| {
                    (
                        name.clone(),
                        failed(
                            vec![display_name.to_owned()],
                            format!("capture thread for {name} panicked"),
                        ),
                    )
                })
            })
            .collect()
    })
}

fn capture_executable(
    executable: &Path,
    display_name: &str,
    args: &[OsString],
    deadline: Instant,
) -> CommandEvidence {
    let command_evidence = command_evidence(display_name, args);
    if Instant::now() >= deadline {
        return timed_out(command_evidence);
    }
    let mut process = Command::new(executable);
    process
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in CREDENTIAL_ENV {
        process.env_remove(name);
    }
    configure_process_group(&mut process);
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return failed(command_evidence, format!("spawn {display_name}: {error}")),
    };

    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return failed(
                command_evidence,
                format!("{display_name} did not provide a stdout pipe"),
            );
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return failed(
                command_evidence,
                format!("{display_name} did not provide a stderr pipe"),
            );
        }
    };
    let stdout_reader = thread::spawn(|| read_capture(stdout));
    let stderr_reader = thread::spawn(|| read_capture(stderr));
    let mut timed_out = false;
    let mut error = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    timed_out = true;
                    terminate_process_group(&mut child);
                    let _ = child.wait();
                    break None;
                };
                thread::sleep(remaining.min(Duration::from_millis(20)));
            }
            Err(wait_error) => {
                terminate_process_group(&mut child);
                let _ = child.wait();
                error = Some(format!("wait for {display_name}: {wait_error}"));
                break None;
            }
        }
    };

    let stdout = join_capture(stdout_reader, display_name, "stdout", &mut error);
    let stderr = join_capture(stderr_reader, display_name, "stderr", &mut error);
    CommandEvidence {
        command: command_evidence,
        status,
        stdout,
        stderr,
        timed_out,
        error,
    }
}

fn command_evidence(display_name: &str, args: &[OsString]) -> Vec<String> {
    std::iter::once(display_name.to_owned())
        .chain(args.iter().map(|part| part.to_string_lossy().into_owned()))
        .collect()
}

fn deadline_after(timeout: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(timeout).unwrap_or(now)
}

fn timed_out(command: Vec<String>) -> CommandEvidence {
    CommandEvidence {
        command,
        status: None,
        stdout: String::new(),
        stderr: String::new(),
        timed_out: true,
        error: None,
    }
}

fn failed(command: Vec<String>, error: String) -> CommandEvidence {
    CommandEvidence {
        command,
        status: None,
        stdout: String::new(),
        stderr: String::new(),
        timed_out: false,
        error: Some(error),
    }
}

#[derive(Debug)]
struct CapturedOutput {
    bytes: Vec<u8>,
    error: Option<String>,
}

fn read_capture<R: Read>(mut reader: R) -> CapturedOutput {
    let mut bytes = Vec::with_capacity(MAX_CAPTURE_BYTES);
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0_usize;
    let mut truncated = false;
    let mut error = None;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let next_total = total.saturating_add(read);
                if total < MAX_CAPTURE_BYTES {
                    let keep = (MAX_CAPTURE_BYTES - total).min(read);
                    bytes.extend_from_slice(&buffer[..keep]);
                }
                truncated |= next_total > MAX_CAPTURE_BYTES;
                total = next_total;
            }
            Err(read_error) => {
                error = Some(read_error.to_string());
                break;
            }
        }
    }
    if truncated {
        let content_limit = MAX_CAPTURE_BYTES.saturating_sub(OUTPUT_TRUNCATION_MARKER.len());
        bytes.truncate(content_limit);
        bytes.extend_from_slice(OUTPUT_TRUNCATION_MARKER);
    }
    CapturedOutput { bytes, error }
}

fn join_capture(
    handle: thread::JoinHandle<CapturedOutput>,
    display_name: &str,
    stream: &str,
    error: &mut Option<String>,
) -> String {
    let captured = handle.join().unwrap_or_else(|_| CapturedOutput {
        bytes: Vec::new(),
        error: Some("capture reader panicked".to_owned()),
    });
    if let Some(read_error) = captured.error
        && error.is_none()
    {
        *error = Some(format!("read {display_name} {stream}: {read_error}"));
    }
    String::from_utf8_lossy(&captured.bytes).into_owned()
}

#[cfg(unix)]
fn configure_process_group(process: &mut Command) {
    use std::os::unix::process::CommandExt;

    process.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_process: &mut Command) {}

fn terminate_process_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id();
        if let Ok(pid) = libc::pid_t::try_from(pid)
            && pid > 1
        {
            // SAFETY: the child was configured with its own process group;
            // targeting the negative pid reaches that child and descendants.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests use local scratch values"
)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn explicit_config_root_wins_over_home() {
        let configured = Path::new("/tmp/velnor-config");
        assert_eq!(
            host::host_config_dir_from_root(configured, "velnor-test"),
            configured.join("hosts/velnor-test")
        );
    }

    #[test]
    fn default_config_layout_is_per_host() {
        assert_eq!(
            host::host_config_dir_from_root(
                Path::new("/Users/test/Library/Application Support/velnor/runner"),
                "velnor-local-test"
            ),
            PathBuf::from(
                "/Users/test/Library/Application Support/velnor/runner/hosts/velnor-local-test"
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_bounds_output_while_draining_the_pipe() {
        let args = vec![
            OsString::from("-c"),
            OsString::from("head -c 1048576 /dev/zero"),
        ];
        let evidence = capture_executable(
            Path::new("/bin/sh"),
            "/bin/sh",
            &args,
            deadline_after(Duration::from_secs(5)),
        );
        assert_eq!(evidence.status, Some(0));
        assert!(!evidence.timed_out);
        assert!(evidence.stdout.len() <= MAX_CAPTURE_BYTES);
        assert!(evidence.stdout.contains("[output truncated]"));
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_descendants_in_the_child_process_group() {
        let marker = env::temp_dir().join(format!(
            "velnorctl-descendant-pid-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let script = format!(
            "sleep 30 & child=$!; printf '%s' \"$child\" > '{}'; wait",
            marker.display()
        );
        let args = vec![OsString::from("-c"), OsString::from(script)];
        let evidence = capture_executable(
            Path::new("/bin/sh"),
            "/bin/sh",
            &args,
            deadline_after(Duration::from_millis(250)),
        );
        assert!(evidence.timed_out);
        let pid = fs::read_to_string(&marker)
            .expect("descendant pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("pid number");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            // SAFETY: signal zero only probes the test descendant.
            let alive = unsafe { libc::kill(pid, 0) == 0 };
            if !alive {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        // SAFETY: signal zero only probes the test descendant.
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "descendant survived timeout"
        );
        fs::remove_file(marker).ok();
    }

    #[cfg(unix)]
    #[test]
    fn parallel_capture_uses_one_shared_deadline() {
        let commands = (0..3)
            .map(|index| {
                (
                    format!("probe-{index}"),
                    vec![OsString::from("-c"), OsString::from("sleep 2")],
                )
            })
            .collect();
        let started = Instant::now();
        let results = capture_parallel_executable(
            Path::new("/bin/sh"),
            "/bin/sh",
            commands,
            Duration::from_millis(250),
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|(_, evidence)| evidence.timed_out));
    }

    #[test]
    fn command_evidence_requires_a_zero_exit_without_timeout_or_spawn_error() {
        let good = CommandEvidence {
            command: vec![BIN_NAME.to_owned()],
            status: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            error: None,
        };
        assert!(good.succeeded());
        assert!(!CommandEvidence {
            timed_out: true,
            ..good.clone()
        }
        .succeeded());
        assert!(!CommandEvidence {
            error: Some("spawn failed".to_owned()),
            ..good
        }
        .succeeded());
    }
}

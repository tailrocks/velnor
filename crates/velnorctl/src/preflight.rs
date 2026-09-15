//! Bounded local command capture used by the operator diagnostics bundle.
//!
//! The bundle invokes the existing CLI surfaces in a child process so its
//! evidence cannot drift from the commands an operator actually runs. The
//! child receives no credential environment variables and every output stream
//! is redirected to a bounded temporary file before the process is waited on.

use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::{CommandError, BIN_NAME};

const MAX_CAPTURE_BYTES: usize = 256 * 1024;
const TEMP_FILE_ATTEMPTS: usize = 32;
const CREDENTIAL_ENV: [&str; 5] = [
    "GITHUB_TOKEN",
    "VELNOR_PAT",
    "ACTIONS_RUNTIME_TOKEN",
    "RUNNER_TOKEN",
    "VELNOR_GITHUB_TOKEN",
];

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

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

/// Resolve the same user-local configuration root used by the macOS CLI.
pub(crate) fn resolve_config_dir() -> Result<PathBuf, CommandError> {
    resolve_config_dir_from(
        env::var_os("VELNOR_CONFIG_DIR").as_deref(),
        env::var_os("HOME").as_deref().map(Path::new),
    )
}

fn resolve_config_dir_from(
    configured: Option<&OsStr>,
    home: Option<&Path>,
) -> Result<PathBuf, CommandError> {
    if let Some(configured) = configured.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(configured));
    }

    #[cfg(target_os = "macos")]
    {
        let home = home.ok_or_else(|| {
            CommandError::new(
                velnor_model::ExitClass::Usage,
                "config.home_missing",
                "HOME is not set; pass VELNOR_CONFIG_DIR pointing at the directory containing execution.toml",
            )
        })?;
        Ok(home.join("Library/Application Support/velnor"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = home;
        Ok(PathBuf::from("/etc/velnor"))
    }
}

/// Capture one invocation of this binary with a bounded wall-clock budget.
pub(crate) fn capture(args: &[OsString], timeout: Duration) -> CommandEvidence {
    let command_evidence = std::iter::once(OsString::from(BIN_NAME))
        .chain(args.iter().cloned())
        .map(|part| part.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let executable = match env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            return CommandEvidence {
                command: command_evidence,
                status: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                error: Some(format!("resolve {BIN_NAME}: {error}")),
            };
        }
    };

    let (stdout_path, stdout_file) = match create_temp_file("stdout") {
        Ok(value) => value,
        Err(error) => return failed(command_evidence, format!("create stdout capture: {error}")),
    };
    let (stderr_path, stderr_file) = match create_temp_file("stderr") {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            return failed(command_evidence, format!("create stderr capture: {error}"));
        }
    };

    let mut process = Command::new(executable);
    process
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    for name in CREDENTIAL_ENV {
        process.env_remove(name);
    }
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return failed(command_evidence, format!("spawn {BIN_NAME}: {error}"));
        }
    };

    let started = Instant::now();
    let mut timed_out = false;
    let mut error = None;
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
            Err(wait_error) => {
                let _ = child.kill();
                let _ = child.wait();
                error = Some(format!("wait for {BIN_NAME}: {wait_error}"));
                break None;
            }
        }
    };

    let stdout = read_capture(&stdout_path);
    let stderr = read_capture(&stderr_path);
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);
    CommandEvidence {
        command: command_evidence,
        status,
        stdout,
        stderr,
        timed_out,
        error,
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

fn create_temp_file(label: &str) -> io::Result<(PathBuf, File)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let process = std::process::id();
    for attempt in 0..TEMP_FILE_ATTEMPTS {
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            ".velnorctl-diagnostic-{label}-{process}-{now}-{sequence}-{attempt}"
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "exhausted unique diagnostic capture paths",
    ))
}

fn read_capture(path: &Path) -> String {
    let mut bytes = Vec::new();
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) => return format!("cannot read captured output: {error}"),
    };
    let read_limit = u64::try_from(MAX_CAPTURE_BYTES.saturating_add(1)).unwrap_or(u64::MAX);
    if file.take(read_limit).read_to_end(&mut bytes).is_err() {
        return "cannot read captured output".to_owned();
    }
    if bytes.len() > MAX_CAPTURE_BYTES {
        bytes.truncate(MAX_CAPTURE_BYTES);
        bytes.extend_from_slice(b"\n[output truncated]\n");
    }
    String::from_utf8_lossy(&bytes).into_owned()
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

    #[test]
    fn explicit_config_root_wins_over_home() {
        let configured = OsStr::new("/tmp/velnor-config");
        assert_eq!(
            resolve_config_dir_from(Some(configured), Some(Path::new("/Users/test")))
                .expect("config root"),
            PathBuf::from("/tmp/velnor-config")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_config_root_is_user_local() {
        assert_eq!(
            resolve_config_dir_from(None, Some(Path::new("/Users/test"))).expect("config root"),
            PathBuf::from("/Users/test/Library/Application Support/velnor")
        );
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

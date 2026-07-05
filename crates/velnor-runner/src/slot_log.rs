//! Forensic log files for daemon slots and the daemon supervisor.
//!
//! Incident analysis must be possible from on-disk logs alone (master-plan
//! P1.9): every broker poll outcome, control message, token refresh, registry
//! reconcile check, and recycle decision is appended to a dedicated file under
//! `<config-dir>/logs/`, so a split-brain between "broker polling succeeds"
//! and "GitHub runner registry says offline/missing" is reconstructable after
//! the fact without raising journald verbosity.
//!
//! Writes are best-effort append-open-close per line: a logging failure must
//! never affect the runner, and short-lived handles survive log directory
//! removal, rotation, and daemon restarts without coordination.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Rotate a forensic log once it grows past this size; one rotated generation
/// (`<name>.1`) is kept, bounding disk use to ~2x per file.
const ROTATE_BYTES: u64 = 32 * 1024 * 1024;
const ROTATE_CHECK_INTERVAL: u32 = 128;

pub const LIFECYCLE_LOG: &str = "lifecycle.log";
pub const BROKER_LOG: &str = "broker.log";
pub const REGISTRY_LOG: &str = "registry.log";
pub const DAEMON_LOG: &str = "daemon.log";

#[derive(Debug, Default)]
struct LogFileState {
    dir_ensured: bool,
    writes_since_rotate_check: u32,
}

static LOG_FILE_STATES: OnceLock<Mutex<HashMap<PathBuf, LogFileState>>> = OnceLock::new();

/// Forensic logger bound to one log directory and one identity prefix
/// (e.g. `runner=velnor-fixture-slot-1 agent_id=4237 session=ab12cd34`).
#[derive(Clone, Debug)]
pub struct SlotForensics {
    log_dir: PathBuf,
    identity: String,
}

impl SlotForensics {
    pub fn new(log_dir: PathBuf, identity: String) -> Self {
        Self { log_dir, identity }
    }

    /// Replace the identity prefix (e.g. once the broker session id is known).
    pub fn set_identity(&mut self, identity: String) {
        self.identity = identity;
    }

    pub fn lifecycle(&self, message: &str) {
        self.append(LIFECYCLE_LOG, message);
    }

    pub fn broker(&self, message: &str) {
        self.append(BROKER_LOG, message);
    }

    pub fn registry(&self, message: &str) {
        self.append(REGISTRY_LOG, message);
    }

    fn append(&self, file_name: &str, message: &str) {
        append_log_line(&self.log_dir, file_name, &self.identity, message);
    }
}

/// Append one timestamped, identity-prefixed line to `<dir>/<file_name>`,
/// rotating first when the file is large. Never fails: forensic logging is
/// strictly additive to runner behavior. Every line is mirrored as a
/// structured tracing event so trace.jsonl/OTLP carry the same record.
pub fn append_log_line(dir: &Path, file_name: &str, identity: &str, message: &str) {
    tracing::info!(target: "velnor::forensic", file = file_name, identity, message);
    let path = dir.join(file_name);
    ensure_log_dir_once(&path, dir);
    if should_check_rotation(&path) {
        rotate_if_large(&path, ROTATE_BYTES);
    }
    let line = format_log_line(&now_rfc3339(), identity, message);
    match write_log_line(&path, &line) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            recreate_log_dir_and_retry(dir, &path, &line);
        }
        _ => {}
    }
}

fn write_log_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())
}

fn ensure_log_dir_once(path: &Path, dir: &Path) {
    if !needs_log_dir_ensure(path) {
        return;
    }
    if std::fs::create_dir_all(dir).is_ok() {
        mark_log_dir_ensured(path);
    }
}

fn recreate_log_dir_and_retry(dir: &Path, path: &Path, line: &str) {
    if std::fs::create_dir_all(dir).is_ok() {
        mark_log_dir_ensured(path);
        let _ = write_log_line(path, line);
    }
}

fn needs_log_dir_ensure(path: &Path) -> bool {
    let Ok(mut states) = log_file_states().lock() else {
        return true;
    };
    !states.entry(path.to_path_buf()).or_default().dir_ensured
}

fn mark_log_dir_ensured(path: &Path) {
    if let Ok(mut states) = log_file_states().lock() {
        states.entry(path.to_path_buf()).or_default().dir_ensured = true;
    }
}

fn should_check_rotation(path: &Path) -> bool {
    let Ok(mut states) = log_file_states().lock() else {
        return true;
    };
    let state = states.entry(path.to_path_buf()).or_default();
    if state.writes_since_rotate_check == 0 {
        state.writes_since_rotate_check = 1;
        true
    } else if state.writes_since_rotate_check + 1 >= ROTATE_CHECK_INTERVAL {
        state.writes_since_rotate_check = 0;
        false
    } else {
        state.writes_since_rotate_check += 1;
        false
    }
}

fn log_file_states() -> &'static Mutex<HashMap<PathBuf, LogFileState>> {
    LOG_FILE_STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn format_log_line(timestamp: &str, identity: &str, message: &str) -> String {
    // Forensic lines must stay one-line per event for grep/cut analysis.
    let single_line = message.replace('\n', " ");
    if identity.is_empty() {
        format!("{timestamp} {single_line}\n")
    } else {
        format!("{timestamp} [{identity}] {single_line}\n")
    }
}

fn rotate_if_large(path: &Path, max_bytes: u64) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.len() < max_bytes {
        return;
    }
    let rotated = rotated_path(path);
    let _ = std::fs::rename(path, rotated);
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".1");
    path.with_file_name(name)
}

pub fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown-time".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-slot-log-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn format_log_line_is_single_line_with_identity() {
        let line = format_log_line("2026-06-11T00:00:00Z", "runner=x agent_id=1", "a\nb");
        assert_eq!(line, "2026-06-11T00:00:00Z [runner=x agent_id=1] a b\n");
    }

    #[test]
    fn format_log_line_without_identity_omits_brackets() {
        let line = format_log_line("2026-06-11T00:00:00Z", "", "started");
        assert_eq!(line, "2026-06-11T00:00:00Z started\n");
    }

    #[test]
    fn append_creates_directory_and_appends() {
        let dir = unique_temp_dir("append");
        let logs = dir.join("logs");
        append_log_line(&logs, BROKER_LOG, "runner=t", "poll empty status=204");
        append_log_line(&logs, BROKER_LOG, "runner=t", "poll message status=200");
        let content = std::fs::read_to_string(logs.join(BROKER_LOG)).expect("read log");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("[runner=t] poll empty status=204"));
        assert!(lines[1].contains("poll message status=200"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_recreates_removed_directory_once() {
        let dir = unique_temp_dir("recreate");
        let logs = dir.join("logs");
        append_log_line(&logs, BROKER_LOG, "runner=t", "first");
        std::fs::remove_dir_all(&logs).expect("remove logs");

        append_log_line(&logs, BROKER_LOG, "runner=t", "second");

        let content = std::fs::read_to_string(logs.join(BROKER_LOG)).expect("read recreated log");
        assert!(content.contains("[runner=t] second"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_moves_large_file_aside() {
        let dir = unique_temp_dir("rotate");
        let path = dir.join("big.log");
        std::fs::write(&path, vec![b'x'; 64]).expect("write");
        rotate_if_large(&path, 16);
        assert!(!path.exists());
        assert!(dir.join("big.log.1").exists());
        // A second rotation replaces the old generation rather than growing.
        std::fs::write(&path, vec![b'y'; 64]).expect("write");
        rotate_if_large(&path, 16);
        let rotated = std::fs::read(dir.join("big.log.1")).expect("read rotated");
        assert_eq!(rotated, vec![b'y'; 64]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forensics_writes_to_named_streams() {
        let dir = unique_temp_dir("forensics");
        let logs = dir.join("logs");
        let mut forensics = SlotForensics::new(logs.clone(), "runner=a".to_string());
        forensics.lifecycle("session created");
        forensics.set_identity("runner=a session=12345678".to_string());
        forensics.broker("poll empty status=204 consecutive=1");
        forensics.registry("runner online id=7 busy=false");
        assert!(std::fs::read_to_string(logs.join(LIFECYCLE_LOG))
            .expect("lifecycle")
            .contains("session created"));
        let broker = std::fs::read_to_string(logs.join(BROKER_LOG)).expect("broker");
        assert!(broker.contains("session=12345678"));
        assert!(std::fs::read_to_string(logs.join(REGISTRY_LOG))
            .expect("registry")
            .contains("runner online id=7"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

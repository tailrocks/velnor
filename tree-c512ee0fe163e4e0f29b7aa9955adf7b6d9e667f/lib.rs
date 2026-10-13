//! Crash-safe action lifecycle journal.
//!
//! Each append is one SQLite transaction in WAL mode with
//! `synchronous=FULL`. The event payload and checksum are immutable; recovery
//! verifies every row before exposing it. SQLite is used as the embedded
//! transactional store because the Velnor workspace already pins bundled
//! SQLite and uses it for its existing durable control journal.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use velnor_action_model::{
    canonical_json_bytes, ActionKey, ActionState, ActionTiming, Digest, TrustClass,
};

pub const JOURNAL_SCHEMA_VERSION: u32 = 1;

/// One durable action record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRecord {
    pub action_key: ActionKey,
    pub state: ActionState,
    #[serde(default)]
    pub producer_lease_ref: Option<String>,
    #[serde(default)]
    pub consumer_run_ids: BTreeSet<String>,
    #[serde(default)]
    pub output_digests: BTreeMap<String, Digest>,
    pub timing: ActionTiming,
    #[serde(default)]
    pub worker_id: Option<String>,
    pub trust_class: TrustClass,
}

impl ActionRecord {
    /// Compute the key used to group lifecycle events.
    pub fn action_key_digest(&self) -> Result<Digest, JournalError> {
        Ok(self.action_key.digest()?)
    }
}

/// A journal row with its monotonic sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub sequence: i64,
    pub record: ActionRecord,
}

/// Journal failure.
#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal SQLite failure: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("journal JSON failure: {0}")]
    Json(#[from] serde_json::Error),
    #[error("journal model canonicalization failure: {0}")]
    Canonical(#[from] velnor_action_model::CanonicalizationError),
    #[error("journal state is invalid: {0}")]
    InvalidState(String),
    #[error("journal checksum mismatch at sequence {sequence}")]
    ChecksumMismatch { sequence: i64 },
    #[error("journal action key mismatch at sequence {sequence}")]
    KeyMismatch { sequence: i64 },
    #[error("journal state mismatch at sequence {sequence}")]
    StateMismatch { sequence: i64 },
    #[error("journal trust class disagrees with its action policy")]
    TrustClassMismatch,
}

/// Append-only, checksum-verified action journal.
pub struct ActionJournal {
    path: PathBuf,
    connection: Connection,
}

impl ActionJournal {
    /// Open or create a journal. `:memory:` is supported for unit tests.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        if path.as_os_str() != ":memory:" {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent).map_err(|error| {
                    JournalError::InvalidState(format!("create journal parent: {error}"))
                })?;
            }
        }
        let connection = Connection::open(&path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS action_journal_meta (
                 key TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS action_events (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 action_key_digest TEXT NOT NULL,
                 state TEXT NOT NULL,
                 record_json TEXT NOT NULL,
                 checksum TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS action_events_key_sequence
                 ON action_events(action_key_digest, sequence);
             INSERT OR IGNORE INTO action_journal_meta(key, value)
                 VALUES ('schema_version', '1');",
        )?;
        Ok(Self { path, connection })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one immutable lifecycle record atomically.
    pub fn append(&mut self, record: &ActionRecord) -> Result<i64, JournalError> {
        validate_record(record)?;
        let key_digest = record.action_key_digest()?;
        let record_json = String::from_utf8(canonical_json_bytes(record).expect("JSON is UTF-8"))
            .expect("serde_json emits UTF-8");
        let state = state_name(record.state);
        let checksum = checksum_for(&key_digest, state, &record_json);
        let transaction = self.connection.transaction()?;
        #[cfg(all(test, unix))]
        crash_test_if_requested(0);
        transaction.execute(
            "INSERT INTO action_events(action_key_digest, state, record_json, checksum)
             VALUES (?1, ?2, ?3, ?4)",
            params![key_digest.to_string(), state, record_json, checksum],
        )?;
        let sequence = transaction.last_insert_rowid();
        #[cfg(all(test, unix))]
        crash_test_if_requested(1);
        transaction.commit()?;
        #[cfg(all(test, unix))]
        crash_test_if_requested(2);
        Ok(sequence)
    }

    /// Verify and replay all committed records in append order.
    pub fn entries(&self) -> Result<Vec<JournalEntry>, JournalError> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, action_key_digest, state, record_json, checksum
             FROM action_events ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (sequence, key_digest, state, record_json, checksum) = row?;
            let actual_checksum = checksum_for_raw(&key_digest, &state, &record_json);
            if actual_checksum != checksum {
                return Err(JournalError::ChecksumMismatch { sequence });
            }
            let record: ActionRecord = serde_json::from_str(&record_json)?;
            validate_record(&record)?;
            if record.action_key_digest()?.to_string() != key_digest {
                return Err(JournalError::KeyMismatch { sequence });
            }
            if state != state_name(record.state) {
                return Err(JournalError::StateMismatch { sequence });
            }
            entries.push(JournalEntry { sequence, record });
        }
        Ok(entries)
    }

    /// Return the latest committed record for each action key.
    pub fn latest(&self) -> Result<BTreeMap<Digest, ActionRecord>, JournalError> {
        let mut latest = BTreeMap::new();
        for entry in self.entries()? {
            latest.insert(entry.record.action_key_digest()?, entry.record);
        }
        Ok(latest)
    }

    /// Read the schema version stamped by the journal.
    pub fn schema_version(&self) -> Result<u32, JournalError> {
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM action_journal_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        value
            .ok_or_else(|| JournalError::InvalidState("schema version is missing".into()))?
            .parse()
            .map_err(|_| JournalError::InvalidState("schema version is not numeric".into()))
    }
}

fn state_name(state: ActionState) -> &'static str {
    match state {
        ActionState::Planned => "planned",
        ActionState::Waiting => "waiting",
        ActionState::Leased => "leased",
        ActionState::Running => "running",
        ActionState::Publishing => "publishing",
        ActionState::Complete => "complete",
        ActionState::Failed => "failed",
        ActionState::Abandoned => "abandoned",
    }
}

fn validate_record(record: &ActionRecord) -> Result<(), JournalError> {
    if record.trust_class != record.action_key.execution_policy.trust_class {
        return Err(JournalError::TrustClassMismatch);
    }
    Ok(())
}

fn checksum_for(key_digest: &Digest, state: &str, record_json: &str) -> String {
    checksum_for_raw(key_digest.as_str(), state, record_json)
}

fn checksum_for_raw(key_digest: &str, state: &str, record_json: &str) -> String {
    let mut material = Vec::with_capacity(key_digest.len() + state.len() + record_json.len() + 2);
    material.extend_from_slice(key_digest.as_bytes());
    material.push(0);
    material.extend_from_slice(state.as_bytes());
    material.push(0);
    material.extend_from_slice(record_json.as_bytes());
    Digest::from_bytes(&material).to_string()
}

#[cfg(all(test, unix))]
fn crash_test_if_requested(stage: u16) {
    if std::env::var("VELNOR_ACTION_JOURNAL_CRASH_CHILD")
        .ok()
        .as_deref()
        != Some("1")
    {
        return;
    }
    let Some(configured) = std::env::var("VELNOR_ACTION_JOURNAL_CRASH_STAGE").ok() else {
        return;
    };
    if configured.parse::<u16>().ok() == Some(stage) {
        // SAFETY: the test child deliberately terminates itself to verify
        // SQLite recovery at each public append boundary.
        unsafe {
            libc::raise(libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn digest(seed: u8) -> Digest {
        Digest::from_bytes(&[seed])
    }

    fn record(seed: u8, state: ActionState) -> ActionRecord {
        ActionRecord {
            action_key: ActionKey {
                command_digest: digest(seed),
                input_root: digest(seed + 1),
                image_digest: digest(seed + 2),
                toolchain_digest: digest(seed + 3),
                platform: velnor_action_model::PlatformIdentity::new("linux", "x86_64", None),
                environment_digest: digest(seed + 4),
                dependency_outputs: vec![],
                execution_policy: velnor_action_model::ExecutionPolicy {
                    trust_class: TrustClass::Trusted,
                    ..Default::default()
                },
            },
            state,
            producer_lease_ref: Some(format!("lease-{seed}")),
            consumer_run_ids: BTreeSet::from([format!("run-{seed}")]),
            output_digests: BTreeMap::from([("root".into(), digest(seed + 5))]),
            timing: ActionTiming {
                started_at_ms: u64::from(seed),
                duration_ms: 10,
                cpu_ms: Some(8),
            },
            worker_id: Some("worker-a".into()),
            trust_class: TrustClass::Trusted,
        }
    }

    #[test]
    fn append_replay_and_latest_are_durable() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("journal.sqlite");
        let mut journal = ActionJournal::open(&path).unwrap();
        journal.append(&record(1, ActionState::Planned)).unwrap();
        journal.append(&record(1, ActionState::Complete)).unwrap();
        assert_eq!(journal.schema_version().unwrap(), JOURNAL_SCHEMA_VERSION);
        assert_eq!(journal.entries().unwrap().len(), 2);
        assert_eq!(
            journal.latest().unwrap().values().next().unwrap().state,
            ActionState::Complete
        );
        drop(journal);
        let reopened = ActionJournal::open(&path).unwrap();
        assert_eq!(reopened.entries().unwrap().len(), 2);
    }

    #[test]
    fn crash_recovery_reopen_fuzz_has_no_torn_records() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("journal.sqlite");
        for index in 0..1_000u16 {
            let mut journal = ActionJournal::open(&path).unwrap();
            journal
                .append(&record((index % 200) as u8, ActionState::Running))
                .unwrap();
            drop(journal);
            let reopened = ActionJournal::open(&path).unwrap();
            reopened.entries().unwrap();
        }
        let journal = ActionJournal::open(&path).unwrap();
        assert_eq!(journal.entries().unwrap().len(), 1_000);
    }

    #[cfg(unix)]
    #[test]
    fn sigkill_recovery_fuzz_1000_iterations() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("journal.sqlite");
        let mut journal = ActionJournal::open(&path).unwrap();
        journal.append(&record(1, ActionState::Planned)).unwrap();
        drop(journal);

        let mut expected_entries = 1;
        for index in 0..1_000u16 {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::crash_child", "--nocapture"])
                .env("VELNOR_ACTION_JOURNAL_CRASH_PATH", &path)
                .env("VELNOR_ACTION_JOURNAL_CRASH_INDEX", index.to_string())
                .env("VELNOR_ACTION_JOURNAL_CRASH_CHILD", "1")
                .env("VELNOR_ACTION_JOURNAL_CRASH_STAGE", (index % 3).to_string())
                .status()
                .unwrap();
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGKILL));
            let journal = ActionJournal::open(&path).unwrap();
            if index % 3 == 2 {
                expected_entries += 1;
            }
            assert_eq!(journal.entries().unwrap().len(), expected_entries);
        }
    }

    #[cfg(unix)]
    #[test]
    fn crash_child() {
        let Some(path) = std::env::var_os("VELNOR_ACTION_JOURNAL_CRASH_PATH") else {
            return;
        };
        let index = std::env::var("VELNOR_ACTION_JOURNAL_CRASH_INDEX")
            .unwrap()
            .parse::<u16>()
            .unwrap();
        let mut journal = ActionJournal::open(path).unwrap();
        let record = record((index % 200) as u8 + 10, ActionState::Running);
        journal.append(&record).unwrap();
    }

    #[test]
    fn checksum_rejects_tampered_record() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("journal.sqlite");
        let mut journal = ActionJournal::open(&path).unwrap();
        journal.append(&record(1, ActionState::Planned)).unwrap();
        journal
            .connection
            .execute("UPDATE action_events SET record_json = 'tampered'", [])
            .unwrap();
        assert!(matches!(
            journal.entries(),
            Err(JournalError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn trust_class_mismatch_is_rejected_before_append() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("journal.sqlite");
        let mut journal = ActionJournal::open(&path).unwrap();
        let mut invalid = record(1, ActionState::Planned);
        invalid.trust_class = TrustClass::Release;
        assert!(matches!(
            journal.append(&invalid),
            Err(JournalError::TrustClassMismatch)
        ));
        assert!(journal.entries().unwrap().is_empty());
    }
}

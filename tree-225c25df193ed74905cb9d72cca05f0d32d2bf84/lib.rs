//! Crash-safe action lifecycle journal.
//!
//! Each append is one SQLite transaction in WAL mode with
//! `synchronous=FULL`. The event payload and checksum are immutable; recovery
//! verifies every row before exposing it. SQLite is used as the embedded
//! transactional store because the Velnor workspace already pins bundled
//! SQLite and uses it for its existing durable control journal.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use velnor_action_model::{
    canonical_json_bytes, ActionKey, ActionState, ActionTiming, Clock, Digest, LogicalInstant,
    ProducerLease, TrustClass,
};
use velnor_model::{
    InvalidTelemetry, TelemetryEnvelope, TelemetryEnvelopeInput, TelemetryEvent, TelemetryFields,
    TelemetryLane, Timestamp,
};

pub mod supersession;

pub const JOURNAL_SCHEMA_VERSION: u32 = 3;

/// Production clock for durable lease deadlines.
///
/// Logical time is Unix milliseconds so a restarted process uses the same
/// epoch as the process that persisted the lease. Clones share the wake
/// channel used by lease managers that observe the same journal.
#[derive(Clone, Debug)]
pub struct TokioClock {
    expiry_wake: Arc<tokio::sync::Notify>,
}

impl TokioClock {
    /// Create a production clock with a shared expiry wake channel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            expiry_wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn unix_now() -> LogicalInstant {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        LogicalInstant::from_millis(millis)
    }
}

impl Default for TokioClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TokioClock {
    fn now(&self) -> LogicalInstant {
        Self::unix_now()
    }

    fn sleep_until(
        &self,
        deadline: LogicalInstant,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let wake = Arc::clone(&self.expiry_wake);
        let now = self.now();
        Box::pin(async move {
            let notified = wake.notified();
            if deadline == LogicalInstant::MAX {
                notified.await;
                return;
            }
            let delay_ms = deadline.as_millis().saturating_sub(now.as_millis());
            let _ = tokio::time::timeout(Duration::from_millis(delay_ms), notified).await;
        })
    }

    fn wake_expiry(&self) {
        self.expiry_wake.notify_one();
    }
}

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
    #[error("journal digest failure: {0}")]
    Digest(#[from] velnor_action_model::DigestError),
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
    #[error("lease is already held for action {action_key_digest}")]
    LeaseBusy { action_key_digest: Digest },
    #[error(
        "lease is abandonable for action {action_key_digest}; takeover belongs to the coordinator"
    )]
    LeaseAbandonable { action_key_digest: Digest },
    #[error("lease was already released for action {action_key_digest}")]
    LeaseReleased { action_key_digest: Digest },
    #[error("lease was not found for action {action_key_digest}")]
    LeaseNotFound { action_key_digest: Digest },
    #[error("lease owner or fencing generation does not match")]
    LeaseFenced,
    #[error("lease expired before the requested transition")]
    LeaseExpired,
    #[error("lease heartbeat is due at logical instant {next_at:?}")]
    HeartbeatTooSoon { next_at: LogicalInstant },
    #[error("lease duration must be positive and heartbeat must not exceed it")]
    InvalidLeaseDuration,
    #[error("lease owner must not be empty")]
    InvalidLeaseOwner,
}

/// Durable producer lease state visible to recovery and coordinator layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseStatus {
    Active,
    Abandoned,
    Released,
}

/// Secret-safe lease transition kind for the TASK-003 envelope adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseTransitionKind {
    Acquired,
    Renewed,
    Released,
    Abandoned,
    Expired,
}

/// Secret-safe observation emitted after a lease transition commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseTelemetryEvent {
    pub action_key_digest: Digest,
    pub generation: u64,
    pub at: LogicalInstant,
    pub kind: LeaseTransitionKind,
}

impl LeaseTelemetryEvent {
    /// Convert a committed transition into the TASK-003 envelope contract.
    ///
    /// The journal deliberately receives run/repository context from its
    /// adapter because those identities belong to the admission boundary,
    /// not the durable action store.
    pub fn into_envelope(
        &self,
        run_id: &str,
        repo: &str,
        trust_domain: &str,
        ts_logical: u64,
    ) -> Result<TelemetryEnvelope, InvalidTelemetry> {
        let fields = TelemetryFields::new(BTreeMap::from([
            ("generation".into(), serde_json::json!(self.generation)),
            ("logical_ms".into(), serde_json::json!(self.at.as_millis())),
        ]))?;
        TelemetryEnvelope::new(TelemetryEnvelopeInput {
            run_id,
            action_key_digest: Some(self.action_key_digest.as_str()),
            lane: TelemetryLane::Velnor,
            repo,
            trust_domain,
            event: self.kind.telemetry_event(),
            ts_logical,
            ts_wall: Timestamp::now(),
            fields,
        })
    }
}

impl LeaseTransitionKind {
    fn telemetry_event(self) -> TelemetryEvent {
        match self {
            Self::Acquired => TelemetryEvent::LeaseAcquired,
            Self::Renewed => TelemetryEvent::LeaseRenewed,
            Self::Released => TelemetryEvent::LeaseReleased,
            Self::Abandoned => TelemetryEvent::LeaseAbandoned,
            Self::Expired => TelemetryEvent::LeaseExpired,
        }
    }
}

type TelemetrySink = Box<dyn FnMut(&LeaseTelemetryEvent) + Send>;

/// Append-only, checksum-verified action journal.
pub struct ActionJournal {
    path: PathBuf,
    connection: Connection,
}

impl ActionJournal {
    /// Open or create a journal. `:memory:` is supported for unit tests.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        if path.as_os_str() != ":memory:"
            && let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                JournalError::InvalidState(format!("create journal parent: {error}"))
            })?;
        }
        let mut connection = Connection::open(&path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
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
             CREATE TABLE IF NOT EXISTS producer_leases (
                 action_key_digest TEXT PRIMARY KEY NOT NULL,
                 action_key_json TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 owner TEXT NOT NULL,
                 expires_at_ms INTEGER NOT NULL,
                 heartbeat_every_ms INTEGER NOT NULL,
                 lease_duration_ms INTEGER NOT NULL,
                 state TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS producer_leases_expiry
                 ON producer_leases(state, expires_at_ms);
             CREATE TABLE IF NOT EXISTS action_consumers (
                 action_key_digest TEXT NOT NULL,
                 run_id TEXT NOT NULL,
                 PRIMARY KEY(action_key_digest, run_id)
             );
             CREATE TABLE IF NOT EXISTS action_retention (
                 action_key_digest TEXT PRIMARY KEY NOT NULL,
                 retained_until_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS action_termination_claims (
                 action_key_digest TEXT PRIMARY KEY NOT NULL,
                 reason TEXT NOT NULL,
                 claimed_at_ms INTEGER NOT NULL,
                 completed INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS action_trust_revocations (
                 action_key_digest TEXT PRIMARY KEY NOT NULL,
                 reason TEXT NOT NULL,
                 revoked_at_ms INTEGER NOT NULL
             );
             INSERT OR IGNORE INTO action_journal_meta(key, value)
                 VALUES ('schema_version', '1');",
        )?;
        migrate_schema(&mut connection)?;
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

fn migrate_schema(connection: &mut Connection) -> Result<(), JournalError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: u32 = transaction
        .query_row(
            "SELECT value FROM action_journal_meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )?
        .parse()
        .map_err(|_| JournalError::InvalidState("schema version is not numeric".into()))?;

    match version {
        1 => {
            transaction.execute(
                "UPDATE action_journal_meta SET value = '2'
                 WHERE key = 'schema_version' AND value = '1'",
                [],
            )?;
            migrate_action_key_identity(&transaction)?;
            transaction.execute(
                "UPDATE action_journal_meta SET value = '3'
                 WHERE key = 'schema_version' AND value = '2'",
                [],
            )?;
        }
        2 => {
            migrate_action_key_identity(&transaction)?;
            transaction.execute(
                "UPDATE action_journal_meta SET value = '3'
                 WHERE key = 'schema_version' AND value = '2'",
                [],
            )?;
        }
        JOURNAL_SCHEMA_VERSION => {}
        version => {
            return Err(JournalError::InvalidState(format!(
                "unsupported journal schema version {version}"
            )));
        }
    }

    transaction.commit()?;
    Ok(())
}

fn migrate_action_key_identity(transaction: &Transaction<'_>) -> Result<(), JournalError> {
    let event_rows = {
        let mut statement = transaction.prepare(
            "SELECT sequence, action_key_digest, state, record_json, checksum
             FROM action_events ORDER BY sequence ASC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let lease_rows = {
        let mut statement = transaction.prepare(
            "SELECT action_key_digest, action_key_json
             FROM producer_leases ORDER BY action_key_digest ASC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut mappings = BTreeMap::new();
    let mut non_adoptable_legacy_identities = BTreeMap::new();
    let mut event_updates = Vec::with_capacity(event_rows.len());
    let mut lease_updates = Vec::with_capacity(lease_rows.len());

    for (sequence, key_digest, state, record_json, checksum) in event_rows {
        if checksum_for_raw(&key_digest, &state, &record_json) != checksum {
            return Err(JournalError::ChecksumMismatch { sequence });
        }
        let record: ActionRecord = serde_json::from_str(&record_json)?;
        validate_record(&record)?;
        if state != state_name(record.state) {
            return Err(JournalError::StateMismatch { sequence });
        }
        let current_digest = record.action_key_digest()?;
        let legacy_digest = legacy_action_key_digest(&record.action_key)?;
        if !action_key_digest_matches(&key_digest, &current_digest, &legacy_digest) {
            return Err(JournalError::KeyMismatch { sequence });
        }
        let current_record_json = canonical_json_string(&record)?;
        let current_checksum = checksum_for(&current_digest, &state, &current_record_json);
        register_action_key_mapping(&mut mappings, &key_digest, &current_digest)?;
        register_legacy_action_key_mapping(
            &mut mappings,
            &mut non_adoptable_legacy_identities,
            &record.action_key,
            &legacy_digest,
            &current_digest,
        )?;
        event_updates.push((sequence, current_record_json, current_checksum));
    }

    for (key_digest, action_key_json) in lease_rows {
        let action_key: ActionKey = serde_json::from_str(&action_key_json)?;
        let current_digest = action_key.digest()?;
        let legacy_digest = legacy_action_key_digest(&action_key)?;
        if !action_key_digest_matches(&key_digest, &current_digest, &legacy_digest) {
            return Err(JournalError::InvalidState(
                "stored lease action key digest mismatch".into(),
            ));
        }
        let current_action_key_json = canonical_json_string(&action_key)?;
        register_action_key_mapping(&mut mappings, &key_digest, &current_digest)?;
        register_legacy_action_key_mapping(
            &mut mappings,
            &mut non_adoptable_legacy_identities,
            &action_key,
            &legacy_digest,
            &current_digest,
        )?;
        lease_updates.push((key_digest, current_action_key_json));
    }

    validate_non_adoptable_legacy_collisions(&mappings, &non_adoptable_legacy_identities)?;
    validate_side_table_action_key_sources(
        transaction,
        &mappings,
        &non_adoptable_legacy_identities,
    )?;
    preflight_action_key_reconciliation(transaction, &mappings)?;

    for (sequence, record_json, checksum) in event_updates {
        transaction.execute(
            "UPDATE action_events SET record_json = ?1, checksum = ?2
             WHERE sequence = ?3",
            params![record_json, checksum, sequence],
        )?;
    }

    for (key_digest, action_key_json) in lease_updates {
        transaction.execute(
            "UPDATE producer_leases SET action_key_json = ?1
             WHERE action_key_digest = ?2",
            params![action_key_json, key_digest],
        )?;
    }

    for (old_digest, new_digest) in &mappings {
        if old_digest == new_digest {
            continue;
        }
        reconcile_action_events(transaction, old_digest, new_digest)?;
        reconcile_producer_lease(transaction, old_digest, new_digest)?;
        reconcile_action_consumers(transaction, old_digest, new_digest)?;
        reconcile_action_retention(transaction, old_digest, new_digest)?;
        reconcile_termination_claim(transaction, old_digest, new_digest)?;
        reconcile_trust_revocation(transaction, old_digest, new_digest)?;
    }
    Ok(())
}

fn validate_side_table_action_key_sources(
    transaction: &Transaction<'_>,
    mappings: &BTreeMap<String, String>,
    non_adoptable_legacy_identities: &BTreeMap<String, String>,
) -> Result<(), JournalError> {
    const DIGEST_TABLES: [(&str, &str); 4] = [
        (
            "action_consumers",
            "SELECT DISTINCT action_key_digest FROM action_consumers",
        ),
        (
            "action_retention",
            "SELECT DISTINCT action_key_digest FROM action_retention",
        ),
        (
            "action_termination_claims",
            "SELECT DISTINCT action_key_digest FROM action_termination_claims",
        ),
        (
            "action_trust_revocations",
            "SELECT DISTINCT action_key_digest FROM action_trust_revocations",
        ),
    ];

    for (table, query) in DIGEST_TABLES {
        let digests = {
            let mut statement = transaction.prepare(query)?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for digest in digests {
            if non_adoptable_legacy_identities.contains_key(&digest) {
                return Err(JournalError::InvalidState(format!(
                    "action key migration cannot resolve {table}.action_key_digest={digest}: non-adoptable ActionKey cannot use a legacy alias"
                )));
            }
            let has_action_key_source = mappings
                .iter()
                .any(|(source, current)| source == &digest || current == &digest);
            if !has_action_key_source {
                return Err(JournalError::InvalidState(format!(
                    "action key migration cannot resolve {table}.action_key_digest={digest}: no ActionKey source exists"
                )));
            }
        }
    }
    Ok(())
}

fn register_legacy_action_key_mapping(
    mappings: &mut BTreeMap<String, String>,
    non_adoptable_legacy_identities: &mut BTreeMap<String, String>,
    action_key: &ActionKey,
    legacy_digest: &Digest,
    current_digest: &Digest,
) -> Result<(), JournalError> {
    if action_key.execution_policy.adoptable {
        register_action_key_mapping(mappings, legacy_digest.as_str(), current_digest)
    } else {
        register_action_key_mapping(
            non_adoptable_legacy_identities,
            legacy_digest.as_str(),
            current_digest,
        )
    }
}

fn validate_non_adoptable_legacy_collisions(
    mappings: &BTreeMap<String, String>,
    non_adoptable_legacy_identities: &BTreeMap<String, String>,
) -> Result<(), JournalError> {
    for (legacy_digest, current_digest) in non_adoptable_legacy_identities {
        if let Some(mapped_digest) = mappings.get(legacy_digest) {
            if mapped_digest != current_digest {
                return Err(JournalError::InvalidState(format!(
                    "non-adoptable legacy action key digest {legacy_digest} conflicts with current digest {mapped_digest}"
                )));
            }
        }
    }
    Ok(())
}

fn preflight_action_key_reconciliation(
    transaction: &Transaction<'_>,
    mappings: &BTreeMap<String, String>,
) -> Result<(), JournalError> {
    for (old_digest, new_digest) in mappings {
        if old_digest == new_digest {
            continue;
        }
        let Some(old_lease) = stored_lease_row(transaction, old_digest)? else {
            continue;
        };
        let Some(new_lease) = stored_lease_row(transaction, new_digest)? else {
            continue;
        };
        let old_action: ActionKey = serde_json::from_str(&old_lease.action_key_json)?;
        let new_action: ActionKey = serde_json::from_str(&new_lease.action_key_json)?;
        if old_action != new_action
            || old_action.digest()?.as_str() != new_digest
            || new_action.digest()?.as_str() != new_digest
        {
            return Err(JournalError::InvalidState(format!(
                "conflicting producer leases for action key migration {old_digest} -> {new_digest}"
            )));
        }
    }
    Ok(())
}

fn reconcile_action_events(
    transaction: &Transaction<'_>,
    old_digest: &str,
    new_digest: &str,
) -> Result<(), JournalError> {
    transaction.execute(
        "UPDATE action_events SET action_key_digest = ?1 WHERE action_key_digest = ?2",
        params![new_digest, old_digest],
    )?;
    Ok(())
}

fn reconcile_producer_lease(
    transaction: &Transaction<'_>,
    old_digest: &str,
    new_digest: &str,
) -> Result<(), JournalError> {
    let Some(old_lease) = stored_lease_row(transaction, old_digest)? else {
        return Ok(());
    };
    let Some(new_lease) = stored_lease_row(transaction, new_digest)? else {
        transaction.execute(
            "UPDATE producer_leases SET action_key_digest = ?1 WHERE action_key_digest = ?2",
            params![new_digest, old_digest],
        )?;
        return Ok(());
    };

    let winner = if lease_row_precedence(&old_lease) > lease_row_precedence(&new_lease) {
        old_lease
    } else {
        new_lease
    };
    transaction.execute(
        "UPDATE producer_leases
         SET action_key_json = ?1, generation = ?2, owner = ?3,
             expires_at_ms = ?4, heartbeat_every_ms = ?5,
             lease_duration_ms = ?6, state = ?7
         WHERE action_key_digest = ?8",
        params![
            winner.action_key_json,
            i64::try_from(winner.generation).map_err(|_| {
                JournalError::InvalidState("lease generation exceeds SQLite integer range".into())
            })?,
            winner.owner,
            winner.expires_at_ms,
            i64::try_from(winner.heartbeat_every_ms).map_err(|_| {
                JournalError::InvalidState("lease heartbeat exceeds SQLite integer range".into())
            })?,
            i64::try_from(winner.lease_duration_ms).map_err(|_| {
                JournalError::InvalidState("lease duration exceeds SQLite integer range".into())
            })?,
            winner.state.as_str(),
            new_digest,
        ],
    )?;
    transaction.execute(
        "DELETE FROM producer_leases WHERE action_key_digest = ?1",
        [old_digest],
    )?;
    Ok(())
}

fn stored_lease_row(
    transaction: &Transaction<'_>,
    digest: &str,
) -> Result<Option<LeaseRow>, JournalError> {
    transaction
        .query_row(
            "SELECT generation, owner, expires_at_ms, heartbeat_every_ms,
                    lease_duration_ms, state, action_key_json
             FROM producer_leases WHERE action_key_digest = ?1",
            [digest],
            lease_row_from_query,
        )
        .optional()
        .map_err(JournalError::from)
}

fn lease_row_precedence(row: &LeaseRow) -> (u64, u8, i64, &str) {
    let state_rank = match row.state {
        LeaseState::Active => 0,
        LeaseState::Abandoned => 1,
        LeaseState::Released => 2,
    };
    (row.generation, state_rank, row.expires_at_ms, &row.owner)
}

fn reconcile_action_consumers(
    transaction: &Transaction<'_>,
    old_digest: &str,
    new_digest: &str,
) -> Result<(), JournalError> {
    transaction.execute(
        "INSERT OR IGNORE INTO action_consumers(action_key_digest, run_id)
         SELECT ?1, run_id FROM action_consumers WHERE action_key_digest = ?2",
        params![new_digest, old_digest],
    )?;
    transaction.execute(
        "DELETE FROM action_consumers WHERE action_key_digest = ?1",
        [old_digest],
    )?;
    Ok(())
}

fn reconcile_action_retention(
    transaction: &Transaction<'_>,
    old_digest: &str,
    new_digest: &str,
) -> Result<(), JournalError> {
    let old_deadline: Option<i64> = transaction
        .query_row(
            "SELECT retained_until_ms FROM action_retention WHERE action_key_digest = ?1",
            [old_digest],
            |row| row.get(0),
        )
        .optional()?;
    let Some(old_deadline) = old_deadline else {
        return Ok(());
    };
    let new_deadline: Option<i64> = transaction
        .query_row(
            "SELECT retained_until_ms FROM action_retention WHERE action_key_digest = ?1",
            [new_digest],
            |row| row.get(0),
        )
        .optional()?;
    match new_deadline {
        Some(new_deadline) => {
            transaction.execute(
                "UPDATE action_retention SET retained_until_ms = ?1
                 WHERE action_key_digest = ?2",
                params![old_deadline.max(new_deadline), new_digest],
            )?;
            transaction.execute(
                "DELETE FROM action_retention WHERE action_key_digest = ?1",
                [old_digest],
            )?;
        }
        None => {
            transaction.execute(
                "UPDATE action_retention SET action_key_digest = ?1
                 WHERE action_key_digest = ?2",
                params![new_digest, old_digest],
            )?;
        }
    }
    Ok(())
}

fn ensure_failed_action_retention(
    transaction: &Transaction<'_>,
    digest: &str,
    now_ms: i64,
) -> Result<(), JournalError> {
    transaction.execute(
        "INSERT OR IGNORE INTO action_termination_claims(
             action_key_digest, reason, claimed_at_ms, completed
         ) VALUES (?1, 'failed_action', ?2, 0)",
        params![digest, now_ms],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO action_retention(action_key_digest, retained_until_ms)
         VALUES (?1, ?2)",
        params![digest, now_ms],
    )?;
    transaction.execute(
        "UPDATE action_retention SET retained_until_ms = ?2
         WHERE action_key_digest = ?1 AND retained_until_ms > ?2",
        params![digest, now_ms],
    )?;
    Ok(())
}

struct TerminationClaimRow {
    reason: String,
    claimed_at_ms: i64,
    completed: i64,
}

fn validate_termination_claim_phase(phase: i64) -> Result<i64, JournalError> {
    match phase {
        0..=2 => Ok(phase),
        invalid => Err(JournalError::InvalidState(format!(
            "invalid termination claim phase {invalid}; expected 0, 1, or 2"
        ))),
    }
}

fn merge_termination_claim_phases(old_phase: i64, new_phase: i64) -> i64 {
    if old_phase == 1 || new_phase == 1 {
        1
    } else if old_phase == 2 || new_phase == 2 {
        2
    } else {
        0
    }
}

fn termination_claim_row(
    transaction: &Transaction<'_>,
    digest: &str,
) -> Result<Option<TerminationClaimRow>, JournalError> {
    transaction
        .query_row(
            "SELECT reason, claimed_at_ms, completed
             FROM action_termination_claims WHERE action_key_digest = ?1",
            [digest],
            |row| {
                Ok(TerminationClaimRow {
                    reason: row.get(0)?,
                    claimed_at_ms: row.get(1)?,
                    completed: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(JournalError::from)
}

fn reconcile_termination_claim(
    transaction: &Transaction<'_>,
    old_digest: &str,
    new_digest: &str,
) -> Result<(), JournalError> {
    let Some(old_claim) = termination_claim_row(transaction, old_digest)? else {
        if let Some(new_claim) = termination_claim_row(transaction, new_digest)? {
            validate_termination_claim_phase(new_claim.completed)?;
        }
        return Ok(());
    };
    let Some(new_claim) = termination_claim_row(transaction, new_digest)? else {
        validate_termination_claim_phase(old_claim.completed)?;
        transaction.execute(
            "UPDATE action_termination_claims SET action_key_digest = ?1
             WHERE action_key_digest = ?2",
            params![new_digest, old_digest],
        )?;
        return Ok(());
    };
    validate_termination_claim_phase(old_claim.completed)?;
    validate_termination_claim_phase(new_claim.completed)?;
    let old_is_newer = old_claim.claimed_at_ms > new_claim.claimed_at_ms;
    let (reason, claimed_at_ms) = if old_is_newer {
        (old_claim.reason, old_claim.claimed_at_ms)
    } else {
        (new_claim.reason, new_claim.claimed_at_ms)
    };
    transaction.execute(
        "UPDATE action_termination_claims
         SET reason = ?1, claimed_at_ms = ?2,
             completed = ?3
         WHERE action_key_digest = ?4",
        params![
            reason,
            claimed_at_ms,
            merge_termination_claim_phases(old_claim.completed, new_claim.completed),
            new_digest,
        ],
    )?;
    transaction.execute(
        "DELETE FROM action_termination_claims WHERE action_key_digest = ?1",
        [old_digest],
    )?;
    Ok(())
}

struct TrustRevocationRow {
    reason: String,
    revoked_at_ms: i64,
}

fn trust_revocation_row(
    transaction: &Transaction<'_>,
    digest: &str,
) -> Result<Option<TrustRevocationRow>, JournalError> {
    transaction
        .query_row(
            "SELECT reason, revoked_at_ms
             FROM action_trust_revocations WHERE action_key_digest = ?1",
            [digest],
            |row| {
                Ok(TrustRevocationRow {
                    reason: row.get(0)?,
                    revoked_at_ms: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(JournalError::from)
}

fn reconcile_trust_revocation(
    transaction: &Transaction<'_>,
    old_digest: &str,
    new_digest: &str,
) -> Result<(), JournalError> {
    let Some(old_revocation) = trust_revocation_row(transaction, old_digest)? else {
        return Ok(());
    };
    let Some(new_revocation) = trust_revocation_row(transaction, new_digest)? else {
        transaction.execute(
            "UPDATE action_trust_revocations SET action_key_digest = ?1
             WHERE action_key_digest = ?2",
            params![new_digest, old_digest],
        )?;
        return Ok(());
    };
    let old_is_earlier = old_revocation.revoked_at_ms < new_revocation.revoked_at_ms;
    let (reason, revoked_at_ms) = if old_is_earlier {
        (old_revocation.reason, old_revocation.revoked_at_ms)
    } else {
        (new_revocation.reason, new_revocation.revoked_at_ms)
    };
    transaction.execute(
        "UPDATE action_trust_revocations SET reason = ?1, revoked_at_ms = ?2
         WHERE action_key_digest = ?3",
        params![reason, revoked_at_ms, new_digest],
    )?;
    transaction.execute(
        "DELETE FROM action_trust_revocations WHERE action_key_digest = ?1",
        [old_digest],
    )?;
    Ok(())
}

fn action_key_digest_matches(persisted: &str, current: &Digest, legacy: &Digest) -> bool {
    persisted == current.as_str() || persisted == legacy.as_str()
}

fn register_action_key_mapping(
    mappings: &mut BTreeMap<String, String>,
    old_digest: &str,
    current_digest: &Digest,
) -> Result<(), JournalError> {
    if let Some(existing) = mappings.get(old_digest) {
        if existing != current_digest.as_str() {
            return Err(JournalError::InvalidState(format!(
                "action key digest {old_digest} maps to multiple current digests"
            )));
        }
    } else {
        mappings.insert(old_digest.to_owned(), current_digest.to_string());
    }
    Ok(())
}

fn legacy_action_key_digest(action_key: &ActionKey) -> Result<Digest, JournalError> {
    let mut value = serde_json::to_value(action_key)?;
    let policy = value
        .get_mut("execution_policy")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| JournalError::InvalidState("action key policy is missing".into()))?;
    policy.remove("adoptable");
    Ok(Digest::from_bytes(&canonical_json_bytes(&value)?))
}

fn canonical_json_string<T: Serialize>(value: &T) -> Result<String, JournalError> {
    String::from_utf8(canonical_json_bytes(value)?).map_err(|error| {
        JournalError::InvalidState(format!("canonical JSON is not UTF-8: {error}"))
    })
}

/// Durable lease coordinator backed by an [`ActionJournal`].
///
/// Lease deadlines are persisted as logical milliseconds. Expiry waiting is
/// delegated to the injected clock, so production callers can use an
/// event-driven timer and tests can advance time without wall-clock sleeps.
pub struct LeaseManager<C: Clock> {
    journal: ActionJournal,
    clock: C,
    telemetry_sink: Option<TelemetrySink>,
    pending_telemetry: VecDeque<LeaseTelemetryEvent>,
}

impl<C: Clock> LeaseManager<C> {
    /// Open a lease manager with the supplied journal path and clock.
    pub fn open(path: impl AsRef<Path>, clock: C) -> Result<Self, JournalError> {
        let mut manager = Self {
            journal: ActionJournal::open(path)?,
            clock,
            telemetry_sink: None,
            pending_telemetry: VecDeque::new(),
        };
        manager.expire_due_at(manager.clock.now())?;
        Ok(manager)
    }

    /// Wrap an already-open journal with a clock.
    pub fn from_journal(journal: ActionJournal, clock: C) -> Result<Self, JournalError> {
        let mut manager = Self {
            journal,
            clock,
            telemetry_sink: None,
            pending_telemetry: VecDeque::new(),
        };
        manager.expire_due_at(manager.clock.now())?;
        Ok(manager)
    }

    /// Borrow the underlying journal for action-record operations.
    #[must_use]
    pub fn journal(&self) -> &ActionJournal {
        &self.journal
    }

    /// Append an action lifecycle record through the manager-owned journal.
    pub fn append_action(&mut self, record: &ActionRecord) -> Result<i64, JournalError> {
        self.journal.append(record)
    }

    /// Append a terminal action record and release its producer lease in one
    /// SQLite transaction. Keeping the event and fencing transition atomic
    /// prevents a stale producer from publishing a completion after a new
    /// generation has taken over the same action.
    pub fn append_action_and_release(
        &mut self,
        lease: &ProducerLease,
        record: &ActionRecord,
    ) -> Result<i64, JournalError> {
        if record.action_key != lease.action {
            return Err(JournalError::InvalidState(
                "terminal action record does not match producer lease".into(),
            ));
        }
        if record.state != ActionState::Complete {
            return Err(JournalError::InvalidState(
                "terminal lease publication requires a complete action record".into(),
            ));
        }
        validate_record(record)?;
        let now = self.clock.now();
        self.expire_due_at(now)?;
        let digest = lease.action.digest()?;
        let record_json = String::from_utf8(canonical_json_bytes(record).expect("JSON is UTF-8"))
            .expect("serde_json emits UTF-8");
        let state = state_name(record.state);
        let checksum = checksum_for(&digest, state, &record_json);
        let now_ms = sqlite_integer(now.as_millis())?;
        let transaction = self
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = transaction
            .query_row(
                "SELECT generation, owner, expires_at_ms, heartbeat_every_ms,
                        lease_duration_ms, state, action_key_json
                 FROM producer_leases WHERE action_key_digest = ?1",
                [digest.to_string()],
                lease_row_from_query,
            )
            .optional()?
            .ok_or(JournalError::LeaseNotFound {
                action_key_digest: digest.clone(),
            })?;
        verify_stored_action_key(&row.action_key_json, &digest)?;
        if row.state != LeaseState::Active
            || row.generation != lease.generation
            || row.owner != lease.owner
        {
            return Err(JournalError::LeaseFenced);
        }
        if row.expires_at_ms <= now_ms {
            return Err(JournalError::LeaseExpired);
        }
        if let Some(claim) = termination_claim_row(&transaction, digest.as_str())? {
            validate_termination_claim_phase(claim.completed)?;
            return Err(JournalError::InvalidState(
                "action publication is blocked by a termination claim".into(),
            ));
        }
        if trust_revocation_row(&transaction, digest.as_str())?.is_some() {
            return Err(JournalError::InvalidState(
                "action publication is blocked by trust revocation".into(),
            ));
        }
        let sequence = insert_action_record(&transaction, &digest, state, &record_json, &checksum)?;
        let changed = transaction.execute(
            "UPDATE producer_leases SET state = 'released'
             WHERE action_key_digest = ?1 AND generation = ?2 AND owner = ?3
               AND state = 'active' AND expires_at_ms > ?4",
            params![
                digest.to_string(),
                sqlite_integer(lease.generation)?,
                lease.owner,
                now_ms,
            ],
        )?;
        if changed != 1 {
            return Err(JournalError::LeaseFenced);
        }
        transaction.commit()?;
        self.clock.wake_expiry();
        self.emit_telemetry(LeaseTelemetryEvent {
            action_key_digest: digest,
            generation: lease.generation,
            at: now,
            kind: LeaseTransitionKind::Released,
        });
        Ok(sequence)
    }

    /// Read the latest lifecycle record for one action key.
    pub fn latest_action(&self, action: &ActionKey) -> Result<Option<ActionRecord>, JournalError> {
        let digest = action.digest()?;
        Ok(self.journal.latest()?.remove(&digest))
    }

    /// Return the injected clock.
    #[must_use]
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Install a TASK-003 adapter. Events recovered during open are flushed in
    /// commit order before newly emitted transitions.
    pub fn set_telemetry_sink(&mut self, sink: impl FnMut(&LeaseTelemetryEvent) + Send + 'static) {
        self.telemetry_sink = Some(Box::new(sink));
        let pending = self.pending_telemetry.drain(..).collect::<Vec<_>>();
        for event in pending {
            self.emit_telemetry(event);
        }
    }

    /// Drain events when the caller owns a polling adapter instead of a sink.
    pub fn drain_telemetry(&mut self) -> Vec<LeaseTelemetryEvent> {
        self.pending_telemetry.drain(..).collect()
    }

    /// Return the recovered status for one action, if a lease exists.
    pub fn lease_status(&self, action: &ActionKey) -> Result<Option<LeaseStatus>, JournalError> {
        let digest = action.digest()?;
        self.journal
            .connection
            .query_row(
                "SELECT state FROM producer_leases WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|state| parse_lease_status(&state))
            .transpose()
    }

    /// Return whether an action has a live active lease.
    pub fn has_active_lease(&mut self, action: &ActionKey) -> Result<bool, JournalError> {
        let now = self.clock.now();
        self.expire_due_at(now)?;
        let now_ms = sqlite_integer(now.as_millis())?;
        let digest = action.digest()?;
        let lease = self
            .journal
            .connection
            .query_row(
                "SELECT state, expires_at_ms FROM producer_leases
                 WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        Ok(lease.is_some_and(|(state, expires_at_ms)| state == "active" && expires_at_ms > now_ms))
    }

    /// Acquire the single producer lease for an action.
    pub fn acquire(
        &mut self,
        action: &ActionKey,
        owner: impl Into<String>,
        lease_duration_ms: u64,
        heartbeat_every_ms: u64,
    ) -> Result<ProducerLease, JournalError> {
        let owner = owner.into();
        validate_lease_inputs(&owner, lease_duration_ms, heartbeat_every_ms)?;
        let now = self.clock.now();
        self.expire_due_at(now)?;
        let action_key_digest = action.digest()?;
        let action_key_json =
            String::from_utf8(canonical_json_bytes(action).expect("JSON is UTF-8"))
                .expect("serde_json emits UTF-8");
        let now_ms = sqlite_integer(now.as_millis())?;
        let expires_at = now.saturating_add(lease_duration_ms);
        let expires_at_ms = sqlite_integer(expires_at.as_millis())?;
        let generation = {
            let transaction = self
                .journal
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row = transaction
                .query_row(
                    "SELECT generation, owner, expires_at_ms, heartbeat_every_ms,
                            lease_duration_ms, state, action_key_json
                     FROM producer_leases WHERE action_key_digest = ?1",
                    [action_key_digest.to_string()],
                    lease_row_from_query,
                )
                .optional()?;
            let generation = match row {
                None => 1,
                Some(row) => {
                    verify_stored_action_key(&row.action_key_json, &action_key_digest)?;
                    if row.state != LeaseState::Active {
                        match row.state {
                            LeaseState::Abandoned => {
                                return Err(JournalError::LeaseAbandonable { action_key_digest });
                            }
                            LeaseState::Released => {
                                return Err(JournalError::LeaseReleased { action_key_digest });
                            }
                            LeaseState::Active => {
                                return Err(JournalError::InvalidState(
                                    "active lease state classification failed".into(),
                                ));
                            }
                        }
                    } else if row.expires_at_ms > now_ms {
                        return Err(JournalError::LeaseBusy { action_key_digest });
                    } else {
                        return Err(JournalError::LeaseExpired);
                    }
                }
            };
            transaction.execute(
                "INSERT INTO producer_leases(
                     action_key_digest, action_key_json, generation, owner,
                     expires_at_ms, heartbeat_every_ms, lease_duration_ms, state
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active')",
                rusqlite::params![
                    action_key_digest.to_string(),
                    action_key_json,
                    sqlite_integer(generation)?,
                    owner,
                    expires_at_ms,
                    sqlite_integer(heartbeat_every_ms)?,
                    sqlite_integer(lease_duration_ms)?,
                ],
            )?;
            transaction.commit()?;
            generation
        };
        self.clock.wake_expiry();
        let lease = ProducerLease {
            action: action.clone(),
            generation,
            owner,
            expires_at,
            heartbeat_every: heartbeat_every_ms,
            lease_duration: lease_duration_ms,
        };
        self.emit_telemetry(LeaseTelemetryEvent {
            action_key_digest,
            generation,
            at: now,
            kind: LeaseTransitionKind::Acquired,
        });
        Ok(lease)
    }

    /// Renew a live lease using its fencing token.
    pub fn renew(&mut self, lease: &mut ProducerLease) -> Result<(), JournalError> {
        let now = self.clock.now();
        self.expire_due_at(now)?;
        let digest = lease.action.digest()?;
        let transaction = self
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = transaction
            .query_row(
                "SELECT generation, owner, expires_at_ms, heartbeat_every_ms,
                        lease_duration_ms, state, action_key_json
                 FROM producer_leases WHERE action_key_digest = ?1",
                [digest.to_string()],
                lease_row_from_query,
            )
            .optional()?
            .ok_or(JournalError::LeaseNotFound {
                action_key_digest: digest.clone(),
            })?;
        verify_stored_action_key(&row.action_key_json, &digest)?;
        if row.state != LeaseState::Active
            || row.generation != lease.generation
            || row.owner != lease.owner
        {
            return Err(JournalError::LeaseFenced);
        }
        let now_ms = sqlite_integer(now.as_millis())?;
        if row.expires_at_ms <= now_ms {
            return Err(JournalError::LeaseExpired);
        }
        let lease_duration_ms = sqlite_integer(row.lease_duration_ms)?;
        let last_heartbeat_ms = row
            .expires_at_ms
            .checked_sub(lease_duration_ms)
            .ok_or_else(|| {
                JournalError::InvalidState("lease deadline precedes its duration".into())
            })?;
        let next_heartbeat_ms = last_heartbeat_ms
            .checked_add(sqlite_integer(row.heartbeat_every_ms)?)
            .ok_or_else(|| {
                JournalError::InvalidState("lease heartbeat deadline overflow".into())
            })?;
        if now_ms < next_heartbeat_ms {
            return Err(JournalError::HeartbeatTooSoon {
                next_at: logical_instant_from_sql(next_heartbeat_ms)?,
            });
        }
        let expires_at = now.saturating_add(row.lease_duration_ms);
        let changed = transaction.execute(
            "UPDATE producer_leases
             SET expires_at_ms = ?1
             WHERE action_key_digest = ?2 AND generation = ?3 AND owner = ?4
               AND state = 'active' AND expires_at_ms > ?5",
            rusqlite::params![
                sqlite_integer(expires_at.as_millis())?,
                digest.to_string(),
                sqlite_integer(lease.generation)?,
                lease.owner,
                now_ms,
            ],
        )?;
        if changed != 1 {
            return Err(JournalError::LeaseFenced);
        }
        transaction.commit()?;
        self.clock.wake_expiry();
        lease.expires_at = expires_at;
        lease.heartbeat_every = row.heartbeat_every_ms;
        lease.lease_duration = row.lease_duration_ms;
        self.emit_telemetry(LeaseTelemetryEvent {
            action_key_digest: digest,
            generation: lease.generation,
            at: now,
            kind: LeaseTransitionKind::Renewed,
        });
        Ok(())
    }

    /// Mark a live lease complete or failed, fencing its owner from reuse.
    pub fn release(&mut self, lease: &ProducerLease) -> Result<(), JournalError> {
        self.transition_lease(lease, LeaseState::Released)
    }

    /// Mark a live lease abandoned and increment its fencing generation.
    pub fn abandon(&mut self, lease: &ProducerLease) -> Result<(), JournalError> {
        self.transition_lease(lease, LeaseState::Abandoned)
    }

    /// Mark all expired active leases abandoned and increment each token once.
    pub fn expire_due(&mut self) -> Result<usize, JournalError> {
        self.expire_due_at(self.clock.now())
    }

    /// Wait for the next persisted deadline, then expire due leases.
    pub async fn expire_next(&mut self) -> Result<usize, JournalError> {
        loop {
            // MAX is the durable event-driven idle wait. A lease acquired by
            // another manager wakes the shared clock, after which this loop
            // re-reads SQLite instead of polling or returning prematurely.
            let deadline = self.next_expiry()?.unwrap_or(LogicalInstant::MAX);
            self.clock.sleep_until(deadline).await;
            let expired = self.expire_due()?;
            if expired > 0 {
                return Ok(expired);
            }
        }
    }

    /// Return the earliest active persisted expiry deadline.
    ///
    /// The indexed SQLite `MIN` query is the durable deadline heap: recovery
    /// reconstructs the next wake-up from committed state instead of trusting
    /// a process-local heap that could disappear with the daemon.
    pub fn next_expiry(&self) -> Result<Option<LogicalInstant>, JournalError> {
        let value: Option<i64> = self.journal.connection.query_row(
            "SELECT MIN(expires_at_ms) FROM producer_leases WHERE state = 'active'",
            [],
            |row| row.get(0),
        )?;
        value.map(logical_instant_from_sql).transpose()
    }

    /// Attach a run to an action's durable consumer set.
    pub fn attach_consumer(
        &mut self,
        action: &ActionKey,
        run_id: &str,
    ) -> Result<bool, JournalError> {
        validate_run_id(run_id)?;
        let digest = action.digest()?;
        let transaction = self
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO action_consumers(action_key_digest, run_id)
             VALUES (?1, ?2)",
            rusqlite::params![digest.to_string(), run_id],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    /// Detach a run from an action's durable consumer set.
    pub fn detach_consumer(
        &mut self,
        action: &ActionKey,
        run_id: &str,
    ) -> Result<bool, JournalError> {
        validate_run_id(run_id)?;
        let digest = action.digest()?;
        let transaction = self
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "DELETE FROM action_consumers WHERE action_key_digest = ?1 AND run_id = ?2",
            rusqlite::params![digest.to_string(), run_id],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    /// Count live consumers attached to an action.
    pub fn live_consumer_count(&self, action: &ActionKey) -> Result<u64, JournalError> {
        let digest = action.digest()?;
        let count: i64 = self.journal.connection.query_row(
            "SELECT COUNT(*) FROM action_consumers WHERE action_key_digest = ?1",
            [digest.to_string()],
            |row| row.get(0),
        )?;
        u64::try_from(count)
            .map_err(|_| JournalError::InvalidState("consumer count is negative".into()))
    }

    fn transition_lease(
        &mut self,
        lease: &ProducerLease,
        state: LeaseState,
    ) -> Result<(), JournalError> {
        let now = self.clock.now();
        self.expire_due_at(now)?;
        let digest = lease.action.digest()?;
        let transaction = self
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let kind = match state {
            LeaseState::Abandoned => LeaseTransitionKind::Abandoned,
            LeaseState::Released => LeaseTransitionKind::Released,
            LeaseState::Active => {
                return Err(JournalError::InvalidState(
                    "invalid lease transition".into(),
                ));
            }
        };
        let generation = match state {
            LeaseState::Abandoned => lease
                .generation
                .checked_add(1)
                .ok_or_else(|| JournalError::InvalidState("lease generation overflow".into()))?,
            LeaseState::Released => lease.generation,
            LeaseState::Active => {
                return Err(JournalError::InvalidState(
                    "invalid lease transition".into(),
                ));
            }
        };
        let changed = transaction.execute(
            "UPDATE producer_leases SET generation = ?1, state = ?2
             WHERE action_key_digest = ?3 AND generation = ?4 AND owner = ?5
               AND state = 'active'",
            rusqlite::params![
                sqlite_integer(generation)?,
                state.as_str(),
                digest.to_string(),
                sqlite_integer(lease.generation)?,
                lease.owner,
            ],
        )?;
        if changed != 1 {
            return Err(JournalError::LeaseFenced);
        }
        if state == LeaseState::Abandoned {
            ensure_failed_action_retention(
                &transaction,
                &digest.to_string(),
                sqlite_integer(now.as_millis())?,
            )?;
        }
        transaction.commit()?;
        self.clock.wake_expiry();
        self.emit_telemetry(LeaseTelemetryEvent {
            action_key_digest: digest,
            generation,
            at: now,
            kind,
        });
        Ok(())
    }

    fn expire_due_at(&mut self, now: LogicalInstant) -> Result<usize, JournalError> {
        let now_ms = sqlite_integer(now.as_millis())?;
        let transaction = self
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT action_key_digest, generation FROM producer_leases
             WHERE state = 'active' AND expires_at_ms <= ?1",
        )?;
        let rows = statement
            .query_map([now_ms], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut expired = 0;
        let mut events = Vec::with_capacity(rows.len());
        for (digest, generation) in rows {
            let generation = u64::try_from(generation)
                .map_err(|_| JournalError::InvalidState("lease generation is negative".into()))?;
            let next_generation = generation
                .checked_add(1)
                .ok_or_else(|| JournalError::InvalidState("lease generation overflow".into()))?;
            let changed = transaction.execute(
                "UPDATE producer_leases SET generation = ?1, state = 'abandoned'
                 WHERE action_key_digest = ?2 AND generation = ?3
                   AND state = 'active' AND expires_at_ms <= ?4",
                rusqlite::params![
                    sqlite_integer(next_generation)?,
                    digest,
                    sqlite_integer(generation)?,
                    now_ms,
                ],
            )?;
            if changed == 1 {
                ensure_failed_action_retention(&transaction, &digest, now_ms)?;
                expired += 1;
                events.push(LeaseTelemetryEvent {
                    action_key_digest: Digest::parse(&digest)?,
                    generation: next_generation,
                    at: now,
                    kind: LeaseTransitionKind::Expired,
                });
            }
        }
        transaction.commit()?;
        if expired > 0 {
            self.clock.wake_expiry();
        }
        for event in events {
            self.emit_telemetry(event);
        }
        Ok(expired)
    }

    fn emit_telemetry(&mut self, event: LeaseTelemetryEvent) {
        if let Some(mut sink) = self.telemetry_sink.take() {
            let delivered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                sink(&event);
            }))
            .is_ok();
            self.telemetry_sink = Some(sink);
            if !delivered {
                self.pending_telemetry.push_back(event);
            }
        } else {
            self.pending_telemetry.push_back(event);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseState {
    Active,
    Abandoned,
    Released,
}

impl LeaseState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Abandoned => "abandoned",
            Self::Released => "released",
        }
    }
}

struct LeaseRow {
    generation: u64,
    owner: String,
    expires_at_ms: i64,
    heartbeat_every_ms: u64,
    lease_duration_ms: u64,
    state: LeaseState,
    action_key_json: String,
}

fn lease_row_from_query(row: &rusqlite::Row<'_>) -> rusqlite::Result<LeaseRow> {
    let state = match row.get::<_, String>(5)?.as_str() {
        "active" => LeaseState::Active,
        "abandoned" => LeaseState::Abandoned,
        "released" => LeaseState::Released,
        value => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown lease state {value}"),
                )),
            ));
        }
    };
    Ok(LeaseRow {
        generation: u64::try_from(row.get::<_, i64>(0)?).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Integer,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "negative lease generation",
                )),
            )
        })?,
        owner: row.get(1)?,
        expires_at_ms: row.get(2)?,
        heartbeat_every_ms: u64::try_from(row.get::<_, i64>(3)?).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Integer,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "negative heartbeat interval",
                )),
            )
        })?,
        lease_duration_ms: u64::try_from(row.get::<_, i64>(4)?).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Integer,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "negative lease duration",
                )),
            )
        })?,
        state,
        action_key_json: row.get(6)?,
    })
}

fn parse_lease_status(value: &str) -> Result<LeaseStatus, JournalError> {
    match value {
        "active" => Ok(LeaseStatus::Active),
        "abandoned" => Ok(LeaseStatus::Abandoned),
        "released" => Ok(LeaseStatus::Released),
        value => Err(JournalError::InvalidState(format!(
            "unknown lease state {value}"
        ))),
    }
}

fn validate_lease_inputs(
    owner: &str,
    lease_duration_ms: u64,
    heartbeat_every_ms: u64,
) -> Result<(), JournalError> {
    if owner.is_empty() {
        return Err(JournalError::InvalidLeaseOwner);
    }
    if lease_duration_ms == 0 || heartbeat_every_ms == 0 || heartbeat_every_ms > lease_duration_ms {
        return Err(JournalError::InvalidLeaseDuration);
    }
    Ok(())
}

fn validate_run_id(run_id: &str) -> Result<(), JournalError> {
    if run_id.is_empty() {
        Err(JournalError::InvalidState(
            "consumer run ID must not be empty".into(),
        ))
    } else {
        Ok(())
    }
}

fn sqlite_integer(value: u64) -> Result<i64, JournalError> {
    i64::try_from(value).map_err(|_| {
        JournalError::InvalidState("logical lease value exceeds SQLite integer range".into())
    })
}

fn logical_instant_from_sql(value: i64) -> Result<LogicalInstant, JournalError> {
    u64::try_from(value)
        .map(LogicalInstant::from_millis)
        .map_err(|_| JournalError::InvalidState("lease deadline is negative".into()))
}

fn verify_stored_action_key(json: &str, expected: &Digest) -> Result<(), JournalError> {
    let stored: ActionKey = serde_json::from_str(json)?;
    if stored.digest()?.to_string() != expected.to_string() {
        return Err(JournalError::InvalidState(
            "stored lease action key digest mismatch".into(),
        ));
    }
    Ok(())
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

fn insert_action_record(
    transaction: &Transaction<'_>,
    key_digest: &Digest,
    state: &str,
    record_json: &str,
    checksum: &str,
) -> Result<i64, JournalError> {
    transaction.execute(
        "INSERT INTO action_events(action_key_digest, state, record_json, checksum)
         VALUES (?1, ?2, ?3, ?4)",
        params![key_digest.to_string(), state, record_json, checksum],
    )?;
    Ok(transaction.last_insert_rowid())
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
    use std::{
        future::Future,
        io::{BufRead, BufReader, Read, Write},
        pin::Pin,
        process::{Command, Stdio},
        sync::{Arc, Barrier, Mutex},
        thread,
    };
    use tempfile::TempDir;

    #[derive(Clone, Default)]
    struct TestClock {
        now_ms: Arc<Mutex<u64>>,
        wake: Arc<tokio::sync::Notify>,
    }

    impl TestClock {
        fn advance(&self, millis: u64) {
            let mut now = self.now_ms.lock().unwrap();
            *now = now.saturating_add(millis);
            drop(now);
            self.wake.notify_one();
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> LogicalInstant {
            LogicalInstant::from_millis(*self.now_ms.lock().unwrap())
        }

        fn sleep_until(
            &self,
            deadline: LogicalInstant,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            let clock = self.clone();
            Box::pin(async move {
                let notified = clock.wake.notified();
                if clock.now() < deadline {
                    notified.await;
                }
            })
        }

        fn wake_expiry(&self) {
            self.wake.notify_one();
        }
    }

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

    fn action(seed: u8) -> ActionKey {
        record(seed, ActionState::Planned).action_key
    }

    fn record_with_adoptable(seed: u8, state: ActionState, adoptable: bool) -> ActionRecord {
        let mut record = record(seed, state);
        record.action_key.execution_policy.adoptable = adoptable;
        record
    }

    fn remove_adoptable(action_key: &mut serde_json::Value) {
        action_key
            .get_mut("execution_policy")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("adoptable");
    }

    fn canonical_value_json(value: &serde_json::Value) -> String {
        String::from_utf8(canonical_json_bytes(value).unwrap()).unwrap()
    }

    fn legacy_action_key_json(action_key: &ActionKey) -> String {
        let mut value = serde_json::to_value(action_key).unwrap();
        remove_adoptable(&mut value);
        canonical_value_json(&value)
    }

    fn legacy_record_json(record: &ActionRecord) -> String {
        let mut value = serde_json::to_value(record).unwrap();
        remove_adoptable(value.get_mut("action_key").unwrap());
        canonical_value_json(&value)
    }

    fn insert_legacy_event(
        journal: &ActionJournal,
        record: &ActionRecord,
    ) -> (String, String, String) {
        let key_json = legacy_action_key_json(&record.action_key);
        let key_digest = Digest::from_bytes(key_json.as_bytes());
        let record_json = legacy_record_json(record);
        let state = state_name(record.state);
        let checksum = checksum_for(&key_digest, state, &record_json);
        journal
            .connection
            .execute(
                "INSERT INTO action_events(action_key_digest, state, record_json, checksum)
                 VALUES (?1, ?2, ?3, ?4)",
                params![key_digest.to_string(), state, record_json, checksum],
            )
            .unwrap();
        (key_digest.to_string(), record_json, checksum)
    }

    fn insert_legacy_lease(journal: &ActionJournal, action: &ActionKey) -> (String, String) {
        let action_key_json = legacy_action_key_json(action);
        let key_digest = Digest::from_bytes(action_key_json.as_bytes());
        journal
            .connection
            .execute(
                "INSERT INTO producer_leases(
                     action_key_digest, action_key_json, generation, owner,
                     expires_at_ms, heartbeat_every_ms, lease_duration_ms, state
                 ) VALUES (?1, ?2, 1, 'worker-a', 100, 25, 100, 'active')",
                params![key_digest.to_string(), action_key_json],
            )
            .unwrap();
        (key_digest.to_string(), legacy_action_key_json(action))
    }

    fn set_schema_version(journal: &ActionJournal, version: u32) {
        journal
            .connection
            .execute(
                "UPDATE action_journal_meta SET value = ?1 WHERE key = 'schema_version'",
                [version.to_string()],
            )
            .unwrap();
    }

    fn insert_duplicate_termination_claims(
        journal: &mut ActionJournal,
        seed: u8,
        old_phase: i64,
        new_phase: i64,
    ) -> (String, String) {
        let action = action(seed);
        let (old_digest, _, _) = insert_legacy_event(journal, &record(seed, ActionState::Running));
        journal
            .append(&record(seed, ActionState::Complete))
            .unwrap();
        let new_digest = action.digest().unwrap().to_string();
        journal
            .connection
            .execute(
                "INSERT INTO action_termination_claims(
                     action_key_digest, reason, claimed_at_ms, completed
                 ) VALUES (?1, 'old-reason', 10, ?2),
                          (?3, 'new-reason', 20, ?4)",
                params![old_digest, old_phase, new_digest, new_phase],
            )
            .unwrap();
        set_schema_version(journal, 2);
        (old_digest, new_digest)
    }

    #[test]
    fn acquire_contention_has_one_atomic_winner() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("leases.sqlite");
        let action = action(40);
        let clock = TestClock::default();
        let barrier = Arc::new(Barrier::new(2));
        let managers = [
            LeaseManager::open(&path, clock.clone()).unwrap(),
            LeaseManager::open(&path, clock.clone()).unwrap(),
        ];
        let handles = managers
            .into_iter()
            .zip(["worker-a", "worker-b"])
            .map(|(mut manager, owner)| {
                let action = action.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    manager.acquire(&action, owner, 100, 25)
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(JournalError::LeaseBusy { .. })))
                .count(),
            1
        );
    }

    #[test]
    fn terminal_record_and_release_share_one_fencing_transaction() {
        let clock = TestClock::default();
        let action = action(42);
        let mut manager = LeaseManager::open(":memory:", clock).unwrap();
        let lease = manager.acquire(&action, "worker-a", 100, 25).unwrap();
        let mut terminal = record(42, ActionState::Complete);
        terminal.action_key = action.clone();
        terminal.producer_lease_ref = Some(format!("compiler-cache/{}", lease.generation));

        let sequence = manager
            .append_action_and_release(&lease, &terminal)
            .unwrap();

        assert_eq!(sequence, 1);
        assert_eq!(
            manager.lease_status(&action).unwrap(),
            Some(LeaseStatus::Released)
        );
        assert_eq!(
            manager.latest_action(&action).unwrap().unwrap().state,
            ActionState::Complete
        );
    }

    #[test]
    fn terminal_publication_rejects_revoked_active_lease_without_side_effects() {
        let clock = TestClock::default();
        let action = action(65);
        let mut manager = LeaseManager::open(":memory:", clock).unwrap();
        let lease = manager.acquire(&action, "worker-a", 100, 25).unwrap();
        let digest = action.digest().unwrap();
        manager
            .journal
            .connection
            .execute(
                "INSERT INTO action_trust_revocations(
                     action_key_digest, reason, revoked_at_ms
                 ) VALUES (?1, 'test-revocation', 0)",
                [digest.to_string()],
            )
            .unwrap();
        let mut terminal = record(65, ActionState::Complete);
        terminal.action_key = action.clone();
        terminal.producer_lease_ref = Some(format!("compiler-cache/{}", lease.generation));

        let error = manager
            .append_action_and_release(&lease, &terminal)
            .unwrap_err();

        assert!(matches!(
            error,
            JournalError::InvalidState(message)
                if message == "action publication is blocked by trust revocation"
        ));
        assert_eq!(
            manager.lease_status(&action).unwrap(),
            Some(LeaseStatus::Active)
        );
        assert!(manager.latest_action(&action).unwrap().is_none());
        let event_count: i64 = manager
            .journal
            .connection
            .query_row(
                "SELECT COUNT(*) FROM action_events WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event_count, 0);
    }

    #[test]
    fn terminal_publication_rejects_termination_claim_in_all_phases() {
        for (seed, phase) in [(66_u8, 0_i64), (67, 2), (68, 1)] {
            let clock = TestClock::default();
            let action = action(seed);
            let mut manager = LeaseManager::open(":memory:", clock).unwrap();
            let lease = manager.acquire(&action, "worker-a", 100, 25).unwrap();
            let digest = action.digest().unwrap();
            manager
                .journal
                .connection
                .execute(
                    "INSERT INTO action_termination_claims(
                         action_key_digest, reason, claimed_at_ms, completed
                     ) VALUES (?1, 'retention_expired', 0, ?2)",
                    params![digest.to_string(), phase],
                )
                .unwrap();
            let mut terminal = record(seed, ActionState::Complete);
            terminal.action_key = action.clone();
            terminal.producer_lease_ref = Some(format!("compiler-cache/{}", lease.generation));

            let error = manager
                .append_action_and_release(&lease, &terminal)
                .unwrap_err();

            assert!(matches!(
                error,
                JournalError::InvalidState(message)
                    if message == "action publication is blocked by a termination claim"
            ));
            assert_eq!(
                manager.lease_status(&action).unwrap(),
                Some(LeaseStatus::Active)
            );
            assert!(manager.latest_action(&action).unwrap().is_none());
        }
    }

    #[test]
    fn terminal_publication_rejects_malformed_termination_claim_phase() {
        let clock = TestClock::default();
        let action = action(69);
        let mut manager = LeaseManager::open(":memory:", clock).unwrap();
        let lease = manager.acquire(&action, "worker-a", 100, 25).unwrap();
        let digest = action.digest().unwrap();
        manager
            .journal
            .connection
            .execute(
                "INSERT INTO action_termination_claims(
                     action_key_digest, reason, claimed_at_ms, completed
                 ) VALUES (?1, 'retention_expired', 0, 3)",
                [digest.to_string()],
            )
            .unwrap();
        let mut terminal = record(69, ActionState::Complete);
        terminal.action_key = action.clone();
        terminal.producer_lease_ref = Some(format!("compiler-cache/{}", lease.generation));

        let error = manager
            .append_action_and_release(&lease, &terminal)
            .unwrap_err();

        assert!(matches!(
            error,
            JournalError::InvalidState(message)
                if message.contains("invalid termination claim phase 3")
        ));
        assert_eq!(
            manager.lease_status(&action).unwrap(),
            Some(LeaseStatus::Active)
        );
        assert!(manager.latest_action(&action).unwrap().is_none());
    }

    #[test]
    fn heartbeat_renews_with_fencing_token() {
        let clock = TestClock::default();
        let mut manager = LeaseManager::open(":memory:", clock.clone()).unwrap();
        let mut lease = manager.acquire(&action(41), "worker-a", 100, 25).unwrap();
        clock.advance(40);
        lease.lease_duration = 10_000;
        manager.renew(&mut lease).unwrap();
        assert_eq!(lease.expires_at, LogicalInstant::from_millis(140));
        assert_eq!(lease.lease_duration, 100);
        clock.advance(100);
        assert!(matches!(
            manager.renew(&mut lease),
            Err(JournalError::LeaseFenced)
        ));
    }

    #[test]
    fn heartbeat_cadence_is_checked_from_persisted_deadline() {
        let clock = TestClock::default();
        let mut manager = LeaseManager::open(":memory:", clock.clone()).unwrap();
        let mut lease = manager.acquire(&action(50), "worker-a", 100, 25).unwrap();
        assert!(matches!(
            manager.renew(&mut lease),
            Err(JournalError::HeartbeatTooSoon { .. })
        ));
        clock.advance(25);
        manager.renew(&mut lease).unwrap();
        assert_eq!(lease.expires_at, LogicalInstant::from_millis(125));
    }

    #[test]
    fn expiry_increments_generation_and_defers_takeover() {
        let clock = TestClock::default();
        let mut manager = LeaseManager::open(":memory:", clock.clone()).unwrap();
        let lease = manager.acquire(&action(42), "worker-a", 100, 25).unwrap();
        clock.advance(100);
        assert_eq!(manager.expire_due().unwrap(), 1);
        assert_eq!(manager.expire_due().unwrap(), 0);
        assert_eq!(
            manager.lease_status(&lease.action).unwrap(),
            Some(LeaseStatus::Abandoned)
        );
        assert!(matches!(
            manager.acquire(&lease.action, "worker-b", 100, 25),
            Err(JournalError::LeaseAbandonable { .. })
        ));
        let events = manager.drain_telemetry();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].generation, lease.generation + 1);
        assert_eq!(events[1].kind, LeaseTransitionKind::Expired);
    }

    #[test]
    fn restart_reloads_deadline_without_resurrecting_owner() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("restart.sqlite");
        let clock = TestClock::default();
        let action = action(43);
        let mut lease = {
            let mut manager = LeaseManager::open(&path, clock.clone()).unwrap();
            manager.acquire(&action, "worker-a", 100, 25).unwrap()
        };
        clock.advance(99);
        let restarted = LeaseManager::open(&path, clock.clone()).unwrap();
        assert_eq!(
            restarted.next_expiry().unwrap(),
            Some(LogicalInstant::from_millis(100))
        );
        assert_eq!(
            restarted.lease_status(&action).unwrap(),
            Some(LeaseStatus::Active)
        );
        drop(restarted);
        clock.advance(1);
        let mut recovered = LeaseManager::open(&path, clock.clone()).unwrap();
        assert_eq!(recovered.next_expiry().unwrap(), None);
        assert_eq!(
            recovered.lease_status(&action).unwrap(),
            Some(LeaseStatus::Abandoned)
        );
        assert!(matches!(
            recovered.renew(&mut lease),
            Err(JournalError::LeaseFenced)
        ));
    }

    #[test]
    fn consumer_set_is_durable_and_idempotent() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("consumers.sqlite");
        let action = action(44);
        let mut manager = LeaseManager::open(&path, TestClock::default()).unwrap();
        assert!(manager.attach_consumer(&action, "run-a").unwrap());
        assert!(!manager.attach_consumer(&action, "run-a").unwrap());
        assert!(manager.attach_consumer(&action, "run-b").unwrap());
        assert_eq!(manager.live_consumer_count(&action).unwrap(), 2);
        assert!(!manager.detach_consumer(&action, "run-c").unwrap());
        assert!(manager.detach_consumer(&action, "run-a").unwrap());
        drop(manager);
        let restarted = LeaseManager::open(&path, TestClock::default()).unwrap();
        assert_eq!(restarted.live_consumer_count(&action).unwrap(), 1);
    }

    #[test]
    fn schema_upgrade_preserves_existing_journal() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("schema.sqlite");
        let mut journal = ActionJournal::open(&path).unwrap();
        journal.append(&record(48, ActionState::Complete)).unwrap();
        journal
            .connection
            .execute(
                "UPDATE action_journal_meta SET value = '1' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        drop(journal);
        let upgraded = ActionJournal::open(&path).unwrap();
        assert_eq!(upgraded.schema_version().unwrap(), JOURNAL_SCHEMA_VERSION);
        assert_eq!(upgraded.entries().unwrap().len(), 1);
    }

    #[test]
    fn legacy_action_events_migrate_to_current_identity() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("legacy-events.sqlite");
        let record = record(54, ActionState::Complete);
        let journal = ActionJournal::open(&path).unwrap();
        let (legacy_digest, legacy_json, legacy_checksum) = insert_legacy_event(&journal, &record);
        journal
            .connection
            .execute(
                "INSERT INTO action_consumers(action_key_digest, run_id)
                 VALUES (?1, 'run-54')",
                [&legacy_digest],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                 VALUES (?1, 1000)",
                [&legacy_digest],
            )
            .unwrap();
        set_schema_version(&journal, 2);
        drop(journal);

        let migrated = ActionJournal::open(&path).unwrap();
        let current_digest = record.action_key_digest().unwrap().to_string();
        let current_json = canonical_json_string(&record).unwrap();
        let current_checksum =
            checksum_for_raw(&current_digest, state_name(record.state), &current_json);
        let entry = &migrated.entries().unwrap()[0];
        assert_eq!(entry.sequence, 1);
        assert_eq!(entry.record, record);

        let row: (String, String, String, String) = migrated
            .connection
            .query_row(
                "SELECT action_key_digest, record_json, checksum, state
                 FROM action_events WHERE sequence = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                current_digest.clone(),
                current_json,
                current_checksum,
                "complete".into()
            )
        );
        assert_ne!(legacy_digest, row.0);
        assert_ne!(legacy_json, row.1);
        assert_ne!(legacy_checksum, row.2);
        let consumer_digest: String = migrated
            .connection
            .query_row(
                "SELECT action_key_digest FROM action_consumers WHERE run_id = 'run-54'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(consumer_digest, current_digest);
        let retention_digest: String = migrated
            .connection
            .query_row(
                "SELECT action_key_digest FROM action_retention WHERE retained_until_ms = 1000",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retention_digest, current_digest);
        assert_eq!(migrated.schema_version().unwrap(), JOURNAL_SCHEMA_VERSION);
    }

    #[test]
    fn non_adoptable_legacy_alias_collision_fails_closed() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("non-adoptable-legacy-alias.sqlite");
        let non_adoptable = record_with_adoptable(63, ActionState::Planned, false);
        let adoptable = record_with_adoptable(63, ActionState::Planned, true);
        let journal = ActionJournal::open(&path).unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_events(
                     action_key_digest, state, record_json, checksum
                 ) VALUES (?1, ?2, ?3, ?4), (?5, ?6, ?7, ?8)",
                params![
                    non_adoptable.action_key_digest().unwrap().to_string(),
                    state_name(non_adoptable.state),
                    canonical_json_string(&non_adoptable).unwrap(),
                    checksum_for(
                        &non_adoptable.action_key_digest().unwrap(),
                        state_name(non_adoptable.state),
                        &canonical_json_string(&non_adoptable).unwrap(),
                    ),
                    adoptable.action_key_digest().unwrap().to_string(),
                    state_name(adoptable.state),
                    canonical_json_string(&adoptable).unwrap(),
                    checksum_for(
                        &adoptable.action_key_digest().unwrap(),
                        state_name(adoptable.state),
                        &canonical_json_string(&adoptable).unwrap(),
                    ),
                ],
            )
            .unwrap();
        let legacy_digest = legacy_action_key_digest(&non_adoptable.action_key)
            .unwrap()
            .to_string();
        journal
            .connection
            .execute(
                "INSERT INTO action_consumers(action_key_digest, run_id)
                 VALUES (?1, 'legacy-run')",
                [&legacy_digest],
            )
            .unwrap();
        set_schema_version(&journal, 2);
        drop(journal);

        let error = match ActionJournal::open(&path) {
            Ok(_) => panic!("non-adoptable legacy alias unexpectedly migrated"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("non-adoptable legacy action key digest"));

        let connection = Connection::open(&path).unwrap();
        let version: String = connection
            .query_row(
                "SELECT value FROM action_journal_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "2");
        let consumer_digest: String = connection
            .query_row(
                "SELECT action_key_digest FROM action_consumers WHERE run_id = 'legacy-run'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(consumer_digest, legacy_digest);
    }

    #[test]
    fn current_action_key_sources_resolve_legacy_side_rows() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("current-sources-legacy-side-rows.sqlite");
        let record = record(60, ActionState::Complete);
        let action = record.action_key.clone();
        let mut journal = ActionJournal::open(&path).unwrap();
        journal.append(&record).unwrap();

        let current_digest = action.digest().unwrap().to_string();
        let current_json = canonical_json_string(&action).unwrap();
        let legacy_digest = legacy_action_key_digest(&action).unwrap().to_string();
        journal
            .connection
            .execute(
                "INSERT INTO producer_leases(
                     action_key_digest, action_key_json, generation, owner,
                     expires_at_ms, heartbeat_every_ms, lease_duration_ms, state
                 ) VALUES (?1, ?2, 1, 'worker-a', 100, 25, 100, 'active')",
                params![current_digest, current_json],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_consumers(action_key_digest, run_id)
                 VALUES (?1, 'run-60')",
                [&legacy_digest],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                 VALUES (?1, 6000)",
                [&legacy_digest],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_termination_claims(
                     action_key_digest, reason, claimed_at_ms, completed
                 ) VALUES (?1, 'retention_expired', 60, 0)",
                [&legacy_digest],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_trust_revocations(
                     action_key_digest, reason, revoked_at_ms
                 ) VALUES (?1, 'legacy-reason', 61)",
                [&legacy_digest],
            )
            .unwrap();
        set_schema_version(&journal, 2);
        drop(journal);

        let migrated = ActionJournal::open(&path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), JOURNAL_SCHEMA_VERSION);
        assert_eq!(migrated.entries().unwrap()[0].record, record);

        let lease_digest: String = migrated
            .connection
            .query_row("SELECT action_key_digest FROM producer_leases", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(lease_digest, current_digest);
        for table in [
            "action_consumers",
            "action_retention",
            "action_termination_claims",
            "action_trust_revocations",
        ] {
            let query = format!("SELECT COUNT(*) FROM {table} WHERE action_key_digest = ?1");
            let count: i64 = migrated
                .connection
                .query_row(&query, [&current_digest], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 1, "legacy side row missing from {table}");
        }
    }

    #[test]
    fn action_key_migration_rejects_orphan_side_rows_and_rolls_back() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("orphan-side-rows.sqlite");
        let record = record(61, ActionState::Planned);
        let journal = ActionJournal::open(&path).unwrap();
        let (legacy_digest, legacy_json, legacy_checksum) = insert_legacy_event(&journal, &record);
        let orphan_digest = Digest::from_bytes(b"orphan-action-key").to_string();
        journal
            .connection
            .execute(
                "INSERT INTO action_consumers(action_key_digest, run_id)
                 VALUES (?1, 'orphan-run')",
                [&orphan_digest],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                 VALUES (?1, 6100)",
                [&orphan_digest],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_termination_claims(
                     action_key_digest, reason, claimed_at_ms, completed
                 ) VALUES (?1, 'retention_expired', 61, 0)",
                [&orphan_digest],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_trust_revocations(
                     action_key_digest, reason, revoked_at_ms
                 ) VALUES (?1, 'orphan-reason', 62)",
                [&orphan_digest],
            )
            .unwrap();
        set_schema_version(&journal, 2);
        drop(journal);

        let error = match ActionJournal::open(&path) {
            Ok(_) => panic!("orphan side row unexpectedly migrated"),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            format!(
                "journal state is invalid: action key migration cannot resolve action_consumers.action_key_digest={orphan_digest}: no ActionKey source exists"
            )
        );

        let connection = Connection::open(&path).unwrap();
        let version: String = connection
            .query_row(
                "SELECT value FROM action_journal_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "2");
        let event: (String, String, String) = connection
            .query_row(
                "SELECT action_key_digest, record_json, checksum
                 FROM action_events WHERE sequence = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(event, (legacy_digest, legacy_json, legacy_checksum));
        for table in [
            "action_consumers",
            "action_retention",
            "action_termination_claims",
            "action_trust_revocations",
        ] {
            let query = format!("SELECT COUNT(*) FROM {table} WHERE action_key_digest = ?1");
            let count: i64 = connection
                .query_row(&query, [&orphan_digest], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 1, "orphan row missing from {table}");
        }
    }

    #[test]
    fn legacy_producer_leases_migrate_to_current_identity() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("legacy-leases.sqlite");
        let action = action(55);
        let journal = ActionJournal::open(&path).unwrap();
        let (legacy_digest, legacy_json) = insert_legacy_lease(&journal, &action);
        set_schema_version(&journal, 2);
        drop(journal);

        let clock = TestClock::default();
        let mut manager = LeaseManager::open(&path, clock.clone()).unwrap();
        assert_eq!(
            manager.lease_status(&action).unwrap(),
            Some(LeaseStatus::Active)
        );
        let mut lease = velnor_action_model::ProducerLease {
            action: action.clone(),
            generation: 1,
            owner: "worker-a".into(),
            expires_at: LogicalInstant::from_millis(100),
            heartbeat_every: 25,
            lease_duration: 100,
        };
        clock.advance(25);
        manager.renew(&mut lease).unwrap();

        let current_digest = action.digest().unwrap().to_string();
        let current_json = canonical_json_string(&action).unwrap();
        let row: (String, String) = manager
            .journal()
            .connection
            .query_row(
                "SELECT action_key_digest, action_key_json
                 FROM producer_leases",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (current_digest, current_json));
        assert_ne!(legacy_digest, row.0);
        assert_ne!(legacy_json, row.1);
    }

    #[test]
    fn mixed_version_action_keys_reconcile_duplicate_durable_rows() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("mixed-version.sqlite");
        let action = action(58);
        let running_record = record(58, ActionState::Running);
        let mut journal = ActionJournal::open(&path).unwrap();
        let (legacy_digest, _, _) = insert_legacy_event(&journal, &running_record);
        journal.append(&record(58, ActionState::Complete)).unwrap();
        let (legacy_lease_digest, _) = insert_legacy_lease(&journal, &action);
        let current_digest = action.digest().unwrap().to_string();
        let current_json = canonical_json_string(&action).unwrap();

        journal
            .connection
            .execute(
                "INSERT INTO producer_leases(
                     action_key_digest, action_key_json, generation, owner,
                     expires_at_ms, heartbeat_every_ms, lease_duration_ms, state
                 ) VALUES (?1, ?2, 2, 'worker-b', 500, 50, 500, 'active')",
                params![current_digest, current_json],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_consumers(action_key_digest, run_id)
                 VALUES (?1, 'run-legacy'), (?2, 'run-current'),
                        (?1, 'run-shared'), (?2, 'run-shared')",
                params![legacy_digest, current_digest],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                 VALUES (?1, 100), (?2, 200)",
                params![legacy_digest, current_digest],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_termination_claims(
                     action_key_digest, reason, claimed_at_ms, completed
                 ) VALUES (?1, 'failed_action', 10, 0),
                          (?2, 'retention_expired', 20, 1)",
                params![legacy_digest, current_digest],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_trust_revocations(
                     action_key_digest, reason, revoked_at_ms
                 ) VALUES (?1, 'legacy-reason', 30),
                          (?2, 'current-reason', 40)",
                params![legacy_digest, current_digest],
            )
            .unwrap();
        assert_eq!(legacy_lease_digest, legacy_digest);
        set_schema_version(&journal, 2);
        drop(journal);

        let migrated = ActionJournal::open(&path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), JOURNAL_SCHEMA_VERSION);
        assert_eq!(migrated.entries().unwrap().len(), 2);

        let lease: (String, i64, String, String) = migrated
            .connection
            .query_row(
                "SELECT action_key_digest, generation, owner, state
                 FROM producer_leases",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            lease,
            (
                current_digest.clone(),
                2,
                "worker-b".into(),
                "active".into()
            )
        );

        let event_digests: Vec<String> = migrated
            .connection
            .prepare("SELECT action_key_digest FROM action_events ORDER BY sequence")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            event_digests,
            vec![current_digest.clone(), current_digest.clone()]
        );

        let consumers: Vec<(String, String)> = migrated
            .connection
            .prepare(
                "SELECT action_key_digest, run_id FROM action_consumers
                 ORDER BY run_id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            consumers,
            vec![
                (current_digest.clone(), "run-current".into()),
                (current_digest.clone(), "run-legacy".into()),
                (current_digest.clone(), "run-shared".into()),
            ]
        );

        let retention: i64 = migrated
            .connection
            .query_row(
                "SELECT retained_until_ms FROM action_retention WHERE action_key_digest = ?1",
                [&current_digest],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retention, 200);
        let claim: (String, i64, i64) = migrated
            .connection
            .query_row(
                "SELECT reason, claimed_at_ms, completed
                 FROM action_termination_claims WHERE action_key_digest = ?1",
                [&current_digest],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(claim, ("retention_expired".into(), 20, 1));
        let revocation: (String, i64) = migrated
            .connection
            .query_row(
                "SELECT reason, revoked_at_ms
                 FROM action_trust_revocations WHERE action_key_digest = ?1",
                [&current_digest],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(revocation, ("legacy-reason".into(), 30));
    }

    #[test]
    fn action_key_migration_rejects_invalid_old_termination_phase() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("invalid-old-termination-phase.sqlite");
        let mut journal = ActionJournal::open(&path).unwrap();
        let (old_digest, new_digest) = insert_duplicate_termination_claims(&mut journal, 64, 3, 0);
        drop(journal);

        let error = match ActionJournal::open(&path) {
            Ok(_) => panic!("invalid old termination phase unexpectedly migrated"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("invalid termination claim phase 3"));

        let connection = Connection::open(&path).unwrap();
        let version: String = connection
            .query_row(
                "SELECT value FROM action_journal_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "2");
        let phases: Vec<(String, i64)> = connection
            .prepare(
                "SELECT action_key_digest, completed
                 FROM action_termination_claims ORDER BY action_key_digest",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(phases, vec![(old_digest, 3), (new_digest, 0)]);
    }

    #[test]
    fn action_key_migration_rejects_invalid_new_termination_phase() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("invalid-new-termination-phase.sqlite");
        let mut journal = ActionJournal::open(&path).unwrap();
        let (old_digest, new_digest) = insert_duplicate_termination_claims(&mut journal, 65, 0, 3);
        drop(journal);

        let error = match ActionJournal::open(&path) {
            Ok(_) => panic!("invalid new termination phase unexpectedly migrated"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("invalid termination claim phase 3"));

        let connection = Connection::open(&path).unwrap();
        let phases: Vec<(String, i64)> = connection
            .prepare(
                "SELECT action_key_digest, completed
                 FROM action_termination_claims ORDER BY action_key_digest",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(phases, vec![(new_digest, 3), (old_digest, 0)]);
    }

    #[test]
    fn action_key_migration_preserves_in_flight_termination_phase() {
        let temp = TempDir::new().unwrap();
        let path = temp
            .path()
            .join("preserve-in-flight-termination-phase.sqlite");
        let mut journal = ActionJournal::open(&path).unwrap();
        let (_, new_digest) = insert_duplicate_termination_claims(&mut journal, 66, 2, 0);
        drop(journal);

        let migrated = ActionJournal::open(&path).unwrap();
        let phase: i64 = migrated
            .connection
            .query_row(
                "SELECT completed FROM action_termination_claims
                 WHERE action_key_digest = ?1",
                [&new_digest],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(phase, 2);
    }

    #[test]
    fn action_key_migration_rolls_back_failed_duplicate_lease_preflight() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("failed-duplicate-preflight.sqlite");
        let action = action(59);
        let journal = ActionJournal::open(&path).unwrap();
        let (legacy_digest, legacy_json) = insert_legacy_lease(&journal, &action);
        let current_digest = action.digest().unwrap().to_string();
        let current_json = canonical_json_string(&action).unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO producer_leases(
                     action_key_digest, action_key_json, generation, owner,
                     expires_at_ms, heartbeat_every_ms, lease_duration_ms, state
                 ) VALUES (?1, ?2, 2, 'worker-b', 500, 50, 500, 'corrupt')",
                params![current_digest, current_json],
            )
            .unwrap();
        set_schema_version(&journal, 2);
        drop(journal);

        let result = ActionJournal::open(&path);
        assert!(matches!(result, Err(JournalError::Sqlite(_))));

        let connection = Connection::open(&path).unwrap();
        let version: String = connection
            .query_row(
                "SELECT value FROM action_journal_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "2");
        let leases: Vec<(String, String, String)> = connection
            .prepare(
                "SELECT action_key_digest, action_key_json, state
                 FROM producer_leases ORDER BY action_key_digest",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            leases,
            vec![
                (legacy_digest, legacy_json, "active".into()),
                (current_digest, current_json, "corrupt".into()),
            ]
        );
    }

    #[test]
    fn action_key_migration_is_idempotent_on_reopen() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("repeated-migration.sqlite");
        let action = action(56);
        let journal = ActionJournal::open(&path).unwrap();
        insert_legacy_event(&journal, &record(56, ActionState::Running));
        insert_legacy_lease(&journal, &action);
        set_schema_version(&journal, 2);
        drop(journal);

        let first = ActionJournal::open(&path).unwrap();
        let first_event: (i64, String, String, String) = first
            .connection
            .query_row(
                "SELECT sequence, action_key_digest, record_json, checksum
                 FROM action_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        let first_lease: (String, String) = first
            .connection
            .query_row(
                "SELECT action_key_digest, action_key_json FROM producer_leases",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(first.schema_version().unwrap(), JOURNAL_SCHEMA_VERSION);
        drop(first);

        let second = ActionJournal::open(&path).unwrap();
        let second_event: (i64, String, String, String) = second
            .connection
            .query_row(
                "SELECT sequence, action_key_digest, record_json, checksum
                 FROM action_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        let second_lease: (String, String) = second
            .connection
            .query_row(
                "SELECT action_key_digest, action_key_json FROM producer_leases",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(second_event, first_event);
        assert_eq!(second_lease, first_lease);
        assert_eq!(second.entries().unwrap().len(), 1);
    }

    #[test]
    fn action_key_migration_rolls_back_on_later_invalid_event() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("failed-migration.sqlite");
        let valid = record(57, ActionState::Planned);
        let journal = ActionJournal::open(&path).unwrap();
        let (legacy_digest, legacy_json, legacy_checksum) = insert_legacy_event(&journal, &valid);
        let invalid_json = "not-json";
        let invalid_digest = Digest::from_bytes(b"invalid-event");
        let invalid_checksum = checksum_for_raw(invalid_digest.as_str(), "planned", invalid_json);
        journal
            .connection
            .execute(
                "INSERT INTO action_events(action_key_digest, state, record_json, checksum)
                 VALUES (?1, 'planned', ?2, ?3)",
                params![invalid_digest.to_string(), invalid_json, invalid_checksum],
            )
            .unwrap();
        set_schema_version(&journal, 2);
        drop(journal);

        let result = ActionJournal::open(&path);
        assert!(matches!(result, Err(JournalError::Json(_))));

        let connection = Connection::open(&path).unwrap();
        let version: String = connection
            .query_row(
                "SELECT value FROM action_journal_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "2");
        let first: (String, String, String) = connection
            .query_row(
                "SELECT action_key_digest, record_json, checksum
                 FROM action_events WHERE sequence = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(first, (legacy_digest, legacy_json, legacy_checksum));
        let second: (String, String, String) = connection
            .query_row(
                "SELECT action_key_digest, record_json, checksum
                 FROM action_events WHERE sequence = 2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            second,
            (
                invalid_digest.to_string(),
                invalid_json.into(),
                invalid_checksum
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_restart_recovers_killed_lease() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("killed-lease.sqlite");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::lease_child", "--nocapture"])
            .env("VELNOR_ACTION_JOURNAL_LEASE_CHILD_PATH", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let ready = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .any(|line| line == "lease-ready");
        assert!(ready, "lease child did not publish readiness");
        child.kill().unwrap();
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));

        let clock = TestClock::default();
        let action = action(53);
        let active = LeaseManager::open(&path, clock.clone()).unwrap();
        assert_eq!(
            active.lease_status(&action).unwrap(),
            Some(LeaseStatus::Active)
        );
        drop(active);
        clock.advance(100);
        let recovered = LeaseManager::open(&path, clock).unwrap();
        assert_eq!(
            recovered.lease_status(&action).unwrap(),
            Some(LeaseStatus::Abandoned)
        );
    }

    #[cfg(unix)]
    #[test]
    fn lease_child() {
        let Some(path) = std::env::var_os("VELNOR_ACTION_JOURNAL_LEASE_CHILD_PATH") else {
            return;
        };
        let mut manager = LeaseManager::open(path, TestClock::default()).unwrap();
        manager.acquire(&action(53), "worker-a", 100, 25).unwrap();
        println!("lease-ready");
        std::io::stdout().flush().unwrap();
        let mut byte = [0_u8; 1];
        let _ = std::io::stdin().read(&mut byte);
    }

    #[test]
    fn telemetry_sink_receives_only_committed_transitions() {
        let clock = TestClock::default();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let mut manager = LeaseManager::open(":memory:", clock.clone()).unwrap();
        manager.set_telemetry_sink(move |event| sink_seen.lock().unwrap().push(event.clone()));
        let mut lease = manager.acquire(&action(49), "worker-a", 100, 25).unwrap();
        clock.advance(40);
        manager.renew(&mut lease).unwrap();
        manager.release(&lease).unwrap();
        let events = seen.lock().unwrap();
        assert_eq!(
            events.iter().map(|event| event.kind).collect::<Vec<_>>(),
            vec![
                LeaseTransitionKind::Acquired,
                LeaseTransitionKind::Renewed,
                LeaseTransitionKind::Released,
            ]
        );
        assert!(events
            .iter()
            .all(|event| event.action_key_digest == lease.action.digest().unwrap()));
        let envelope = events[0]
            .into_envelope("run-a", "repo-a", "trusted", 7)
            .unwrap();
        let envelope = serde_json::to_value(envelope).unwrap();
        assert_eq!(envelope["event"], "lease_acquired");
        assert_eq!(envelope["fields"]["generation"], lease.generation);
    }

    #[cfg(feature = "virtual-time")]
    #[tokio::test(flavor = "current_thread")]
    async fn expiry_wait_is_event_driven_and_uses_virtual_time() {
        let clock = TestClock::default();
        let mut manager = LeaseManager::open(":memory:", clock.clone()).unwrap();
        manager.acquire(&action(45), "worker-a", 100, 25).unwrap();
        let task = tokio::spawn(async move { manager.expire_next().await });
        tokio::task::yield_now().await;
        clock.advance(99);
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        clock.advance(1);
        assert_eq!(task.await.unwrap().unwrap(), 1);
    }

    #[cfg(feature = "virtual-time")]
    #[tokio::test(flavor = "current_thread")]
    async fn expiry_wait_reacts_to_an_earlier_persisted_deadline() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("earlier-deadline.sqlite");
        let clock = TestClock::default();
        let mut manager = LeaseManager::open(&path, clock.clone()).unwrap();
        manager.acquire(&action(51), "worker-a", 100, 25).unwrap();
        let task = tokio::spawn(async move { manager.expire_next().await });
        tokio::task::yield_now().await;

        let mut second_manager = LeaseManager::open(&path, clock.clone()).unwrap();
        second_manager
            .acquire(&action(52), "worker-b", 10, 5)
            .unwrap();
        tokio::task::yield_now().await;
        clock.advance(10);
        assert_eq!(task.await.unwrap().unwrap(), 1);
        assert_eq!(
            second_manager.lease_status(&action(52)).unwrap(),
            Some(LeaseStatus::Abandoned)
        );
    }

    #[cfg(feature = "virtual-time")]
    #[tokio::test(flavor = "current_thread")]
    async fn expiry_wait_reacts_to_a_new_lease_when_idle() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("idle-expiry.sqlite");
        let clock = TestClock::default();
        let manager = LeaseManager::open(&path, clock.clone()).unwrap();
        let task = tokio::spawn(async move {
            let mut manager = manager;
            manager.expire_next().await
        });
        tokio::task::yield_now().await;

        let mut producer = LeaseManager::open(&path, clock.clone()).unwrap();
        producer.acquire(&action(53), "worker-a", 10, 5).unwrap();
        clock.advance(10);

        assert_eq!(task.await.unwrap().unwrap(), 1);
        assert_eq!(
            producer.lease_status(&action(53)).unwrap(),
            Some(LeaseStatus::Abandoned)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn production_clock_wakes_idle_expiry_wait() {
        let clock = TokioClock::new();
        let waiter_clock = clock.clone();
        let waiter = tokio::spawn(async move {
            waiter_clock.sleep_until(LogicalInstant::MAX).await;
        });
        tokio::task::yield_now().await;
        clock.wake_expiry();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn release_and_abandon_fence_previous_generation() {
        let clock = TestClock::default();
        let mut manager = LeaseManager::open(":memory:", clock.clone()).unwrap();
        let lease = manager.acquire(&action(46), "worker-a", 100, 25).unwrap();
        manager.release(&lease).unwrap();
        assert_eq!(
            manager.lease_status(&lease.action).unwrap(),
            Some(LeaseStatus::Released)
        );
        assert!(matches!(
            manager.release(&lease),
            Err(JournalError::LeaseFenced)
        ));

        let lease = manager.acquire(&action(47), "worker-a", 100, 25).unwrap();
        manager.abandon(&lease).unwrap();
        assert_eq!(
            manager.lease_status(&lease.action).unwrap(),
            Some(LeaseStatus::Abandoned)
        );
        assert!(matches!(
            manager.renew(&mut lease.clone()),
            Err(JournalError::LeaseFenced)
        ));
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

    #[cfg(not(feature = "virtual-time"))]
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

    #[cfg(all(unix, not(feature = "virtual-time")))]
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

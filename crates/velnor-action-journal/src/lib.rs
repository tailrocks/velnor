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
                 VALUES ('schema_version', '3');",
        )?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        migrate_adoptable_action_keys(&transaction)?;
        transaction.execute(
            "UPDATE action_journal_meta SET value = ?1 WHERE key = 'schema_version'",
            [JOURNAL_SCHEMA_VERSION.to_string()],
        )?;
        transaction.commit()?;
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

struct ActionEventMigration {
    sequence: i64,
    old_digest: Digest,
    new_digest: Digest,
    state: String,
    record_json: String,
    checksum: String,
}

struct ProducerLeaseMigration {
    old_digest: Digest,
    new_digest: Digest,
    action_key_json: String,
}

fn migrate_adoptable_action_keys(transaction: &Transaction<'_>) -> Result<(), JournalError> {
    let schema_version: String = transaction.query_row(
        "SELECT value FROM action_journal_meta WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    )?;
    let schema_version = schema_version
        .parse::<u32>()
        .map_err(|_| JournalError::InvalidState("schema version is not numeric".to_owned()))?;
    if schema_version > JOURNAL_SCHEMA_VERSION {
        return Err(JournalError::InvalidState(format!(
            "journal schema version {schema_version} is newer than supported version {JOURNAL_SCHEMA_VERSION}"
        )));
    }

    let mut mappings = BTreeMap::new();
    let mut current_digests = BTreeSet::new();
    let mut event_migrations = Vec::new();
    {
        let mut statement = transaction.prepare(
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
        for row in rows {
            let (sequence, action_key_digest, state, record_json, checksum) = row?;
            let Some(migration) = migrate_action_event_row(
                sequence,
                &action_key_digest,
                &state,
                &record_json,
                &checksum,
            )?
            else {
                current_digests.insert(Digest::parse(&action_key_digest)?);
                continue;
            };
            current_digests.insert(migration.new_digest.clone());
            register_action_key_mapping(
                &mut mappings,
                &migration.old_digest,
                &migration.new_digest,
            )?;
            event_migrations.push(migration);
        }
    }

    let mut lease_migrations = Vec::new();
    {
        let mut statement = transaction.prepare(
            "SELECT action_key_digest, action_key_json
             FROM producer_leases",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (action_key_digest, action_key_json) = row?;
            let stored_digest = Digest::parse(&action_key_digest)?;
            let action_key_value: Value = serde_json::from_str(&action_key_json)?;
            let (old_digest, new_digest, migrated_action_key) =
                action_key_migration(&action_key_value)?;
            if stored_digest != old_digest {
                return Err(JournalError::InvalidState(
                    "stored producer lease action key digest mismatch".to_owned(),
                ));
            }
            register_action_key_mapping(&mut mappings, &old_digest, &new_digest)?;
            current_digests.insert(new_digest.clone());
            if old_digest != new_digest {
                let action_key_json = String::from_utf8(
                    canonical_json_bytes(&migrated_action_key).expect("canonical JSON is UTF-8"),
                )
                .expect("serde_json emits UTF-8");
                lease_migrations.push(ProducerLeaseMigration {
                    old_digest,
                    new_digest,
                    action_key_json,
                });
            }
        }
    }

    if schema_version < JOURNAL_SCHEMA_VERSION || !mappings.is_empty() {
        validate_action_key_references(transaction, &mappings, &current_digests)?;
    }

    for migration in event_migrations {
        transaction.execute(
            "UPDATE action_events
             SET action_key_digest = ?1, state = ?2, record_json = ?3, checksum = ?4
             WHERE sequence = ?5",
            params![
                migration.new_digest.to_string(),
                migration.state,
                migration.record_json,
                migration.checksum,
                migration.sequence,
            ],
        )?;
    }
    for migration in lease_migrations {
        transaction.execute(
            "UPDATE producer_leases
             SET action_key_digest = ?1, action_key_json = ?2
             WHERE action_key_digest = ?3",
            params![
                migration.new_digest.to_string(),
                migration.action_key_json,
                migration.old_digest.to_string(),
            ],
        )?;
    }
    for (old_digest, new_digest) in mappings {
        let old_digest = old_digest.to_string();
        let new_digest = new_digest.to_string();
        transaction.execute(
            "UPDATE action_consumers SET action_key_digest = ?1
             WHERE action_key_digest = ?2",
            params![new_digest, old_digest],
        )?;
        transaction.execute(
            "UPDATE action_retention SET action_key_digest = ?1
             WHERE action_key_digest = ?2",
            params![new_digest, old_digest],
        )?;
        transaction.execute(
            "UPDATE action_termination_claims SET action_key_digest = ?1
             WHERE action_key_digest = ?2",
            params![new_digest, old_digest],
        )?;
        transaction.execute(
            "UPDATE action_trust_revocations SET action_key_digest = ?1
             WHERE action_key_digest = ?2",
            params![new_digest, old_digest],
        )?;
    }
    Ok(())
}

fn validate_action_key_references(
    transaction: &Transaction<'_>,
    mappings: &BTreeMap<Digest, Digest>,
    current_digests: &BTreeSet<Digest>,
) -> Result<(), JournalError> {
    for table in [
        "action_consumers",
        "action_retention",
        "action_termination_claims",
        "action_trust_revocations",
    ] {
        let mut statement =
            transaction.prepare(&format!("SELECT action_key_digest FROM {table}"))?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            let digest_text = row?;
            let digest = Digest::parse(&digest_text)?;
            if mappings.contains_key(&digest) || current_digests.contains(&digest) {
                continue;
            }
            return Err(JournalError::InvalidState(format!(
                "unmapped action key digest {digest_text} in {table}"
            )));
        }
    }
    Ok(())
}

fn migrate_action_event_row(
    sequence: i64,
    action_key_digest: &str,
    state: &str,
    record_json: &str,
    checksum: &str,
) -> Result<Option<ActionEventMigration>, JournalError> {
    if checksum_for_raw(action_key_digest, state, record_json) != checksum {
        return Err(JournalError::ChecksumMismatch { sequence });
    }
    let record_value: Value = serde_json::from_str(record_json)?;
    let action_key_value = record_value
        .get("action_key")
        .ok_or_else(|| JournalError::InvalidState("action key is missing from record".into()))?;
    let stored_digest = Digest::parse(action_key_digest)?;
    let (old_digest, new_digest, migrated_action_key) = action_key_migration(action_key_value)?;
    if stored_digest != old_digest {
        return Err(JournalError::KeyMismatch { sequence });
    }

    let record_value = if old_digest != new_digest {
        let mut record_value = record_value;
        record_value["action_key"] = migrated_action_key;
        record_value
    } else {
        record_value
    };
    let record: ActionRecord = serde_json::from_value(record_value)?;
    validate_record(&record)?;
    if state != state_name(record.state) {
        return Err(JournalError::StateMismatch { sequence });
    }
    if old_digest == new_digest {
        return Ok(None);
    }
    let record_json =
        String::from_utf8(canonical_json_bytes(&record).expect("canonical JSON is UTF-8"))
            .expect("serde_json emits UTF-8");
    let checksum = checksum_for(&new_digest, state, &record_json);
    Ok(Some(ActionEventMigration {
        sequence,
        old_digest,
        new_digest,
        state: state.to_owned(),
        record_json,
        checksum,
    }))
}

fn action_key_migration(action_key_value: &Value) -> Result<(Digest, Digest, Value), JournalError> {
    let policy = action_key_value
        .get("execution_policy")
        .and_then(Value::as_object)
        .ok_or_else(|| JournalError::InvalidState("execution policy is malformed".into()))?;
    let action_key: ActionKey = serde_json::from_value(action_key_value.clone())?;
    let current_digest = action_key.digest()?;
    if policy.contains_key("adoptable") {
        return Ok((
            current_digest.clone(),
            current_digest,
            action_key_value.clone(),
        ));
    }

    let mut legacy_action_key = action_key_value.clone();
    legacy_action_key
        .get_mut("execution_policy")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| JournalError::InvalidState("execution policy is malformed".into()))?
        .remove("adoptable");
    let canonical_legacy_action_key = {
        let mut value = serde_json::to_value(&action_key)?;
        value
            .get_mut("execution_policy")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| JournalError::InvalidState("execution policy is malformed".into()))?
            .remove("adoptable");
        value
    };
    if canonical_json_bytes(&legacy_action_key)?
        != canonical_json_bytes(&canonical_legacy_action_key)?
    {
        return Err(JournalError::InvalidState(
            "legacy action key is not canonical".to_owned(),
        ));
    }
    let legacy_digest = Digest::from_bytes(&canonical_json_bytes(&legacy_action_key)?);
    let mut migrated_action_key = action_key_value.clone();
    migrated_action_key
        .get_mut("execution_policy")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| JournalError::InvalidState("execution policy is malformed".into()))?
        .insert("adoptable".to_owned(), Value::Bool(true));
    let migrated_action_key: ActionKey = serde_json::from_value(migrated_action_key.clone())?;
    let new_digest = migrated_action_key.digest()?;
    Ok((
        legacy_digest,
        new_digest,
        serde_json::to_value(migrated_action_key)?,
    ))
}

fn register_action_key_mapping(
    mappings: &mut BTreeMap<Digest, Digest>,
    old_digest: &Digest,
    new_digest: &Digest,
) -> Result<(), JournalError> {
    if old_digest == new_digest {
        return Ok(());
    }
    if mappings.iter().any(|(existing_old, existing_new)| {
        existing_old != old_digest && existing_new == new_digest
    }) {
        return Err(JournalError::InvalidState(
            "legacy action key digests converge on one migrated digest".to_owned(),
        ));
    }
    if let Some(existing) = mappings.insert(old_digest.clone(), new_digest.clone()) {
        if existing != *new_digest {
            return Err(JournalError::InvalidState(
                "legacy action key has inconsistent migrated digest".to_owned(),
            ));
        }
    }
    Ok(())
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

    fn legacy_record_material(record: &ActionRecord) -> (Digest, String, String) {
        let mut value = serde_json::to_value(record).unwrap();
        value["action_key"]["execution_policy"]
            .as_object_mut()
            .unwrap()
            .remove("adoptable");
        let action_key_value = value["action_key"].clone();
        let old_digest = Digest::from_bytes(&canonical_json_bytes(&action_key_value).unwrap());
        let record_json = String::from_utf8(canonical_json_bytes(&value).unwrap()).unwrap();
        let action_key_json =
            String::from_utf8(canonical_json_bytes(&action_key_value).unwrap()).unwrap();
        (old_digest, record_json, action_key_json)
    }

    fn insert_legacy_event(journal: &ActionJournal, record: &ActionRecord) -> Digest {
        let (old_digest, record_json, _) = legacy_record_material(record);
        let state = state_name(record.state);
        let checksum = checksum_for(&old_digest, state, &record_json);
        journal
            .connection
            .execute(
                "INSERT INTO action_events(
                     action_key_digest, state, record_json, checksum
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![old_digest.to_string(), state, record_json, checksum],
            )
            .unwrap();
        old_digest
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
    fn pre_adoptable_rows_migrate_keys_and_all_digest_references() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("adoptable-migration.sqlite");
        let legacy_record = record(54, ActionState::Running);
        let action = legacy_record.action_key.clone();
        let new_digest = action.digest().unwrap();
        let (old_digest, record_json, action_key_json) = legacy_record_material(&legacy_record);
        let state = state_name(legacy_record.state);
        let checksum = checksum_for(&old_digest, state, &record_json);
        let journal = ActionJournal::open(&path).unwrap();
        journal
            .connection
            .execute(
                "UPDATE action_journal_meta SET value = '2' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_events(
                     action_key_digest, state, record_json, checksum
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![old_digest.to_string(), state, record_json, checksum],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO producer_leases(
                     action_key_digest, action_key_json, generation, owner,
                     expires_at_ms, heartbeat_every_ms, lease_duration_ms, state
                 ) VALUES (?1, ?2, 1, 'worker-a', 100, 25, 100, 'active')",
                params![old_digest.to_string(), action_key_json],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_consumers(action_key_digest, run_id)
                 VALUES (?1, 'run-a')",
                [old_digest.to_string()],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                 VALUES (?1, 500)",
                [old_digest.to_string()],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_termination_claims(
                     action_key_digest, reason, claimed_at_ms, completed
                 ) VALUES (?1, 'no_consumers', 0, 0)",
                [old_digest.to_string()],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_trust_revocations(
                     action_key_digest, reason, revoked_at_ms
                 ) VALUES (?1, 'legacy', 0)",
                [old_digest.to_string()],
            )
            .unwrap();
        drop(journal);

        let reopened = ActionJournal::open(&path).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), JOURNAL_SCHEMA_VERSION);
        let entries = reopened.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].record.action_key.execution_policy.adoptable);
        assert_eq!(entries[0].record.action_key.digest().unwrap(), new_digest);
        for table in [
            "action_consumers",
            "action_retention",
            "action_termination_claims",
            "action_trust_revocations",
        ] {
            let migrated: i64 = reopened
                .connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE action_key_digest = ?1"),
                    [new_digest.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            let legacy: i64 = reopened
                .connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE action_key_digest = ?1"),
                    [old_digest.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(migrated, 1, "{table}");
            assert_eq!(legacy, 0, "{table}");
        }
        drop(reopened);

        let clock = TestClock::default();
        let mut manager = LeaseManager::open(&path, clock.clone()).unwrap();
        assert_eq!(
            manager.lease_status(&action).unwrap(),
            Some(LeaseStatus::Active)
        );
        assert_eq!(manager.live_consumer_count(&action).unwrap(), 1);
        let mut lease = ProducerLease {
            action,
            generation: 1,
            owner: "worker-a".to_owned(),
            expires_at: LogicalInstant::from_millis(100),
            heartbeat_every: 25,
            lease_duration: 100,
        };
        clock.advance(25);
        manager.renew(&mut lease).unwrap();
        manager.release(&lease).unwrap();
        assert_eq!(
            manager.lease_status(&lease.action).unwrap(),
            Some(LeaseStatus::Released)
        );
    }

    #[test]
    fn malformed_adoptable_migration_rolls_back() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("malformed-adoptable-migration.sqlite");
        let valid_record = record(55, ActionState::Running);
        let malformed_record = record(56, ActionState::Running);
        let journal = ActionJournal::open(&path).unwrap();
        let valid_old_digest = insert_legacy_event(&journal, &valid_record);
        let (malformed_old_digest, _, _) = legacy_record_material(&malformed_record);
        let malformed_state = state_name(malformed_record.state);
        let malformed_json = "not-json";
        let malformed_checksum =
            checksum_for(&malformed_old_digest, malformed_state, malformed_json);
        journal
            .connection
            .execute(
                "INSERT INTO action_events(
                     action_key_digest, state, record_json, checksum
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    malformed_old_digest.to_string(),
                    malformed_state,
                    malformed_json,
                    malformed_checksum,
                ],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "UPDATE action_journal_meta SET value = '2' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        drop(journal);

        assert!(matches!(
            ActionJournal::open(&path),
            Err(JournalError::Json(_))
        ));
        let connection = rusqlite::Connection::open(&path).unwrap();
        let schema_version: String = connection
            .query_row(
                "SELECT value FROM action_journal_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(schema_version, "2");
        let stored_json: String = connection
            .query_row(
                "SELECT record_json FROM action_events WHERE action_key_digest = ?1",
                [valid_old_digest.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!stored_json.contains("\"adoptable\""));
    }

    #[test]
    fn orphan_digest_reference_migration_fails_and_rolls_back_atomically() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("orphan-adoptable-migration.sqlite");
        let legacy_record = record(57, ActionState::Running);
        let new_digest = legacy_record.action_key.digest().unwrap();
        let orphan_digest = digest(200);
        assert_ne!(new_digest, orphan_digest);

        let journal = ActionJournal::open(&path).unwrap();
        let old_digest = insert_legacy_event(&journal, &legacy_record);
        journal
            .connection
            .execute(
                "INSERT INTO action_consumers(action_key_digest, run_id)
                 VALUES (?1, ?2)",
                params![old_digest.to_string(), "run-valid"],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_consumers(action_key_digest, run_id)
                 VALUES (?1, ?2)",
                params![orphan_digest.to_string(), "run-orphan"],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                 VALUES (?1, 500)",
                [old_digest.to_string()],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                 VALUES (?1, 500)",
                [orphan_digest.to_string()],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_termination_claims(
                     action_key_digest, reason, claimed_at_ms, completed
                 ) VALUES (?1, 'legacy', 0, 0)",
                [old_digest.to_string()],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_termination_claims(
                     action_key_digest, reason, claimed_at_ms, completed
                 ) VALUES (?1, 'orphan', 0, 0)",
                [orphan_digest.to_string()],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_trust_revocations(
                     action_key_digest, reason, revoked_at_ms
                 ) VALUES (?1, 'legacy', 0)",
                [old_digest.to_string()],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "INSERT INTO action_trust_revocations(
                     action_key_digest, reason, revoked_at_ms
                 ) VALUES (?1, 'orphan', 0)",
                [orphan_digest.to_string()],
            )
            .unwrap();
        journal
            .connection
            .execute(
                "UPDATE action_journal_meta SET value = '2' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        drop(journal);

        let error = match ActionJournal::open(&path) {
            Ok(_) => panic!("orphan digest reference must abort migration"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unmapped action key digest"));

        let connection = rusqlite::Connection::open(&path).unwrap();
        let schema_version: String = connection
            .query_row(
                "SELECT value FROM action_journal_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(schema_version, "2");
        let stored_json: String = connection
            .query_row(
                "SELECT record_json FROM action_events WHERE action_key_digest = ?1",
                [old_digest.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!stored_json.contains("\"adoptable\""));
        for table in [
            "action_consumers",
            "action_retention",
            "action_termination_claims",
            "action_trust_revocations",
        ] {
            for (digest, expected) in [
                (&old_digest, 1_i64),
                (&orphan_digest, 1_i64),
                (&new_digest, 0_i64),
            ] {
                let count: i64 = connection
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE action_key_digest = ?1"),
                        [digest.to_string()],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(count, expected, "{table} {digest}");
            }
        }
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

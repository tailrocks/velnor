//! Sequential, idempotent, transactional schema migrations.

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use velnor_model::{ExitClass, Timestamp};

use super::error::{StoreError, StoreResult};
use super::rfc3339;

/// Current schema version every fresh or reopened database converges to.
pub const LATEST_SCHEMA_VERSION: u32 = 12;

/// Lease after which an abandoned migration lock is considered stale.
pub(crate) const LOCK_LEASE: Duration = Duration::from_secs(15);

/// One sequential schema step; `version` equals its position starting at 1.
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS instances (
    instance_slug TEXT PRIMARY KEY,
    host TEXT NOT NULL,
    daemon_version TEXT NOT NULL,
    slots_configured INTEGER NOT NULL DEFAULT 0,
    slots_busy INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS slots (
    instance_slug TEXT NOT NULL,
    name TEXT NOT NULL,
    host TEXT NOT NULL,
    slot_index INTEGER NOT NULL,
    slot_kind TEXT NOT NULL,
    phase TEXT NOT NULL,
    job_name TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (instance_slug, name)
);
CREATE TABLE IF NOT EXISTS runner_registrations (
    instance_slug TEXT NOT NULL,
    runner_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    ephemeral INTEGER NOT NULL DEFAULT 0,
    online INTEGER NOT NULL DEFAULT 0,
    labels_json TEXT NOT NULL DEFAULT '{}',
    registered_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (instance_slug, runner_id)
);
CREATE TABLE IF NOT EXISTS jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_slug TEXT NOT NULL,
    job_uid TEXT NOT NULL,
    repository TEXT NOT NULL,
    workflow TEXT NOT NULL,
    job_name TEXT NOT NULL,
    run_id INTEGER,
    attempt INTEGER,
    head_ref TEXT,
    head_sha TEXT,
    trigger_event TEXT,
    queued_at TEXT,
    acquired_at TEXT,
    runner_name TEXT,
    trust_scope TEXT,
    resource_policy TEXT,
    phase TEXT NOT NULL,
    conclusion TEXT,
    infrastructure_category TEXT,
    updated_at TEXT NOT NULL,
    UNIQUE (instance_slug, job_uid)
);
CREATE INDEX IF NOT EXISTS idx_jobs_repository ON jobs (repository);
CREATE TABLE IF NOT EXISTS job_transitions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_slug TEXT NOT NULL,
    job_uid TEXT NOT NULL,
    transition_token TEXT NOT NULL,
    correlation_id TEXT NOT NULL,
    reason TEXT NOT NULL,
    message TEXT,
    transition_time TEXT NOT NULL,
    UNIQUE (instance_slug, job_uid, transition_token)
);
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_slug TEXT NOT NULL,
    event_kind TEXT NOT NULL,
    subject TEXT NOT NULL,
    correlation_id TEXT,
    occurred_at TEXT NOT NULL,
    detail TEXT
);
CREATE TABLE IF NOT EXISTS reconciliations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_slug TEXT NOT NULL,
    kind TEXT NOT NULL,
    subject TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    detail TEXT
);
CREATE INDEX IF NOT EXISTS idx_events_subject ON events (instance_slug, subject, id);
";

/// Historical v2 DDL. It is retained byte-for-byte because migration history
/// is append-only. v4 repairs databases that passed through this index.
const SCHEMA_V2: &str = "
CREATE UNIQUE INDEX IF NOT EXISTS uq_jobs_instance_run_attempt
ON jobs (instance_slug, run_id, attempt)
WHERE run_id IS NOT NULL AND attempt IS NOT NULL;
";

/// Repair databases that passed through the original v2 unique index.
const SCHEMA_V4: &str = "
DROP INDEX IF EXISTS uq_jobs_instance_run_attempt;
CREATE INDEX IF NOT EXISTS idx_jobs_instance_run_attempt
ON jobs (instance_slug, run_id, attempt)
WHERE run_id IS NOT NULL AND attempt IS NOT NULL;
";

/// Add the normalized execution-slot identity without rewriting history.
const SCHEMA_V5: &str = "
ALTER TABLE jobs ADD COLUMN slot_name TEXT;
";

/// Durable per-instance lifecycle intent and idempotency ledger.
const SCHEMA_V6: &str = "
ALTER TABLE instances ADD COLUMN desired_state TEXT NOT NULL DEFAULT 'ready';
ALTER TABLE instances ADD COLUMN observed_state TEXT NOT NULL DEFAULT 'ready';
ALTER TABLE instances ADD COLUMN resource_version INTEGER NOT NULL DEFAULT 1;
CREATE TABLE IF NOT EXISTS lifecycle_operations (
    instance_slug TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    target TEXT NOT NULL,
    reason TEXT NOT NULL,
    desired_state TEXT NOT NULL,
    resource_version INTEGER NOT NULL,
    phase TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (instance_slug, idempotency_key),
    UNIQUE (operation_id)
);
";

/// Persist dynamic stable-slot intent alongside lifecycle state.
const SCHEMA_V7: &str = "
ALTER TABLE instances ADD COLUMN desired_slots INTEGER;
ALTER TABLE lifecycle_operations ADD COLUMN desired_slots INTEGER;
";

/// Indexes for the normalized event stream. API projections never get stored
/// as opaque JSON blobs in the operational database.
const SCHEMA_V8: &str = "
CREATE INDEX IF NOT EXISTS idx_events_instance_id
ON events (instance_slug, id);
CREATE INDEX IF NOT EXISTS idx_events_instance_kind_id
ON events (instance_slug, event_kind COLLATE NOCASE, id);
CREATE INDEX IF NOT EXISTS idx_events_instance_subject_id
ON events (instance_slug, subject COLLATE NOCASE, id);
CREATE TABLE IF NOT EXISTS event_stream_state (
    instance_slug TEXT PRIMARY KEY,
    first_retained_id INTEGER NOT NULL CHECK (first_retained_id >= 1),
    high_water_id INTEGER NOT NULL CHECK (high_water_id >= 0),
    updated_at TEXT NOT NULL,
    CHECK (first_retained_id <= high_water_id + 1)
);
INSERT OR IGNORE INTO event_stream_state
    (instance_slug, first_retained_id, high_water_id, updated_at)
SELECT instance_slug, MIN(id), MAX(id), strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
FROM events
GROUP BY instance_slug;
";

/// Retention indexes, exact lifecycle ancestry lookup, and transactional row
/// counters. The expression indexes normalize RFC3339 values to SQLite's
/// Julian-day representation for indexed oldest-row lookup; Rust still
/// returns the original timestamp text and rejects malformed values.
const SCHEMA_V9: &str = "
ALTER TABLE retention_state ADD COLUMN job_rows INTEGER NOT NULL DEFAULT 0;
ALTER TABLE retention_state ADD COLUMN event_rows INTEGER NOT NULL DEFAULT 0;
ALTER TABLE retention_state ADD COLUMN transition_rows INTEGER NOT NULL DEFAULT 0;
UPDATE retention_state SET
    job_rows = (SELECT COUNT(*) FROM jobs),
    event_rows = (SELECT COUNT(*) FROM events),
    transition_rows = (SELECT COUNT(*) FROM job_transitions)
WHERE singleton = 0;
CREATE INDEX IF NOT EXISTS idx_jobs_retention_phase_id
ON jobs (phase, id, updated_at);
CREATE INDEX IF NOT EXISTS idx_events_retention_time_id
ON events (julianday(occurred_at), id);
CREATE INDEX IF NOT EXISTS idx_transitions_retention_time_id
ON job_transitions (julianday(transition_time), id);
CREATE INDEX IF NOT EXISTS idx_transitions_ancestry
ON job_transitions (instance_slug, job_uid, correlation_id, reason);
CREATE INDEX IF NOT EXISTS idx_events_ancestry
ON events (instance_slug, subject, correlation_id, event_kind, id);
CREATE INDEX IF NOT EXISTS idx_reconciliations_retention
ON reconciliations (instance_slug, status, subject);
CREATE TRIGGER IF NOT EXISTS retention_jobs_insert
AFTER INSERT ON jobs
BEGIN
    UPDATE retention_state SET job_rows = job_rows + 1 WHERE singleton = 0;
END;
CREATE TRIGGER IF NOT EXISTS retention_jobs_delete
AFTER DELETE ON jobs
BEGIN
    UPDATE retention_state
    SET job_rows = CASE WHEN job_rows > 0 THEN job_rows - 1 ELSE 0 END
    WHERE singleton = 0;
END;
CREATE TRIGGER IF NOT EXISTS retention_events_insert
AFTER INSERT ON events
BEGIN
    UPDATE retention_state SET event_rows = event_rows + 1 WHERE singleton = 0;
END;
CREATE TRIGGER IF NOT EXISTS retention_events_delete
AFTER DELETE ON events
BEGIN
    UPDATE retention_state
    SET event_rows = CASE WHEN event_rows > 0 THEN event_rows - 1 ELSE 0 END
    WHERE singleton = 0;
END;
CREATE TRIGGER IF NOT EXISTS retention_transitions_insert
AFTER INSERT ON job_transitions
BEGIN
    UPDATE retention_state SET transition_rows = transition_rows + 1 WHERE singleton = 0;
END;
CREATE TRIGGER IF NOT EXISTS retention_transitions_delete
AFTER DELETE ON job_transitions
BEGIN
    UPDATE retention_state
    SET transition_rows = CASE WHEN transition_rows > 0 THEN transition_rows - 1 ELSE 0 END
    WHERE singleton = 0;
END;
";

/// Replay form for tests and operators that deliberately re-run migration
/// history. SQLite has no portable `ADD COLUMN IF NOT EXISTS`; the caller
/// selects this form after detecting the v9 columns.
const SCHEMA_V9_REPLAY: &str = "
UPDATE retention_state SET
    job_rows = (SELECT COUNT(*) FROM jobs),
    event_rows = (SELECT COUNT(*) FROM events),
    transition_rows = (SELECT COUNT(*) FROM job_transitions)
WHERE singleton = 0;
CREATE INDEX IF NOT EXISTS idx_jobs_retention_phase_id
ON jobs (phase, id, updated_at);
CREATE INDEX IF NOT EXISTS idx_events_retention_time_id
ON events (julianday(occurred_at), id);
CREATE INDEX IF NOT EXISTS idx_transitions_retention_time_id
ON job_transitions (julianday(transition_time), id);
CREATE INDEX IF NOT EXISTS idx_transitions_ancestry
ON job_transitions (instance_slug, job_uid, correlation_id, reason);
CREATE INDEX IF NOT EXISTS idx_events_ancestry
ON events (instance_slug, subject, correlation_id, event_kind, id);
CREATE INDEX IF NOT EXISTS idx_reconciliations_retention
ON reconciliations (instance_slug, status, subject);
CREATE TRIGGER IF NOT EXISTS retention_jobs_insert
AFTER INSERT ON jobs
BEGIN
    UPDATE retention_state SET job_rows = job_rows + 1 WHERE singleton = 0;
END;
CREATE TRIGGER IF NOT EXISTS retention_jobs_delete
AFTER DELETE ON jobs
BEGIN
    UPDATE retention_state
    SET job_rows = CASE WHEN job_rows > 0 THEN job_rows - 1 ELSE 0 END
    WHERE singleton = 0;
END;
CREATE TRIGGER IF NOT EXISTS retention_events_insert
AFTER INSERT ON events
BEGIN
    UPDATE retention_state SET event_rows = event_rows + 1 WHERE singleton = 0;
END;
CREATE TRIGGER IF NOT EXISTS retention_events_delete
AFTER DELETE ON events
BEGIN
    UPDATE retention_state
    SET event_rows = CASE WHEN event_rows > 0 THEN event_rows - 1 ELSE 0 END
    WHERE singleton = 0;
END;
CREATE TRIGGER IF NOT EXISTS retention_transitions_insert
AFTER INSERT ON job_transitions
BEGIN
    UPDATE retention_state SET transition_rows = transition_rows + 1 WHERE singleton = 0;
END;
CREATE TRIGGER IF NOT EXISTS retention_transitions_delete
AFTER DELETE ON job_transitions
BEGIN
    UPDATE retention_state
    SET transition_rows = CASE WHEN transition_rows > 0 THEN transition_rows - 1 ELSE 0 END
    WHERE singleton = 0;
END;
";

/// Durable cross-process retention ownership. The singleton table keeps lease
/// state separate from retention counters so this migration is naturally
/// idempotent on fresh, reopened, and partially initialized databases.
const SCHEMA_V10: &str = "
CREATE TABLE IF NOT EXISTS retention_lease (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 0),
    owner TEXT,
    expires_at INTEGER
);
INSERT OR IGNORE INTO retention_lease (singleton) VALUES (0);
";

/// Fencing generation for the durable retention lease. Every successful
/// takeover (including an explicit renewal by the same owner) advances this
/// value, invalidating capabilities issued by the prior generation.
const SCHEMA_V11: &str = "
ALTER TABLE retention_lease
    ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;
";

/// Exact ownership for lifecycle events. Existing events remain generic with
/// NULL ownership; only new state-machine writes receive a transition ID.
/// Retention must never infer ancestry from subject, correlation, or kind.
const SCHEMA_V12: &str = "
ALTER TABLE events ADD COLUMN transition_id INTEGER REFERENCES job_transitions(id);
ALTER TABLE events ADD COLUMN reconciliation_id INTEGER REFERENCES reconciliations(id);
CREATE INDEX IF NOT EXISTS idx_events_transition_id
ON events (transition_id, id);
CREATE INDEX IF NOT EXISTS idx_events_reconciliation_id
ON events (reconciliation_id, id);
CREATE INDEX IF NOT EXISTS idx_transitions_instance_job_id
ON job_transitions (instance_slug, job_uid, id);
CREATE INDEX IF NOT EXISTS idx_events_instance_transition_id
ON events (instance_slug, transition_id, id);
CREATE INDEX IF NOT EXISTS idx_events_instance_reconciliation_id
ON events (instance_slug, reconciliation_id, id);
";

const SCHEMA_V12_REPLAY: &str = "
CREATE INDEX IF NOT EXISTS idx_events_transition_id
ON events (transition_id, id);
CREATE INDEX IF NOT EXISTS idx_events_reconciliation_id
ON events (reconciliation_id, id);
CREATE INDEX IF NOT EXISTS idx_transitions_instance_job_id
ON job_transitions (instance_slug, job_uid, id);
CREATE INDEX IF NOT EXISTS idx_events_instance_transition_id
ON events (instance_slug, transition_id, id);
CREATE INDEX IF NOT EXISTS idx_events_instance_reconciliation_id
ON events (instance_slug, reconciliation_id, id);
";

const SCHEMA_V6_REPLAY: &str = "
CREATE TABLE IF NOT EXISTS lifecycle_operations (
    instance_slug TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    target TEXT NOT NULL,
    reason TEXT NOT NULL,
    desired_state TEXT NOT NULL,
    resource_version INTEGER NOT NULL,
    phase TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (instance_slug, idempotency_key),
    UNIQUE (operation_id)
);
";

/// Bounded retention state (Plan 066 step 5): the singleton row records the
/// last completed prune so accounting can publish it without re-deriving.
const SCHEMA_V3: &str = "
CREATE TABLE IF NOT EXISTS retention_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 0),
    last_prune_at TEXT,
    deleted_events INTEGER NOT NULL DEFAULT 0,
    deleted_jobs INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO retention_state (singleton) VALUES (0);
";

/// Every migration in order; appending is the only allowed change.
pub static MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "operational-store-baseline",
        sql: SCHEMA_V1,
    },
    Migration {
        version: 2,
        name: "jobs-summary-run-attempt-identity",
        sql: SCHEMA_V2,
    },
    Migration {
        version: 3,
        name: "bounded-retention-state",
        sql: SCHEMA_V3,
    },
    Migration {
        version: 4,
        name: "job-run-attempt-index-is-not-unique",
        sql: SCHEMA_V4,
    },
    Migration {
        version: 5,
        name: "job-slot-identity",
        sql: SCHEMA_V5,
    },
    Migration {
        version: 6,
        name: "durable-lifecycle-intent",
        sql: SCHEMA_V6,
    },
    Migration {
        version: 7,
        name: "durable-scale-intent",
        sql: SCHEMA_V7,
    },
    Migration {
        version: 8,
        name: "normalized-event-cursor-state",
        sql: SCHEMA_V8,
    },
    Migration {
        version: 9,
        name: "bounded-retention-counters-and-indexes",
        sql: SCHEMA_V9,
    },
    Migration {
        version: 10,
        name: "durable-retention-lease",
        sql: SCHEMA_V10,
    },
    Migration {
        version: 11,
        name: "fenced-retention-lease-generation",
        sql: SCHEMA_V11,
    },
    Migration {
        version: 12,
        name: "exact-lifecycle-event-ancestry",
        sql: SCHEMA_V12,
    },
];

const META_TABLES_SQL: &str = "
CREATE TABLE IF NOT EXISTS schema_version (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 0),
    version INTEGER NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS migration_lock (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 0),
    owner TEXT,
    acquired_at TEXT,
    heartbeat_at TEXT
);
";

/// Create the version and lock tables and seed their single rows.
pub(crate) fn ensure_meta_tables(conn: &Connection) -> StoreResult<()> {
    conn.execute_batch(META_TABLES_SQL)?;
    let now = rfc3339(Timestamp::now());
    conn.execute(
        "INSERT OR IGNORE INTO schema_version (singleton, version, updated_at) VALUES (0, 0, ?1)",
        [&now],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO migration_lock (singleton, owner, acquired_at, heartbeat_at)
         VALUES (0, NULL, NULL, NULL)",
        [],
    )?;
    Ok(())
}

/// The stored schema version; zero before the first migration ran.
pub(crate) fn current_version(conn: &Connection) -> StoreResult<u32> {
    let version: u32 = conn.query_row(
        "SELECT version FROM schema_version WHERE singleton = 0",
        [],
        |row| row.get(0),
    )?;
    if version >= LATEST_SCHEMA_VERSION && !v12_schema_complete(conn)? {
        return Err(StoreError::new(
            ExitClass::Operation,
            "store.schema.incomplete",
        )
        .with_remediation(
            "schema version 12 is recorded but its exact lifecycle-event identity columns or indexes are incomplete; restore the database from a consistent backup or rerun the migration transaction",
        ));
    }
    Ok(version)
}

fn lock_row(conn: &Connection) -> StoreResult<(Option<String>, Option<String>)> {
    let row: (Option<String>, Option<String>) = conn.query_row(
        "SELECT owner, heartbeat_at FROM migration_lock WHERE singleton = 0",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(row)
}

/// Whether a non-empty owner still holds a fresh heartbeat lease.
fn lease_live(owner: &Option<String>, heartbeat_at: &Option<String>) -> bool {
    if owner.as_deref().is_none_or(str::is_empty) {
        return false;
    }
    match heartbeat_at
        .as_deref()
        .and_then(|raw| Timestamp::parse(raw).ok())
    {
        Some(beat) => {
            let age = Timestamp::now().as_offset_datetime() - beat.as_offset_datetime();
            if age.is_negative() {
                Duration::try_from(-age).is_ok_and(|d| d < LOCK_LEASE)
            } else {
                age.is_zero() || Duration::try_from(age).is_ok_and(|d| d < LOCK_LEASE)
            }
        }
        None => false,
    }
}

/// Refresh the heartbeat of the lock held by `owner`.
fn heartbeat(conn: &Connection, owner: &str) -> StoreResult<()> {
    let now = rfc3339(Timestamp::now());
    let updated = conn.execute(
        "UPDATE migration_lock SET heartbeat_at = ?1 WHERE singleton = 0 AND owner = ?2",
        rusqlite::params![now, owner],
    )?;
    if updated != 1 {
        return Err(
            StoreError::new(ExitClass::Conflict, "store.migration.lock.lost").with_remediation(
                "migration ownership changed; abort and retry from durable state",
            ),
        );
    }
    Ok(())
}

/// Release the migration lock when still owned by `owner`.
pub(crate) fn release_lock(conn: &Connection, owner: &str) -> StoreResult<()> {
    conn.execute(
        "UPDATE migration_lock SET owner = NULL, acquired_at = NULL, heartbeat_at = NULL
         WHERE singleton = 0 AND owner = ?1",
        [owner],
    )?;
    Ok(())
}

/// Try to take the migration lock for `owner` without waiting.
///
/// Returns `Ok(true)` when acquired. Returns `Ok(false)` when another
/// process holds a live lease; a stale lease (crashed daemon past
/// [`LOCK_LEASE`]) is stolen by exact-owner CAS so two stealers cannot both
/// win.
fn try_acquire_lock(conn: &Connection, owner: &str) -> StoreResult<bool> {
    let (held_owner, held_beat) = lock_row(conn)?;
    if lease_live(&held_owner, &held_beat) {
        return Ok(false);
    }
    let now = rfc3339(Timestamp::now());
    let stolen = conn.execute(
        "UPDATE migration_lock SET owner = ?1, acquired_at = ?2, heartbeat_at = ?2
         WHERE singleton = 0
           AND ((owner IS NULL OR owner = '')
                OR (owner = ?3 AND heartbeat_at IS ?4))",
        rusqlite::params![owner, now, held_owner, held_beat],
    )?;
    Ok(stolen > 0)
}

/// Wait up to `wait` total for the migration lock, polling [`LOCK_POLL`].
///
/// # Errors
/// [`ExitClass::Timeout`] naming the current owner when the wait elapses.
pub(crate) fn acquire_lock(conn: &Connection, owner: &str, wait: Duration) -> StoreResult<()> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        if try_acquire_lock(conn, owner)? {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            let holder = lock_row(conn)?.0.unwrap_or_else(|| "<unknown>".to_owned());
            return Err(StoreError::new(ExitClass::Timeout, "store.migration.lock.busy")
                .with_remediation(format!(
                    "migration lock held by {holder}; verify that daemon is alive or wait for its {LEASE_SECS}s lease to expire"
                )));
        }
        std::thread::sleep(LOCK_POLL);
    }
}

const LEASE_SECS: u64 = LOCK_LEASE.as_secs();
const LOCK_POLL: Duration = Duration::from_millis(25);

/// Apply every pending migration under the held lock.
///
/// Each migration runs in its own transaction together with the version
/// bump, so any failure rolls back both DDL and version atomically.
/// `hook`, when provided, runs inside that transaction before commit and is
/// the test seam for injected mid-migration failure.
pub(crate) fn apply_pending(
    conn: &mut Connection,
    owner: &str,
    hook: Option<&dyn Fn(u32) -> StoreResult<()>>,
) -> StoreResult<u32> {
    let mut version = current_version(conn)?;
    for migration in MIGRATIONS {
        if migration.version <= version {
            continue;
        }
        heartbeat(conn, owner)?;
        // Immediate: migration DDL must not race a lock upgrade against
        // concurrent daemon readers; the busy timeout governs the wait.
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        heartbeat(&transaction, owner)?;
        // A v1 database may already contain multiple jobs sharing one run
        // and attempt. Historical v2 cannot create its unique index there;
        // record v2 without that DDL and let appended v4 perform the safe
        // non-unique repair. No rows are rewritten or discarded.
        let slot_column_exists = migration.version == 5 && has_slot_column(&transaction)?;
        let lifecycle_columns_exist =
            migration.version == 6 && has_column(&transaction, "instances", "desired_state")?;
        let scale_columns_exist = migration.version == 7
            && has_column(&transaction, "instances", "desired_slots")?
            && has_column(&transaction, "lifecycle_operations", "desired_slots")?;
        let retention_columns_exist =
            migration.version == 9 && has_column(&transaction, "retention_state", "job_rows")?;
        let retention_generation_exists =
            migration.version == 11 && has_column(&transaction, "retention_lease", "generation")?;
        let event_transition_column_exists =
            migration.version == 12 && has_column(&transaction, "events", "transition_id")?;
        let event_reconciliation_column_exists =
            migration.version == 12 && has_column(&transaction, "events", "reconciliation_id")?;
        if migration.version == 12
            && event_transition_column_exists != event_reconciliation_column_exists
        {
            return Err(StoreError::new(
                ExitClass::Operation,
                "store.schema.incomplete",
            )
            .with_remediation(
                "v12 exact lifecycle-event identity is partial; both transition_id and reconciliation_id must be present before the schema version can advance",
            ));
        }
        if (migration.version != 2 || !has_run_attempt_duplicates(&transaction)?)
            && !slot_column_exists
            && !retention_generation_exists
        {
            let sql = if lifecycle_columns_exist {
                SCHEMA_V6_REPLAY
            } else if scale_columns_exist {
                ""
            } else if retention_columns_exist {
                SCHEMA_V9_REPLAY
            } else if event_transition_column_exists && event_reconciliation_column_exists {
                SCHEMA_V12_REPLAY
            } else {
                migration.sql
            };
            transaction.execute_batch(sql)?;
        }
        if migration.version == 12 && !v12_schema_complete(&transaction)? {
            return Err(StoreError::new(
                ExitClass::Operation,
                "store.schema.incomplete",
            )
            .with_remediation(
                "v12 exact lifecycle-event identity columns and indexes did not converge transactionally; the schema version remains unchanged",
            ));
        }
        if let Some(hook) = hook {
            hook(migration.version)?;
        }
        let rendered = rfc3339(Timestamp::now());
        transaction.execute(
            "UPDATE schema_version SET version = ?1, updated_at = ?2 WHERE singleton = 0",
            rusqlite::params![migration.version, rendered],
        )?;
        transaction.commit()?;
        // A lease may expire while DDL runs. Do not report success unless the
        // same owner still controls the migration lock after the commit.
        heartbeat(conn, owner)?;
        version = migration.version;
    }
    Ok(version)
}

fn has_run_attempt_duplicates(conn: &rusqlite::Transaction<'_>) -> StoreResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM jobs
             WHERE run_id IS NOT NULL AND attempt IS NOT NULL
             GROUP BY instance_slug, run_id, attempt
             HAVING COUNT(*) > 1
         )",
        [],
        |row| row.get(0),
    )?)
}

fn has_slot_column(conn: &rusqlite::Transaction<'_>) -> StoreResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM pragma_table_info('jobs') WHERE name = 'slot_name'
         )",
        [],
        |row| row.get(0),
    )?)
}

fn has_column(conn: &rusqlite::Transaction<'_>, table: &str, column: &str) -> StoreResult<bool> {
    let sql = format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1)");
    Ok(conn.query_row(&sql, [column], |row| row.get(0))?)
}

fn has_column_connection(conn: &Connection, table: &str, column: &str) -> StoreResult<bool> {
    let sql = format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1)");
    Ok(conn.query_row(&sql, [column], |row| row.get(0))?)
}

fn has_index_columns(
    conn: &Connection,
    index: &str,
    table: &str,
    expected: &[&str],
) -> StoreResult<bool> {
    let actual_table: Option<String> = conn
        .query_row(
            "SELECT tbl_name FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [index],
            |row| row.get(0),
        )
        .optional()?;
    if actual_table.as_deref() != Some(table) {
        return Ok(false);
    }
    let index_list: Option<(i64, String, i64)> = conn
        .query_row(
            "SELECT \"unique\", origin, partial
             FROM pragma_index_list(?1) WHERE name = ?2",
            rusqlite::params![table, index],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((unique, origin, partial)) = index_list else {
        return Ok(false);
    };
    if unique != 0 || origin != "c" || partial != 0 {
        return Ok(false);
    }

    // `index_info` checks names and order only. `index_xinfo` also exposes
    // DESC flags, collations, expressions, and implicit rowid entries, so a
    // same-named but semantically different index cannot satisfy migration
    // replay by accident.
    let mut statement = conn.prepare(
        "SELECT seqno, cid, name, \"desc\", coll, key
         FROM pragma_index_xinfo(?1)
         WHERE key = 1
         ORDER BY seqno",
    )?;
    let actual = statement
        .query_map([index], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(actual.len() == expected.len()
        && actual
            .iter()
            .enumerate()
            .all(|(position, (seqno, cid, name, desc, coll))| {
                *seqno == position as i64
                    && *cid >= 0
                    && *desc == 0
                    && coll.as_deref() == Some("BINARY")
                    && name.as_deref() == Some(expected[position])
            }))
}

fn v12_schema_complete(conn: &Connection) -> StoreResult<bool> {
    if !has_column_connection(conn, "events", "transition_id")?
        || !has_column_connection(conn, "events", "reconciliation_id")?
    {
        return Ok(false);
    }
    [
        (
            "idx_events_transition_id",
            "events",
            ["transition_id", "id"].as_slice(),
        ),
        (
            "idx_events_reconciliation_id",
            "events",
            ["reconciliation_id", "id"].as_slice(),
        ),
        (
            "idx_transitions_instance_job_id",
            "job_transitions",
            ["instance_slug", "job_uid", "id"].as_slice(),
        ),
        (
            "idx_events_instance_transition_id",
            "events",
            ["instance_slug", "transition_id", "id"].as_slice(),
        ),
        (
            "idx_events_instance_reconciliation_id",
            "events",
            ["instance_slug", "reconciliation_id", "id"].as_slice(),
        ),
    ]
    .into_iter()
    .try_fold(true, |complete, (index, table, columns)| {
        Ok(complete && has_index_columns(conn, index, table, columns)?)
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rusqlite::Connection;
    use velnor_model::Timestamp;

    use crate::store::Store;

    use super::*;

    struct TempDb {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempDb {
        fn new(label: &str) -> Self {
            let nonce = Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
                .unsigned_abs();
            let dir = std::env::temp_dir().join(format!(
                "velnor-migration-{label}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("temporary directory");
            let path = dir.join("state.db");
            Self { dir, path }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn far_future_heartbeat_is_not_live_forever() {
        assert!(!lease_live(
            &Some("future-owner".to_owned()),
            &Some("2099-01-01T00:00:00Z".to_owned())
        ));
    }

    #[test]
    fn partial_v12_identity_fails_closed_before_version_bump() {
        let temp = TempDb::new("partial-v12");
        let mut conn = Connection::open(&temp.path).expect("open database");
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        ensure_meta_tables(&conn).unwrap();
        for migration in MIGRATIONS.iter().take(11) {
            conn.execute_batch(migration.sql).unwrap();
            conn.execute(
                "UPDATE schema_version SET version = ?1, updated_at = ?2 WHERE singleton = 0",
                rusqlite::params![migration.version, "1970-01-01T00:00:00Z"],
            )
            .unwrap();
        }
        conn.execute(
            "ALTER TABLE events ADD COLUMN transition_id INTEGER REFERENCES job_transitions(id)",
            [],
        )
        .unwrap();

        acquire_lock(&conn, "partial-v12-test", Duration::from_secs(1)).unwrap();
        let error = apply_pending(&mut conn, "partial-v12-test", None).unwrap_err();
        assert_eq!(error.envelope.reason, "store.schema.incomplete");
        assert_eq!(current_version(&conn).unwrap(), 11);
        assert!(!has_column_connection(&conn, "events", "reconciliation_id").unwrap());
        release_lock(&conn, "partial-v12-test").unwrap();
    }

    #[test]
    fn wrong_v12_identity_index_fails_closed_before_version_bump() {
        let temp = TempDb::new("wrong-v12-index");
        let mut conn = Connection::open(&temp.path).expect("open database");
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        ensure_meta_tables(&conn).unwrap();
        for migration in MIGRATIONS.iter().take(11) {
            conn.execute_batch(migration.sql).unwrap();
            conn.execute(
                "UPDATE schema_version SET version = ?1, updated_at = ?2 WHERE singleton = 0",
                rusqlite::params![migration.version, "1970-01-01T00:00:00Z"],
            )
            .unwrap();
        }
        conn.execute_batch(
            "ALTER TABLE events ADD COLUMN transition_id INTEGER;
             ALTER TABLE events ADD COLUMN reconciliation_id INTEGER;
             CREATE INDEX idx_events_instance_transition_id
                 ON events (transition_id);",
        )
        .unwrap();

        acquire_lock(&conn, "wrong-v12-test", Duration::from_secs(1)).unwrap();
        let error = apply_pending(&mut conn, "wrong-v12-test", None).unwrap_err();
        assert_eq!(error.envelope.reason, "store.schema.incomplete");
        assert_eq!(current_version(&conn).unwrap(), 11);
        release_lock(&conn, "wrong-v12-test").unwrap();
    }

    #[test]
    fn recorded_v12_without_exact_identity_index_fails_closed() {
        let temp = TempDb::new("incomplete-v12-index");
        let store = Store::open(&temp.path).expect("initial migration");
        let connection = store.lock_conn().expect("store lock");
        connection
            .execute("DROP INDEX idx_events_instance_transition_id", [])
            .unwrap();

        let error = current_version(&connection).unwrap_err();
        assert_eq!(error.envelope.reason, "store.schema.incomplete");
    }

    #[test]
    fn recorded_v12_with_semantically_wrong_identity_index_fails_closed() {
        let temp = TempDb::new("wrong-v12-index-semantics");
        let store = Store::open(&temp.path).expect("initial migration");
        let connection = store.lock_conn().expect("store lock");

        connection
            .execute("DROP INDEX idx_events_transition_id", [])
            .unwrap();
        connection
            .execute(
                "CREATE UNIQUE INDEX idx_events_transition_id
                 ON events (transition_id COLLATE NOCASE, id DESC)",
                [],
            )
            .unwrap();

        let error = current_version(&connection).unwrap_err();
        assert_eq!(error.envelope.reason, "store.schema.incomplete");
    }

    #[test]
    fn recorded_v12_with_partial_identity_index_fails_closed() {
        let temp = TempDb::new("partial-v12-index-semantics");
        let store = Store::open(&temp.path).expect("initial migration");
        let connection = store.lock_conn().expect("store lock");

        connection
            .execute("DROP INDEX idx_events_transition_id", [])
            .unwrap();
        connection
            .execute(
                "CREATE INDEX idx_events_transition_id
                 ON events (transition_id, id)
                 WHERE transition_id IS NOT NULL",
                [],
            )
            .unwrap();

        let error = current_version(&connection).unwrap_err();
        assert_eq!(error.envelope.reason, "store.schema.incomplete");
    }

    #[test]
    fn upgrades_v7_to_v8_backfilling_instance_event_watermarks() {
        let temp = TempDb::new("v7-v8-events");
        let mut conn = Connection::open(&temp.path).expect("open legacy database");
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        ensure_meta_tables(&conn).unwrap();

        for migration in MIGRATIONS.iter().take(7) {
            conn.execute_batch(migration.sql).unwrap();
            conn.execute(
                "UPDATE schema_version SET version = ?1, updated_at = ?2 WHERE singleton = 0",
                rusqlite::params![migration.version, "1970-01-01T00:00:00Z"],
            )
            .unwrap();
        }
        conn.execute_batch(
            "INSERT INTO events
                 (instance_slug, event_kind, subject, correlation_id, occurred_at, detail)
             VALUES
                 ('instance-a', 'started', 'job-a', NULL, '1970-01-01T00:00:00Z', NULL),
                 ('instance-b', 'started', 'job-b', NULL, '1970-01-01T00:00:00Z', NULL),
                 ('instance-a', 'completed', 'job-a', NULL, '1970-01-01T00:00:00Z', NULL);",
        )
        .unwrap();

        acquire_lock(&conn, "v7-v8-test", Duration::from_secs(1)).unwrap();
        assert_eq!(
            apply_pending(&mut conn, "v7-v8-test", None).unwrap(),
            LATEST_SCHEMA_VERSION
        );
        release_lock(&conn, "v7-v8-test").unwrap();

        let watermarks: Vec<(String, i64, i64)> = {
            let mut statement = conn
                .prepare(
                    "SELECT instance_slug, first_retained_id, high_water_id
                     FROM event_stream_state ORDER BY instance_slug",
                )
                .unwrap();
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            watermarks,
            vec![
                ("instance-a".to_owned(), 1, 3),
                ("instance-b".to_owned(), 2, 2),
            ]
        );
        for index_name in [
            "idx_events_instance_id",
            "idx_events_instance_kind_id",
            "idx_events_instance_subject_id",
            "idx_jobs_retention_phase_id",
            "idx_events_retention_time_id",
            "idx_transitions_retention_time_id",
            "idx_transitions_ancestry",
            "idx_events_ancestry",
            "idx_reconciliations_retention",
        ] {
            let present: bool = conn
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1
                     )",
                    [index_name],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(present, "missing migration index {index_name}");
        }
        let counters: (i64, i64, i64) = conn
            .query_row(
                "SELECT job_rows, event_rows, transition_rows
                 FROM retention_state WHERE singleton = 0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(counters, (0, 3, 0));
        drop(conn);

        let reopened = Store::open(&temp.path).expect("reopen migrated database");
        assert_eq!(reopened.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        let rows = reopened.events_after("instance-a", 0, 10).unwrap();
        assert_eq!(
            rows.iter().map(|event| event.id).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(
            reopened.event_bounds("instance-a").unwrap(),
            (Some(1), Some(3))
        );
    }
}

//! Sequential, idempotent, transactional schema migrations.

use std::time::Duration;

use rusqlite::Connection;
use velnor_model::{ExitClass, Timestamp};

use super::error::{StoreError, StoreResult};
use super::rfc3339;

/// Current schema version every fresh or reopened database converges to.
pub const LATEST_SCHEMA_VERSION: u32 = 4;

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
        if migration.version != 2 || !has_run_attempt_duplicates(&transaction)? {
            transaction.execute_batch(migration.sql)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn far_future_heartbeat_is_not_live_forever() {
        assert!(!lease_live(
            &Some("future-owner".to_owned()),
            &Some("2099-01-01T00:00:00Z".to_owned())
        ));
    }
}

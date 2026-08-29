//! Durable operational state store.
//!
//! One host-shared SQLite database (`/var/lib/velnor/state.db`) holds the
//! sanitized lifecycle model: instances, slots, runner registrations, job
//! summaries, idempotent transitions, append-only events, and
//! reconciliations. Every key is instance-namespaced so multiple daemon
//! processes share the file without colliding. Schema changes run through a
//! single serialized migration lock; WAL mode plus a bounded busy timeout
//! let ordinary writers proceed concurrently. No credential, mask, raw job
//! message, or job log ever enters this database.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::Connection;
use velnor_model::{ExitClass, Timestamp};

pub mod error;
pub mod migrations;
pub mod records;
pub mod retention;

pub use error::{StoreError, StoreResult};
pub use migrations::LATEST_SCHEMA_VERSION;
pub use records::{
    EventRow, EventWindow, InstanceRow, JobRow, JobSummary, LifecycleInstanceRow,
    LifecycleOperationRequest, LifecycleOperationRow, ReconciliationRow, RunnerRegistrationRow,
    SlotRow, StoredEvent, Transition,
};
pub use retention::{
    PhysicalBudgetStatus, PrunePhase, PruneReport, RetentionBudget, RetentionLease,
    RetentionMaintenanceBudget, RetentionMaintenanceReport, StoreAccounting, WalCheckpointStatus,
    DEFAULT_RETENTION_RESERVE_BYTES,
};

/// Default operational database location; created only by deployment, never
/// implicitly by the daemon when its parent directory is absent.
pub const DEFAULT_STATE_DB_PATH: &str = "/var/lib/velnor/state.db";

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// Cold-start contention windows are structurally retried, never ignored:
/// several daemons opening one fresh database simultaneously race the WAL
/// journal-mode switch and meta-table seed before any lock coordination
/// exists, and SQLite's busy handler does not cover every such window. A
/// bounded backoff around connection setup closes that flake class while
/// keeping open semantics identical (same success value, same failure
/// envelope once retries exhaust).
const SETUP_RETRIES: u32 = 5;
const SETUP_BACKOFF_STEP: Duration = Duration::from_millis(40);

fn is_transient_contention(error: &StoreError) -> bool {
    error.envelope.reason == "store.locked"
}

/// Tunables for [`Store::open_with`].
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// Total wait for the migration lock before failing with
    /// `store.migration.lock.busy`.
    pub migration_lock_wait: Duration,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            migration_lock_wait: Duration::from_secs(30),
        }
    }
}

/// Handle over the shared operational database.
///
/// The primary connection is mutex-serialized per handle; concurrent daemon
/// *processes* rely on WAL mode and the busy timeout instead. Retention
/// maintenance opens a separate, nonblocking connection so checkpointing and
/// accounting never wait behind this handle's ordinary store work.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl Store {
    /// Open (and migrate) the default operational database.
    ///
    /// # Errors
    /// Any open or migration failure as a [`MachineErrorEnvelope`]-backed
    /// [`StoreError`]; a missing parent directory names the exact path.
    pub fn open_default() -> StoreResult<Self> {
        Self::open(DEFAULT_STATE_DB_PATH)
    }

    /// Open (and migrate) the database at an injectable path.
    ///
    /// # Errors
    /// See [`Store::open_default`].
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        Self::open_with(path, OpenOptions::default())
    }

    /// Open with explicit options.
    ///
    /// The parent directory must already exist; the store never creates it,
    /// so an unprovisioned host fails closed naming `/var/lib/velnor`.
    ///
    /// # Errors
    /// Parent-missing, connection, and migration failures carry envelope
    /// classes (`UNAVAILABLE`, `OPERATION`, `TIMEOUT`).
    pub fn open_with(path: impl AsRef<Path>, options: OpenOptions) -> StoreResult<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.is_dir() {
                return Err(
                    StoreError::new(ExitClass::Unavailable, "store.parent.missing")
                        .with_remediation(format!(
                            "create directory {} before starting the daemon that owns {}",
                            parent.display(),
                            path.display()
                        )),
                );
            }
        }
        let mut conn = {
            let mut attempt = 0;
            loop {
                match open_setup(path) {
                    Ok(conn) => break conn,
                    Err(error) if is_transient_contention(&error) && attempt < SETUP_RETRIES => {
                        attempt += 1;
                        std::thread::sleep(SETUP_BACKOFF_STEP * attempt);
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        if migrations::current_version(&conn)? < LATEST_SCHEMA_VERSION {
            let owner = lock_owner_token();
            migrations::acquire_lock(&conn, &owner, options.migration_lock_wait)?;
            let result = migrations::apply_pending(&mut conn, &owner, None);
            migrations::release_lock(&conn, &owner)?;
            result?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        })
    }

    /// The database file this store owns.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn lock_conn(&self) -> StoreResult<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| StoreError::new(ExitClass::Operation, "store.lock.poisoned"))
    }

    /// Open a short-lived connection for post-commit maintenance and
    /// accounting. It deliberately never waits on a competing database
    /// writer; callers must surface a busy result instead of delaying job
    /// execution behind maintenance.
    pub(crate) fn open_maintenance_connection(&self) -> StoreResult<Connection> {
        let connection = Connection::open(&self.path).map_err(|error| {
            StoreError::from(error).with_remediation(format!(
                "open the nonblocking maintenance connection for {}",
                self.path.display()
            ))
        })?;
        connection.busy_timeout(Duration::ZERO)?;
        Ok(connection)
    }

    /// Current schema version of the opened database.
    ///
    /// # Errors
    /// Propagates read failures through the envelope.
    pub fn schema_version(&self) -> StoreResult<u32> {
        let conn = self.lock_conn()?;
        migrations::current_version(&conn)
    }
}

pub(crate) fn rfc3339(timestamp: Timestamp) -> String {
    timestamp
        .to_rfc3339()
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn configure_connection(conn: &Connection) -> StoreResult<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // Incremental auto-vacuum lets bounded retention actually release pages
    // after pruning (`PRAGMA incremental_vacuum` post-prune). The pragma only
    // takes effect for a database created or VACUUMed afterwards; existing
    // databases keep their mode until such a VACUUM, which is safe.
    // Values: 0 = none, 1 = full, 2 = incremental.
    let auto_vacuum: i64 = conn.query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
    if auto_vacuum != 2 {
        conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
    }
    let wal: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !wal.eq_ignore_ascii_case("wal") {
        return Err(
            StoreError::new(ExitClass::Operation, "store.wal.unavailable")
                .with_remediation("the filesystem must support WAL journaling"),
        );
    }
    conn.execute_batch(
        "PRAGMA foreign_keys=ON;
         PRAGMA synchronous=NORMAL;
         PRAGMA wal_autocheckpoint=1000;
         PRAGMA journal_size_limit=67108864;",
    )?;
    Ok(())
}

/// One attempt of the contention-retried setup section: open, WAL
/// configuration, and meta-table seeding. Any `store.locked` outcome here
/// is retried by [`Store::open_with`]; everything else fails immediately.
fn open_setup(path: &Path) -> StoreResult<Connection> {
    let conn = Connection::open(path).map_err(|error| {
        StoreError::from(error).with_remediation(format!(
            "verify filesystem permissions for {}",
            path.display()
        ))
    })?;
    configure_connection(&conn)?;
    migrations::ensure_meta_tables(&conn)?;
    Ok(conn)
}

fn lock_owner_token() -> String {
    let nanos = Timestamp::now()
        .as_offset_datetime()
        .unix_timestamp_nanos()
        .unsigned_abs();
    format!("pid:{}+{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rusqlite::{params, Connection};
    use velnor_model::{EventReason, Slug, Timestamp};

    use super::migrations;
    use super::records::test_connection;
    use super::*;
    use crate::store::records::{EventRow, InstanceRow, JobRow, SlotRow, Transition};

    struct TempDb {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempDb {
        fn new(label: &str) -> Self {
            let nanos = Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
                .unsigned_abs();
            let dir = std::env::temp_dir().join(format!(
                "velnor-store-{label}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("temp dir created");
            let path = dir.join("state.db");
            Self { dir, path }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn instance(slug: &str) -> InstanceRow {
        InstanceRow {
            instance_slug: slug.to_owned(),
            host: "sentry".to_owned(),
            daemon_version: "0.1.0".to_owned(),
            slots_configured: 4,
            slots_busy: 1,
            updated_at: Timestamp::now(),
        }
    }

    fn job(slug: &str, uid: &str, repository: &str) -> JobRow {
        JobRow {
            instance_slug: slug.to_owned(),
            job_uid: uid.to_owned(),
            repository: repository.to_owned(),
            workflow: ".github/workflows/ci.yml".to_owned(),
            job_name: "build".to_owned(),
            run_id: Some(42),
            attempt: Some(1),
            head_ref: Some("main".to_owned()),
            head_sha: Some("abc123".to_owned()),
            trigger_event: Some("push".to_owned()),
            queued_at: Some(Timestamp::UNIX_EPOCH),
            acquired_at: None,
            slot_name: Some("slot-0".to_owned()),
            runner_name: None,
            trust_scope: Some("trusted".to_owned()),
            resource_policy: Some("standard".to_owned()),
            phase: "queued".to_owned(),
            conclusion: None,
            infrastructure_category: None,
            updated_at: Timestamp::now(),
        }
    }

    fn acquire(token_suffix: &str) -> Transition {
        Transition {
            token: format!("acquire-{token_suffix}"),
            correlation_id: Slug::validate("correlation_id", "corr-1").expect("valid slug"),
            reason: EventReason::JobAcquired,
            message: Some("slot assigned".to_owned()),
            transition_time: Timestamp::now(),
            conclusion: None,
            infrastructure_category: None,
        }
    }

    fn transition(
        token: &str,
        correlation: &str,
        reason: EventReason,
        conclusion: Option<&str>,
    ) -> Transition {
        Transition {
            token: token.to_owned(),
            correlation_id: Slug::validate("correlation_id", correlation).expect("valid slug"),
            reason,
            message: Some(format!("{} observed", reason.as_str())),
            transition_time: Timestamp::now(),
            conclusion: conclusion.map(str::to_owned),
            infrastructure_category: None,
        }
    }

    /// Walk one job through the full legal path; returns applied flags.
    fn walk_happy_path(store: &Store, instance_slug: &str, job_uid: &str) -> Vec<bool> {
        let steps = [
            ("t-acquire", EventReason::JobAcquired, None),
            ("t-wait", EventReason::JobWaiting, None),
            ("t-start", EventReason::JobStarted, None),
            ("t-complete", EventReason::JobCompleted, Some("success")),
        ];
        steps
            .iter()
            .map(|(token, reason, conclusion)| {
                store
                    .record_job_transition(
                        instance_slug,
                        job_uid,
                        &transition(token, "corr-happy", *reason, *conclusion),
                    )
                    .expect("legal step applies")
            })
            .collect()
    }

    #[test]
    fn migrates_empty_database_to_latest_version() {
        let temp = TempDb::new("empty-migrate");
        let store = Store::open(&temp.path).expect("open migrates");
        assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        let conn = test_connection(&store);
        let tables: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN
                 ('instances','slots','runner_registrations','jobs','job_transitions',
                  'events','reconciliations')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 7);
    }

    #[test]
    fn reopen_keeps_version_and_data() {
        let temp = TempDb::new("reopen");
        {
            let store = Store::open(&temp.path).expect("first open");
            store.upsert_instance(&instance("inst-a")).unwrap();
            store.record_job(&job("inst-a", "j1", "org/repo")).unwrap();
        }
        let reopened = Store::open(&temp.path).expect("second open");
        assert_eq!(reopened.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        let summaries = reopened.job_summaries("inst-a").unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].repository, "org/repo");
        assert_eq!(summaries[0].phase, "queued");
    }

    #[test]
    fn raw_job_row_rejects_secret_markers_before_persistence() {
        let temp = TempDb::new("raw-job-safety");
        let store = Store::open(&temp.path).expect("open store");
        let mut row = job("raw", "job-1", "org/repo");
        row.job_name = "secret-token-value".to_owned();
        let error = store
            .record_job(&row)
            .expect_err("raw row must fail closed");
        assert_eq!(error.envelope.reason, "store.job.summary.invalid");
        assert!(store.job_summaries("raw").unwrap().is_empty());
    }

    #[test]
    fn repeated_migration_is_idempotent() {
        let temp = TempDb::new("idempotent");
        {
            let store = Store::open(&temp.path).expect("seed database");
            store.upsert_instance(&instance("preserved")).unwrap();
            store
                .record_job(&job("preserved", "j1", "org/preserved"))
                .unwrap();
        }
        for _ in 0..3 {
            let store = Store::open(&temp.path).expect("open idempotently");
            assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        }
        let conn = Connection::open(&temp.path).unwrap();
        migrations::ensure_meta_tables(&conn).unwrap();
        conn.execute("UPDATE schema_version SET version = 0", [])
            .unwrap();
        migrations::acquire_lock(&conn, "idempotence-test", Duration::from_secs(1)).unwrap();
        let mut conn = conn;
        assert_eq!(
            migrations::apply_pending(&mut conn, "idempotence-test", None).unwrap(),
            LATEST_SCHEMA_VERSION
        );
        migrations::release_lock(&conn, "idempotence-test").unwrap();
        let version_rows: u32 = conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version_rows, 1);
        let lock_rows: u32 = conn
            .query_row("SELECT COUNT(*) FROM migration_lock", [], |r| r.get(0))
            .unwrap();
        assert_eq!(lock_rows, 1);
        let reopened = Store::open(&temp.path).expect("reopen after reapplication");
        let summaries = reopened.job_summaries("preserved").unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].repository, "org/preserved");
        let retention_state: u32 = test_connection(&reopened)
            .query_row("SELECT COUNT(*) FROM retention_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(retention_state, 1);
    }

    #[test]
    fn retention_lease_migration_is_present_after_reopen_and_replay() {
        let temp = TempDb::new("retention-lease-migration");
        {
            let store = Store::open(&temp.path).expect("fresh migration");
            let lease_rows: u32 = test_connection(&store)
                .query_row(
                    "SELECT COUNT(*) FROM retention_lease WHERE singleton = 0",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(lease_rows, 1);
        }

        let reopened = Store::open(&temp.path).expect("reopen migration");
        assert_eq!(reopened.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        let lease_rows: u32 = test_connection(&reopened)
            .query_row("SELECT COUNT(*) FROM retention_lease", [], |row| row.get(0))
            .unwrap();
        assert_eq!(lease_rows, 1);
        let generation: i64 = test_connection(&reopened)
            .query_row(
                "SELECT generation FROM retention_lease WHERE singleton = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generation, 0);

        let mut conn = Connection::open(&temp.path).unwrap();
        conn.busy_timeout(BUSY_TIMEOUT).unwrap();
        conn.execute("UPDATE schema_version SET version = 9", [])
            .unwrap();
        migrations::acquire_lock(&conn, "retention-lease-replay", Duration::from_secs(1)).unwrap();
        assert_eq!(
            migrations::apply_pending(&mut conn, "retention-lease-replay", None).unwrap(),
            LATEST_SCHEMA_VERSION
        );
        migrations::release_lock(&conn, "retention-lease-replay").unwrap();
        let lease_rows: u32 = conn
            .query_row("SELECT COUNT(*) FROM retention_lease", [], |row| row.get(0))
            .unwrap();
        assert_eq!(lease_rows, 1);
    }

    #[test]
    fn retention_lease_generation_migration_is_idempotent_on_replay() {
        let temp = TempDb::new("retention-lease-generation-replay");
        let store = Store::open(&temp.path).expect("initial migration");
        drop(store);

        let mut conn = Connection::open(&temp.path).unwrap();
        conn.busy_timeout(BUSY_TIMEOUT).unwrap();
        conn.execute("UPDATE schema_version SET version = 0", [])
            .unwrap();
        migrations::acquire_lock(&conn, "generation-replay", Duration::from_secs(1)).unwrap();
        assert_eq!(
            migrations::apply_pending(&mut conn, "generation-replay", None).unwrap(),
            LATEST_SCHEMA_VERSION
        );
        let columns: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('retention_lease')
                 WHERE name = 'generation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 1);
        migrations::release_lock(&conn, "generation-replay").unwrap();
    }

    #[test]
    fn retention_lease_migration_rolls_back_ddl_and_version() {
        let temp = TempDb::new("retention-lease-rollback");
        let store = Store::open(&temp.path).expect("initial migration");
        drop(store);

        let mut conn = Connection::open(&temp.path).unwrap();
        conn.busy_timeout(BUSY_TIMEOUT).unwrap();
        conn.execute("DROP TABLE retention_lease", []).unwrap();
        conn.execute("UPDATE schema_version SET version = 9", [])
            .unwrap();
        migrations::acquire_lock(&conn, "retention-lease-rollback", Duration::from_secs(1))
            .unwrap();

        let failure = migrations::apply_pending(
            &mut conn,
            "retention-lease-rollback",
            Some(&|version| {
                if version == 10 {
                    Err(StoreError::new(
                        velnor_model::ExitClass::Operation,
                        "store.test.retention-lease-rollback",
                    ))
                } else {
                    Ok(())
                }
            }),
        )
        .unwrap_err();
        assert_eq!(
            failure.envelope.reason,
            "store.test.retention-lease-rollback"
        );
        assert_eq!(migrations::current_version(&conn).unwrap(), 9);
        let lease_table_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'retention_lease'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lease_table_count, 0);

        assert_eq!(
            migrations::apply_pending(&mut conn, "retention-lease-rollback", None).unwrap(),
            LATEST_SCHEMA_VERSION
        );
        migrations::release_lock(&conn, "retention-lease-rollback").unwrap();
    }

    #[test]
    fn upgrades_v1_schema_preserving_rows_and_indexes() {
        let temp = TempDb::new("v1-upgrade");
        let mut conn = Connection::open(&temp.path).unwrap();
        conn.busy_timeout(BUSY_TIMEOUT).unwrap();
        migrations::ensure_meta_tables(&conn).unwrap();
        conn.execute_batch(migrations::MIGRATIONS[0].sql).unwrap();
        conn.execute("UPDATE schema_version SET version = 1", [])
            .unwrap();
        conn.execute(
            "INSERT INTO jobs
             (instance_slug, job_uid, repository, workflow, job_name, run_id, attempt,
              phase, updated_at)
             VALUES ('legacy', 'legacy-job', 'org/legacy', 'ci.yml', 'build', 42, 1,
                     'queued', '1970-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs
             (instance_slug, job_uid, repository, workflow, job_name, run_id, attempt,
              phase, updated_at)
             VALUES ('legacy', 'legacy-job-2', 'org/legacy', 'ci.yml', 'test', 42, 1,
                     'queued', '1970-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        migrations::acquire_lock(&conn, "v1-upgrade", Duration::from_secs(1)).unwrap();
        assert_eq!(
            migrations::apply_pending(&mut conn, "v1-upgrade", None).unwrap(),
            LATEST_SCHEMA_VERSION
        );
        migrations::release_lock(&conn, "v1-upgrade").unwrap();

        let index_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                WHERE type = 'index' AND name = 'idx_jobs_instance_run_attempt'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 1);
        let job_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs
                 WHERE instance_slug = 'legacy' AND run_id = 42 AND attempt = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(job_count, 2);
        let retention_count: u32 = conn
            .query_row("SELECT COUNT(*) FROM retention_state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(retention_count, 1);
    }

    #[test]
    fn upgrades_legacy_v2_unique_index_without_dropping_rows() {
        let temp = TempDb::new("v2-index-repair");
        let mut conn = Connection::open(&temp.path).unwrap();
        conn.busy_timeout(BUSY_TIMEOUT).unwrap();
        migrations::ensure_meta_tables(&conn).unwrap();
        conn.execute_batch(migrations::MIGRATIONS[0].sql).unwrap();
        conn.execute_batch(
            "CREATE UNIQUE INDEX uq_jobs_instance_run_attempt
             ON jobs (instance_slug, run_id, attempt)
             WHERE run_id IS NOT NULL AND attempt IS NOT NULL;",
        )
        .unwrap();
        conn.execute_batch(migrations::MIGRATIONS[2].sql).unwrap();
        conn.execute("UPDATE schema_version SET version = 2", [])
            .unwrap();
        conn.execute(
            "INSERT INTO jobs
             (instance_slug, job_uid, repository, workflow, job_name, run_id, attempt,
              phase, updated_at)
             VALUES ('legacy', 'legacy-job', 'org/legacy', 'ci.yml', 'build', 7, 1,
                     'queued', '1970-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        migrations::acquire_lock(&conn, "v2-repair", Duration::from_secs(1)).unwrap();
        assert_eq!(
            migrations::apply_pending(&mut conn, "v2-repair", None).unwrap(),
            LATEST_SCHEMA_VERSION
        );
        migrations::release_lock(&conn, "v2-repair").unwrap();
        let old_index: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'uq_jobs_instance_run_attempt'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_index, 0);
        let rows: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE job_uid = 'legacy-job'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn injected_failure_inside_migration_rolls_back_cleanly() {
        let temp = TempDb::new("rollback");
        let mut conn = Connection::open(&temp.path).unwrap();
        conn.busy_timeout(BUSY_TIMEOUT).unwrap();
        migrations::ensure_meta_tables(&conn).unwrap();
        assert_eq!(migrations::current_version(&conn).unwrap(), 0);
        migrations::acquire_lock(&conn, "tester", Duration::from_secs(1)).unwrap();

        let failure = migrations::apply_pending(
            &mut conn,
            "tester",
            Some(&|version| {
                if version == 1 {
                    Err(StoreError::new(
                        velnor_model::ExitClass::Operation,
                        "store.test.injected",
                    ))
                } else {
                    Ok(())
                }
            }),
        );
        assert_eq!(failure.unwrap_err().envelope.reason, "store.test.injected");
        assert_eq!(migrations::current_version(&conn).unwrap(), 0);
        let jobs_table: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='jobs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(jobs_table, 0, "rolled-back DDL must not persist");

        let applied = migrations::apply_pending(&mut conn, "tester", None).expect("retry succeeds");
        assert_eq!(applied, LATEST_SCHEMA_VERSION);
        assert_eq!(migrations::release_lock(&conn, "tester"), Ok(()));
    }

    #[test]
    fn injected_v2_and_v3_failures_roll_back_independently() {
        let temp = TempDb::new("rollback-later");
        let mut conn = Connection::open(&temp.path).unwrap();
        conn.busy_timeout(BUSY_TIMEOUT).unwrap();
        migrations::ensure_meta_tables(&conn).unwrap();
        conn.execute_batch(migrations::MIGRATIONS[0].sql).unwrap();
        conn.execute("UPDATE schema_version SET version = 1", [])
            .unwrap();
        migrations::acquire_lock(&conn, "later-rollback", Duration::from_secs(1)).unwrap();

        let v2_failure = migrations::apply_pending(
            &mut conn,
            "later-rollback",
            Some(&|version| {
                if version == 2 {
                    Err(StoreError::new(
                        velnor_model::ExitClass::Operation,
                        "store.test.v2-injected",
                    ))
                } else {
                    Ok(())
                }
            }),
        )
        .unwrap_err();
        assert_eq!(v2_failure.envelope.reason, "store.test.v2-injected");
        assert_eq!(migrations::current_version(&conn).unwrap(), 1);
        let v2_index_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_jobs_instance_run_attempt'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v2_index_count, 0);

        let v3_failure = migrations::apply_pending(
            &mut conn,
            "later-rollback",
            Some(&|version| {
                if version == 3 {
                    Err(StoreError::new(
                        velnor_model::ExitClass::Operation,
                        "store.test.v3-injected",
                    ))
                } else {
                    Ok(())
                }
            }),
        )
        .unwrap_err();
        assert_eq!(v3_failure.envelope.reason, "store.test.v3-injected");
        assert_eq!(migrations::current_version(&conn).unwrap(), 2);
        let retention_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'retention_state'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retention_count, 0);

        assert_eq!(
            migrations::apply_pending(&mut conn, "later-rollback", None).unwrap(),
            LATEST_SCHEMA_VERSION
        );
        migrations::release_lock(&conn, "later-rollback").unwrap();
    }

    #[test]
    fn injected_v4_failure_restores_historical_unique_index() {
        let temp = TempDb::new("rollback-v4");
        let mut conn = Connection::open(&temp.path).unwrap();
        conn.busy_timeout(BUSY_TIMEOUT).unwrap();
        migrations::ensure_meta_tables(&conn).unwrap();
        conn.execute_batch(migrations::MIGRATIONS[0].sql).unwrap();
        conn.execute_batch(migrations::MIGRATIONS[2].sql).unwrap();
        conn.execute_batch(
            "CREATE UNIQUE INDEX uq_jobs_instance_run_attempt
             ON jobs (instance_slug, run_id, attempt)
             WHERE run_id IS NOT NULL AND attempt IS NOT NULL;",
        )
        .unwrap();
        conn.execute("UPDATE schema_version SET version = 3", [])
            .unwrap();
        migrations::acquire_lock(&conn, "rollback-v4", Duration::from_secs(1)).unwrap();
        let error = migrations::apply_pending(
            &mut conn,
            "rollback-v4",
            Some(&|version| {
                if version == 4 {
                    Err(StoreError::new(
                        velnor_model::ExitClass::Operation,
                        "store.test.v4-injected",
                    ))
                } else {
                    Ok(())
                }
            }),
        )
        .unwrap_err();
        assert_eq!(error.envelope.reason, "store.test.v4-injected");
        assert_eq!(migrations::current_version(&conn).unwrap(), 3);
        let old_index: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'uq_jobs_instance_run_attempt'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_index, 1);
        assert_eq!(
            migrations::apply_pending(&mut conn, "rollback-v4", None).unwrap(),
            LATEST_SCHEMA_VERSION
        );
        migrations::release_lock(&conn, "rollback-v4").unwrap();
    }

    #[test]
    fn five_concurrent_daemons_migrate_read_write_without_corruption() {
        // Stress hardening for the cold-parallel flake class: the exact
        // scenario runs five times, each on a fresh database file, so a
        // one-in-N startup race cannot hide behind a lucky single pass.
        // Original assertions are preserved per iteration.
        for iteration in 0..5 {
            let temp = TempDb::new(&format!("concurrent-{iteration}"));
            let shared = Arc::new(temp.path.clone());
            let start = Arc::new(Barrier::new(10));
            let active = Arc::new(Barrier::new(10));
            let handles: Vec<_> = (0..10)
                .map(|index| {
                    let path = Arc::clone(&shared);
                    let start = Arc::clone(&start);
                    let active = Arc::clone(&active);
                    thread::spawn(move || {
                        start.wait();
                        let store = Store::open(path.as_path()).expect("concurrent open");
                        active.wait();
                        if index < 5 {
                            let slug = format!("daemon-{index}");
                            for write_iteration in 0..10 {
                                store.upsert_instance(&instance(&slug)).unwrap();
                                let row = job(&slug, &format!("job-{index}"), "org/concurrent");
                                store.record_job(&row).unwrap();
                                assert!(store
                                    .record_job_transition(
                                        &slug,
                                        &row.job_uid,
                                        &acquire(&format!("c-{write_iteration}")),
                                    )
                                    .unwrap());
                                store
                                    .append_event(&EventRow {
                                        instance_slug: slug.clone(),
                                        event_kind: "slot.ready".to_owned(),
                                        subject: slug.clone(),
                                        correlation_id: None,
                                        occurred_at: Timestamp::now(),
                                        detail: None,
                                    })
                                    .unwrap();
                            }
                        } else {
                            for _ in 0..50 {
                                assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
                                let summaries = store.job_summaries("daemon-0").unwrap();
                                assert!(summaries.len() <= 1);
                            }
                        }
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("writer thread succeeded");
            }
            let store = Store::open(&temp.path).expect("final reopen");
            assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
            for index in 0..5 {
                let slug = format!("daemon-{index}");
                assert_eq!(store.job_summaries(&slug).unwrap().len(), 1);
                assert_eq!(
                    store
                        .transition_count(&slug, &format!("job-{index}"))
                        .unwrap(),
                    10
                );
                assert_eq!(store.event_count(&slug, &slug).unwrap(), 10);
            }
            let conn = test_connection(&store);
            let integrity: String = conn
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                .unwrap();
            assert_eq!(integrity, "ok");
        }
    }

    #[test]
    fn migration_lock_serializes_second_opener() {
        let temp = TempDb::new("lock");
        let first = Store::open(&temp.path).expect("first open");
        first.upsert_instance(&instance("keeper")).unwrap();
        let keeper_job = job("keeper", "j1", "org/lock");
        first.record_job(&keeper_job).unwrap();

        let now = rfc3339(Timestamp::now());
        {
            let conn = test_connection(&first);
            conn.execute(
                "UPDATE migration_lock SET owner = 'ghost-daemon', acquired_at = ?1, heartbeat_at = ?1",
                [&now],
            )
            .unwrap();
            // Force a pending migration so the next opener must take the lock.
            conn.execute("UPDATE schema_version SET version = 0", [])
                .unwrap();
        }

        let blocked = Store::open_with(
            &temp.path,
            OpenOptions {
                migration_lock_wait: Duration::from_millis(120),
            },
        );
        let error = blocked.expect_err("live foreign lease blocks the opener");
        assert_eq!(
            error.envelope.class,
            velnor_model::ExitClass::Timeout.as_str()
        );
        assert_eq!(error.envelope.reason, "store.migration.lock.busy");
        let remediation = error.envelope.remediation.expect("names holder");
        assert!(remediation.contains("ghost-daemon"), "{remediation}");

        {
            let conn = test_connection(&first);
            conn.execute(
                "UPDATE migration_lock SET owner = NULL, acquired_at = NULL, heartbeat_at = NULL",
                [],
            )
            .unwrap();
        }
        let second = Store::open_with(
            &temp.path,
            OpenOptions {
                migration_lock_wait: Duration::from_secs(10),
            },
        )
        .expect("released lock admits second opener");
        assert_eq!(second.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        let summaries = second.job_summaries("keeper").unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].repository, "org/lock");
        let owner: Option<String> = test_connection(&second)
            .query_row(
                "SELECT owner FROM migration_lock WHERE singleton = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(owner, None, "lock is released after a no-op migration");

        let stale = rfc3339(Timestamp::parse("2000-01-01T00:00:00Z").unwrap());
        {
            let conn = test_connection(&second);
            conn.execute(
                "UPDATE migration_lock SET owner = 'crashed-daemon', acquired_at = ?1, heartbeat_at = ?1",
                [&stale],
            )
            .unwrap();
            conn.execute("UPDATE schema_version SET version = 0", [])
                .unwrap();
        }
        let recovered = Store::open_with(
            &temp.path,
            OpenOptions {
                migration_lock_wait: Duration::from_secs(1),
            },
        )
        .expect("stale migration lease is recoverable after restart");
        assert_eq!(recovered.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        assert_eq!(recovered.job_summaries("keeper").unwrap().len(), 1);
    }

    #[test]
    fn migration_aborts_when_lock_ownership_is_lost() {
        let temp = TempDb::new("lost-lock");
        let mut conn = Connection::open(&temp.path).unwrap();
        conn.busy_timeout(BUSY_TIMEOUT).unwrap();
        migrations::ensure_meta_tables(&conn).unwrap();
        migrations::acquire_lock(&conn, "original-owner", Duration::from_secs(1)).unwrap();
        conn.execute(
            "UPDATE migration_lock SET owner = 'replacement-owner' WHERE singleton = 0",
            [],
        )
        .unwrap();
        let error = migrations::apply_pending(&mut conn, "original-owner", None)
            .expect_err("migration must fail after ownership changes");
        assert_eq!(error.envelope.reason, "store.migration.lock.lost");
    }

    #[test]
    fn instance_namespacing_isolates_keys() {
        let temp = TempDb::new("namespace");
        let store = Store::open(&temp.path).unwrap();
        store.upsert_instance(&instance("alpha")).unwrap();
        store.upsert_instance(&instance("beta")).unwrap();
        store
            .upsert_slot(&SlotRow {
                instance_slug: "alpha".to_owned(),
                name: "slot-0".to_owned(),
                host: "sentry".to_owned(),
                slot_index: 0,
                slot_kind: "stable".to_owned(),
                phase: "ready".to_owned(),
                job_name: None,
                updated_at: Timestamp::now(),
            })
            .unwrap();
        store
            .record_job(&job("alpha", "shared-uid", "org/alpha-repo"))
            .unwrap();
        store
            .record_job(&job("beta", "shared-uid", "org/beta-repo"))
            .unwrap();

        let alpha = store.job_summaries("alpha").unwrap();
        assert_eq!(alpha.len(), 1);
        assert_eq!(alpha[0].repository, "org/alpha-repo");
        let beta = store.job_summaries("beta").unwrap();
        assert_eq!(beta.len(), 1);
        assert_eq!(beta[0].repository, "org/beta-repo");

        assert!(store
            .record_job_transition("beta", "shared-uid", &acquire("n"))
            .unwrap());
        let alpha_row = &store.job_summaries("alpha").unwrap()[0];
        assert_eq!(
            alpha_row.phase, "queued",
            "alpha untouched by beta transition"
        );
        assert_eq!(store.job_summaries("beta").unwrap()[0].phase, "acquired");

        let conn = test_connection(&store);
        let slot_count: u32 = conn
            .query_row("SELECT COUNT(*) FROM slots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(slot_count, 1, "slots are scoped per instance key");
    }

    #[test]
    fn full_happy_path_emits_each_required_reason_exactly_once_across_replay() {
        let temp = TempDb::new("happy-path");
        let store = Store::open(&temp.path).unwrap();
        store.upsert_instance(&instance("hp")).unwrap();
        // Distinct (run_id, attempt) identities: the summary identity index
        // is (instance_slug, run_id, attempt).
        for (job_uid, run_id) in [("job-a", 101), ("job-b", 102), ("job-c", 103)] {
            let mut row = job("hp", job_uid, "org/happy");
            row.attempt = Some(1);
            row.run_id = Some(run_id);
            store.record_job(&row).unwrap();
        }

        let applied = walk_happy_path(&store, "hp", "job-a");
        assert_eq!(applied, vec![true, true, true, true]);

        // Every replay of an already-applied token is a no-op success.
        for _ in 0..2 {
            let replayed = walk_happy_path(&store, "hp", "job-a");
            assert_eq!(
                replayed,
                vec![false, false, false, false],
                "replay must never duplicate a step"
            );
        }

        // Canceled and rejected branches walk the same prefix to `started`.
        let canceled = walk_with_suffix(&store, "hp", "job-b", EventReason::JobCanceled, "cancel");
        assert_eq!(canceled, vec![true, true, true, true]);
        let rejected = walk_with_suffix(&store, "hp", "job-c", EventReason::JobRejected, "reject");
        assert_eq!(rejected, vec![true, true, true, true]);

        // Each required job reason was emitted exactly once per job.
        for (job_uid, mut expected) in [
            (
                "job-a",
                vec![
                    "job.acquired",
                    "job.waiting",
                    "job.started",
                    "job.completed",
                ],
            ),
            (
                "job-b",
                vec!["job.acquired", "job.waiting", "job.started", "job.canceled"],
            ),
            (
                "job-c",
                vec!["job.acquired", "job.waiting", "job.started", "job.rejected"],
            ),
        ] {
            let reasons = stored_reasons(&store, "hp", job_uid);
            expected.sort_unstable();
            let mut actual = reasons.clone();
            actual.sort_unstable();
            assert_eq!(actual, expected, "{job_uid} reasons");
            assert_eq!(reasons.len(), 4);
            assert_eq!(store.transition_count("hp", job_uid).unwrap(), 4);
            assert_eq!(store.event_count("hp", job_uid).unwrap(), 4);
        }

        // Terminal phases match each branch (summaries are id-DESC).
        let summaries = store.job_summaries("hp").unwrap();
        assert_eq!(summaries[0].job_uid, "job-c");
        assert_eq!(summaries[0].phase, "rejected");
        assert_eq!(
            summaries
                .iter()
                .find(|s| s.job_uid == "job-a")
                .expect("job-a summary")
                .phase,
            "completed"
        );
        assert_eq!(
            summaries
                .iter()
                .find(|s| s.job_uid == "job-b")
                .expect("job-b summary")
                .phase,
            "canceled"
        );
    }

    fn walk_with_suffix(
        store: &Store,
        instance_slug: &str,
        job_uid: &str,
        terminal: EventReason,
        suffix: &str,
    ) -> Vec<bool> {
        let conclusion = match terminal {
            EventReason::JobCanceled => Some("cancelled"),
            EventReason::JobRejected => None,
            _ => None,
        };
        [
            (
                format!("t-{suffix}-acquire"),
                EventReason::JobAcquired,
                None,
            ),
            (format!("t-{suffix}-wait"), EventReason::JobWaiting, None),
            (format!("t-{suffix}-start"), EventReason::JobStarted, None),
            (format!("t-{suffix}-terminal"), terminal, conclusion),
        ]
        .iter()
        .map(|(token, reason, concl)| {
            store
                .record_job_transition(
                    instance_slug,
                    job_uid,
                    &transition(token, "corr-walk", *reason, *concl),
                )
                .expect("legal step applies")
        })
        .collect()
    }

    fn stored_reasons(store: &Store, instance_slug: &str, job_uid: &str) -> Vec<String> {
        let conn = test_connection(store);
        let mut statement = conn
            .prepare(
                "SELECT reason FROM job_transitions
                 WHERE instance_slug = ?1 AND job_uid = ?2 ORDER BY id",
            )
            .expect("reason query prepares");
        let rows = statement
            .query_map(params![instance_slug, job_uid], |row| row.get(0))
            .expect("reason query maps");
        rows.map(|row| row.expect("reason row")).collect()
    }

    fn stored_correlations(store: &Store, instance_slug: &str, job_uid: &str) -> Vec<String> {
        let conn = test_connection(store);
        let mut statement = conn
            .prepare(
                "SELECT correlation_id FROM job_transitions
                 WHERE instance_slug = ?1 AND job_uid = ?2 ORDER BY id",
            )
            .expect("correlation query prepares");
        let rows = statement
            .query_map(params![instance_slug, job_uid], |row| row.get(0))
            .expect("correlation query maps");
        rows.map(|row| row.expect("correlation row")).collect()
    }

    #[test]
    fn transitions_carry_validated_correlation_and_utc_timestamp() {
        let temp = TempDb::new("correlation");
        let store = Store::open(&temp.path).unwrap();
        store.upsert_instance(&instance("co")).unwrap();
        store.record_job(&job("co", "j", "org/corr")).unwrap();

        // Missing and blank correlations fail closed at the validated-slug
        // construction seam: no such Transition can exist.
        for blank in ["", "   ", "\t"] {
            let error = Slug::validate("correlation_id", blank).expect_err(blank);
            assert_eq!(error.field, "correlation_id");
        }
        assert!(Slug::validate("correlation_id", "corr-run-42").is_ok());

        // The epoch anchor proves the stored instant is the exact UTC RFC
        // 3339 rendering of the supplied Timestamp.
        let mut step = transition("t-acquire", "corr-run-42", EventReason::JobAcquired, None);
        step.transition_time = Timestamp::UNIX_EPOCH;
        assert!(store.record_job_transition("co", "j", &step).unwrap());

        assert_eq!(stored_correlations(&store, "co", "j"), vec!["corr-run-42"]);
        let conn = test_connection(&store);
        let stored_time: String = conn
            .query_row(
                "SELECT transition_time FROM job_transitions
                 WHERE instance_slug = 'co' AND job_uid = 'j'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_time, "1970-01-01T00:00:00Z");
        assert!(stored_time.ends_with('Z'), "stored instants render UTC");
    }

    #[test]
    fn different_event_after_terminal_is_rejected_naming_from_to() {
        let temp = TempDb::new("after-terminal");
        let store = Store::open(&temp.path).unwrap();
        store.upsert_instance(&instance("at")).unwrap();
        store.record_job(&job("at", "j", "org/term")).unwrap();

        // Cancel at started: legal terminal branch.
        let cancel_steps: Vec<bool> = [
            ("t1-acquire", EventReason::JobAcquired),
            ("t1-wait", EventReason::JobWaiting),
            ("t1-start", EventReason::JobStarted),
            ("t1-cancel", EventReason::JobCanceled),
        ]
        .iter()
        .map(|(token, reason)| {
            store
                .record_job_transition(
                    "at",
                    "j",
                    &transition(token, "corr-at", *reason, Some("cancelled")),
                )
                .expect("cancel path is legal")
        })
        .collect();
        assert_eq!(cancel_steps, vec![true, true, true, true]);
        assert_eq!(store.job_summaries("at").unwrap()[0].phase, "canceled");

        // A DIFFERENT event after the terminal state is rejected outright.
        let error = store
            .record_job_transition(
                "at",
                "j",
                &transition(
                    "t2-complete",
                    "corr-at",
                    EventReason::JobCompleted,
                    Some("success"),
                ),
            )
            .expect_err("completed after canceled must be rejected");
        assert_eq!(error.envelope.class, ExitClass::Conflict.as_str());
        assert_eq!(error.envelope.reason, "store.job.transition.illegal");
        let remediation = error.envelope.remediation.expect("names from/to");
        assert!(remediation.contains("canceled"), "{remediation}");
        assert!(remediation.contains("completed"), "{remediation}");

        // And so is any other late event.
        let error = store
            .record_job_transition(
                "at",
                "j",
                &transition("t3-start", "corr-at", EventReason::JobStarted, None),
            )
            .expect_err("started after canceled must be rejected");
        assert_eq!(error.envelope.reason, "store.job.transition.illegal");

        // Nothing from the rejected attempts persisted.
        assert_eq!(store.transition_count("at", "j").unwrap(), 4);
        assert_eq!(store.event_count("at", "j").unwrap(), 4);

        // Same-token replay of the applied terminal event stays a no-op
        // success even past the terminal state.
        assert!(!store
            .record_job_transition(
                "at",
                "j",
                &transition(
                    "t1-cancel",
                    "corr-at",
                    EventReason::JobCanceled,
                    Some("cancelled")
                ),
            )
            .unwrap());
        assert_eq!(store.transition_count("at", "j").unwrap(), 4);
        assert_eq!(store.event_count("at", "j").unwrap(), 4);
    }

    #[test]
    fn impossible_transition_matrix_spot_checks_fail_conflict_writing_nothing() {
        let temp = TempDb::new("impossible");
        let store = Store::open(&temp.path).unwrap();
        store.upsert_instance(&instance("im")).unwrap();
        for (job_uid, run_id) in [("done", 201), ("axed", 202), ("fresh", 203)] {
            let mut row = job("im", job_uid, "org/matrix");
            row.attempt = Some(1);
            row.run_id = Some(run_id);
            store.record_job(&row).unwrap();
        }

        // completed -> started
        let steps: [(&str, &str, EventReason); 4] = [
            ("d1", "done", EventReason::JobAcquired),
            ("d2", "done", EventReason::JobWaiting),
            ("d3", "done", EventReason::JobStarted),
            ("d4", "done", EventReason::JobCompleted),
        ];
        for (token, job_uid, reason) in steps {
            assert!(store
                .record_job_transition("im", job_uid, &transition(token, "corr-im", reason, None))
                .unwrap());
        }
        let error = store
            .record_job_transition(
                "im",
                "done",
                &transition("x1", "corr-im", EventReason::JobStarted, None),
            )
            .expect_err("completed→started is illegal");
        assert_eq!(error.envelope.class, ExitClass::Conflict.as_str());
        let remediation = error.envelope.remediation.expect("names from/to");
        assert!(
            remediation.contains("completed") && remediation.contains("acquired"),
            "{remediation}"
        );

        // canceled -> completed
        for (token, reason) in [
            ("a1", EventReason::JobAcquired),
            ("a2", EventReason::JobWaiting),
            ("a3", EventReason::JobStarted),
            ("a4", EventReason::JobCanceled),
        ] {
            assert!(store
                .record_job_transition("im", "axed", &transition(token, "corr-im", reason, None))
                .unwrap());
        }
        let error = store
            .record_job_transition(
                "im",
                "axed",
                &transition("x2", "corr-im", EventReason::JobCompleted, Some("success")),
            )
            .expect_err("canceled→completed is illegal");
        assert_eq!(error.envelope.reason, "store.job.transition.illegal");
        let remediation = error.envelope.remediation.expect("names from/to");
        assert!(
            remediation.contains("canceled") && remediation.contains("completed"),
            "{remediation}"
        );

        // queued -> started directly skips two edges.
        let error = store
            .record_job_transition(
                "im",
                "fresh",
                &transition("x3", "corr-im", EventReason::JobStarted, None),
            )
            .expect_err("queued→started is illegal");
        assert_eq!(error.envelope.reason, "store.job.transition.illegal");

        // Non-job reasons never move job state through this seam either.
        let error = store
            .record_job_transition(
                "im",
                "fresh",
                &transition("x4", "corr-im", EventReason::CapacityPressure, None),
            )
            .expect_err("capacity.pressure is not a job transition");
        assert!(error.to_string().contains("not a job transition"));

        // Rejected attempts wrote nothing anywhere.
        for (job_uid, expected) in [("done", 4), ("axed", 4), ("fresh", 0)] {
            assert_eq!(store.transition_count("im", job_uid).unwrap(), expected);
            assert_eq!(store.event_count("im", job_uid).unwrap(), expected);
        }
        let summaries = store.job_summaries("im").unwrap();
        assert_eq!(
            summaries
                .iter()
                .find(|s| s.job_uid == "fresh")
                .expect("fresh summary")
                .phase,
            "queued",
            "rejected attempts never moved the untouched job"
        );
    }
}

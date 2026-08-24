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

pub use error::{StoreError, StoreResult};
pub use migrations::LATEST_SCHEMA_VERSION;
pub use records::{
    EventRow, InstanceRow, JobRow, JobSummary, ReconciliationRow, RunnerRegistrationRow, SlotRow,
    Transition,
};

/// Default operational database location; created only by deployment, never
/// implicitly by the daemon when its parent directory is absent.
pub const DEFAULT_STATE_DB_PATH: &str = "/var/lib/velnor/state.db";

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

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
/// The inner connection is mutex-serialized per handle; concurrent daemon
/// *processes* rely on WAL mode and the busy timeout instead.
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
        let mut conn = Connection::open(path).map_err(|error| {
            StoreError::from(error).with_remediation(format!(
                "verify filesystem permissions for {}",
                path.display()
            ))
        })?;
        configure_connection(&conn)?;
        migrations::ensure_meta_tables(&conn)?;
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
    let wal: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !wal.eq_ignore_ascii_case("wal") {
        return Err(
            StoreError::new(ExitClass::Operation, "store.wal.unavailable")
                .with_remediation("the filesystem must support WAL journaling"),
        );
    }
    conn.execute_batch(
        "PRAGMA foreign_keys=ON;
         PRAGMA synchronous=NORMAL;",
    )?;
    Ok(())
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
    use std::sync::Arc;
    use std::thread;

    use rusqlite::Connection;
    use velnor_model::Timestamp;

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
            runner_name: None,
            trust_scope: Some("trusted".to_owned()),
            resource_policy: Some("standard".to_owned()),
            phase: "queued".to_owned(),
            conclusion: None,
            infrastructure_category: None,
            updated_at: Timestamp::now(),
        }
    }

    fn acquire(store: &str) -> Transition {
        Transition {
            token: format!("acquire-{store}"),
            correlation_id: "corr-1".to_owned(),
            reason: "job.acquired".to_owned(),
            message: Some("slot assigned".to_owned()),
            transition_time: Timestamp::now(),
            next_phase: "running".to_owned(),
            conclusion: None,
            infrastructure_category: None,
        }
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
    fn repeated_migration_is_idempotent() {
        let temp = TempDb::new("idempotent");
        for _ in 0..3 {
            let store = Store::open(&temp.path).expect("open idempotently");
            assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        }
        let conn = Connection::open(&temp.path).unwrap();
        let version_rows: u32 = conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version_rows, 1);
        let lock_rows: u32 = conn
            .query_row("SELECT COUNT(*) FROM migration_lock", [], |r| r.get(0))
            .unwrap();
        assert_eq!(lock_rows, 1);
    }

    #[test]
    fn injected_failure_inside_migration_rolls_back_cleanly() {
        let temp = TempDb::new("rollback");
        let mut conn = Connection::open(&temp.path).unwrap();
        conn.busy_timeout(BUSY_TIMEOUT).unwrap();
        migrations::ensure_meta_tables(&conn).unwrap();
        assert_eq!(migrations::current_version(&conn).unwrap(), 0);

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
    fn five_concurrent_daemons_migrate_and_write_without_corruption() {
        let temp = TempDb::new("concurrent");
        let shared = Arc::new(temp.path.clone());
        let handles: Vec<_> = (0..5)
            .map(|index| {
                let path = Arc::clone(&shared);
                thread::spawn(move || {
                    let slug = format!("daemon-{index}");
                    let store = Store::open(path.as_path()).expect("concurrent open");
                    store.upsert_instance(&instance(&slug)).unwrap();
                    let row = job(&slug, &format!("job-{index}"), "org/concurrent");
                    store.record_job(&row).unwrap();
                    assert!(store
                        .record_job_transition(&slug, &row.job_uid, &acquire("c"))
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
                1
            );
            assert_eq!(store.event_count(&slug, &slug).unwrap(), 1);
        }
        let conn = test_connection(&store);
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
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
        assert_eq!(store.job_summaries("beta").unwrap()[0].phase, "running");

        let conn = test_connection(&store);
        let slot_count: u32 = conn
            .query_row("SELECT COUNT(*) FROM slots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(slot_count, 1, "slots are scoped per instance key");
    }
}

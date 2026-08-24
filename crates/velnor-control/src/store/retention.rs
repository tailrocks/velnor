//! Bounded retention and database accounting (Plan 066 step 5).
//!
//! Pruning runs inside one immediate transaction so a crash mid-prune leaves
//! the previous state fully intact. Age comparisons happen in Rust through
//! [`velnor_model::Timestamp::parse`] rather than SQL string comparison,
//! because variable-fraction RFC 3339 renderings do not order
//! lexicographically. Deletion is bounded per call (`batch_size`, loop caps),
//! protects every active/nonterminal job plus its transition/event ancestry,
//! current instance/slot/registration state, and records its result in the
//! `retention_state` singleton for accounting.

use std::time::Duration;

use rusqlite::{params, params_from_iter};
use velnor_model::{ExitClass, JobState, Timestamp};

use super::error::{StoreError, StoreResult};
use super::rfc3339;
use super::Store;

/// Retention limits for one prune pass.
#[derive(Debug, Clone)]
pub struct RetentionBudget {
    /// Delete events strictly older than this.
    pub max_event_age: Option<Duration>,
    /// Keep at most this many events overall (newest first).
    pub max_event_rows: u64,
    /// Delete jobs in a terminal state whose last update is older than this.
    pub max_terminal_job_age: Option<Duration>,
    /// Keep at most this many terminal jobs overall (newest first).
    pub max_terminal_job_rows: u64,
    /// Best-effort byte ceiling: once total database bytes exceed it, oldest
    /// prunable rows keep going in batches until under the ceiling or until
    /// nothing prunable remains.
    pub max_database_bytes: u64,
    /// Rows examined/deleted per batch; bounds transaction work per step.
    pub batch_size: u64,
}

impl Default for RetentionBudget {
    fn default() -> Self {
        Self {
            max_event_age: Some(Duration::from_secs(30 * 24 * 3600)),
            max_event_rows: 100_000,
            max_terminal_job_age: Some(Duration::from_secs(90 * 24 * 3600)),
            max_terminal_job_rows: 20_000,
            // Soft ceiling: exceeding it prunes; it never corrupts state.
            max_database_bytes: 512 * 1024 * 1024,
            batch_size: 500,
        }
    }
}

/// What one completed prune pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneReport {
    pub deleted_events: u64,
    pub deleted_jobs: u64,
    pub deleted_transitions: u64,
    pub database_bytes: u64,
    pub wal_bytes: u64,
}

/// Point-in-time accounting snapshot published by the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreAccounting {
    pub job_rows: u64,
    pub event_rows: u64,
    pub transition_rows: u64,
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub last_prune_at: Option<String>,
    pub last_deleted_events: u64,
    pub last_deleted_jobs: u64,
}

/// Test seam phases; production callers pass `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrunePhase {
    AfterEventPrune,
    AfterJobPrune,
}

/// The closed taxonomy's active (nonterminal) phase spellings; derived, so
/// taxonomy changes update the SQL automatically.
fn active_phase_list() -> String {
    let states: Vec<String> = JobState::ALL
        .iter()
        .filter(|state| !state.is_terminal())
        .map(|state| concat_quote(state.as_str()))
        .collect();
    format!("({})", states.join(","))
}

fn concat_quote(raw: &str) -> String {
    format!("'{raw}'")
}

impl Store {
    /// Run one bounded prune pass with the given budget.
    ///
    /// # Errors
    /// Any persistence failure; the transaction rolls back so a failed pass
    /// deletes nothing.
    pub fn prune_history(&self, budget: &RetentionBudget) -> StoreResult<PruneReport> {
        self.prune_history_inner(budget, None)
    }

    pub(crate) fn prune_history_inner(
        &self,
        budget: &RetentionBudget,
        hook: Option<&dyn Fn(PrunePhase) -> StoreResult<()>>,
    ) -> StoreResult<PruneReport> {
        let mut conn = self.lock_conn()?;
        let now = Timestamp::now();

        let (deleted_events, deleted_jobs, deleted_transitions) = {
            let transaction =
                conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let deleted_events = Self::prune_expired_events(&transaction, budget, now)?;
            if let Some(hook) = hook {
                hook(PrunePhase::AfterEventPrune)?;
            }
            let (deleted_jobs, deleted_transitions) =
                Self::prune_terminal_jobs(&transaction, budget, now)?;
            if let Some(hook) = hook {
                hook(PrunePhase::AfterJobPrune)?;
            }
            transaction.execute(
                "UPDATE retention_state
                 SET last_prune_at = ?1, deleted_events = ?2, deleted_jobs = ?3
                 WHERE singleton = 0",
                params![
                    rfc3339(now),
                    i64::try_from(deleted_events).unwrap_or(i64::MAX),
                    i64::try_from(deleted_jobs).unwrap_or(i64::MAX),
                ],
            )?;
            transaction.commit()?;
            (deleted_events, deleted_jobs, deleted_transitions)
        };

        // Post-commit accounting uses the still-held connection guard. The
        // checkpoint keeps the WAL from growing without bound; when the
        // ceiling is still exceeded, a bounded incremental vacuum releases
        // freed pages so the byte budget converges. A busy sibling writer
        // can skip the truncation but never fails the prune.
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        if Self::database_bytes(&conn)? > budget.max_database_bytes {
            // Page-bounded: at most 500 pages per pass.
            let _ = conn.execute_batch("PRAGMA incremental_vacuum(500);");
        }
        let database_bytes = Self::database_bytes(&conn)?;
        drop(conn);

        Ok(PruneReport {
            deleted_events,
            deleted_jobs,
            deleted_transitions,
            database_bytes,
            wal_bytes: Self::wal_bytes_for_path(&self.path),
        })
    }

    /// Current accounting snapshot; never mutates resource state.
    ///
    /// # Errors
    /// Any read failure.
    pub fn accounting(&self) -> StoreResult<StoreAccounting> {
        let conn = self.lock_conn()?;
        let job_rows: i64 = conn.query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))?;
        let event_rows: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        let transition_rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM job_transitions", [], |r| r.get(0))?;
        let database_bytes = Self::database_bytes(&conn)?;
        let (last_prune_at, last_deleted_events, last_deleted_jobs): (Option<String>, i64, i64) =
            conn.query_row(
                "SELECT last_prune_at, deleted_events, deleted_jobs FROM retention_state
             WHERE singleton = 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
        drop(conn);
        Ok(StoreAccounting {
            job_rows: job_rows.max(0) as u64,
            event_rows: event_rows.max(0) as u64,
            transition_rows: transition_rows.max(0) as u64,
            database_bytes,
            wal_bytes: Self::wal_bytes_for_path(&self.path),
            last_prune_at,
            last_deleted_events: last_deleted_events.max(0) as u64,
            last_deleted_jobs: last_deleted_jobs.max(0) as u64,
        })
    }

    /// Total main-database file bytes (`page_count * page_size`).
    fn database_bytes(conn: &rusqlite::Connection) -> StoreResult<u64> {
        let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        Ok((page_count.max(0) as u64).saturating_mul(page_size.max(0) as u64))
    }

    /// Sidecar WAL bytes; zero when no WAL file exists right now.
    #[must_use]
    pub fn wal_bytes_for_path(path: &std::path::Path) -> u64 {
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        std::path::PathBuf::from(wal)
            .metadata()
            .map(|meta| meta.len())
            .unwrap_or_default()
    }

    /// Delete expired events, protecting events whose subject is still an
    /// active (nonterminal) job. Bounded by `batch_size` per statement.
    fn prune_expired_events(
        conn: &rusqlite::Connection,
        budget: &RetentionBudget,
        now: Timestamp,
    ) -> StoreResult<u64> {
        let mut deleted = 0u64;
        if let Some(max_age) = budget.max_event_age {
            let cutoff = minus_age(now, max_age);
            loop {
                let batch = protected_expired_older_than(
                    conn,
                    "events",
                    "occurred_at",
                    budget.batch_size,
                    cutoff,
                )?;
                if batch.ids.is_empty() || !batch.exhausted {
                    break;
                }
                deleted += delete_by_ids(conn, "DELETE FROM events WHERE id IN", &batch.ids)?;
            }
        }
        if budget.max_event_rows > 0 {
            let keep_from_id: Option<i64> = conn.query_row(
                "SELECT MIN(id) FROM (
                     SELECT id FROM events ORDER BY id DESC LIMIT ?1
                 )",
                [i64::try_from(budget.max_event_rows).unwrap_or(i64::MAX)],
                |r| r.get(0),
            )?;
            if let Some(keep_from_id) = keep_from_id.filter(|id| *id > 1) {
                // Bounded batches until the cap window is enforced so one
                // pass converges even when the backlog exceeds `batch_size`.
                for _ in 0..10_000 {
                    let victims =
                        protected_ids_below(conn, "events", keep_from_id, budget.batch_size)?;
                    if victims.is_empty() {
                        break;
                    }
                    deleted += delete_by_ids(conn, "DELETE FROM events WHERE id IN", &victims)?;
                    if (victims.len() as u64) < budget.batch_size {
                        break;
                    }
                }
            }
        }
        Ok(deleted)
    }

    /// Delete terminal jobs that are past their age budget or past the
    /// retention row cap, together with their transitions and their own job
    /// events (ancestry stays referentially valid or goes entirely).
    /// Active/nonterminal jobs are untouchable.
    fn prune_terminal_jobs(
        conn: &rusqlite::Connection,
        budget: &RetentionBudget,
        now: Timestamp,
    ) -> StoreResult<(u64, u64)> {
        const TERMINAL_STATES: &str = "('completed','canceled','rejected')";
        let mut deleted_jobs = 0u64;
        let mut deleted_transitions = 0u64;

        // Age rule: oldest-first bounded scans stop at the first fresh row.
        if let Some(max_age) = budget.max_terminal_job_age {
            let cutoff = minus_age(now, max_age);
            loop {
                let batch = unprotected_expired_batch(
                    conn,
                    &format!(
                        "SELECT id, updated_at FROM jobs
                         WHERE phase IN {TERMINAL_STATES} ORDER BY id ASC LIMIT ?1"
                    ),
                    budget.batch_size,
                    cutoff,
                )?;
                if batch.ids.is_empty() || !batch.exhausted {
                    break;
                }
                for id in &batch.ids {
                    let (jobs, transitions) = delete_job_with_ancestry(conn, *id)?;
                    deleted_jobs += jobs;
                    deleted_transitions += transitions;
                }
            }
        }

        // Row-cap rule: everything strictly below the newest-N window goes.
        // The window query is unbounded above so the cap always converges.
        if budget.max_terminal_job_rows > 0 {
            let keep_from_id: Option<i64> = conn.query_row(
                &format!(
                    "SELECT MIN(id) FROM (
                         SELECT id FROM jobs WHERE phase IN {TERMINAL_STATES}
                         ORDER BY id DESC LIMIT ?1
                     )"
                ),
                [i64::try_from(budget.max_terminal_job_rows).unwrap_or(i64::MAX)],
                |r| r.get(0),
            )?;
            if let Some(keep_from_id) = keep_from_id.filter(|id| *id > 1) {
                loop {
                    let victims: Vec<i64> = {
                        let mut statement = conn.prepare_cached(&format!(
                            "SELECT id FROM jobs
                             WHERE id < ?1 AND phase IN {TERMINAL_STATES}
                             ORDER BY id ASC LIMIT ?2"
                        ))?;
                        let mut rows =
                            statement.query(params![keep_from_id, budget.batch_size as i64])?;
                        let mut ids = Vec::new();
                        while let Some(row) = rows.next()? {
                            ids.push(row.get(0)?);
                        }
                        ids
                    };
                    if victims.is_empty() {
                        break;
                    }
                    for id in &victims {
                        let (jobs, transitions) = delete_job_with_ancestry(conn, *id)?;
                        deleted_jobs += jobs;
                        deleted_transitions += transitions;
                    }
                }
            }
        }

        // Byte-ceiling passes with hard iteration bounds so one pathological
        // database cannot hold the write lock forever.
        if budget.max_database_bytes > 0 {
            for _ in 0..200 {
                if Self::database_bytes(conn)? <= budget.max_database_bytes {
                    break;
                }
                let victim: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM jobs
                         WHERE phase IN ('completed','canceled','rejected')
                         ORDER BY id ASC LIMIT 1",
                        [],
                        |r| r.get(0),
                    )
                    .map(Some)
                    .or_else(|error| match error {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        other => Err(other),
                    })?;
                let Some(victim) = victim else { break };
                let (jobs, transitions) = delete_job_with_ancestry(conn, victim)?;
                deleted_jobs += jobs;
                deleted_transitions += transitions;
            }
            for _ in 0..200 {
                if Self::database_bytes(conn)? <= budget.max_database_bytes {
                    break;
                }
                let victims = protected_ids_below(conn, "events", i64::MAX, budget.batch_size)?;
                if victims.is_empty() {
                    break;
                }
                delete_by_ids(conn, "DELETE FROM events WHERE id IN", &victims)?;
            }
        }
        Ok((deleted_jobs, deleted_transitions))
    }
}

/// Delete one job row plus its transitions and its own job events; returns
/// `(deleted_jobs, deleted_transitions)`.
fn delete_job_with_ancestry(conn: &rusqlite::Connection, id: i64) -> StoreResult<(u64, u64)> {
    let identity: Option<(String, String)> = conn
        .query_row(
            "SELECT instance_slug, job_uid FROM jobs WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some((instance_slug, job_uid)) = identity else {
        return Ok((0, 0));
    };
    let transitions = conn.execute(
        "DELETE FROM job_transitions WHERE instance_slug = ?1 AND job_uid = ?2",
        params![instance_slug, job_uid],
    )? as u64;
    conn.execute(
        "DELETE FROM events WHERE instance_slug = ?1 AND subject = ?2",
        params![instance_slug, job_uid],
    )?;
    let jobs = conn.execute("DELETE FROM jobs WHERE id = ?1", [id])? as u64;
    Ok((jobs, transitions))
}

struct ExpiredBatch {
    ids: Vec<i64>,
    /// Whether scanning stopped because the batch ran out of rows rather
    /// than because a live (non-expired) row was reached.
    exhausted: bool,
}

/// Oldest-first `(id, time)` batch kept only while strictly older than
/// `cutoff`, judged in Rust so RFC 3339 rendering variance cannot mis-order
/// deletions. `sql` yields `(id, timestamp_text)` rows.
fn scan_expired_batch(
    conn: &rusqlite::Connection,
    sql: &str,
    batch: u64,
    cutoff: Timestamp,
) -> StoreResult<ExpiredBatch> {
    let mut statement = conn.prepare_cached(sql)?;
    let mut rows = statement.query([batch as i64])?;
    let mut ids = Vec::new();
    let mut exhausted = true;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let raw: String = row.get(1)?;
        let parsed = Timestamp::parse(&raw).map_err(|_| stale_timestamp_error())?;
        if parsed < cutoff {
            ids.push(id);
        } else {
            exhausted = false;
            break;
        }
    }
    Ok(ExpiredBatch { ids, exhausted })
}

/// Events version of [`scan_expired_batch`]: additionally skips (and scans
/// past) events whose subject belongs to an active (nonterminal) job, so a
/// long-running job's early events never unprotect later prunable rows nor
/// get deleted themselves.
fn protected_expired_older_than(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    batch: u64,
    cutoff: Timestamp,
) -> StoreResult<ExpiredBatch> {
    let sql = format!(
        "SELECT id, {column} FROM {table}
         WHERE NOT EXISTS (
             SELECT 1 FROM jobs j
             WHERE j.instance_slug = {table}.instance_slug
               AND j.job_uid = {table}.subject
               AND j.phase IN {active}
         )
         ORDER BY id ASC LIMIT ?1",
        active = active_phase_list(),
    );
    scan_expired_batch(conn, &sql, batch, cutoff)
}

/// Jobs version of [`scan_expired_batch`]: the terminal-state filter is part
/// of the supplied SQL, so no subject protection applies here.
fn unprotected_expired_batch(
    conn: &rusqlite::Connection,
    sql: &str,
    batch: u64,
    cutoff: Timestamp,
) -> StoreResult<ExpiredBatch> {
    scan_expired_batch(conn, sql, batch, cutoff)
}

/// IDs strictly below `keep_from_id`, excluding rows whose `(instance_slug,
/// subject)` pair belongs to an active (nonterminal) job. Only meaningful
/// for the events table.
fn protected_ids_below(
    conn: &rusqlite::Connection,
    table: &str,
    keep_from_id: i64,
    batch: u64,
) -> StoreResult<Vec<i64>> {
    let sql = format!(
        "SELECT id FROM {table}
         WHERE id < ?1
           AND NOT EXISTS (
               SELECT 1 FROM jobs j
               WHERE j.instance_slug = {table}.instance_slug
                 AND j.job_uid = {table}.subject
                 AND j.phase IN {active}
           )
         ORDER BY id ASC LIMIT ?2",
        active = active_phase_list(),
    );
    let mut statement = conn.prepare_cached(&sql)?;
    let mut rows = statement.query(params![keep_from_id, batch as i64])?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next()? {
        ids.push(row.get(0)?);
    }
    Ok(ids)
}

fn delete_by_ids(conn: &rusqlite::Connection, prefix: &str, ids: &[i64]) -> StoreResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!("{prefix} ({placeholders})");
    Ok(conn.execute(&sql, params_from_iter(ids.iter()))? as u64)
}

fn minus_age(now: Timestamp, age: Duration) -> Timestamp {
    now.minus(age)
}

fn stale_timestamp_error() -> StoreError {
    StoreError::new(ExitClass::Operation, "store.retention.timestamp").with_remediation(
        "a stored timestamp is no longer valid RFC 3339; rewrite the row before pruning",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::records::test_connection;

    fn temp_store(label: &str) -> (std::path::PathBuf, Store) {
        let nanos = Timestamp::now()
            .as_offset_datetime()
            .unix_timestamp_nanos()
            .unsigned_abs();
        let dir = std::env::temp_dir().join(format!("velnor-retention-unit-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        (dir, Store::open(&path).unwrap())
    }

    fn seed_expired_events(store: &Store, count: u64) {
        let conn = test_connection(store);
        for index in 0..count {
            conn.execute(
                "INSERT INTO events (instance_slug, event_kind, subject, correlation_id, occurred_at, detail)
                 VALUES ('u', 'gc.completed', 'host', NULL, '2000-01-01T00:00:00Z', NULL)",
                [],
            )
            .unwrap();
            let _ = index;
        }
    }

    #[test]
    fn failure_after_event_prune_rolls_everything_back() {
        let (_dir, store) = temp_store("rollback-events");
        seed_expired_events(&store, 3);
        let before = store.accounting().unwrap().event_rows;

        let budget = RetentionBudget {
            max_event_age: Some(Duration::from_secs(1)),
            ..RetentionBudget::default()
        };
        let error = store
            .prune_history_inner(
                &budget,
                Some(&|phase| {
                    assert_eq!(phase, PrunePhase::AfterEventPrune);
                    Err(StoreError::new(ExitClass::Operation, "test.injected"))
                }),
            )
            .expect_err("injected failure surfaces");

        assert_eq!(error.envelope.reason, "test.injected");
        // The transaction rolled back: nothing was deleted.
        assert_eq!(store.accounting().unwrap().event_rows, before);
    }

    #[test]
    fn failure_after_job_prune_still_deletes_nothing() {
        let (_dir, store) = temp_store("rollback-jobs");
        seed_expired_events(&store, 2);
        let before = store.accounting().unwrap().event_rows;

        let budget = RetentionBudget {
            max_event_age: Some(Duration::from_secs(1)),
            ..RetentionBudget::default()
        };
        let _ = store.prune_history_inner(
            &budget,
            Some(&|phase| match phase {
                PrunePhase::AfterEventPrune => Ok(()),
                PrunePhase::AfterJobPrune => {
                    Err(StoreError::new(ExitClass::Operation, "late.boom"))
                }
            }),
        );

        assert_eq!(store.accounting().unwrap().event_rows, before);
        // And the recorded last-prune marker was not committed either.
        assert!(store.accounting().unwrap().last_prune_at.is_none());
    }

    #[test]
    fn successful_pass_records_state_and_truncates_wal() {
        let (dir, store) = temp_store("wal-truncate");
        seed_expired_events(&store, 5);
        let budget = RetentionBudget {
            max_event_age: Some(Duration::from_secs(1)),
            ..RetentionBudget::default()
        };
        let report = store.prune_history(&budget).unwrap();
        assert_eq!(report.deleted_events, 5);
        assert_eq!(report.wal_bytes, 0);
        drop(store);
        // WAL sidecar is gone or zero-length after the truncate checkpoint.
        let wal_bytes = Store::wal_bytes_for_path(&dir.join("state.db"));
        assert_eq!(wal_bytes, 0);
    }
}

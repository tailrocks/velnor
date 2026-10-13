//! Bounded retention and database accounting (Plan 066 step 5).
//!
//! Pruning writes run inside one immediate transaction so a crash before its
//! commit leaves the previous state fully intact. Post-commit maintenance is
//! independently reported and cannot roll back those writes. Age comparisons
//! happen in Rust through
//! [`velnor_model::Timestamp::parse`] rather than SQL string comparison,
//! because variable-fraction RFC 3339 renderings do not order
//! lexicographically. Deletion is bounded per call (`batch_size`, loop caps),
//! protects every active/nonterminal job plus its transition/event ancestry,
//! current instance/slot/registration state, and records its result in the
//! `retention_state` singleton for accounting.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use rusqlite::{params, params_from_iter, OptionalExtension};
use velnor_model::{ExitClass, Timestamp};

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
    /// Logical pressure threshold for deleting oldest prunable rows. This is
    /// not a physical byte ceiling: the job path defers WAL checkpointing, so
    /// SQLite may retain file bytes above this value until bounded maintenance
    /// runs separately.
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

const MIN_EFFECTIVE_BATCH_SIZE: u64 = 1;
/// Prune SQL never fetches more than this many rows per statement. Keeping
/// this below the public accounting batch cap makes the write-lock ceiling
/// independent of hostile `RetentionBudget::batch_size` values.
const MAX_PRUNE_BATCH_SIZE: u64 = 64;
const MIN_PRUNE_BATCHES: u64 = 8;
const MAX_PRUNE_BATCHES: u64 = 8;
/// Configured defaults are valid, but an unbounded newest-N query is not. A
/// caller above this explicit supported ceiling gets a typed error instead of
/// silently disabling row retention. The query returns one row and is backed
/// by the migration's indexed ID ordering.
const MAX_RETENTION_WINDOW_ROWS: u64 = 100_000;

/// Lease owner tokens are operational identities, not arbitrary payloads.
const MAX_RETENTION_LEASE_OWNER_BYTES: usize = 128;

fn prune_batch_size(budget: &RetentionBudget) -> u64 {
    budget
        .batch_size
        .clamp(MIN_EFFECTIVE_BATCH_SIZE, MAX_PRUNE_BATCH_SIZE)
}

fn prune_pass_batch_limit(_budget: &RetentionBudget) -> u64 {
    // Keep write-lock work independent of user-configured row windows.
    MAX_PRUNE_BATCHES.max(MIN_PRUNE_BATCHES)
}

/// What one completed prune pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneReport {
    pub deleted_events: u64,
    pub deleted_jobs: u64,
    pub deleted_transitions: u64,
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub total_bytes: u64,
    pub oldest_retained_at: Option<String>,
    /// False means the oldest timestamp is only a bounded observation and is
    /// intentionally omitted from `oldest_retained_at`.
    pub oldest_retained_at_complete: bool,
    /// True when SQLite maintenance was intentionally not performed on this
    /// job path. In particular, `wal_bytes` was not reclaimed.
    pub maintenance_deferred: bool,
    pub maintenance_reason: Option<String>,
}

/// Maintenance status after a prune commit. SQLite does not expose a bounded
/// WAL-checkpoint frame count, so the job-path policy defers checkpointing and
/// reports that fact instead of running potentially unbounded work.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MaintenanceStatus {
    deferred: bool,
    reason: Option<String>,
}

/// Point-in-time accounting snapshot published by the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreAccounting {
    pub job_rows: u64,
    pub event_rows: u64,
    pub transition_rows: u64,
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub total_bytes: u64,
    pub oldest_retained_at: Option<String>,
    /// False means the oldest timestamp is only a bounded observation and is
    /// intentionally omitted from `oldest_retained_at`.
    pub oldest_retained_at_complete: bool,
    /// Accounting does not perform SQLite maintenance, so this is true and
    /// the WAL is never claimed to have been reclaimed by this snapshot.
    pub maintenance_deferred: bool,
    pub maintenance_reason: Option<String>,
    pub last_prune_at: Option<String>,
    pub last_deleted_events: u64,
    pub last_deleted_jobs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RetentionSnapshot {
    job_rows: u64,
    event_rows: u64,
    transition_rows: u64,
    database_bytes: u64,
    wal_bytes: u64,
    total_bytes: u64,
    oldest_retained_at: Option<String>,
    oldest_retained_at_complete: bool,
    last_prune_at: Option<String>,
    last_deleted_events: u64,
    last_deleted_jobs: u64,
    maintenance_status: MaintenanceStatus,
}

/// Test seam phases; production callers pass `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrunePhase {
    AfterEventPrune,
    AfterJobPrune,
}

/// Only these exact spellings are terminal for retention. Any other value,
/// including a future or malformed phase, protects its event ancestry.
const CANONICAL_TERMINAL_PHASES: &str = "('completed','canceled','rejected')";
/// Reconciliation states outside this closed set are treated as active or
/// unknown. Their instance's events stay protected until an explicit terminal
/// state is recorded.
const CLOSED_RECONCILIATION_STATUSES: &str = "('completed','canceled','failed','rejected')";

fn retention_budget_error(detail: impl Into<String>) -> StoreError {
    StoreError::new(ExitClass::Operation, "store.retention.budget").with_remediation(detail)
}

fn validate_budget(budget: &RetentionBudget) -> StoreResult<()> {
    for (field, value) in [
        ("max_event_rows", budget.max_event_rows),
        ("max_terminal_job_rows", budget.max_terminal_job_rows),
    ] {
        if value > MAX_RETENTION_WINDOW_ROWS {
            return Err(retention_budget_error(format!(
                "{field}={value} exceeds the supported retention window of {MAX_RETENTION_WINDOW_ROWS}; reduce the value or run more frequent bounded passes"
            )));
        }
    }
    Ok(())
}

impl Store {
    /// Try to own retention maintenance at a supplied clock value. A live
    /// foreign owner blocks; an empty, missing, or expired lease is replaced
    /// by an atomic update. The owner token is operational identity only and
    /// must never contain a secret.
    pub fn try_acquire_retention_lease_at(
        &self,
        owner: &str,
        now_unix: u64,
        lease_duration: Duration,
    ) -> StoreResult<bool> {
        if owner.is_empty() || owner.len() > MAX_RETENTION_LEASE_OWNER_BYTES {
            return Err(StoreError::new(
                ExitClass::Usage,
                "store.retention.lease.owner",
            )
            .with_remediation(format!(
                "retention lease owner must be 1..={MAX_RETENTION_LEASE_OWNER_BYTES} bytes and contain no secret"
            )));
        }
        let now = sqlite_i64(now_unix);
        let expires_at = sqlite_i64(now_unix.saturating_add(lease_duration.as_secs()));
        let mut conn = self.open_maintenance_connection()?;
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE retention_lease
             SET owner = ?1, expires_at = ?2
             WHERE singleton = 0
               AND (owner IS NULL
                    OR owner = ''
                    OR expires_at IS NULL
                    OR expires_at <= ?3
                    OR owner = ?1)",
            params![owner, expires_at, now],
        )?;
        if changed == 0 {
            let lease_row_exists: bool = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM retention_lease WHERE singleton = 0
                 )",
                [],
                |row| row.get(0),
            )?;
            if !lease_row_exists {
                return Err(StoreError::new(
                    ExitClass::Unavailable,
                    "store.retention.lease.missing",
                ));
            }
        }
        transaction.commit()?;
        Ok(changed == 1)
    }

    /// Release the lease only when its owner still matches. This is
    /// idempotent and cannot clear a successor that took over after expiry.
    pub fn release_retention_lease(&self, owner: &str) -> StoreResult<bool> {
        let mut conn = self.open_maintenance_connection()?;
        release_retention_lease_connection(&mut conn, owner)
    }

    /// Try to own retention maintenance using the current Unix-second clock.
    pub fn try_acquire_retention_lease(
        &self,
        owner: &str,
        lease_duration: Duration,
    ) -> StoreResult<bool> {
        let now = Timestamp::now().as_offset_datetime().unix_timestamp();
        self.try_acquire_retention_lease_at(owner, u64::try_from(now).unwrap_or(0), lease_duration)
    }

    /// Run one bounded prune pass with the given budget.
    ///
    /// Every candidate query is limited to `MAX_PRUNE_BATCH_SIZE`, every
    /// repeated phase is capped at `MAX_PRUNE_BATCHES`, and newest-window
    /// discovery is capped at `MAX_RETENTION_WINDOW_ROWS`. Thus configured
    /// retention counts cannot extend the immediate write transaction without
    /// bound; a later pass can continue safe over-retention.
    ///
    /// # Errors
    /// Pre-commit persistence failures roll back the prune transaction. A
    /// post-commit maintenance failure is returned explicitly; committed
    /// deletions remain durable and are not rolled back.
    pub fn prune_history(&self, budget: &RetentionBudget) -> StoreResult<PruneReport> {
        self.prune_history_inner(budget, None)
    }

    pub(crate) fn prune_history_inner(
        &self,
        budget: &RetentionBudget,
        hook: Option<&dyn Fn(PrunePhase) -> StoreResult<()>>,
    ) -> StoreResult<PruneReport> {
        validate_budget(budget)?;
        let now = Timestamp::now();

        let (deleted_events, deleted_jobs, deleted_transitions) = {
            let mut conn = self.lock_conn()?;
            let transaction =
                conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let deleted_events = Self::prune_expired_events(&transaction, budget, now)?;
            if let Some(hook) = hook {
                hook(PrunePhase::AfterEventPrune)?;
            }
            let (more_events, deleted_jobs, deleted_transitions) =
                Self::prune_terminal_jobs(&transaction, &self.path, budget, now)?;
            let deleted_events = deleted_events.saturating_add(more_events);
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

        // Maintenance uses a separate zero-wait connection. Checkpointing is
        // intentionally deferred because SQLite cannot bound its frame work.
        let snapshot = self.maintain_after_prune(budget)?;

        Ok(PruneReport {
            deleted_events,
            deleted_jobs,
            deleted_transitions,
            database_bytes: snapshot.database_bytes,
            wal_bytes: snapshot.wal_bytes,
            total_bytes: snapshot.total_bytes,
            oldest_retained_at: snapshot.oldest_retained_at,
            oldest_retained_at_complete: snapshot.oldest_retained_at_complete,
            maintenance_deferred: snapshot.maintenance_status.deferred,
            maintenance_reason: snapshot.maintenance_status.reason,
        })
    }

    fn maintain_after_prune(&self, budget: &RetentionBudget) -> StoreResult<RetentionSnapshot> {
        let mut maintenance = self
            .open_maintenance_connection()
            .map_err(|error| maintenance_error("open", error))?;
        let maintenance_status = MaintenanceStatus {
            deferred: true,
            reason: Some(
                "WAL checkpoint deferred: SQLite exposes no bounded frame-count checkpoint; a dedicated maintenance worker must perform it".to_owned(),
            ),
        };
        let (database_bytes, wal_bytes) = Self::total_database_bytes(&maintenance, &self.path)
            .map_err(|error| maintenance_error("database accounting", error))?;
        if budget.max_database_bytes > 0
            && database_bytes.saturating_add(wal_bytes) > budget.max_database_bytes
        {
            maintenance
                .execute_batch("PRAGMA incremental_vacuum(500)")
                .map_err(|error| maintenance_error("incremental vacuum", error))?;
        }
        read_retention_snapshot(&mut maintenance, &self.path).map(|snapshot| RetentionSnapshot {
            job_rows: snapshot.job_rows,
            event_rows: snapshot.event_rows,
            transition_rows: snapshot.transition_rows,
            last_prune_at: snapshot.last_prune_at,
            last_deleted_events: snapshot.last_deleted_events,
            last_deleted_jobs: snapshot.last_deleted_jobs,
            database_bytes: snapshot.database_bytes,
            wal_bytes: snapshot.wal_bytes,
            total_bytes: snapshot.total_bytes,
            oldest_retained_at: snapshot.oldest_retained_at,
            oldest_retained_at_complete: snapshot.oldest_retained_at_complete,
            maintenance_status,
        })
    }

    /// Current accounting snapshot; never mutates resource state.
    ///
    /// # Errors
    /// Any read failure.
    pub fn accounting(&self) -> StoreResult<StoreAccounting> {
        let mut conn = self
            .open_maintenance_connection()
            .map_err(|error| maintenance_error("open", error))?;
        let snapshot = read_retention_snapshot(&mut conn, &self.path)
            .map_err(|error| maintenance_error("accounting snapshot", error))?;
        Ok(StoreAccounting {
            job_rows: snapshot.job_rows,
            event_rows: snapshot.event_rows,
            transition_rows: snapshot.transition_rows,
            database_bytes: snapshot.database_bytes,
            wal_bytes: snapshot.wal_bytes,
            total_bytes: snapshot.total_bytes,
            oldest_retained_at: snapshot.oldest_retained_at,
            oldest_retained_at_complete: snapshot.oldest_retained_at_complete,
            maintenance_deferred: true,
            maintenance_reason: Some(
                "WAL checkpoint deferred: accounting does not perform unbounded SQLite maintenance"
                    .to_owned(),
            ),
            last_prune_at: snapshot.last_prune_at,
            last_deleted_events: snapshot.last_deleted_events,
            last_deleted_jobs: snapshot.last_deleted_jobs,
        })
    }

    /// Total main-database file bytes (`page_count * page_size`).
    fn database_bytes(conn: &rusqlite::Connection) -> StoreResult<u64> {
        let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        Ok((page_count.max(0) as u64).saturating_mul(page_size.max(0) as u64))
    }

    /// Sidecar WAL bytes; zero when no WAL file exists right now. Any other
    /// metadata failure is returned instead of being silently reported as 0.
    pub fn wal_bytes_for_path(path: &Path) -> StoreResult<u64> {
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        match Path::new(&wal).metadata() {
            Ok(meta) => Ok(meta.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(maintenance_error("WAL accounting", error)),
        }
    }

    fn total_database_bytes(conn: &rusqlite::Connection, path: &Path) -> StoreResult<(u64, u64)> {
        Ok((Self::database_bytes(conn)?, Self::wal_bytes_for_path(path)?))
    }

    /// Delete expired events, protecting events whose subject is still an
    /// active or unknown-phase job. Bounded by the effective batch size per
    /// statement.
    fn prune_expired_events(
        conn: &rusqlite::Connection,
        budget: &RetentionBudget,
        now: Timestamp,
    ) -> StoreResult<u64> {
        let mut deleted = 0u64;
        if let Some(max_age) = budget.max_event_age {
            let cutoff = minus_age(now, max_age);
            let mut cursor = 0_i64;
            for _ in 0..prune_pass_batch_limit(budget) {
                let batch = protected_expired_older_than(
                    conn,
                    "events",
                    "occurred_at",
                    prune_batch_size(budget),
                    cutoff,
                    cursor,
                )?;
                deleted += delete_events_by_ids(conn, &batch.ids, now)?;
                let Some(last_id) = batch.last_id else { break };
                cursor = last_id;
                if batch.exhausted {
                    break;
                }
            }
        }
        if budget.max_event_rows > 0 {
            let keep_from_id = newest_window_start(
                conn,
                "events",
                None,
                budget.max_event_rows,
                prune_batch_size(budget),
            )?;
            if let Some(keep_from_id) = keep_from_id.filter(|id| *id > 1) {
                // Bounded batches move toward the cap; repeated passes finish
                // larger backlogs without one pass monopolizing the store.
                for _ in 0..prune_pass_batch_limit(budget) {
                    let victims = protected_ids_below(
                        conn,
                        "events",
                        keep_from_id,
                        prune_batch_size(budget),
                    )?;
                    if victims.is_empty() {
                        break;
                    }
                    deleted += delete_events_by_ids(conn, &victims, now)?;
                    if (victims.len() as u64) < prune_batch_size(budget) {
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
        path: &Path,
        budget: &RetentionBudget,
        now: Timestamp,
    ) -> StoreResult<(u64, u64, u64)> {
        let mut deleted_jobs = 0u64;
        let mut deleted_transitions = 0u64;
        let mut deleted_events = 0u64;

        // Age rule: bounded scans continue past fresh rows because insertion
        // order and event time are independent.
        if let Some(max_age) = budget.max_terminal_job_age {
            let cutoff = minus_age(now, max_age);
            let mut cursor = 0_i64;
            for _ in 0..prune_pass_batch_limit(budget) {
                let batch = unprotected_expired_batch(
                    conn,
                    &format!(
                        "SELECT id, updated_at FROM jobs
                         WHERE phase IN {CANONICAL_TERMINAL_PHASES}
                           AND id > ?2 ORDER BY id ASC LIMIT ?1"
                    ),
                    prune_batch_size(budget),
                    cutoff,
                    cursor,
                )?;

                for id in &batch.ids {
                    let (jobs, transitions, events) = delete_job_with_ancestry(conn, *id, now)?;
                    deleted_jobs += jobs;
                    deleted_transitions += transitions;
                    deleted_events += events;
                }
                let Some(last_id) = batch.last_id else { break };
                cursor = last_id;
                if batch.exhausted {
                    break;
                }
            }
        }

        // Row-cap rule: everything strictly below the newest-N window is
        // eligible. The bounded pass may require another invocation.
        if budget.max_terminal_job_rows > 0 {
            let keep_from_id = newest_window_start(
                conn,
                "jobs",
                Some(&format!("phase IN {CANONICAL_TERMINAL_PHASES}")),
                budget.max_terminal_job_rows,
                prune_batch_size(budget),
            )?;
            if let Some(keep_from_id) = keep_from_id.filter(|id| *id > 1) {
                for _ in 0..prune_pass_batch_limit(budget) {
                    let victims: Vec<i64> = {
                        let mut statement = conn.prepare_cached(&format!(
                            "SELECT id FROM jobs
                             WHERE id < ?1 AND phase IN {CANONICAL_TERMINAL_PHASES}
                             ORDER BY id ASC LIMIT ?2"
                        ))?;
                        let mut rows = statement.query(params![
                            keep_from_id,
                            i64::try_from(prune_batch_size(budget)).unwrap_or(i64::MAX),
                        ])?;
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
                        let (jobs, transitions, events) = delete_job_with_ancestry(conn, *id, now)?;
                        deleted_jobs += jobs;
                        deleted_transitions += transitions;
                        deleted_events += events;
                    }
                }
            }
        }

        // Byte-ceiling passes with hard iteration bounds so one pathological
        // database cannot hold the write lock forever.
        if budget.max_database_bytes > 0 {
            for _ in 0..prune_pass_batch_limit(budget) {
                let (database_bytes, wal_bytes) = Self::total_database_bytes(conn, path)?;
                if database_bytes.saturating_add(wal_bytes) <= budget.max_database_bytes {
                    break;
                }
                let victim: Option<i64> = conn
                    .query_row(
                        &format!(
                            "SELECT id FROM jobs
                             WHERE phase IN {CANONICAL_TERMINAL_PHASES}
                             ORDER BY id ASC LIMIT 1"
                        ),
                        [],
                        |r| r.get(0),
                    )
                    .map(Some)
                    .or_else(|error| match error {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        other => Err(other),
                    })?;
                let Some(victim) = victim else { break };
                let (jobs, transitions, events) = delete_job_with_ancestry(conn, victim, now)?;
                deleted_jobs += jobs;
                deleted_transitions += transitions;
                deleted_events += events;
            }
            for _ in 0..prune_pass_batch_limit(budget) {
                let (database_bytes, wal_bytes) = Self::total_database_bytes(conn, path)?;
                if database_bytes.saturating_add(wal_bytes) <= budget.max_database_bytes {
                    break;
                }
                let victims =
                    protected_ids_below(conn, "events", i64::MAX, prune_batch_size(budget))?;
                if victims.is_empty() {
                    break;
                }
                deleted_events += delete_events_by_ids(conn, &victims, now)?;
            }
        }
        Ok((deleted_events, deleted_jobs, deleted_transitions))
    }
}

fn sqlite_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn release_retention_lease_connection(
    conn: &mut rusqlite::Connection,
    owner: &str,
) -> StoreResult<bool> {
    let changed = conn.execute(
        "UPDATE retention_lease
         SET owner = NULL, expires_at = NULL
         WHERE singleton = 0 AND owner = ?1",
        [owner],
    )?;
    Ok(changed == 1)
}

fn read_retention_snapshot(
    conn: &mut rusqlite::Connection,
    path: &Path,
) -> StoreResult<RetentionSnapshot> {
    let transaction = conn.transaction()?;
    // v9 triggers maintain these counters in the same transactions as the
    // source rows. Accounting therefore remains O(1) in table size and does
    // not pin a long read snapshot while a writer waits for a checkpoint.
    let (job_rows, event_rows, transition_rows): (i64, i64, i64) = transaction.query_row(
        "SELECT job_rows, event_rows, transition_rows
         FROM retention_state WHERE singleton = 0",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let (database_bytes, wal_bytes) = Store::total_database_bytes(&transaction, path)?;
    let (last_prune_at, last_deleted_events, last_deleted_jobs): (Option<String>, i64, i64) =
        transaction.query_row(
            "SELECT last_prune_at, deleted_events, deleted_jobs FROM retention_state
             WHERE singleton = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let (oldest_retained_at, oldest_retained_at_complete) = oldest_retained_at(&transaction)?;
    transaction.commit()?;

    Ok(RetentionSnapshot {
        job_rows: job_rows.max(0) as u64,
        event_rows: event_rows.max(0) as u64,
        transition_rows: transition_rows.max(0) as u64,
        database_bytes,
        wal_bytes,
        total_bytes: database_bytes.saturating_add(wal_bytes),
        oldest_retained_at,
        oldest_retained_at_complete,
        last_prune_at,
        last_deleted_events: last_deleted_events.max(0) as u64,
        last_deleted_jobs: last_deleted_jobs.max(0) as u64,
        maintenance_status: MaintenanceStatus {
            deferred: false,
            reason: None,
        },
    })
}

/// Find the oldest retained event/transition using v9's normalized expression
/// indexes. RFC 3339 text is not ordered safely, so SQLite orders by Julian
/// day and Rust returns the original timestamp. Malformed rows produce an
/// explicit incomplete result; accounting never labels a partial result as
/// complete and never turns post-commit reporting into a retry signal.
fn oldest_retained_at(conn: &rusqlite::Connection) -> StoreResult<(Option<String>, bool)> {
    let malformed: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM events WHERE julianday(occurred_at) IS NULL
         ) OR EXISTS(
             SELECT 1 FROM job_transitions WHERE julianday(transition_time) IS NULL
         )",
        [],
        |row| row.get(0),
    )?;
    if malformed {
        return Ok((None, false));
    }
    let mut oldest: Option<Timestamp> = None;
    for (table, column) in [
        ("events", "occurred_at"),
        ("job_transitions", "transition_time"),
    ] {
        let sql = format!(
            "SELECT {column} FROM {table}
             WHERE julianday({column}) IS NOT NULL
             ORDER BY julianday({column}) ASC, id ASC LIMIT 1"
        );
        let raw: Option<String> = conn.query_row(&sql, [], |row| row.get(0)).optional()?;
        let Some(raw) = raw else { continue };
        let Ok(timestamp) = Timestamp::parse(&raw) else {
            return Ok((None, false));
        };
        if oldest.is_none_or(|current| timestamp < current) {
            oldest = Some(timestamp);
        }
    }
    Ok((oldest.map(rfc3339), true))
}

/// Find the oldest ID inside a newest-N retention window without materializing
/// the window in Rust. The explicit supported ceiling is validated before a
/// prune starts; larger values fail closed instead of disabling row retention.
fn newest_window_start(
    conn: &rusqlite::Connection,
    table: &str,
    predicate: Option<&str>,
    keep_rows: u64,
    batch_size: u64,
) -> StoreResult<Option<i64>> {
    if keep_rows > MAX_RETENTION_WINDOW_ROWS {
        return Err(retention_budget_error(
            "row window exceeds the supported retention ceiling",
        ));
    }
    // OFFSET is bounded by the validated ceiling and returns one ID. This
    // avoids allocating the newest-N window or repeatedly reacquiring a
    // statement for each page while the immediate transaction is held.
    let _ = batch_size;
    let offset = i64::try_from(keep_rows.saturating_sub(1)).unwrap_or(i64::MAX);
    let where_clause = predicate.map_or_else(String::new, |value| format!(" AND ({value})"));
    let sql = format!(
        "SELECT id FROM {table}
         WHERE 1 = 1{where_clause}
         ORDER BY id DESC LIMIT 1 OFFSET ?1"
    );
    conn.query_row(&sql, [offset], |row| row.get(0))
        .optional()
        .map_err(StoreError::from)
}

/// Keep cursor validity independent from currently retained rows. A fully
/// pruned stream still rejects cursors below its high-water mark. Refresh only
/// instances changed by a bounded delete batch; refreshing every stream row
/// would turn a bounded prune into a fleet-wide scan.
fn refresh_event_stream_state_for_instance(
    conn: &rusqlite::Connection,
    instance_slug: &str,
    now: Timestamp,
) -> StoreResult<()> {
    conn.execute(
        "UPDATE event_stream_state
         SET first_retained_id = COALESCE(
                 (SELECT MIN(id) FROM events WHERE events.instance_slug = event_stream_state.instance_slug),
                 high_water_id + 1
             ),
             high_water_id = MAX(
                 high_water_id,
                 COALESCE((SELECT MAX(id) FROM events WHERE events.instance_slug = event_stream_state.instance_slug), 0)
             ),
             updated_at = ?1
         WHERE instance_slug = ?2",
        params![rfc3339(now), instance_slug],
    )?;
    Ok(())
}

/// Delete one job row plus its transitions and its own job events; returns
/// `(deleted_jobs, deleted_transitions, deleted_events)`.
fn delete_job_with_ancestry(
    conn: &rusqlite::Connection,
    id: i64,
    now: Timestamp,
) -> StoreResult<(u64, u64, u64)> {
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
        return Ok((0, 0, 0));
    };
    let reconciliation_open: bool = conn.query_row(
        &format!(
            "SELECT EXISTS(
                 SELECT 1 FROM reconciliations
                 WHERE instance_slug = ?1 AND status NOT IN {CLOSED_RECONCILIATION_STATUSES}
             )"
        ),
        [&instance_slug],
        |row| row.get(0),
    )?;
    if reconciliation_open {
        // Reconciliation owns the instance's operational history until it
        // explicitly reaches a closed state. This is conservative by design:
        // no job or ancestry is deleted while ownership is ambiguous.
        return Ok((0, 0, 0));
    }

    // Pull at most one bounded chunk of exact transition ancestry. The
    // extra row detects fan-out without scanning or deleting an unbounded
    // number of transitions in the write transaction.
    let transition_limit = MAX_PRUNE_BATCH_SIZE.saturating_add(1);
    let transition_ids = select_transition_ids(conn, &instance_slug, &job_uid, transition_limit)?;

    // The transition correlation is the exact lifecycle ancestry key. Do not
    // delete by subject/event-kind: generic operational events may reuse a job
    // UID or a lifecycle-looking kind and must survive job deletion.
    let event_ids = select_transition_event_ids(
        conn,
        &instance_slug,
        &job_uid,
        MAX_PRUNE_BATCH_SIZE.saturating_add(1),
    )?;
    let mut events = 0;
    if !event_ids.is_empty() {
        let bounded_ids = &event_ids[..event_ids.len().min(MAX_PRUNE_BATCH_SIZE as usize)];
        events = delete_events_by_ids(conn, bounded_ids, now)?;
    }
    if event_ids.len() > MAX_PRUNE_BATCH_SIZE as usize {
        return Ok((0, 0, events));
    }

    if transition_ids.len() > MAX_PRUNE_BATCH_SIZE as usize {
        let bounded_ids = &transition_ids[..MAX_PRUNE_BATCH_SIZE as usize];
        let transitions = delete_transition_rows_by_ids(conn, bounded_ids)?;
        return Ok((0, transitions, events));
    }

    let transitions = delete_transition_rows_by_ids(conn, &transition_ids)?;
    let jobs = conn.execute("DELETE FROM jobs WHERE id = ?1", [id])? as u64;
    Ok((jobs, transitions, events))
}

fn select_transition_ids(
    conn: &rusqlite::Connection,
    instance_slug: &str,
    job_uid: &str,
    limit: u64,
) -> StoreResult<Vec<i64>> {
    let mut statement = conn.prepare_cached(
        "SELECT id FROM job_transitions
         WHERE instance_slug = ?1 AND job_uid = ?2
         ORDER BY id ASC LIMIT ?3",
    )?;
    let mut rows = statement.query(params![
        instance_slug,
        job_uid,
        i64::try_from(limit).unwrap_or(i64::MAX)
    ])?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next()? {
        ids.push(row.get(0)?);
    }
    Ok(ids)
}

fn select_transition_event_ids(
    conn: &rusqlite::Connection,
    instance_slug: &str,
    job_uid: &str,
    limit: u64,
) -> StoreResult<Vec<i64>> {
    let mut statement = conn.prepare_cached(
        "SELECT e.id FROM events e
         WHERE e.instance_slug = ?1
           AND EXISTS (
               SELECT 1 FROM job_transitions t
               WHERE t.instance_slug = ?1
                 AND t.job_uid = ?2
                 AND t.correlation_id = e.correlation_id
                 AND e.subject = t.job_uid
                 AND e.event_kind = 'job.transition.' || t.reason
           )
         ORDER BY e.id ASC LIMIT ?3",
    )?;
    let mut rows = statement.query(params![
        instance_slug,
        job_uid,
        i64::try_from(limit).unwrap_or(i64::MAX)
    ])?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next()? {
        ids.push(row.get(0)?);
    }
    Ok(ids)
}

fn delete_transition_rows_by_ids(conn: &rusqlite::Connection, ids: &[i64]) -> StoreResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    Ok(conn.execute(
        &format!("DELETE FROM job_transitions WHERE id IN ({placeholders})"),
        params_from_iter(ids.iter()),
    )? as u64)
}

struct ExpiredBatch {
    ids: Vec<i64>,
    last_id: Option<i64>,
    /// Whether the scan reached the end of the candidate rows.
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
    cursor: i64,
) -> StoreResult<ExpiredBatch> {
    let batch = batch.clamp(MIN_EFFECTIVE_BATCH_SIZE, MAX_PRUNE_BATCH_SIZE);
    let mut statement = conn.prepare_cached(sql)?;
    let mut rows = statement.query(params![i64::try_from(batch).unwrap_or(i64::MAX), cursor])?;
    let mut ids = Vec::new();
    let mut last_id = None;
    let mut row_count = 0;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        last_id = Some(id);
        row_count += 1;
        let raw: String = row.get(1)?;
        let parsed = Timestamp::parse(&raw).map_err(|_| stale_timestamp_error())?;
        if parsed < cutoff {
            ids.push(id);
        }
    }
    Ok(ExpiredBatch {
        ids,
        last_id,
        exhausted: row_count < batch,
    })
}

/// Events version of [`scan_expired_batch`]: additionally skips (and scans
/// past) events whose subject belongs to a nonterminal or unknown-phase job,
/// or to an open/unknown reconciliation. A long-running job's early events
/// never get deleted themselves.
fn protected_expired_older_than(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    batch: u64,
    cutoff: Timestamp,
    cursor: i64,
) -> StoreResult<ExpiredBatch> {
    let sql = format!(
        "SELECT id, {column} FROM {table}
         WHERE NOT EXISTS (
             SELECT 1 FROM jobs j
             WHERE j.instance_slug = {table}.instance_slug
               AND j.job_uid = {table}.subject
               AND j.phase NOT IN {terminal}
         ) AND NOT EXISTS (
             SELECT 1 FROM reconciliations r
             WHERE r.instance_slug = {table}.instance_slug
               AND r.status NOT IN {reconciliation_terminal}
         ) AND {table}.id > ?2
         ORDER BY id ASC LIMIT ?1",
        terminal = CANONICAL_TERMINAL_PHASES,
        reconciliation_terminal = CLOSED_RECONCILIATION_STATUSES,
    );
    scan_expired_batch(conn, &sql, batch, cutoff, cursor)
}

/// Jobs version of [`scan_expired_batch`]: the terminal-state filter is part
/// of the supplied SQL, so no subject protection applies here.
fn unprotected_expired_batch(
    conn: &rusqlite::Connection,
    sql: &str,
    batch: u64,
    cutoff: Timestamp,
    cursor: i64,
) -> StoreResult<ExpiredBatch> {
    scan_expired_batch(conn, sql, batch, cutoff, cursor)
}

/// IDs strictly below `keep_from_id`, excluding rows protected by a
/// nonterminal/unknown job or reconciliation in the same instance. Only
/// meaningful for the events table.
fn protected_ids_below(
    conn: &rusqlite::Connection,
    table: &str,
    keep_from_id: i64,
    batch: u64,
) -> StoreResult<Vec<i64>> {
    let batch = batch.clamp(MIN_EFFECTIVE_BATCH_SIZE, MAX_PRUNE_BATCH_SIZE);
    let sql = format!(
        "SELECT id FROM {table}
         WHERE id < ?1
           AND NOT EXISTS (
               SELECT 1 FROM jobs j
               WHERE j.instance_slug = {table}.instance_slug
                 AND j.job_uid = {table}.subject
                 AND j.phase NOT IN {terminal}
           ) AND NOT EXISTS (
               SELECT 1 FROM reconciliations r
               WHERE r.instance_slug = {table}.instance_slug
                 AND r.status NOT IN {reconciliation_terminal}
           )
         ORDER BY id ASC LIMIT ?2",
        terminal = CANONICAL_TERMINAL_PHASES,
        reconciliation_terminal = CLOSED_RECONCILIATION_STATUSES,
    );
    let mut statement = conn.prepare_cached(&sql)?;
    let mut rows = statement.query(params![
        keep_from_id,
        i64::try_from(batch).unwrap_or(i64::MAX)
    ])?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next()? {
        ids.push(row.get(0)?);
    }
    Ok(ids)
}

fn delete_events_by_ids(
    conn: &rusqlite::Connection,
    ids: &[i64],
    now: Timestamp,
) -> StoreResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let instances: BTreeSet<String> = {
        let mut statement = conn.prepare_cached(&format!(
            "SELECT DISTINCT instance_slug FROM events WHERE id IN ({placeholders})"
        ))?;
        let mut rows = statement.query(params_from_iter(ids.iter()))?;
        let mut instances = BTreeSet::new();
        while let Some(row) = rows.next()? {
            instances.insert(row.get(0)?);
        }
        instances
    };
    let deleted = conn.execute(
        &format!("DELETE FROM events WHERE id IN ({placeholders})"),
        params_from_iter(ids.iter()),
    )? as u64;
    for instance_slug in instances {
        refresh_event_stream_state_for_instance(conn, &instance_slug, now)?;
    }
    Ok(deleted)
}

fn minus_age(now: Timestamp, age: Duration) -> Timestamp {
    // `Timestamp::minus` delegates to a checked time subtraction that can
    // panic for a hostile Duration::MAX configuration. Operational rows are
    // epoch-based; clamping at the epoch preserves safe retention semantics
    // for extreme budgets without process termination.
    let now_seconds = now.as_offset_datetime().unix_timestamp().max(0) as u64;
    now.minus(Duration::from_secs(age.as_secs().min(now_seconds)))
}

fn stale_timestamp_error() -> StoreError {
    StoreError::new(ExitClass::Operation, "store.retention.timestamp").with_remediation(
        "a stored timestamp is no longer valid RFC 3339; rewrite the row before pruning",
    )
}

fn maintenance_error(operation: &str, detail: impl std::fmt::Display) -> StoreError {
    StoreError::new(ExitClass::Operation, "store.retention.maintenance").with_remediation(format!(
        "retention maintenance {operation} failed; a prior prune commit is not rolled back: {detail}"
    ))
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

    #[test]
    fn retention_lease_allows_one_same_path_owner() {
        let (_dir, first) = temp_store("lease-competing");
        let second = Store::open(first.path()).unwrap();

        assert!(first
            .try_acquire_retention_lease_at("owner-a", 100, Duration::from_secs(30))
            .unwrap());
        assert!(!second
            .try_acquire_retention_lease_at("owner-b", 101, Duration::from_secs(30))
            .unwrap());

        let connection = test_connection(&first);
        let lease: (Option<String>, Option<i64>) = connection
            .query_row(
                "SELECT owner, expires_at FROM retention_lease WHERE singleton = 0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(lease, (Some("owner-a".to_owned()), Some(130)));
    }

    #[test]
    fn retention_lease_expired_owner_is_taken_over_atomically() {
        let (_dir, first) = temp_store("lease-takeover");
        let second = Store::open(first.path()).unwrap();

        assert!(first
            .try_acquire_retention_lease_at("owner-a", 100, Duration::from_secs(10))
            .unwrap());
        assert!(second
            .try_acquire_retention_lease_at("owner-b", 110, Duration::from_secs(20))
            .unwrap());
        assert!(!first
            .try_acquire_retention_lease_at("owner-a", 111, Duration::from_secs(10))
            .unwrap());
    }

    #[test]
    fn retention_lease_rejects_non_owner_release() {
        let (_dir, store) = temp_store("lease-release");

        assert!(store
            .try_acquire_retention_lease_at("owner-a", 100, Duration::from_secs(30))
            .unwrap());
        assert!(!store.release_retention_lease("owner-b").unwrap());
        assert!(store.release_retention_lease("owner-a").unwrap());
        assert!(!store.release_retention_lease("owner-a").unwrap());
    }

    #[test]
    fn retention_lease_expiry_saturates_without_overflow() {
        let (_dir, store) = temp_store("lease-overflow");

        assert!(store
            .try_acquire_retention_lease_at("owner-a", u64::MAX, Duration::from_secs(u64::MAX),)
            .unwrap());
        let connection = test_connection(&store);
        let expires_at: i64 = connection
            .query_row(
                "SELECT expires_at FROM retention_lease WHERE singleton = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(expires_at, i64::MAX);
    }

    #[test]
    fn retention_lease_contention_and_sql_validation_are_distinguishable() {
        let (_dir, store) = temp_store("lease-errors");

        let usage = store
            .try_acquire_retention_lease_at("", 100, Duration::from_secs(30))
            .expect_err("invalid owner is a SQL-independent validation failure");
        assert_eq!(usage.envelope.reason, "store.retention.lease.owner");

        assert!(store
            .try_acquire_retention_lease_at("owner-a", 100, Duration::from_secs(30))
            .unwrap());
        assert!(!store
            .try_acquire_retention_lease_at("owner-b", 101, Duration::from_secs(30))
            .unwrap());
    }

    #[test]
    fn prune_batch_limit_scales_and_stays_bounded() {
        let budget = RetentionBudget {
            max_event_rows: 10_000,
            max_terminal_job_rows: 2_000,
            batch_size: 500,
            ..RetentionBudget::default()
        };
        assert_eq!(prune_pass_batch_limit(&budget), MAX_PRUNE_BATCHES);

        let tiny_budget = RetentionBudget {
            max_event_rows: 0,
            max_terminal_job_rows: 0,
            batch_size: 0,
            ..RetentionBudget::default()
        };
        assert_eq!(prune_pass_batch_limit(&tiny_budget), MIN_PRUNE_BATCHES);

        let huge_budget = RetentionBudget {
            max_event_rows: u64::MAX,
            max_terminal_job_rows: u64::MAX,
            batch_size: u64::MAX,
            ..RetentionBudget::default()
        };
        assert_eq!(prune_pass_batch_limit(&huge_budget), MAX_PRUNE_BATCHES);
        assert_eq!(prune_batch_size(&huge_budget), MAX_PRUNE_BATCH_SIZE);
    }

    #[test]
    fn newest_window_rejects_unsupported_retention_counts() {
        let (_dir, store) = temp_store("window-bound");
        let connection = test_connection(&store);

        let error = newest_window_start(&connection, "events", None, u64::MAX, 1)
            .expect_err("oversized windows must fail closed");
        assert_eq!(error.envelope.reason, "store.retention.budget");
    }

    #[test]
    fn oversized_budget_is_rejected_without_mutation() {
        let (_dir, store) = temp_store("budget-rejection");
        seed_expired_events(&store, 2);
        let budget = RetentionBudget {
            max_event_rows: MAX_RETENTION_WINDOW_ROWS + 1,
            max_event_age: Some(Duration::from_secs(1)),
            max_terminal_job_age: None,
            max_terminal_job_rows: 0,
            max_database_bytes: 0,
            batch_size: 1,
        };

        let error = store
            .prune_history(&budget)
            .expect_err("unsupported row windows must be explicit");

        assert_eq!(error.envelope.reason, "store.retention.budget");
        assert_eq!(store.accounting().unwrap().event_rows, 2);
    }

    #[test]
    fn age_pruning_stops_at_the_conservative_pass_bound() {
        let (_dir, store) = temp_store("age-bound");
        seed_expired_events(&store, 10);
        let budget = RetentionBudget {
            max_event_age: Some(Duration::from_secs(1)),
            max_event_rows: 0,
            max_terminal_job_age: None,
            max_terminal_job_rows: 0,
            max_database_bytes: 0,
            batch_size: 1,
        };

        let report = store.prune_history(&budget).unwrap();

        assert_eq!(prune_pass_batch_limit(&budget), MIN_PRUNE_BATCHES);
        assert_eq!(report.deleted_events, MIN_PRUNE_BATCHES);
        assert_eq!(store.accounting().unwrap().event_rows, 2);
    }

    #[test]
    fn unknown_job_phase_protects_event_ancestry() {
        let (_dir, store) = temp_store("unknown-phase");
        let connection = test_connection(&store);
        connection
            .execute(
                "INSERT INTO jobs
                 (instance_slug, job_uid, repository, workflow, job_name, phase, updated_at)
                 VALUES ('u', 'job-unknown', 'repo', 'workflow', 'job', 'future_phase',
                         '2000-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO events
                 (instance_slug, event_kind, subject, occurred_at, detail)
                 VALUES ('u', 'job.updated', 'job-unknown', '2000-01-01T00:00:00Z', NULL)",
                [],
            )
            .unwrap();
        drop(connection);

        let budget = RetentionBudget {
            max_event_age: Some(Duration::from_secs(1)),
            max_event_rows: 0,
            max_terminal_job_age: None,
            max_terminal_job_rows: 0,
            max_database_bytes: 0,
            batch_size: 1,
        };

        let report = store.prune_history(&budget).unwrap();

        assert_eq!(report.deleted_events, 0);
        assert_eq!(store.accounting().unwrap().event_rows, 1);
    }

    #[test]
    fn maintenance_connection_does_not_wait_on_busy_writers() {
        let (_dir, store) = temp_store("maintenance-connection");
        let connection = store.open_maintenance_connection().unwrap();
        let timeout: i64 = connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 0);

        let error = maintenance_error("test", "busy");
        assert_eq!(error.envelope.reason, "store.retention.maintenance");
        assert!(error
            .envelope
            .remediation
            .as_deref()
            .is_some_and(|message| message.contains("not rolled back")));
    }

    #[test]
    fn accounting_reports_oldest_retained_event_or_transition_in_one_snapshot() {
        let (_dir, store) = temp_store("oldest-retained");
        let connection = test_connection(&store);
        connection
            .execute(
                "INSERT INTO events
                 (instance_slug, event_kind, subject, occurred_at)
                 VALUES ('oldest', 'capacity.pressure', 'host', '2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO job_transitions
                 (instance_slug, job_uid, transition_token, correlation_id, reason,
                  transition_time)
                 VALUES ('oldest', 'job-1', 'token-1', 'corr-1', 'job.started',
                         '2020-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        drop(connection);

        let accounting = store.accounting().unwrap();

        assert_eq!(accounting.event_rows, 1);
        assert_eq!(accounting.transition_rows, 1);
        assert!(accounting.maintenance_deferred);
        assert!(accounting
            .maintenance_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("checkpoint deferred")));
        assert_eq!(
            accounting.oldest_retained_at.as_deref(),
            Some("2020-01-01T00:00:00Z")
        );
        assert!(accounting.oldest_retained_at_complete);
    }

    #[test]
    fn accounting_reports_malformed_timestamp_as_incomplete() {
        let (_dir, store) = temp_store("oldest-bound");
        let connection = test_connection(&store);
        connection
            .execute(
                "INSERT INTO events
                 (instance_slug, event_kind, subject, occurred_at)
                 VALUES ('bounded', 'capacity.pressure', 'host', 'not-a-timestamp')",
                [],
            )
            .unwrap();
        drop(connection);

        let accounting = store.accounting().unwrap();

        assert!(!accounting.oldest_retained_at_complete);
        assert!(accounting.oldest_retained_at.is_none());
    }

    #[test]
    fn malformed_candidate_rolls_back_before_commit() {
        let (_dir, store) = temp_store("malformed-candidate");
        let connection = test_connection(&store);
        connection
            .execute(
                "INSERT INTO events
                 (instance_slug, event_kind, subject, occurred_at)
                 VALUES ('malformed', 'gc.completed', 'host', 'not-a-timestamp')",
                [],
            )
            .unwrap();
        drop(connection);

        let budget = RetentionBudget {
            max_event_age: Some(Duration::from_secs(1)),
            max_event_rows: 0,
            max_terminal_job_age: None,
            max_terminal_job_rows: 0,
            max_database_bytes: 0,
            batch_size: 1,
        };
        let error = store
            .prune_history(&budget)
            .expect_err("malformed deletion candidates fail before commit");

        assert_eq!(error.envelope.reason, "store.retention.timestamp");
        assert_eq!(store.accounting().unwrap().event_rows, 1);
    }

    #[test]
    fn terminal_deletion_preserves_shared_subject_non_job_events() {
        let (_dir, store) = temp_store("shared-subject");
        let connection = test_connection(&store);
        connection
            .execute(
                "INSERT INTO jobs
                 (instance_slug, job_uid, repository, workflow, job_name, phase, updated_at)
                 VALUES ('shared', 'same-subject', 'repo', 'workflow', 'job', 'completed',
                         '2000-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO job_transitions
                 (instance_slug, job_uid, transition_token, correlation_id, reason,
                  transition_time)
                 VALUES ('shared', 'same-subject', 'token-1', 'corr-shared',
                         'job.completed', '2000-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        for (kind, detail) in [
            ("job.transition.job.completed", "job ancestry"),
            ("capacity.pressure", "unrelated operational event"),
        ] {
            connection
                .execute(
                    "INSERT INTO events
                     (instance_slug, event_kind, subject, correlation_id,
                      occurred_at, detail)
                     VALUES ('shared', ?1, 'same-subject', ?3,
                             '2000-01-01T00:00:00Z', ?2)",
                    params![
                        kind,
                        detail,
                        if kind == "job.transition.job.completed" {
                            Some("corr-shared")
                        } else {
                            None
                        },
                    ],
                )
                .unwrap();
        }
        drop(connection);

        let budget = RetentionBudget {
            max_event_age: None,
            max_event_rows: 0,
            max_terminal_job_age: Some(Duration::from_secs(1)),
            max_terminal_job_rows: 0,
            max_database_bytes: 0,
            batch_size: 1,
        };
        let report = store.prune_history(&budget).unwrap();

        assert_eq!(report.deleted_jobs, 1);
        assert_eq!(store.event_count("shared", "same-subject").unwrap(), 1);
        assert_eq!(
            store.accounting().unwrap().oldest_retained_at.as_deref(),
            Some("2000-01-01T00:00:00Z")
        );
    }

    #[test]
    fn large_terminal_ancestry_converges_in_bounded_exact_chunks() {
        let (_dir, store) = temp_store("large-ancestry");
        let connection = test_connection(&store);
        connection
            .execute(
                "INSERT INTO jobs
                 (instance_slug, job_uid, repository, workflow, job_name, phase, updated_at)
                 VALUES ('fanout', 'job-1', 'repo', 'workflow', 'job', 'completed',
                         '2000-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        for index in 0..(MAX_PRUNE_BATCH_SIZE + 6) {
            let correlation = format!("corr-{index}");
            connection
                .execute(
                    "INSERT INTO job_transitions
                     (instance_slug, job_uid, transition_token, correlation_id, reason,
                      transition_time)
                     VALUES ('fanout', 'job-1', ?1, ?2, 'job.completed',
                             '2000-01-01T00:00:00Z')",
                    params![format!("token-{index}"), correlation],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO events
                     (instance_slug, event_kind, subject, correlation_id, occurred_at)
                     VALUES ('fanout', 'job.transition.job.completed', 'job-1', ?1,
                             '2000-01-01T00:00:00Z')",
                    [correlation],
                )
                .unwrap();
        }
        drop(connection);

        let budget = RetentionBudget {
            max_event_age: None,
            max_event_rows: 0,
            max_terminal_job_age: Some(Duration::from_secs(1)),
            max_terminal_job_rows: 0,
            max_database_bytes: 0,
            batch_size: 1,
        };

        let first = store.prune_history(&budget).unwrap();
        assert_eq!(first.deleted_jobs, 0);
        assert_eq!(first.deleted_events, MAX_PRUNE_BATCH_SIZE);
        assert!(store.accounting().unwrap().job_rows == 1);

        let second = store.prune_history(&budget).unwrap();
        assert_eq!(second.deleted_jobs, 0);
        assert!(second.deleted_transitions <= MAX_PRUNE_BATCH_SIZE);

        let third = store.prune_history(&budget).unwrap();
        assert_eq!(third.deleted_jobs, 1);
        assert_eq!(store.accounting().unwrap().job_rows, 0);
        assert_eq!(store.accounting().unwrap().transition_rows, 0);
        assert_eq!(store.accounting().unwrap().event_rows, 0);
    }

    #[test]
    fn extreme_age_budget_is_saturating_and_does_not_panic() {
        let (_dir, store) = temp_store("extreme-age");
        seed_expired_events(&store, 1);
        let budget = RetentionBudget {
            max_event_age: Some(Duration::MAX),
            max_event_rows: 0,
            max_terminal_job_age: None,
            max_terminal_job_rows: 0,
            max_database_bytes: 0,
            batch_size: u64::MAX,
        };

        let report = store.prune_history(&budget).unwrap();

        assert_eq!(report.deleted_events, 0);
        assert_eq!(
            report.oldest_retained_at.as_deref(),
            Some("2000-01-01T00:00:00Z")
        );
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
    fn successful_pass_records_state_and_reports_wal() {
        let (dir, store) = temp_store("wal-truncate");
        seed_expired_events(&store, 5);
        let budget = RetentionBudget {
            max_event_age: Some(Duration::from_secs(1)),
            ..RetentionBudget::default()
        };
        let report = store.prune_history(&budget).unwrap();
        assert_eq!(report.deleted_events, 5);
        assert_eq!(
            report.wal_bytes,
            Store::wal_bytes_for_path(&dir.join("state.db")).unwrap()
        );
        assert_eq!(
            report.total_bytes,
            report.database_bytes.saturating_add(report.wal_bytes)
        );
        assert!(report.maintenance_deferred);
        assert!(report
            .maintenance_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("checkpoint deferred")));
        drop(store);
        // Checkpointing is intentionally deferred from the job path.
    }
}

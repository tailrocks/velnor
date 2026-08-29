//! Bounded retention and database accounting (Plan 066 step 5).
//!
//! Pruning writes run inside bounded immediate transactions so a crash before
//! any commit leaves the previous state fully intact. Post-commit maintenance
//! is independently reported and cannot roll back those writes. Age comparisons
//! happen in Rust through
//! [`velnor_model::Timestamp::parse`] rather than SQL string comparison,
//! because variable-fraction RFC 3339 renderings do not order
//! lexicographically. Deletion is bounded per call (`batch_size`, loop caps),
//! protects every active/nonterminal job plus its transition/event ancestry,
//! current instance/slot/registration state, and records its result in the
//! `retention_state` singleton for accounting.

use std::collections::BTreeSet;
use std::ffi::CString;
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::{params, params_from_iter, OptionalExtension};
use uuid::Uuid;
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
    /// Physical pressure threshold for the database plus WAL. Retention first
    /// applies bounded logical deletion; the separate maintenance phase then
    /// attempts to prove this physical target.
    pub max_database_bytes: u64,
    /// Rows examined/deleted per batch; bounds transaction work per step.
    pub batch_size: u64,
}

/// Conservative filesystem reserve used when the caller does not provide a
/// more specific maintenance policy. This matches the runner's hard reserve
/// contract and is deliberately absolute; percentages alone are unsafe on
/// both small and large filesystems.
pub const DEFAULT_RETENTION_RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Bounds for the explicit post-prune maintenance operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionMaintenanceBudget {
    /// Physical main-database plus WAL target. Zero disables this check.
    pub max_database_bytes: u64,
    /// Minimum available bytes on the filesystem containing the database.
    pub min_free_bytes: u64,
    /// Maximum PASSIVE checkpoint calls in one operation.
    pub max_checkpoint_attempts: u8,
    /// Maximum incremental-vacuum pages in one operation.
    pub max_vacuum_pages: u64,
}

impl Default for RetentionMaintenanceBudget {
    fn default() -> Self {
        Self {
            max_database_bytes: RetentionBudget::default().max_database_bytes,
            min_free_bytes: DEFAULT_RETENTION_RESERVE_BYTES,
            max_checkpoint_attempts: 1,
            max_vacuum_pages: 500,
        }
    }
}

impl From<&RetentionBudget> for RetentionMaintenanceBudget {
    fn from(budget: &RetentionBudget) -> Self {
        Self {
            max_database_bytes: budget.max_database_bytes,
            ..Self::default()
        }
    }
}

/// Machine-readable result of one bounded WAL checkpoint attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalCheckpointStatus {
    pub attempted: bool,
    pub busy_frames: u64,
    pub log_frames: u64,
    pub checkpointed_frames: u64,
}

impl WalCheckpointStatus {
    fn not_attempted() -> Self {
        Self {
            attempted: false,
            busy_frames: 0,
            log_frames: 0,
            checkpointed_frames: 0,
        }
    }

    fn complete(&self) -> bool {
        self.attempted && self.busy_frames == 0 && self.checkpointed_frames >= self.log_frames
    }
}

/// Physical-budget evaluation. `Unmeasurable` is intentionally distinct from
/// `WithinBudget`; no caller may mistake a failed statvfs probe for proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalBudgetStatus {
    Disabled,
    WithinBudget,
    Exceeded,
    ReserveViolation,
    Deferred,
    Unmeasurable,
    Unconfigured,
}

/// Result of the explicit bounded post-prune maintenance operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionMaintenanceReport {
    pub database_bytes_before: u64,
    pub wal_bytes_before: u64,
    pub total_bytes_before: u64,
    pub database_bytes_after: u64,
    pub wal_bytes_after: u64,
    pub total_bytes_after: u64,
    pub free_bytes_before: Option<u64>,
    pub free_bytes_after: Option<u64>,
    pub reserve_bytes: u64,
    pub reserve_violation: bool,
    pub physical_budget_status: PhysicalBudgetStatus,
    pub checkpoint: WalCheckpointStatus,
    pub vacuum_pages_attempted: u64,
    pub vacuum_pages_reclaimed: u64,
    pub maintenance_deferred: bool,
    pub maintenance_reason: Option<String>,
}

/// Opaque capability authorizing one retention owner and fencing generation.
/// The generation changes on every successful acquisition, so a capability
/// retained by a stale process cannot renew, release, or mutate the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionLease {
    owner: String,
    generation: u64,
}

impl RetentionLease {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
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
const MAX_MAINTENANCE_VACUUM_PAGES: u64 = 500;
const MAX_RELEASE_RETRY_ATTEMPTS: u8 = 2;
const RELEASE_RETRY_BACKOFF: Duration = Duration::from_millis(25);
/// Configured defaults are valid, but an unbounded newest-N query is not. A
/// caller above this explicit supported ceiling gets a typed error instead of
/// silently disabling row retention. The query returns one row and is backed
/// by the migration's indexed ID ordering.
const MAX_RETENTION_WINDOW_ROWS: u64 = 100_000;

/// Lease owner tokens are operational identities, not arbitrary payloads.
const MIN_RETENTION_LEASE_OWNER_BYTES: usize = 8;
const MAX_RETENTION_LEASE_OWNER_BYTES: usize = 128;
const MAX_RETENTION_LEASE_DURATION: Duration = Duration::from_secs(30 * 60);
/// A retention pass is maintenance, never a reason to extend a job or writer
/// wait indefinitely. Later passes continue safe over-retention.
const MAX_RETENTION_PASS_DURATION: Duration = Duration::from_secs(2);

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
    pub free_bytes: Option<u64>,
    pub reserve_bytes: u64,
    pub reserve_violation: bool,
    pub physical_budget_status: PhysicalBudgetStatus,
    pub checkpoint: WalCheckpointStatus,
    pub vacuum_pages_reclaimed: u64,
    pub oldest_retained_at: Option<String>,
    /// False means the oldest timestamp is only a bounded observation and is
    /// intentionally omitted from `oldest_retained_at`.
    pub oldest_retained_at_complete: bool,
    /// True when SQLite maintenance was intentionally not performed on this
    /// job path. In particular, `wal_bytes` was not reclaimed.
    pub maintenance_deferred: bool,
    pub maintenance_reason: Option<String>,
}

/// Failure phase for one retention invocation.
#[derive(Debug)]
pub enum PruneFailure {
    /// The deletion transaction did not commit; retry is safe.
    PreCommit(StoreError),
    /// Deletion committed, but reporting/maintenance failed; retrying can
    /// repeat work and is therefore not implied to be safe.
    PostCommit(StoreError),
    /// The capability expired or was fenced before the next mutation. The
    /// `committed` flag preserves whether earlier deletion is durable.
    LeaseLost { committed: bool, error: StoreError },
}

impl PruneFailure {
    #[must_use]
    pub fn is_post_commit(&self) -> bool {
        matches!(
            self,
            Self::PostCommit(_)
                | Self::LeaseLost {
                    committed: true,
                    ..
                }
        )
    }

    #[must_use]
    pub fn is_lease_lost(&self) -> bool {
        matches!(self, Self::LeaseLost { .. })
    }

    #[must_use]
    pub fn error(&self) -> &StoreError {
        match self {
            Self::PreCommit(error) | Self::PostCommit(error) => error,
            Self::LeaseLost { error, .. } => error,
        }
    }

    fn into_store_error(self) -> StoreError {
        match self {
            Self::PreCommit(error) | Self::PostCommit(error) => error,
            Self::LeaseLost { error, .. } => error,
        }
    }
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
    pub free_bytes: Option<u64>,
    pub reserve_bytes: u64,
    pub reserve_violation: bool,
    pub physical_budget_status: PhysicalBudgetStatus,
    pub checkpoint: WalCheckpointStatus,
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
    free_bytes: Option<u64>,
    oldest_retained_at: Option<String>,
    oldest_retained_at_complete: bool,
    last_prune_at: Option<String>,
    last_deleted_events: u64,
    last_deleted_jobs: u64,
}

#[derive(Debug, Default, Clone, Copy)]
struct PruneCounts {
    deleted_events: u64,
    deleted_jobs: u64,
    deleted_transitions: u64,
}

impl PruneCounts {
    fn add(&mut self, other: Self) {
        self.deleted_events = self.deleted_events.saturating_add(other.deleted_events);
        self.deleted_jobs = self.deleted_jobs.saturating_add(other.deleted_jobs);
        self.deleted_transitions = self
            .deleted_transitions
            .saturating_add(other.deleted_transitions);
    }
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
const EXACT_EVENT_OWNERSHIP: &str = "(
    events.transition_id IS NULL OR EXISTS (
        SELECT 1 FROM job_transitions t
        WHERE t.id = events.transition_id
          AND t.instance_slug = events.instance_slug
          AND t.job_uid = events.subject
    )
) AND (
    events.reconciliation_id IS NULL OR EXISTS (
        SELECT 1 FROM reconciliations r
        WHERE r.id = events.reconciliation_id
          AND r.instance_slug = events.instance_slug
    )
)";

fn retention_budget_error(detail: impl Into<String>) -> StoreError {
    StoreError::new(ExitClass::Operation, "store.retention.budget").with_remediation(detail)
}

fn retention_lease_error(reason: &str, detail: impl Into<String>) -> StoreError {
    StoreError::new(ExitClass::Usage, reason).with_remediation(detail)
}

fn validate_retention_lease_owner(owner: &str) -> StoreResult<()> {
    if !(MIN_RETENTION_LEASE_OWNER_BYTES..=MAX_RETENTION_LEASE_OWNER_BYTES).contains(&owner.len())
        || !owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(retention_lease_error(
            "store.retention.lease.owner",
            format!(
                "retention lease owner must be {MIN_RETENTION_LEASE_OWNER_BYTES}..={MAX_RETENTION_LEASE_OWNER_BYTES} ASCII bytes using only letters, digits, '-', '_', or '.'"
            ),
        ));
    }
    Ok(())
}

fn validate_retention_lease_duration(lease_duration: Duration) -> StoreResult<u64> {
    if lease_duration.is_zero()
        || lease_duration.subsec_nanos() != 0
        || lease_duration > MAX_RETENTION_LEASE_DURATION
    {
        return Err(retention_lease_error(
            "store.retention.lease.duration",
            format!(
                "retention lease TTL must be a whole-second duration in 1..={:?}",
                MAX_RETENTION_LEASE_DURATION
            ),
        ));
    }
    Ok(lease_duration.as_secs())
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

fn retention_deadline_error() -> StoreError {
    StoreError::new(ExitClass::Timeout, "store.retention.deadline").with_remediation(
        "retention maintenance exceeded its bounded execution deadline; retry in a later pass",
    )
}

fn require_retention_deadline(deadline: Instant) -> StoreResult<()> {
    if Instant::now() >= deadline {
        Err(retention_deadline_error())
    } else {
        Ok(())
    }
}

fn retention_sql_error(error: rusqlite::Error) -> StoreError {
    if matches!(
        &error,
        rusqlite::Error::SqliteFailure(inner, _)
            if inner.code == rusqlite::ErrorCode::OperationInterrupted
    ) {
        retention_deadline_error()
    } else {
        StoreError::from(error)
    }
}

/// Interrupt one SQLite connection at the absolute retention deadline. This
/// covers pager work such as WAL checkpointing that does not invoke a VDBE
/// progress callback. The sender is closed before joining, so completed
/// operations do not leave timer threads behind.
struct SqliteDeadlineInterrupt {
    cancel: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl SqliteDeadlineInterrupt {
    fn start(connection: &rusqlite::Connection, deadline: Instant) -> StoreResult<Self> {
        let handle = connection.get_interrupt_handle();
        let (cancel, receiver) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("velnor-retention-deadline".to_owned())
            .spawn(move || {
                if receiver
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .is_err()
                {
                    handle.interrupt();
                }
            })
            .map_err(|error| {
                StoreError::new(ExitClass::Unavailable, "store.retention.deadline.timer")
                    .with_remediation(format!(
                        "start the retention deadline timer before SQLite maintenance: {error}"
                    ))
            })?;
        Ok(Self {
            cancel: Some(cancel),
            worker: Some(worker),
        })
    }
}

impl Drop for SqliteDeadlineInterrupt {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn map_retention_interruption(error: StoreError) -> StoreError {
    if error.envelope.reason == "store.sqlite.interrupted" {
        retention_deadline_error()
    } else {
        error
    }
}

impl Store {
    /// Try to own retention maintenance at a supplied clock value. A live
    /// foreign owner is a normal non-admission result. A successful update
    /// returns an opaque capability fenced by a monotonically increasing
    /// generation.
    pub(crate) fn try_acquire_retention_lease_at(
        &self,
        owner: &str,
        now_unix: u64,
        lease_duration: Duration,
    ) -> StoreResult<Option<RetentionLease>> {
        validate_retention_lease_owner(owner)?;
        let lease_seconds = validate_retention_lease_duration(lease_duration)?;
        let now = i64::try_from(now_unix).map_err(|_| {
            retention_lease_error(
                "store.retention.lease.clock",
                "retention lease clock value exceeds SQLite integer range",
            )
        })?;
        let expires_at = now_unix
            .checked_add(lease_seconds)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| {
                retention_lease_error(
                    "store.retention.lease.clock",
                    "retention lease expiry exceeds SQLite integer range",
                )
            })?;
        let mut conn = self.open_maintenance_connection()?;
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<(Option<String>, Option<i64>, i64)> = transaction
            .query_row(
                "SELECT owner, expires_at, generation
                 FROM retention_lease WHERE singleton = 0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((held_owner, held_expires_at, generation)) = row else {
            return Err(StoreError::new(
                ExitClass::Unavailable,
                "store.retention.lease.missing",
            ));
        };
        if generation < 0 {
            return Err(retention_lease_error(
                "store.retention.lease.generation",
                "retention lease generation must be a nonnegative SQLite integer",
            ));
        }
        let live_owner = held_owner.as_deref().is_some_and(|held| {
            !held.is_empty() && held_expires_at.is_some_and(|expires| expires > now)
        });
        if live_owner {
            transaction.commit()?;
            return Ok(None);
        }
        let generation = u64::try_from(generation)
            .ok()
            .and_then(|value| value.checked_add(1))
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| {
                retention_lease_error(
                    "store.retention.lease.generation",
                    "retention lease generation exhausted; refusing to wrap or saturate",
                )
            })?;
        let changed = transaction.execute(
            "UPDATE retention_lease
             SET owner = ?1, expires_at = ?2, generation = ?3
             WHERE singleton = 0
               AND owner IS ?4
               AND expires_at IS ?5
               AND generation = ?6",
            params![
                owner,
                expires_at,
                generation,
                held_owner,
                held_expires_at,
                generation - 1
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::new(
                ExitClass::Conflict,
                "store.retention.lease.lost",
            ));
        }
        transaction.commit()?;
        Ok(Some(RetentionLease {
            owner: owner.to_owned(),
            generation: u64::try_from(generation).map_err(|_| {
                retention_lease_error(
                    "store.retention.lease.generation",
                    "retention lease generation is outside the capability range",
                )
            })?,
        }))
    }

    /// Release only the exact owner and generation represented by `lease`.
    pub fn release_retention_lease(&self, lease: &RetentionLease) -> StoreResult<bool> {
        let mut conn = self.open_maintenance_connection()?;
        release_retention_lease_connection(&mut conn, lease)
    }

    /// Renew a capability without changing its fencing generation. A stale or
    /// expired capability returns `false` and cannot extend a successor.
    pub(crate) fn renew_retention_lease_at(
        &self,
        lease: &RetentionLease,
        now_unix: u64,
        lease_duration: Duration,
    ) -> StoreResult<bool> {
        let lease_seconds = validate_retention_lease_duration(lease_duration)?;
        let now = i64::try_from(now_unix).map_err(|_| {
            retention_lease_error(
                "store.retention.lease.clock",
                "retention lease clock value exceeds SQLite integer range",
            )
        })?;
        let expires_at = now_unix
            .checked_add(lease_seconds)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| {
                retention_lease_error(
                    "store.retention.lease.clock",
                    "retention lease expiry exceeds SQLite integer range",
                )
            })?;
        let generation = i64::try_from(lease.generation).map_err(|_| {
            retention_lease_error(
                "store.retention.lease.generation",
                "retention lease generation exceeds SQLite integer range",
            )
        })?;
        let mut conn = self.open_maintenance_connection()?;
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE retention_lease
             SET expires_at = ?1
             WHERE singleton = 0 AND owner = ?2 AND generation = ?3
               AND expires_at > ?4",
            params![expires_at, lease.owner, generation, now],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    fn renew_retention_lease(&self, lease: &RetentionLease) -> StoreResult<()> {
        let now = Timestamp::now().as_offset_datetime().unix_timestamp();
        let now = u64::try_from(now).map_err(|_| {
            retention_lease_error(
                "store.retention.lease.clock",
                "retention lease clock is before the Unix epoch",
            )
        })?;
        if self.renew_retention_lease_at(lease, now, MAX_RETENTION_LEASE_DURATION)? {
            Ok(())
        } else {
            Err(
                StoreError::new(ExitClass::Conflict, "store.retention.lease.lost")
                    .with_remediation(
                        "retention ownership expired or was fenced by a newer generation",
                    ),
            )
        }
    }

    /// Try to own retention maintenance using the current Unix-second clock.
    pub fn try_acquire_retention_lease(
        &self,
        owner: &str,
        lease_duration: Duration,
    ) -> StoreResult<Option<RetentionLease>> {
        let now = Timestamp::now().as_offset_datetime().unix_timestamp();
        let now = u64::try_from(now).map_err(|_| {
            retention_lease_error(
                "store.retention.lease.clock",
                "retention lease clock is before the Unix epoch",
            )
        })?;
        self.try_acquire_retention_lease_at(owner, now, lease_duration)
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
        self.prune_history_outcome(budget)
            .map_err(PruneFailure::into_store_error)
    }

    /// Run pruning while preserving whether an error happened before or after
    /// the durable deletion transaction committed. Callers must not retry a
    /// post-commit failure as if the deletion had rolled back.
    pub fn prune_history_outcome(
        &self,
        budget: &RetentionBudget,
    ) -> Result<PruneReport, PruneFailure> {
        let lease = self
            .acquire_private_retention_lease()
            .map_err(PruneFailure::PreCommit)?;
        let result = self.prune_history_outcome_with_lease(budget, &lease);
        let release = self.release_retention_lease_final(&lease);
        match (result, release) {
            (Ok(report), Ok(())) => Ok(report),
            (Ok(_), Err(error)) => Err(PruneFailure::PostCommit(error)),
            (Err(failure), Ok(())) => Err(failure),
            // A pre-commit failure means the deletion transaction rolled back.
            // Lease finalization failure cannot change that fact into a
            // post-commit result; preserving the original phase keeps retry
            // admission safe.
            (Err(failure), Err(_release_error)) => Err(failure),
        }
    }

    /// Run pruning with an acquired lease capability. The capability is
    /// checked before the transaction, before each bounded batch, and before
    /// every maintenance action.
    pub fn prune_history_outcome_with_lease(
        &self,
        budget: &RetentionBudget,
        lease: &RetentionLease,
    ) -> Result<PruneReport, PruneFailure> {
        self.prune_history_outcome_inner(budget, None, lease)
    }

    /// Run only the bounded physical maintenance phase under an already
    /// acquired lease. This is intentionally separate from job persistence:
    /// callers may schedule it independently and a deferred/failed
    /// checkpoint never makes durable deletion look uncommitted.
    pub fn run_bounded_maintenance_with_lease(
        &self,
        budget: &RetentionMaintenanceBudget,
        lease: &RetentionLease,
    ) -> StoreResult<RetentionMaintenanceReport> {
        let deadline = Instant::now() + MAX_RETENTION_PASS_DURATION;
        self.maintain_after_prune(budget, lease, deadline)
    }

    #[cfg(test)]
    pub(crate) fn prune_history_inner(
        &self,
        budget: &RetentionBudget,
        hook: Option<&dyn Fn(PrunePhase) -> StoreResult<()>>,
    ) -> StoreResult<PruneReport> {
        let lease = self
            .acquire_private_retention_lease()
            .map_err(PruneFailure::PreCommit)
            .map_err(PruneFailure::into_store_error)?;
        let result = self.prune_history_outcome_inner(budget, hook, &lease);
        let release = self.release_retention_lease_final(&lease);
        let result = match (result, release) {
            (Ok(report), Ok(())) => Ok(report),
            (Ok(_), Err(error)) => Err(PruneFailure::PostCommit(error)),
            (Err(failure), Ok(())) => Err(failure),
            // The test-only direct seam has the same rollback invariant as
            // the public path: failed/no-deletion is never post-commit merely
            // because lease cleanup also failed.
            (Err(failure), Err(_release_error)) => Err(failure),
        };
        result.map_err(PruneFailure::into_store_error)
    }

    fn acquire_private_retention_lease(&self) -> StoreResult<RetentionLease> {
        let owner = format!("velnor-store-{}", Uuid::new_v4().simple());
        self.try_acquire_retention_lease(&owner, MAX_RETENTION_LEASE_DURATION)?
            .ok_or_else(|| {
                retention_lease_error(
                    "store.retention.lease.busy",
                    "retention maintenance is owned by another daemon; retry later",
                )
            })
    }

    fn require_retention_lease(&self, lease: &RetentionLease) -> StoreResult<()> {
        let connection = self.open_maintenance_connection()?;
        Self::require_retention_lease_connection(&connection, lease)
    }

    fn require_retention_lease_connection(
        conn: &rusqlite::Connection,
        lease: &RetentionLease,
    ) -> StoreResult<()> {
        let generation = i64::try_from(lease.generation).map_err(|_| {
            retention_lease_error(
                "store.retention.lease.generation",
                "retention lease generation exceeds SQLite integer range",
            )
        })?;
        let now = Timestamp::now().as_offset_datetime().unix_timestamp();
        let live: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM retention_lease
                 WHERE singleton = 0 AND owner = ?1 AND generation = ?2
                   AND expires_at > ?3
             )",
            params![lease.owner, generation, now],
            |row| row.get(0),
        )?;
        if !live {
            return Err(
                StoreError::new(ExitClass::Conflict, "store.retention.lease.lost")
                    .with_remediation(
                        "retention ownership expired or was fenced by a newer generation",
                    ),
            );
        }
        Ok(())
    }

    /// Extend the exact capability inside the deletion transaction immediately
    /// before commit. This is the commit fence: if a lease expired while the
    /// transaction was doing work, this conditional update affects zero rows,
    /// the caller returns without committing, and SQLite rolls back all
    /// deletion work. A successful update moves expiry beyond the commit so a
    /// wall-clock tick after this fence cannot invalidate the commit.
    fn fence_retention_lease_for_commit(
        transaction: &rusqlite::Transaction<'_>,
        lease: &RetentionLease,
    ) -> StoreResult<()> {
        let now = Timestamp::now().as_offset_datetime().unix_timestamp();
        let expires_at = now
            .checked_add(
                i64::try_from(MAX_RETENTION_LEASE_DURATION.as_secs()).map_err(|_| {
                    retention_lease_error(
                        "store.retention.lease.clock",
                        "retention lease duration exceeds SQLite integer range",
                    )
                })?,
            )
            .ok_or_else(|| {
                retention_lease_error(
                    "store.retention.lease.clock",
                    "retention lease expiry exceeds SQLite integer range",
                )
            })?;
        let generation = i64::try_from(lease.generation).map_err(|_| {
            retention_lease_error(
                "store.retention.lease.generation",
                "retention lease generation exceeds SQLite integer range",
            )
        })?;
        let changed = transaction.execute(
            "UPDATE retention_lease
             SET expires_at = ?1
             WHERE singleton = 0 AND owner = ?2 AND generation = ?3
               AND expires_at > ?4",
            params![expires_at, lease.owner, generation, now],
        )?;
        if changed != 1 {
            return Err(
                StoreError::new(ExitClass::Conflict, "store.retention.lease.lost")
                    .with_remediation(
                        "retention lease expired or was fenced before the deletion commit",
                    ),
            );
        }
        Ok(())
    }

    fn prune_history_outcome_inner(
        &self,
        budget: &RetentionBudget,
        hook: Option<&dyn Fn(PrunePhase) -> StoreResult<()>>,
        lease: &RetentionLease,
    ) -> Result<PruneReport, PruneFailure> {
        validate_budget(budget).map_err(PruneFailure::PreCommit)?;
        self.require_retention_lease(lease)
            .map_err(|error| PruneFailure::LeaseLost {
                committed: false,
                error,
            })?;
        let now = Timestamp::now();
        let deadline = Instant::now() + MAX_RETENTION_PASS_DURATION;
        let mut totals = PruneCounts::default();
        let mut committed = false;

        if budget.max_event_age.is_some() {
            for _ in 0..prune_pass_batch_limit(budget) {
                let batch = self.execute_prune_batch(
                    lease,
                    now,
                    deadline,
                    committed,
                    &totals,
                    |transaction| Self::prune_expired_event_batch(transaction, budget, now, lease),
                )?;
                let had_work = batch.deleted_events > 0;
                totals.add(batch);
                committed = true;
                if !had_work {
                    break;
                }
            }
        }
        if budget.max_event_rows > 0 {
            for _ in 0..prune_pass_batch_limit(budget) {
                let batch = self.execute_prune_batch(
                    lease,
                    now,
                    deadline,
                    committed,
                    &totals,
                    |transaction| Self::prune_event_row_batch(transaction, budget, now, lease),
                )?;
                let had_work = batch.deleted_events > 0;
                totals.add(batch);
                committed = true;
                if !had_work {
                    break;
                }
            }
        }
        if !committed {
            let batch =
                self.execute_prune_batch(lease, now, deadline, false, &totals, |_transaction| {
                    Ok(PruneCounts::default())
                })?;
            totals.add(batch);
            committed = true;
        }
        if let Some(hook) = hook {
            hook(PrunePhase::AfterEventPrune).map_err(PruneFailure::PostCommit)?;
        }

        if budget.max_terminal_job_age.is_some() {
            for _ in 0..prune_pass_batch_limit(budget) {
                let batch = self.execute_prune_batch(
                    lease,
                    now,
                    deadline,
                    committed,
                    &totals,
                    |transaction| {
                        Self::prune_terminal_job_age_batch(transaction, budget, now, lease)
                    },
                )?;
                let had_work = batch.deleted_jobs > 0
                    || batch.deleted_events > 0
                    || batch.deleted_transitions > 0;
                totals.add(batch);
                committed = true;
                if !had_work {
                    break;
                }
            }
        }
        if budget.max_terminal_job_rows > 0 {
            for _ in 0..prune_pass_batch_limit(budget) {
                let batch = self.execute_prune_batch(
                    lease,
                    now,
                    deadline,
                    committed,
                    &totals,
                    |transaction| {
                        Self::prune_terminal_job_row_batch(transaction, budget, now, lease)
                    },
                )?;
                let had_work = batch.deleted_jobs > 0
                    || batch.deleted_events > 0
                    || batch.deleted_transitions > 0;
                totals.add(batch);
                committed = true;
                if !had_work {
                    break;
                }
            }
        }
        if budget.max_database_bytes > 0 {
            for _ in 0..prune_pass_batch_limit(budget) {
                let batch = self.execute_prune_batch(
                    lease,
                    now,
                    deadline,
                    committed,
                    &totals,
                    |transaction| {
                        Self::prune_byte_pressure_batch(transaction, &self.path, budget, now, lease)
                    },
                )?;
                let had_work = batch.deleted_jobs > 0
                    || batch.deleted_events > 0
                    || batch.deleted_transitions > 0;
                totals.add(batch);
                committed = true;
                if !had_work {
                    break;
                }
            }
        }
        if let Some(hook) = hook {
            hook(PrunePhase::AfterJobPrune).map_err(PruneFailure::PostCommit)?;
        }

        // Physical maintenance is a separate bounded phase. It still runs in
        // the retention worker, never in the job completion transaction.
        let maintenance = self
            .maintain_after_prune(&RetentionMaintenanceBudget::from(budget), lease, deadline)
            .map_err(|error| {
                if error.envelope.reason == "store.retention.lease.lost" {
                    PruneFailure::LeaseLost {
                        committed: true,
                        error,
                    }
                } else {
                    PruneFailure::PostCommit(error)
                }
            })?;
        let snapshot = self
            .read_snapshot_for_maintenance(deadline)
            .map_err(PruneFailure::PostCommit)?;

        Ok(PruneReport {
            deleted_events: totals.deleted_events,
            deleted_jobs: totals.deleted_jobs,
            deleted_transitions: totals.deleted_transitions,
            database_bytes: maintenance.database_bytes_after,
            wal_bytes: maintenance.wal_bytes_after,
            total_bytes: maintenance.total_bytes_after,
            free_bytes: maintenance.free_bytes_after,
            reserve_bytes: maintenance.reserve_bytes,
            reserve_violation: maintenance.reserve_violation,
            physical_budget_status: maintenance.physical_budget_status,
            checkpoint: maintenance.checkpoint,
            vacuum_pages_reclaimed: maintenance.vacuum_pages_reclaimed,
            oldest_retained_at: snapshot.oldest_retained_at,
            oldest_retained_at_complete: snapshot.oldest_retained_at_complete,
            maintenance_deferred: maintenance.maintenance_deferred,
            maintenance_reason: maintenance.maintenance_reason,
        })
    }

    fn execute_prune_batch<F>(
        &self,
        lease: &RetentionLease,
        now: Timestamp,
        deadline: Instant,
        committed_before: bool,
        totals: &PruneCounts,
        operation: F,
    ) -> Result<PruneCounts, PruneFailure>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> StoreResult<PruneCounts>,
    {
        require_retention_deadline(deadline).map_err(|error| {
            if committed_before {
                PruneFailure::PostCommit(error)
            } else {
                PruneFailure::PreCommit(error)
            }
        })?;
        self.renew_retention_lease(lease)
            .map_err(|error| PruneFailure::LeaseLost {
                committed: committed_before,
                error,
            })?;
        require_retention_deadline(deadline).map_err(|error| {
            if committed_before {
                PruneFailure::PostCommit(error)
            } else {
                PruneFailure::PreCommit(error)
            }
        })?;
        let mut conn = self.lock_conn_until(deadline).map_err(|error| {
            if committed_before {
                PruneFailure::PostCommit(error)
            } else {
                PruneFailure::PreCommit(error)
            }
        })?;
        // Retention must never restart a multi-second SQLite busy wait for
        // every statement. Lock contention fails fast; the absolute deadline
        // still bounds the Rust-side mutex and explicit operation checks.
        conn.busy_timeout(Duration::ZERO).map_err(|error| {
            let error = StoreError::from(error);
            if committed_before {
                PruneFailure::PostCommit(error)
            } else {
                PruneFailure::PreCommit(error)
            }
        })?;
        if let Err(error) = conn.progress_handler(1_000, Some(move || Instant::now() >= deadline)) {
            let _ = conn.busy_timeout(super::BUSY_TIMEOUT);
            let error = StoreError::from(error);
            return Err(if committed_before {
                PruneFailure::PostCommit(error)
            } else {
                PruneFailure::PreCommit(error)
            });
        }
        let transaction_result = (|| {
            let transaction = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|error| {
                    let error = retention_sql_error(error);
                    if committed_before {
                        PruneFailure::PostCommit(error)
                    } else {
                        PruneFailure::PreCommit(error)
                    }
                })?;
            require_retention_deadline(deadline).map_err(|error| {
                if committed_before {
                    PruneFailure::PostCommit(error)
                } else {
                    PruneFailure::PreCommit(error)
                }
            })?;
            Self::require_retention_lease_connection(&transaction, lease).map_err(|error| {
                let error = map_retention_interruption(error);
                if error.envelope.reason == "store.retention.deadline" {
                    return if committed_before {
                        PruneFailure::PostCommit(error)
                    } else {
                        PruneFailure::PreCommit(error)
                    };
                }
                PruneFailure::LeaseLost {
                    committed: committed_before,
                    error,
                }
            })?;
            let batch = operation(&transaction).map_err(|error| {
                let error = map_retention_interruption(error);
                if error.envelope.reason == "store.retention.lease.lost" {
                    PruneFailure::LeaseLost {
                        committed: committed_before,
                        error,
                    }
                } else if committed_before {
                    PruneFailure::PostCommit(error)
                } else {
                    PruneFailure::PreCommit(error)
                }
            })?;
            require_retention_deadline(deadline).map_err(|error| {
                if committed_before {
                    PruneFailure::PostCommit(error)
                } else {
                    PruneFailure::PreCommit(error)
                }
            })?;
            Self::fence_retention_lease_for_commit(&transaction, lease).map_err(|error| {
                let error = map_retention_interruption(error);
                if error.envelope.reason == "store.retention.deadline" {
                    return if committed_before {
                        PruneFailure::PostCommit(error)
                    } else {
                        PruneFailure::PreCommit(error)
                    };
                }
                PruneFailure::LeaseLost {
                    committed: committed_before,
                    error,
                }
            })?;
            let cumulative_events = totals.deleted_events.saturating_add(batch.deleted_events);
            let cumulative_jobs = totals.deleted_jobs.saturating_add(batch.deleted_jobs);
            transaction
                .execute(
                    "UPDATE retention_state
                 SET last_prune_at = ?1, deleted_events = ?2, deleted_jobs = ?3
                 WHERE singleton = 0",
                    params![
                        rfc3339(now),
                        i64::try_from(cumulative_events).unwrap_or(i64::MAX),
                        i64::try_from(cumulative_jobs).unwrap_or(i64::MAX),
                    ],
                )
                .map_err(|error| {
                    let error = retention_sql_error(error);
                    if committed_before {
                        PruneFailure::PostCommit(error)
                    } else {
                        PruneFailure::PreCommit(error)
                    }
                })?;
            require_retention_deadline(deadline).map_err(|error| {
                if committed_before {
                    PruneFailure::PostCommit(error)
                } else {
                    PruneFailure::PreCommit(error)
                }
            })?;
            transaction.commit().map_err(|error| {
                let error = retention_sql_error(error);
                // SQLite commit errors have an ambiguous outcome. Treat the
                // batch as post-commit/unknown so callers never repeat a
                // deletion that may already be durable.
                PruneFailure::PostCommit(error)
            })?;
            Ok(batch)
        })();
        let clear_progress = conn.progress_handler(0, None::<fn() -> bool>);
        let restore = conn.busy_timeout(super::BUSY_TIMEOUT).map_err(|error| {
            let error = StoreError::from(error);
            PruneFailure::PostCommit(error)
        });
        match (transaction_result, clear_progress, restore) {
            // The transaction result owns the commit phase. Cleanup errors
            // are deliberately not allowed to relabel a pre-commit or lease
            // failure; both cleanup attempts have already run, and the next
            // use re-establishes the bounded connection settings.
            (Err(error), _, _) => Err(error),
            (Ok(_), Err(error), _) => Err(PruneFailure::PostCommit(StoreError::from(error))),
            (Ok(_), Ok(()), Err(error)) => Err(error),
            (Ok(batch), Ok(()), Ok(())) => Ok(batch),
        }
    }

    fn read_snapshot_for_maintenance(&self, deadline: Instant) -> StoreResult<RetentionSnapshot> {
        require_retention_deadline(deadline)?;
        let mut connection = self
            .open_maintenance_connection()
            .map_err(|error| maintenance_error("open accounting connection", error))?;
        let _deadline_interrupt = SqliteDeadlineInterrupt::start(&connection, deadline)?;
        let result = read_retention_snapshot(&mut connection, &self.path);
        require_retention_deadline(deadline)?;
        result.map_err(map_retention_interruption)
    }

    fn maintain_after_prune(
        &self,
        budget: &RetentionMaintenanceBudget,
        lease: &RetentionLease,
        deadline: Instant,
    ) -> StoreResult<RetentionMaintenanceReport> {
        self.maintain_after_prune_inner(budget, lease, deadline)
            .map_err(map_retention_interruption)
    }

    fn maintain_after_prune_inner(
        &self,
        budget: &RetentionMaintenanceBudget,
        lease: &RetentionLease,
        deadline: Instant,
    ) -> StoreResult<RetentionMaintenanceReport> {
        require_retention_deadline(deadline)?;
        self.renew_retention_lease(lease)?;
        require_retention_deadline(deadline)?;
        let maintenance = self
            .open_maintenance_connection()
            .map_err(|error| maintenance_error_preserving("open", error))?;
        let _deadline_interrupt = SqliteDeadlineInterrupt::start(&maintenance, deadline)?;
        require_retention_deadline(deadline)?;
        Self::require_retention_lease_connection(&maintenance, lease)?;
        let (database_bytes_before, wal_bytes_before) =
            Self::total_database_bytes(&maintenance, &self.path).map_err(|error| {
                maintenance_error_preserving("database accounting before", error)
            })?;
        require_retention_deadline(deadline)?;
        let total_bytes_before = database_bytes_before.saturating_add(wal_bytes_before);
        let free_bytes_before = filesystem_free_bytes(&self.path);
        let reserve_violation_before =
            free_bytes_before.is_some_and(|free_bytes| free_bytes < budget.min_free_bytes);

        let mut checkpoint = WalCheckpointStatus::not_attempted();
        let mut vacuum_pages_attempted = 0;
        let mut vacuum_pages_reclaimed = 0;
        let mut deferred_reason = None;

        if reserve_violation_before {
            deferred_reason = Some(
                "filesystem reserve is already violated; maintenance deferred to avoid consuming emergency space"
                    .to_owned(),
            );
        } else if free_bytes_before.is_none() {
            deferred_reason = Some(
                "filesystem free-space accounting is unavailable; physical enforcement is unmeasurable"
                    .to_owned(),
            );
        } else {
            let attempts = budget.max_checkpoint_attempts.min(1);
            for _ in 0..attempts {
                require_retention_deadline(deadline)?;
                Self::require_retention_lease_connection(&maintenance, lease)?;
                checkpoint = Self::passive_checkpoint(&maintenance).map_err(|error| {
                    maintenance_error_preserving("PASSIVE WAL checkpoint", error)
                })?;
                require_retention_deadline(deadline)?;
            }
            if !checkpoint.complete() {
                deferred_reason = Some(format!(
                    "PASSIVE checkpoint incomplete: busy_frames={} log_frames={} checkpointed_frames={}",
                    checkpoint.busy_frames,
                    checkpoint.log_frames,
                    checkpoint.checkpointed_frames
                ));
            } else {
                let auto_vacuum: i64 = maintenance
                    .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
                    .map_err(|error| {
                        maintenance_error_preserving("read auto-vacuum mode", error)
                    })?;
                let vacuum_pages = budget.max_vacuum_pages.min(MAX_MAINTENANCE_VACUUM_PAGES);
                if budget.max_database_bytes > 0
                    && total_bytes_before > budget.max_database_bytes
                    && auto_vacuum == 2
                    && vacuum_pages > 0
                {
                    require_retention_deadline(deadline)?;
                    Self::require_retention_lease_connection(&maintenance, lease)?;
                    let before: i64 = maintenance
                        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
                        .map_err(|error| maintenance_error_preserving("read free pages", error))?;
                    vacuum_pages_attempted = vacuum_pages;
                    maintenance
                        .execute_batch(&format!("PRAGMA incremental_vacuum({});", vacuum_pages))
                        .map_err(|error| {
                            maintenance_error_preserving("incremental vacuum", error)
                        })?;
                    require_retention_deadline(deadline)?;
                    let after: i64 = maintenance
                        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
                        .map_err(|error| {
                            maintenance_error_preserving("read reclaimed pages", error)
                        })?;
                    vacuum_pages_reclaimed =
                        before.saturating_sub(after).max(0).try_into().unwrap_or(0);
                }
            }
        }

        require_retention_deadline(deadline)?;
        Self::require_retention_lease_connection(&maintenance, lease)?;
        let (database_bytes_after, wal_bytes_after) =
            Self::total_database_bytes(&maintenance, &self.path).map_err(|error| {
                maintenance_error_preserving("database accounting after", error)
            })?;
        let total_bytes_after = database_bytes_after.saturating_add(wal_bytes_after);
        let free_bytes_after = filesystem_free_bytes(&self.path);
        let reserve_violation =
            free_bytes_after.is_some_and(|free_bytes| free_bytes < budget.min_free_bytes);
        let physical_budget_status = if budget.max_database_bytes == 0 {
            PhysicalBudgetStatus::Disabled
        } else if free_bytes_after.is_none() || free_bytes_before.is_none() {
            PhysicalBudgetStatus::Unmeasurable
        } else if reserve_violation {
            PhysicalBudgetStatus::ReserveViolation
        } else if deferred_reason.is_some() {
            PhysicalBudgetStatus::Deferred
        } else if total_bytes_after > budget.max_database_bytes {
            PhysicalBudgetStatus::Exceeded
        } else {
            PhysicalBudgetStatus::WithinBudget
        };
        let maintenance_deferred = !matches!(
            physical_budget_status,
            PhysicalBudgetStatus::Disabled | PhysicalBudgetStatus::WithinBudget
        );
        if deferred_reason.is_none() && physical_budget_status == PhysicalBudgetStatus::Exceeded {
            deferred_reason = Some(format!(
                "bounded maintenance completed but physical budget remains exceeded: total_bytes={} limit={}",
                total_bytes_after, budget.max_database_bytes
            ));
        }
        require_retention_deadline(deadline)?;
        Ok(RetentionMaintenanceReport {
            database_bytes_before,
            wal_bytes_before,
            total_bytes_before,
            database_bytes_after,
            wal_bytes_after,
            total_bytes_after,
            free_bytes_before,
            free_bytes_after,
            reserve_bytes: budget.min_free_bytes,
            reserve_violation,
            physical_budget_status,
            checkpoint,
            vacuum_pages_attempted,
            vacuum_pages_reclaimed,
            maintenance_deferred,
            maintenance_reason: deferred_reason,
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
            free_bytes: snapshot.free_bytes,
            reserve_bytes: DEFAULT_RETENTION_RESERVE_BYTES,
            reserve_violation: snapshot
                .free_bytes
                .is_some_and(|free| free < DEFAULT_RETENTION_RESERVE_BYTES),
            physical_budget_status: PhysicalBudgetStatus::Unconfigured,
            checkpoint: WalCheckpointStatus::not_attempted(),
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

    /// Run SQLite's non-blocking checkpoint primitive and preserve all three
    /// status counters. PASSIVE is the only checkpoint mode permitted here:
    /// it never waits for readers and never truncates a live WAL from a job
    /// path.
    fn passive_checkpoint(conn: &rusqlite::Connection) -> StoreResult<WalCheckpointStatus> {
        passive_checkpoint(conn)
    }

    /// One event batch. Age and row-cap phases are deliberately separate so
    /// each phase commits before the next starts.
    fn prune_expired_event_batch(
        conn: &rusqlite::Transaction<'_>,
        budget: &RetentionBudget,
        now: Timestamp,
        lease: &RetentionLease,
    ) -> StoreResult<PruneCounts> {
        let cutoff = budget.max_event_age.map(|age| minus_age(now, age));
        if let Some(cutoff) = cutoff {
            reject_malformed_timestamp_rows(conn, "events", "occurred_at")?;
            let ids = select_expired_event_ids(conn, cutoff, prune_batch_size(budget))?;
            Self::require_retention_lease_connection(conn, lease)?;
            return Ok(PruneCounts {
                deleted_events: delete_events_by_ids(conn, &ids, now)?,
                ..PruneCounts::default()
            });
        }
        Ok(PruneCounts::default())
    }

    fn prune_event_row_batch(
        conn: &rusqlite::Transaction<'_>,
        budget: &RetentionBudget,
        now: Timestamp,
        lease: &RetentionLease,
    ) -> StoreResult<PruneCounts> {
        let keep_from_id = newest_window_start(conn, "events", None, budget.max_event_rows, 1)?;
        let Some(keep_from_id) = keep_from_id.filter(|id| *id > 1) else {
            return Ok(PruneCounts::default());
        };
        Self::require_retention_lease_connection(conn, lease)?;
        let ids = protected_ids_below(conn, "events", keep_from_id, prune_batch_size(budget))?;
        Ok(PruneCounts {
            deleted_events: delete_events_by_ids(conn, &ids, now)?,
            ..PruneCounts::default()
        })
    }

    fn prune_terminal_job_age_batch(
        conn: &rusqlite::Transaction<'_>,
        budget: &RetentionBudget,
        now: Timestamp,
        lease: &RetentionLease,
    ) -> StoreResult<PruneCounts> {
        let Some(max_age) = budget.max_terminal_job_age else {
            return Ok(PruneCounts::default());
        };
        let cutoff = minus_age(now, max_age);
        reject_malformed_timestamp_rows(conn, "jobs", "updated_at")?;
        let ids = select_expired_terminal_job_ids(conn, cutoff)?;
        let Some(id) = ids.first().copied() else {
            return Ok(PruneCounts::default());
        };
        Self::require_retention_lease_connection(conn, lease)?;
        let (deleted_jobs, deleted_transitions, deleted_events) =
            delete_job_with_ancestry(conn, id, now)?;
        Ok(PruneCounts {
            deleted_events,
            deleted_jobs,
            deleted_transitions,
        })
    }

    fn prune_terminal_job_row_batch(
        conn: &rusqlite::Transaction<'_>,
        budget: &RetentionBudget,
        now: Timestamp,
        lease: &RetentionLease,
    ) -> StoreResult<PruneCounts> {
        let keep_from_id = newest_window_start(
            conn,
            "jobs",
            Some(&format!("phase IN {CANONICAL_TERMINAL_PHASES}")),
            budget.max_terminal_job_rows,
            1,
        )?;
        let Some(keep_from_id) = keep_from_id.filter(|id| *id > 1) else {
            return Ok(PruneCounts::default());
        };
        let id: Option<i64> = conn
            .query_row(
                &format!(
                    "SELECT id FROM jobs
                     WHERE id < ?1 AND phase IN {CANONICAL_TERMINAL_PHASES}
                       AND NOT EXISTS (
                           SELECT 1 FROM reconciliations r
                           WHERE r.instance_slug = jobs.instance_slug
                             AND r.status NOT IN {CLOSED_RECONCILIATION_STATUSES}
                       )
                     ORDER BY id ASC LIMIT 1"
                ),
                [keep_from_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(PruneCounts::default());
        };
        Self::require_retention_lease_connection(conn, lease)?;
        let (deleted_jobs, deleted_transitions, deleted_events) =
            delete_job_with_ancestry(conn, id, now)?;
        Ok(PruneCounts {
            deleted_events,
            deleted_jobs,
            deleted_transitions,
        })
    }

    /// Logical pressure pruning is intentionally truthful: it removes one
    /// bounded candidate when the measured DB+WAL bytes exceed the requested
    /// threshold, but does not claim to enforce a physical ceiling because
    /// checkpointing remains deferred.
    fn prune_byte_pressure_batch(
        conn: &rusqlite::Transaction<'_>,
        path: &Path,
        budget: &RetentionBudget,
        now: Timestamp,
        lease: &RetentionLease,
    ) -> StoreResult<PruneCounts> {
        let (database_bytes, wal_bytes) = Self::total_database_bytes(conn, path)?;
        if database_bytes.saturating_add(wal_bytes) <= budget.max_database_bytes {
            return Ok(PruneCounts::default());
        }
        let id: Option<i64> = conn
            .query_row(
                &format!(
                    "SELECT id FROM jobs
                     WHERE phase IN {CANONICAL_TERMINAL_PHASES}
                       AND NOT EXISTS (
                           SELECT 1 FROM reconciliations r
                           WHERE r.instance_slug = jobs.instance_slug
                             AND r.status NOT IN {CLOSED_RECONCILIATION_STATUSES}
                       )
                     ORDER BY id ASC LIMIT 1"
                ),
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = id {
            Self::require_retention_lease_connection(conn, lease)?;
            let (deleted_jobs, deleted_transitions, deleted_events) =
                delete_job_with_ancestry(conn, id, now)?;
            return Ok(PruneCounts {
                deleted_events,
                deleted_jobs,
                deleted_transitions,
            });
        }
        let ids = protected_ids_below(conn, "events", i64::MAX, prune_batch_size(budget))?;
        Self::require_retention_lease_connection(conn, lease)?;
        Ok(PruneCounts {
            deleted_events: delete_events_by_ids(conn, &ids, now)?,
            ..PruneCounts::default()
        })
    }
}

fn passive_checkpoint(conn: &rusqlite::Connection) -> StoreResult<WalCheckpointStatus> {
    let (busy_frames, log_frames, checkpointed_frames): (i64, i64, i64) =
        conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let to_u64 = |value: i64| {
        u64::try_from(value).map_err(|_| {
            StoreError::new(ExitClass::Operation, "store.retention.maintenance.status")
                .with_remediation("SQLite returned a negative WAL checkpoint counter")
        })
    };
    Ok(WalCheckpointStatus {
        attempted: true,
        busy_frames: to_u64(busy_frames)?,
        log_frames: to_u64(log_frames)?,
        checkpointed_frames: to_u64(checkpointed_frames)?,
    })
}

/// Return available bytes on the filesystem containing `path` without
/// invoking a shell or parsing human-formatted `df` output. The small FFI
/// buffer intentionally contains more words than every supported Unix
/// `statvfs` layout; only the POSIX prefix (block size and available blocks)
/// is read. The call writes a plain C structure into an aligned buffer and
/// does not retain any pointer after returning.
#[cfg(unix)]
fn filesystem_free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    struct StatvfsBuffer {
        words: [usize; 32],
    }

    unsafe extern "C" {
        fn statvfs(path: *const std::ffi::c_char, buffer: *mut StatvfsBuffer) -> i32;
    }

    let probe = if path.exists() {
        path
    } else {
        path.parent().filter(|parent| parent.exists())?
    };
    let path = CString::new(probe.as_os_str().as_bytes()).ok()?;
    let mut buffer = StatvfsBuffer { words: [0; 32] };
    // POSIX statvfs stores f_frsize at word 1. Linux keeps f_bavail at word
    // 4; macOS uses 32-bit block counters packed into the preceding words.
    // The oversized repr(C) buffer is aligned and large enough for the full
    // platform structure, while avoiding shell utilities and platform-
    // specific struct declarations.
    let result = unsafe { statvfs(path.as_ptr(), &mut buffer) };
    if result != 0 {
        return None;
    }
    let block_size = buffer.words[1];
    #[cfg(target_os = "macos")]
    let available_blocks = buffer.words[3] & (u32::MAX as usize);
    #[cfg(not(target_os = "macos"))]
    let available_blocks = buffer.words[4];
    block_size
        .checked_mul(available_blocks)
        .and_then(|bytes| u64::try_from(bytes).ok())
}

#[cfg(not(unix))]
fn filesystem_free_bytes(_path: &Path) -> Option<u64> {
    None
}

fn release_retention_lease_connection(
    conn: &mut rusqlite::Connection,
    lease: &RetentionLease,
) -> StoreResult<bool> {
    let generation = i64::try_from(lease.generation).map_err(|_| {
        retention_lease_error(
            "store.retention.lease.generation",
            "retention lease generation exceeds SQLite integer range",
        )
    })?;
    let changed = conn.execute(
        "UPDATE retention_lease
         SET owner = NULL, expires_at = NULL
         WHERE singleton = 0 AND owner = ?1 AND generation = ?2",
        params![lease.owner, generation],
    )?;
    Ok(changed == 1)
}

fn release_retention_lease_failure(detail: impl Into<String>) -> StoreError {
    StoreError::new(ExitClass::Operation, "store.retention.lease.finalize").with_remediation(detail)
}

impl Store {
    /// Finalize a prune-owned lease without treating a lost release as a
    /// successful pass. Only transient SQLite contention is retried, and the
    /// exact owner/generation predicate makes every retry idempotent. Any
    /// failure is surfaced as post-commit finalization failure so callers do
    /// not re-run logical deletion as if it had rolled back.
    pub fn release_retention_lease_final(&self, lease: &RetentionLease) -> StoreResult<()> {
        let mut last_error = None;
        for attempt in 0..MAX_RELEASE_RETRY_ATTEMPTS {
            match self.release_retention_lease(lease) {
                Ok(true) => return Ok(()),
                Ok(false) => {
                    return Err(release_retention_lease_failure(
                        "retention lease release matched no live owner/generation; the prune result is finalization-failed and must not be retried as pre-commit",
                    ));
                }
                Err(error)
                    if error.envelope.reason == "store.locked"
                        && attempt + 1 < MAX_RELEASE_RETRY_ATTEMPTS =>
                {
                    last_error = Some(error);
                    std::thread::sleep(RELEASE_RETRY_BACKOFF);
                }
                Err(error) => {
                    return Err(release_retention_lease_failure(format!(
                        "retention lease release failed after bounded finalization attempt; the prune result is finalization-failed and must not be retried as pre-commit (reason={})",
                        error.envelope.reason
                    )));
                }
            }
        }
        let reason = last_error
            .as_ref()
            .map_or("store.locked", |error| error.envelope.reason.as_str());
        Err(release_retention_lease_failure(format!(
            "retention lease release remained contended after {MAX_RELEASE_RETRY_ATTEMPTS} bounded attempts; the prune result is finalization-failed and must not be retried as pre-commit (reason={reason})"
        )))
    }
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
        free_bytes: filesystem_free_bytes(path),
        oldest_retained_at,
        oldest_retained_at_complete,
        last_prune_at,
        last_deleted_events: last_deleted_events.max(0) as u64,
        last_deleted_jobs: last_deleted_jobs.max(0) as u64,
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
             SELECT 1 FROM events WHERE julianday(occurred_at) IS NULL LIMIT 1
         ) OR EXISTS(
             SELECT 1 FROM job_transitions
             WHERE julianday(transition_time) IS NULL LIMIT 1
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
    let bounded_transition_ids =
        &transition_ids[..transition_ids.len().min(MAX_PRUNE_BATCH_SIZE as usize)];
    let event_ids = select_transition_event_ids(
        conn,
        &instance_slug,
        bounded_transition_ids,
        MAX_PRUNE_BATCH_SIZE.saturating_add(1),
    )?;
    let mut events = 0;
    if !event_ids.is_empty() {
        let bounded_ids = &event_ids[..event_ids.len().min(MAX_PRUNE_BATCH_SIZE as usize)];
        events = delete_events_for_instance_by_ids(conn, &instance_slug, bounded_ids, now)?;
    }
    if event_ids.len() > MAX_PRUNE_BATCH_SIZE as usize {
        return Ok((0, 0, events));
    }

    if transition_ids.len() > MAX_PRUNE_BATCH_SIZE as usize {
        let bounded_ids = &transition_ids[..MAX_PRUNE_BATCH_SIZE as usize];
        let transitions =
            delete_transition_rows_by_ids(conn, &instance_slug, &job_uid, bounded_ids)?;
        return Ok((0, transitions, events));
    }

    let transitions =
        delete_transition_rows_by_ids(conn, &instance_slug, &job_uid, &transition_ids)?;
    let jobs = conn.execute(
        "DELETE FROM jobs
         WHERE id = ?1 AND instance_slug = ?2 AND job_uid = ?3",
        params![id, instance_slug, job_uid],
    )? as u64;
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
    transition_ids: &[i64],
    limit: u64,
) -> StoreResult<Vec<i64>> {
    if transition_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; transition_ids.len()].join(",");
    let mut statement = conn.prepare_cached(&format!(
        "SELECT e.id FROM events e
             WHERE e.instance_slug = ?1
               AND e.transition_id IN ({placeholders})
               AND EXISTS (
                   SELECT 1 FROM job_transitions t
                   WHERE t.id = e.transition_id
                     AND t.instance_slug = e.instance_slug
               )
             ORDER BY e.id ASC LIMIT ?{}",
        transition_ids.len() + 2
    ))?;
    let instance = instance_slug.to_owned();
    let mut values: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(transition_ids.len() + 2);
    values.push(&instance);
    values.extend(transition_ids.iter().map(|id| id as &dyn rusqlite::ToSql));
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    values.push(&limit);
    let mut rows = statement.query(params_from_iter(values.iter()))?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next()? {
        ids.push(row.get(0)?);
    }
    Ok(ids)
}

fn delete_transition_rows_by_ids(
    conn: &rusqlite::Connection,
    instance_slug: &str,
    job_uid: &str,
    ids: &[i64],
) -> StoreResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let instance = instance_slug.to_owned();
    let job = job_uid.to_owned();
    let mut values: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 2);
    values.push(&instance);
    values.push(&job);
    values.extend(ids.iter().map(|id| id as &dyn rusqlite::ToSql));
    Ok(conn.execute(
        &format!(
            "DELETE FROM job_transitions
             WHERE instance_slug = ?1 AND job_uid = ?2
               AND id IN ({})",
            placeholders
                .split(',')
                .enumerate()
                .map(|(index, _)| format!("?{}", index + 3))
                .collect::<Vec<_>>()
                .join(",")
        ),
        params_from_iter(values.iter()),
    )? as u64)
}

fn reject_malformed_timestamp_rows(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> StoreResult<()> {
    let sql = format!(
        "SELECT 1 FROM {table}
         WHERE julianday({column}) IS NULL
         LIMIT 1"
    );
    if conn
        .query_row(&sql, [], |row| row.get::<_, i64>(0))
        .optional()?
        .is_some()
    {
        return Err(stale_timestamp_error());
    }
    Ok(())
}

fn select_expired_event_ids(
    conn: &rusqlite::Connection,
    cutoff: Timestamp,
    batch: u64,
) -> StoreResult<Vec<i64>> {
    let mut statement = conn.prepare_cached(&format!(
        "SELECT id FROM events
         WHERE julianday(occurred_at) < julianday(?1)
           AND {EXACT_EVENT_OWNERSHIP}
           AND NOT EXISTS (
               SELECT 1 FROM jobs j
               WHERE j.instance_slug = events.instance_slug
                 AND j.job_uid = events.subject
                 AND j.phase NOT IN {CANONICAL_TERMINAL_PHASES}
           )
           AND NOT EXISTS (
               SELECT 1 FROM reconciliations r
               WHERE r.instance_slug = events.instance_slug
                 AND r.status NOT IN {CLOSED_RECONCILIATION_STATUSES}
           )
         ORDER BY julianday(occurred_at) ASC, id ASC LIMIT ?2"
    ))?;
    let mut rows = statement.query(params![
        rfc3339(cutoff),
        i64::try_from(batch.clamp(MIN_EFFECTIVE_BATCH_SIZE, MAX_PRUNE_BATCH_SIZE))
            .unwrap_or(i64::MAX),
    ])?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next()? {
        ids.push(row.get(0)?);
    }
    Ok(ids)
}

fn select_expired_terminal_job_ids(
    conn: &rusqlite::Connection,
    cutoff: Timestamp,
) -> StoreResult<Vec<i64>> {
    let mut statement = conn.prepare_cached(&format!(
        "SELECT id FROM jobs
         WHERE phase IN {CANONICAL_TERMINAL_PHASES}
           AND julianday(updated_at) < julianday(?1)
           AND NOT EXISTS (
               SELECT 1 FROM reconciliations r
               WHERE r.instance_slug = jobs.instance_slug
                 AND r.status NOT IN {CLOSED_RECONCILIATION_STATUSES}
           )
         ORDER BY julianday(updated_at) ASC, id ASC LIMIT 1"
    ))?;
    let mut rows = statement.query([rfc3339(cutoff)])?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next()? {
        ids.push(row.get(0)?);
    }
    Ok(ids)
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
           AND {EXACT_EVENT_OWNERSHIP}
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
            "SELECT DISTINCT events.instance_slug FROM events
             WHERE events.id IN ({placeholders}) AND {EXACT_EVENT_OWNERSHIP}"
        ))?;
        let mut rows = statement.query(params_from_iter(ids.iter()))?;
        let mut instances = BTreeSet::new();
        while let Some(row) = rows.next()? {
            instances.insert(row.get(0)?);
        }
        instances
    };
    let deleted = conn.execute(
        &format!(
            "DELETE FROM events
             WHERE id IN ({placeholders}) AND {EXACT_EVENT_OWNERSHIP}"
        ),
        params_from_iter(ids.iter()),
    )? as u64;
    for instance_slug in instances {
        refresh_event_stream_state_for_instance(conn, &instance_slug, now)?;
    }
    Ok(deleted)
}

fn delete_events_for_instance_by_ids(
    conn: &rusqlite::Connection,
    instance_slug: &str,
    ids: &[i64],
    now: Timestamp,
) -> StoreResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders = (0..ids.len())
        .map(|index| format!("?{}", index + 2))
        .collect::<Vec<_>>()
        .join(",");
    let instance = instance_slug.to_owned();
    let mut values: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 1);
    values.push(&instance);
    values.extend(ids.iter().map(|id| id as &dyn rusqlite::ToSql));
    let deleted = conn.execute(
        &format!(
            "DELETE FROM events
             WHERE instance_slug = ?1 AND id IN ({placeholders})
               AND {EXACT_EVENT_OWNERSHIP}"
        ),
        params_from_iter(values),
    )? as u64;
    refresh_event_stream_state_for_instance(conn, instance_slug, now)?;
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

fn maintenance_error_preserving<E>(operation: &str, detail: E) -> StoreError
where
    E: Into<StoreError>,
{
    let detail = detail.into();
    if detail.envelope.reason == "store.sqlite.interrupted" {
        retention_deadline_error()
    } else {
        maintenance_error(operation, detail)
    }
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
            .try_acquire_retention_lease_at("owner-a-1", 100, Duration::from_secs(30))
            .unwrap()
            .is_some());
        assert!(!second
            .try_acquire_retention_lease_at("owner-b-1", 101, Duration::from_secs(30))
            .unwrap()
            .is_some());

        let connection = test_connection(&first);
        let lease: (Option<String>, Option<i64>) = connection
            .query_row(
                "SELECT owner, expires_at FROM retention_lease WHERE singleton = 0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(lease, (Some("owner-a-1".to_owned()), Some(130)));
    }

    #[test]
    fn retention_lease_expired_owner_is_taken_over_atomically() {
        let (_dir, first) = temp_store("lease-takeover");
        let second = Store::open(first.path()).unwrap();

        let first_lease = first
            .try_acquire_retention_lease_at("owner-a-1", 100, Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let second_lease = second
            .try_acquire_retention_lease_at("owner-b-1", 110, Duration::from_secs(20))
            .unwrap()
            .unwrap();
        assert!(second_lease.generation() > first_lease.generation());
        assert!(!first
            .renew_retention_lease_at(&first_lease, 111, Duration::from_secs(10))
            .unwrap());
        assert!(!first.release_retention_lease(&first_lease).unwrap());
        assert!(second.release_retention_lease(&second_lease).unwrap());
    }

    #[test]
    fn live_same_owner_reacquisition_does_not_fence_capability() {
        let (_dir, store) = temp_store("lease-same-owner");
        let lease = store
            .try_acquire_retention_lease_at("owner-a-1", 100, Duration::from_secs(30))
            .unwrap()
            .unwrap();
        assert!(store
            .try_acquire_retention_lease_at("owner-a-1", 101, Duration::from_secs(30))
            .unwrap()
            .is_none());
        assert!(store
            .renew_retention_lease_at(&lease, 101, Duration::from_secs(30))
            .unwrap());
        assert!(store.release_retention_lease(&lease).unwrap());
    }

    #[test]
    fn stale_capability_cannot_authorize_pruning_after_takeover() {
        let (_dir, first) = temp_store("lease-fenced-prune");
        let second = Store::open(first.path()).unwrap();
        let now = Timestamp::now()
            .as_offset_datetime()
            .unix_timestamp()
            .try_into()
            .unwrap();
        let first_lease = first
            .try_acquire_retention_lease_at("owner-a-1", now, Duration::from_secs(30))
            .unwrap()
            .unwrap();
        let second_lease = second
            .try_acquire_retention_lease_at("owner-b-1", now + 30, Duration::from_secs(30))
            .unwrap()
            .unwrap();
        let error = first
            .prune_history_outcome_with_lease(&RetentionBudget::default(), &first_lease)
            .expect_err("fenced capability must stop before mutation");
        assert!(error.is_lease_lost());
        assert!(!error.is_post_commit());
        assert!(second.release_retention_lease(&second_lease).unwrap());
    }

    #[test]
    fn lease_expiry_before_commit_rolls_back_deletion() {
        let (_dir, store) = temp_store("lease-commit-fence");
        seed_expired_events(&store, 1);
        let lease = store
            .try_acquire_retention_lease_at("owner-a-1", 100, Duration::from_secs(30))
            .unwrap()
            .unwrap();
        let budget = RetentionBudget {
            max_event_age: Some(Duration::from_secs(1)),
            max_event_rows: 0,
            max_terminal_job_age: None,
            max_terminal_job_rows: 0,
            max_database_bytes: 0,
            batch_size: 1,
        };

        let result = store.execute_prune_batch(
            &lease,
            Timestamp::now(),
            Instant::now() + MAX_RETENTION_PASS_DURATION,
            false,
            &PruneCounts::default(),
            |transaction| {
                let counts = Store::prune_expired_event_batch(
                    transaction,
                    &budget,
                    Timestamp::now(),
                    &lease,
                )?;
                // Model a lease expiring after deletion work but before the
                // commit fence. The transaction must roll back the delete.
                transaction.execute(
                    "UPDATE retention_lease SET expires_at = 0
                     WHERE singleton = 0 AND owner = ?1 AND generation = ?2",
                    params![lease.owner, i64::try_from(lease.generation).unwrap()],
                )?;
                Ok(counts)
            },
        );

        assert!(matches!(
            result,
            Err(PruneFailure::LeaseLost {
                committed: false,
                ..
            })
        ));
        assert_eq!(store.accounting().unwrap().event_rows, 1);
    }

    #[test]
    fn direct_prune_acquires_private_lease_and_respects_contention() {
        let (_dir, store) = temp_store("direct-prune-lease");
        seed_expired_events(&store, 2);
        let now = Timestamp::now()
            .as_offset_datetime()
            .unix_timestamp()
            .try_into()
            .unwrap();
        let owner_lease = store
            .try_acquire_retention_lease_at("owner-a-1", now, Duration::from_secs(30))
            .unwrap()
            .unwrap();
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
            .expect_err("direct pruning must not bypass a live lease");
        assert_eq!(error.envelope.reason, "store.retention.lease.busy");
        assert_eq!(store.accounting().unwrap().event_rows, 2);
        assert!(store.release_retention_lease(&owner_lease).unwrap());
    }

    #[test]
    fn retention_lease_rejects_non_owner_release() {
        let (_dir, store) = temp_store("lease-release");

        let lease = store
            .try_acquire_retention_lease_at("owner-a-1", 100, Duration::from_secs(30))
            .unwrap()
            .unwrap();
        let stale = RetentionLease {
            owner: "owner-b-1".to_owned(),
            generation: lease.generation,
        };
        assert!(!store.release_retention_lease(&stale).unwrap());
        assert!(store.release_retention_lease(&lease).unwrap());
        assert!(!store.release_retention_lease(&lease).unwrap());
    }

    #[test]
    fn prune_lease_finalization_rejects_unmatched_release() {
        let (_dir, store) = temp_store("lease-finalization");
        let lease = store
            .try_acquire_retention_lease_at("owner-a-1", 100, Duration::from_secs(30))
            .unwrap()
            .unwrap();
        let stale = RetentionLease {
            owner: "owner-b-1".to_owned(),
            generation: lease.generation,
        };

        let error = store
            .release_retention_lease_final(&stale)
            .expect_err("unmatched release must fail finalization");
        assert_eq!(error.envelope.reason, "store.retention.lease.finalize");
        assert!(error
            .envelope
            .remediation
            .as_deref()
            .is_some_and(|detail| detail.contains("must not be retried as pre-commit")));
        assert!(store.release_retention_lease(&lease).unwrap());
    }

    #[test]
    fn retention_lease_rejects_overflow_without_saturation() {
        let (_dir, store) = temp_store("lease-overflow");

        let error = store
            .try_acquire_retention_lease_at("owner-a-1", u64::MAX, Duration::from_secs(u64::MAX))
            .expect_err("oversized TTL and clock must not saturate");
        assert_eq!(error.envelope.reason, "store.retention.lease.duration");
    }

    #[test]
    fn retention_lease_contention_and_sql_validation_are_distinguishable() {
        let (_dir, store) = temp_store("lease-errors");

        let usage = store
            .try_acquire_retention_lease_at("", 100, Duration::from_secs(30))
            .expect_err("invalid owner is a SQL-independent validation failure");
        assert_eq!(usage.envelope.reason, "store.retention.lease.owner");

        assert!(store
            .try_acquire_retention_lease_at("owner-a-1", 100, Duration::from_secs(30))
            .unwrap()
            .is_some());
        assert!(!store
            .try_acquire_retention_lease_at("owner-b-1", 101, Duration::from_secs(30))
            .unwrap()
            .is_some());
    }

    #[test]
    fn retention_lease_rejects_weak_or_unsafe_owner_tokens() {
        let (_dir, store) = temp_store("lease-owner-validation");
        for owner in ["short", "owner/token", "owner token"] {
            let error = store
                .try_acquire_retention_lease_at(owner, 100, Duration::from_secs(30))
                .expect_err("owner validation must fail closed");
            assert_eq!(error.envelope.reason, "store.retention.lease.owner");
        }
        let too_long = "x".repeat(MAX_RETENTION_LEASE_OWNER_BYTES + 1);
        let error = store
            .try_acquire_retention_lease_at(&too_long, 100, Duration::from_secs(30))
            .expect_err("oversized owner must fail closed");
        assert_eq!(error.envelope.reason, "store.retention.lease.owner");
    }

    #[test]
    fn retention_lease_rejects_zero_subsecond_and_overlong_ttl() {
        let (_dir, store) = temp_store("lease-duration-validation");
        for duration in [
            Duration::ZERO,
            Duration::from_millis(999),
            MAX_RETENTION_LEASE_DURATION + Duration::from_secs(1),
        ] {
            let error = store
                .try_acquire_retention_lease_at("owner-a-1", 100, duration)
                .expect_err("TTL validation must fail closed");
            assert_eq!(error.envelope.reason, "store.retention.lease.duration");
        }
        let error = store
            .try_acquire_retention_lease_at("owner-a-1", u64::MAX, Duration::from_secs(1))
            .expect_err("clock overflow must not saturate");
        assert_eq!(error.envelope.reason, "store.retention.lease.clock");
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
    fn state_machine_event_keeps_exact_transition_identity() {
        let (_dir, store) = temp_store("exact-transition-event");
        let connection = test_connection(&store);
        connection
            .execute(
                "INSERT INTO jobs
                 (instance_slug, job_uid, repository, workflow, job_name, phase, updated_at)
                 VALUES ('exact', 'job-1', 'repo', 'workflow', 'job', 'started',
                         '2000-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        drop(connection);

        let applied = store
            .record_job_transition(
                "exact",
                "job-1",
                &crate::store::Transition {
                    token: "token-1".to_owned(),
                    correlation_id: velnor_model::Slug::validate("correlation_id", "corr-exact")
                        .unwrap(),
                    reason: velnor_model::EventReason::JobCompleted,
                    message: None,
                    transition_time: Timestamp::now(),
                    conclusion: Some("success".to_owned()),
                    infrastructure_category: None,
                },
            )
            .unwrap();
        assert!(applied);

        let connection = test_connection(&store);
        let linkage: (i64, i64) = connection
            .query_row(
                "SELECT t.id, e.transition_id
                 FROM job_transitions t
                 JOIN events e ON e.transition_id = t.id
                 WHERE t.instance_slug = 'exact' AND t.job_uid = 'job-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(linkage.0, linkage.1);
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
        let transition_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO events
                 (instance_slug, event_kind, subject, correlation_id,
                  occurred_at, detail, transition_id)
                 VALUES ('shared', 'job.transition.job.completed', 'same-subject',
                         'corr-shared', '2000-01-01T00:00:00Z', 'job ancestry', ?1)",
                [transition_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO events
                 (instance_slug, event_kind, subject, occurred_at, detail)
                 VALUES ('shared', 'capacity.pressure', 'same-subject',
                         '2000-01-01T00:00:00Z', 'unrelated operational event')",
                [],
            )
            .unwrap();
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
            let transition_id = connection.last_insert_rowid();
            connection
                .execute(
                    "INSERT INTO events
                     (instance_slug, event_kind, subject, correlation_id, occurred_at,
                      transition_id)
                     VALUES ('fanout', 'job.transition.job.completed', 'job-1', ?1,
                             '2000-01-01T00:00:00Z', ?2)",
                    params![correlation, transition_id],
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
        assert_eq!(report.deleted_events, MAX_PRUNE_BATCH_SIZE + 6);
        assert_eq!(report.deleted_transitions, MAX_PRUNE_BATCH_SIZE + 6);
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
    fn failure_after_event_prune_preserves_committed_batch() {
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
        // The hook runs after the committed event batch. The injected error
        // stops later phases, but cannot roll back durable partial progress.
        assert_eq!(store.accounting().unwrap().event_rows, 0);
        assert_eq!(before, 3);
        assert!(store.accounting().unwrap().last_prune_at.is_some());
    }

    #[test]
    fn failure_after_job_prune_preserves_prior_committed_batches() {
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

        assert_eq!(store.accounting().unwrap().event_rows, 0);
        assert_eq!(before, 2);
        assert!(store.accounting().unwrap().last_prune_at.is_some());
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
        assert!(report.wal_bytes > 0);
        let current_wal_bytes = Store::wal_bytes_for_path(&dir.join("state.db")).unwrap();
        assert!(current_wal_bytes >= report.wal_bytes);
        assert_eq!(
            report.total_bytes,
            report.database_bytes.saturating_add(report.wal_bytes)
        );
        assert_eq!(
            report.physical_budget_status,
            PhysicalBudgetStatus::WithinBudget
        );
        assert!(!report.maintenance_deferred);
        assert!(report.checkpoint.attempted);
        assert_eq!(report.checkpoint.busy_frames, 0);
        drop(store);
        // The PASSIVE result is explicit even when SQLite keeps the sidecar
        // inode allocated after all frames have been checkpointed.
    }

    #[test]
    fn bounded_maintenance_reports_wal_status_and_survives_reopen() {
        let (dir, store) = temp_store("bounded-maintenance-reopen");
        seed_expired_events(&store, 2);
        let lease = store
            .try_acquire_retention_lease("maint-owner-1", MAX_RETENTION_LEASE_DURATION)
            .unwrap()
            .unwrap();
        let report = store
            .run_bounded_maintenance_with_lease(
                &RetentionMaintenanceBudget {
                    max_database_bytes: u64::MAX,
                    min_free_bytes: 0,
                    max_checkpoint_attempts: 1,
                    max_vacuum_pages: 1,
                },
                &lease,
            )
            .unwrap();

        assert!(report.checkpoint.attempted);
        assert!(report.checkpoint.checkpointed_frames <= report.checkpoint.log_frames);
        assert_eq!(
            report.physical_budget_status,
            PhysicalBudgetStatus::WithinBudget
        );
        assert!(!report.reserve_violation);
        assert!(report.free_bytes_before.is_some());
        assert!(report.free_bytes_after.is_some());
        assert!(report.vacuum_pages_attempted <= 1);
        store.release_retention_lease(&lease).unwrap();

        drop(store);
        let reopened = Store::open(dir.join("state.db")).unwrap();
        let accounting = reopened.accounting().unwrap();
        assert!(accounting.free_bytes.is_some());
        assert_eq!(
            accounting.physical_budget_status,
            PhysicalBudgetStatus::Unconfigured
        );
        assert!(!accounting.checkpoint.attempted);
    }

    #[test]
    fn maintenance_defers_when_reserve_is_violated() {
        let (_dir, store) = temp_store("maintenance-reserve");
        let lease = store
            .try_acquire_retention_lease("reserve-owner-1", MAX_RETENTION_LEASE_DURATION)
            .unwrap()
            .unwrap();
        let report = store
            .run_bounded_maintenance_with_lease(
                &RetentionMaintenanceBudget {
                    max_database_bytes: 1,
                    min_free_bytes: u64::MAX,
                    max_checkpoint_attempts: 1,
                    max_vacuum_pages: 500,
                },
                &lease,
            )
            .unwrap();

        assert_eq!(
            report.physical_budget_status,
            PhysicalBudgetStatus::ReserveViolation
        );
        assert!(report.reserve_violation);
        assert!(!report.checkpoint.attempted);
        assert!(report.maintenance_deferred);
        assert!(report
            .maintenance_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("reserve")));
        store.release_retention_lease(&lease).unwrap();
    }

    #[test]
    fn maintenance_reports_deferred_when_checkpoint_budget_is_zero() {
        let (_dir, store) = temp_store("maintenance-bounded");
        let lease = store
            .try_acquire_retention_lease("bounded-owner-1", MAX_RETENTION_LEASE_DURATION)
            .unwrap()
            .unwrap();
        let report = store
            .run_bounded_maintenance_with_lease(
                &RetentionMaintenanceBudget {
                    max_database_bytes: 1,
                    min_free_bytes: 0,
                    max_checkpoint_attempts: 0,
                    max_vacuum_pages: 0,
                },
                &lease,
            )
            .unwrap();

        assert_eq!(
            report.physical_budget_status,
            PhysicalBudgetStatus::Deferred
        );
        assert!(report.maintenance_deferred);
        assert!(!report.checkpoint.attempted);
        assert_eq!(report.vacuum_pages_attempted, 0);
        store.release_retention_lease(&lease).unwrap();
    }

    #[test]
    fn maintenance_clamps_hostile_vacuum_page_budget() {
        let (_dir, store) = temp_store("maintenance-vacuum-cap");
        let lease = store
            .try_acquire_retention_lease("vacuum-owner-1", MAX_RETENTION_LEASE_DURATION)
            .unwrap()
            .unwrap();
        let report = store
            .run_bounded_maintenance_with_lease(
                &RetentionMaintenanceBudget {
                    max_database_bytes: 1,
                    min_free_bytes: 0,
                    max_checkpoint_attempts: 1,
                    max_vacuum_pages: u64::MAX,
                },
                &lease,
            )
            .unwrap();

        assert!(report.vacuum_pages_attempted <= MAX_MAINTENANCE_VACUUM_PAGES);
        assert_ne!(
            report.physical_budget_status,
            PhysicalBudgetStatus::WithinBudget
        );
        store.release_retention_lease(&lease).unwrap();
    }
}

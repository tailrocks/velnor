//! Host-wide `max_jobs=N` permit ledger shared by every local lane.
//!
//! One top-level job lifecycle holds exactly one permit, from the point
//! capacity is committed for acquisition (or assignable readiness) until
//! terminal work and owned cleanup are confirmed. Reserved, acquiring,
//! provisioning, assignable, running, cleaning, and uncertain states are
//! all included in the single count: row presence is occupancy, whatever
//! the state. Offered but unacquired work is durable queued demand, not
//! occupied capacity. Demand ordering and permit acquisition share one
//! immediate transaction, so no lane can spend a permit ahead of older
//! eligible work.
//!
//! Lanes ([`PermitLane`]) share the one `N` and the same oldest-first demand
//! queue. There is no per-lane reservation.
//!
//! Durability and crash recovery:
//!
//! * The ledger is a host-wide SQLite database. Every daemon on the host
//!   must resolve to the same file; multi-process contention is bounded by
//!   a busy timeout, and every mutation runs in an immediate transaction.
//! * Grants and lifecycle mutations are generation-fenced:
//!   [`PermitLedger::begin_epoch`] bumps the generation at daemon startup,
//!   and acquire/transition/release calls carrying a stale generation are
//!   rejected. The legacy release methods remain for non-worker cleanup;
//!   worker lifecycle paths use the explicit `*_fenced` methods.
//! * Capacity is advertised only after reconciliation:
//!   [`PermitLedger::advertised_free`] returns `None` until
//!   [`PermitLedger::reconcile`] has run in the current epoch.
//!   Reconciliation never deletes: observed-but-unrecorded work is adopted
//!   as counted occupancy, and recorded-but-unobserved work is marked
//!   [`PermitState::Uncertain`] (still counted). A cleanup failure retains
//!   its visible reservation; occupied work is never erased by resetting a
//!   semaphore.
//! * Acquisition is idempotent per holder: a duplicate delivery for the
//!   same holder returns [`AcquireOutcome::AlreadyHeld`] without spending
//!   a second permit. A fresh grant atomically changes the oldest eligible
//!   demand to granted while inserting its permit.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, Transaction};

/// SQLite busy timeout for multi-process ledger contention.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// An unrefreshed offer this old is no longer eligible to block another
/// lane. Active queues refresh on redelivery; the original age remains in
/// the row so a later redelivery keeps its place.
pub const DEMAND_STALE_AFTER_SECS: u64 = 300;

/// How long one guard acquisition keeps yielding to older eligible demand
/// before departing for redelivery.
///
/// Strict oldest-first admission means only the head of the queue may
/// grant, so a younger waiter must outlive the older attempts ahead of
/// it. A live head acts within milliseconds (one immediate transaction),
/// and the backoff between retries yields the SQLite lock so the head
/// can act. Departure parks the demand (see
/// [`PermitLedger::park_demand`]) rather than leaving a head-blocking
/// row. Matches the SQLite busy timeout: lock contention already blocks
/// this long.
pub const DEFERRED_WAIT_BUDGET: Duration = Duration::from_secs(5);

/// First pause between [`AcquireOutcome::Deferred`] retries; doubles per
/// consecutive wait.
pub const DEFERRED_WAIT_MIN: Duration = Duration::from_millis(1);

/// Backoff cap between [`AcquireOutcome::Deferred`] retries.
pub const DEFERRED_WAIT_MAX: Duration = Duration::from_millis(50);

/// Pause before retrying after `attempt` consecutive [`AcquireOutcome::Deferred`]
/// outcomes: exponential from [`DEFERRED_WAIT_MIN`], capped at
/// [`DEFERRED_WAIT_MAX`]. The sleeps yield the SQLite lock — a tight spin
/// re-locks unfairly and starves the head it waits for.
#[must_use]
pub fn deferred_wait(attempt: u32) -> Duration {
    let scaled = DEFERRED_WAIT_MIN.saturating_mul(1 << attempt.min(10));
    scaled.min(DEFERRED_WAIT_MAX)
}

/// A local lane sharing the one host-wide `N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermitLane {
    /// Velnor-native daemon/slot acquisitions.
    Native,
    /// Official-runner Scale Set acquisitions (D1 adapter hook point).
    ScaleSet,
}

impl PermitLane {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::ScaleSet => "scale-set",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "native" => Some(Self::Native),
            "scale-set" => Some(Self::ScaleSet),
            _ => None,
        }
    }
}

/// Lifecycle of one durable demand. Only `Eligible` rows take part in the
/// oldest-first admission decision. A granted row owns a permit; terminal
/// and cancelled rows cannot block later work or be revived by redelivery.
/// A waiting row is a departed waiter holding its queue ticket: it blocks
/// nothing, and redelivery revives it at its original age.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DemandState {
    Eligible,
    Granted,
    Terminal,
    Cancelled,
    Waiting,
}

impl DemandState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Granted => "granted",
            Self::Terminal => "terminal",
            Self::Cancelled => "cancelled",
            Self::Waiting => "waiting",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "eligible" => Some(Self::Eligible),
            "granted" => Some(Self::Granted),
            "terminal" => Some(Self::Terminal),
            "cancelled" => Some(Self::Cancelled),
            "waiting" => Some(Self::Waiting),
            _ => None,
        }
    }
}

/// One durable demand observation. `first_seen_unix` and `sequence` never
/// change after insertion; redelivery only refreshes `updated_unix`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermitDemand {
    pub holder: String,
    pub lane: PermitLane,
    pub scope: String,
    pub first_seen_unix: u64,
    pub sequence: i64,
    pub state: DemandState,
    pub updated_unix: u64,
}

/// Lifecycle state of one held permit. Every state counts toward `N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermitState {
    Reserved,
    Acquiring,
    Provisioning,
    Assignable,
    Running,
    Cleaning,
    Uncertain,
}

impl PermitState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Acquiring => "acquiring",
            Self::Provisioning => "provisioning",
            Self::Assignable => "assignable",
            Self::Running => "running",
            Self::Cleaning => "cleaning",
            Self::Uncertain => "uncertain",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "reserved" => Some(Self::Reserved),
            "acquiring" => Some(Self::Acquiring),
            "provisioning" => Some(Self::Provisioning),
            "assignable" => Some(Self::Assignable),
            "running" => Some(Self::Running),
            "cleaning" => Some(Self::Cleaning),
            "uncertain" => Some(Self::Uncertain),
            _ => None,
        }
    }
}

/// One counted occupant of the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermitHolder {
    pub holder: String,
    pub lane: PermitLane,
    pub state: PermitState,
    pub acquired_unix: u64,
    pub updated_unix: u64,
    pub generation: u64,
    /// Host pid of the acquiring process, when the lane records one.
    /// Same-holder redelivery may adopt a dead attempt; startup never uses
    /// local pid or root evidence alone to erase another daemon's row.
    pub pid: Option<u32>,
}

/// Outcome of [`PermitLedger::adopt_if_pid_dead`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptOutcome {
    /// The dead attempt's row was adopted: same holder, new pid, new state.
    Adopted,
    /// The holding attempt may still be alive (or belongs to another
    /// lane): not adopted.
    LiveHolder,
    /// The row vanished between calls; retry the acquire.
    Missing,
    /// The caller fenced on a stale generation; re-read and retry.
    StaleGeneration,
}

/// Outcome of [`PermitLedger::acquire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireOutcome {
    /// A fresh permit was granted.
    Acquired,
    /// The holder already holds a permit (duplicate delivery); no second
    /// permit was spent.
    AlreadyHeld,
    /// `occupied >= max_jobs`; no permit was granted.
    Full,
    /// Capacity is available, but an older eligible demand must acquire
    /// first. The demand remains durable and keeps its original age.
    Deferred,
    /// This demand was already terminal or cancelled and cannot be revived.
    Closed,
    /// The caller fenced on a stale generation; re-read and retry.
    StaleGeneration,
    /// No `max_jobs` was ever configured; refusing rather than guessing.
    NotConfigured,
}

/// What [`PermitLedger::reconcile`] did. Reconciliation adopts and marks;
/// it never deletes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Observed holders that had no row; adopted as counted occupancy.
    pub adopted: Vec<String>,
    /// Recorded holders that were not observed; marked uncertain (counted).
    pub marked_uncertain: Vec<String>,
    /// Recorded holders confirmed by observation.
    pub confirmed: Vec<String>,
}

/// Errors from ledger operations.
#[derive(Debug)]
pub enum LedgerError {
    Storage(rusqlite::Error),
    UnknownLane(String),
    UnknownState(String),
    UnknownDemandState(String),
    UnknownHolder(String),
    DemandLaneMismatch {
        holder: String,
        expected: PermitLane,
        seen: String,
    },
    StaleGeneration {
        expected: u64,
        seen: u64,
    },
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "permit ledger storage: {error}"),
            Self::UnknownLane(lane) => write!(f, "permit ledger holds unknown lane {lane:?}"),
            Self::UnknownState(state) => write!(f, "permit ledger holds unknown state {state:?}"),
            Self::UnknownDemandState(state) => {
                write!(f, "permit ledger holds unknown demand state {state:?}")
            }
            Self::UnknownHolder(holder) => {
                write!(f, "permit ledger holds no permit for {holder:?}")
            }
            Self::DemandLaneMismatch {
                holder,
                expected,
                seen,
            } => write!(
                f,
                "permit demand {holder:?} belongs to lane {seen:?}, not {expected:?}"
            ),
            Self::StaleGeneration { expected, seen } => write!(
                f,
                "permit ledger generation moved from {seen} to {expected}; re-read and retry"
            ),
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<rusqlite::Error> for LedgerError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error)
    }
}

/// Current Unix time in seconds, used for durable demand and permit
/// observation timestamps.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Host-wide `max_jobs=N` permit ledger.
#[derive(Debug)]
pub struct PermitLedger {
    path: PathBuf,
    conn: Connection,
}

/// Move the former native-only queue into the host-wide queue before any
/// admission operation can observe the new schema. The transaction makes
/// the migration safe when multiple daemon processes open the ledger at
/// once. Native `first_seen_unix` and relative tie order survive the move;
/// the new global sequence starts after rows already present in the shared
/// queue.
fn migrate_legacy_native_demand(conn: &mut Connection) -> Result<(), LedgerError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'native_demand'
         )",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        tx.commit()?;
        return Ok(());
    }
    let legacy_rows: Vec<(String, String, i64, i64, String, i64)> = {
        let mut select = tx.prepare(
            "SELECT request_id, scope, first_seen_unix, sequence, state, updated_unix
             FROM native_demand ORDER BY first_seen_unix, sequence, request_id",
        )?;
        select
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?
    };
    let mut next_sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM permit_demands",
        [],
        |row| row.get(0),
    )?;
    for (request_id, scope, first_seen, _legacy_sequence, raw_state, updated) in legacy_rows {
        let state = DemandState::parse(&raw_state)
            .ok_or_else(|| LedgerError::UnknownDemandState(raw_state.clone()))?;
        let holder = format!("native/{request_id}");
        let already_migrated: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM permit_demands WHERE holder = ?1)",
            params![holder],
            |row| row.get(0),
        )?;
        if already_migrated {
            continue;
        }
        let sequence = next_sequence;
        next_sequence = next_sequence.saturating_add(1);
        tx.execute(
            "INSERT INTO permit_demands
             (holder, lane, scope, first_seen_unix, sequence, state, updated_unix)
             VALUES (?1, 'native', ?2, ?3, ?4, ?5, ?6)",
            params![holder, scope, first_seen, sequence, state.as_str(), updated],
        )?;
    }
    tx.execute_batch("DROP TABLE native_demand;")?;
    tx.commit()?;
    Ok(())
}

fn read_demand(conn: &Connection, holder: &str) -> Result<Option<PermitDemand>, LedgerError> {
    let row: Option<(String, String, String, i64, i64, String, i64)> = conn
        .query_row(
            "SELECT holder, lane, scope, first_seen_unix, sequence, state, updated_unix
             FROM permit_demands WHERE holder = ?1",
            params![holder],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(holder, raw_lane, scope, first_seen, sequence, raw_state, updated)| {
            let lane = PermitLane::parse(&raw_lane)
                .ok_or_else(|| LedgerError::UnknownLane(raw_lane.clone()))?;
            let state = DemandState::parse(&raw_state)
                .ok_or_else(|| LedgerError::UnknownDemandState(raw_state.clone()))?;
            Ok(PermitDemand {
                holder,
                lane,
                scope,
                first_seen_unix: first_seen.max(0) as u64,
                sequence,
                state,
                updated_unix: updated.max(0) as u64,
            })
        },
    )
    .transpose()
}

fn check_demand_lane(
    holder: &str,
    demand: &PermitDemand,
    lane: PermitLane,
) -> Result<(), LedgerError> {
    if demand.lane == lane {
        return Ok(());
    }
    Err(LedgerError::DemandLaneMismatch {
        holder: holder.to_owned(),
        expected: lane,
        seen: demand.lane.as_str().to_owned(),
    })
}

fn ensure_demand_tx(
    tx: &Transaction<'_>,
    holder: &str,
    lane: PermitLane,
    scope: &str,
    first_seen_unix: u64,
    updated_unix: u64,
    initial_state: DemandState,
) -> Result<PermitDemand, LedgerError> {
    if let Some(demand) = read_demand(tx, holder)? {
        check_demand_lane(holder, &demand, lane)?;
        return Ok(demand);
    }
    let sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM permit_demands",
        [],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO permit_demands
         (holder, lane, scope, first_seen_unix, sequence, state, updated_unix)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            holder,
            lane.as_str(),
            scope,
            i64::try_from(first_seen_unix).unwrap_or(i64::MAX),
            sequence,
            initial_state.as_str(),
            i64::try_from(updated_unix).unwrap_or(i64::MAX),
        ],
    )?;
    Ok(PermitDemand {
        holder: holder.to_owned(),
        lane,
        scope: scope.to_owned(),
        first_seen_unix,
        sequence,
        state: initial_state,
        updated_unix,
    })
}

impl PermitLedger {
    /// Open (creating parent directories and schema) the ledger database.
    pub fn open(path: &Path) -> Result<Self, LedgerError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                LedgerError::Storage(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                    Some(format!("create ledger dir {}: {error}", parent.display())),
                ))
            })?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS permit_meta (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                max_jobs INTEGER,
                generation INTEGER NOT NULL DEFAULT 0,
                reconciled_generation INTEGER NOT NULL DEFAULT -1
            );
            INSERT OR IGNORE INTO permit_meta (id, max_jobs, generation, reconciled_generation)
                VALUES (1, NULL, 0, -1);
            CREATE TABLE IF NOT EXISTS permits (
                holder TEXT PRIMARY KEY,
                lane TEXT NOT NULL,
                state TEXT NOT NULL,
                acquired_unix INTEGER NOT NULL,
                updated_unix INTEGER NOT NULL,
                generation INTEGER NOT NULL,
                pid INTEGER
            );
            CREATE TABLE IF NOT EXISTS permit_demands (
                holder TEXT PRIMARY KEY,
                lane TEXT NOT NULL,
                scope TEXT NOT NULL,
                first_seen_unix INTEGER NOT NULL,
                sequence INTEGER NOT NULL UNIQUE,
                state TEXT NOT NULL,
                updated_unix INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_permit_demands_oldest
                ON permit_demands (state, first_seen_unix, sequence);",
        )?;
        migrate_legacy_native_demand(&mut conn)?;
        Ok(Self {
            path: path.to_path_buf(),
            conn,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Configured host-wide `N`, when one was set.
    pub fn max_jobs(&self) -> Result<Option<u32>, LedgerError> {
        let max: Option<i64> =
            self.conn
                .query_row("SELECT max_jobs FROM permit_meta WHERE id = 1", [], |row| {
                    row.get(0)
                })?;
        Ok(max.and_then(|max| u32::try_from(max).ok()))
    }

    /// Set the host-wide `N`. Only the daemon startup path calls this;
    /// slots and one-shot acquisitions never resize the ledger.
    pub fn set_max_jobs(&mut self, max_jobs: u32) -> Result<(), LedgerError> {
        self.conn.execute(
            "UPDATE permit_meta SET max_jobs = ?1 WHERE id = 1",
            params![i64::from(max_jobs)],
        )?;
        Ok(())
    }

    /// Adopt the first fallback capacity atomically across daemon starts.
    /// Returns true only for the process that initialized an unset ledger.
    pub fn set_max_jobs_if_unset(&mut self, max_jobs: u32) -> Result<bool, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let configured: Option<i64> =
            tx.query_row("SELECT max_jobs FROM permit_meta WHERE id = 1", [], |row| {
                row.get(0)
            })?;
        let adopted = configured.is_none();
        if adopted {
            tx.execute(
                "UPDATE permit_meta SET max_jobs = ?1 WHERE id = 1 AND max_jobs IS NULL",
                params![i64::from(max_jobs)],
            )?;
        }
        tx.commit()?;
        Ok(adopted)
    }

    /// Current generation. Callers fence mutations on this value.
    pub fn generation(&self) -> Result<u64, LedgerError> {
        let generation: i64 = self.conn.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(generation.max(0) as u64)
    }

    /// Start a new epoch (daemon startup): bump the generation and require
    /// a fresh [`Self::reconcile`] before capacity is advertised.
    pub fn begin_epoch(&mut self) -> Result<u64, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let generation: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let next = generation.saturating_add(1);
        tx.execute(
            "UPDATE permit_meta SET generation = ?1 WHERE id = 1",
            params![next],
        )?;
        tx.commit()?;
        Ok(next.max(0) as u64)
    }

    /// Whether [`Self::reconcile`] ran in the current generation.
    pub fn reconciled(&self) -> Result<bool, LedgerError> {
        let (generation, reconciled): (i64, i64) = self.conn.query_row(
            "SELECT generation, reconciled_generation FROM permit_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(generation == reconciled)
    }

    /// Free capacity (`N - occupied`), only after this epoch reconciled.
    /// `None` means "do not advertise yet", never "infinite".
    pub fn advertised_free(&self) -> Result<Option<u32>, LedgerError> {
        if !self.reconciled()? {
            return Ok(None);
        }
        let Some(max) = self.max_jobs()? else {
            return Ok(None);
        };
        Ok(Some(max.saturating_sub(self.occupied()?)))
    }

    /// Counted occupants across every lane and state.
    pub fn occupied(&self) -> Result<u32, LedgerError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM permits", [], |row| row.get(0))?;
        Ok(u32::try_from(count.max(0)).unwrap_or(u32::MAX))
    }

    /// Counted occupants in one lane.
    pub fn occupied_by_lane(&self, lane: PermitLane) -> Result<u32, LedgerError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM permits WHERE lane = ?1",
            params![lane.as_str()],
            |row| row.get(0),
        )?;
        Ok(u32::try_from(count.max(0)).unwrap_or(u32::MAX))
    }

    /// Every counted occupant, ordered by holder.
    pub fn holders(&self) -> Result<Vec<PermitHolder>, LedgerError> {
        let mut stmt = self.conn.prepare(
            "SELECT holder, lane, state, acquired_unix, updated_unix, generation, pid
             FROM permits ORDER BY holder",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })?;
        let mut holders = Vec::new();
        for row in rows {
            let (holder, lane, state, acquired_unix, updated_unix, generation, pid) = row?;
            let lane = PermitLane::parse(&lane).ok_or_else(|| LedgerError::UnknownLane(lane))?;
            let state =
                PermitState::parse(&state).ok_or_else(|| LedgerError::UnknownState(state))?;
            holders.push(PermitHolder {
                holder,
                lane,
                state,
                acquired_unix: acquired_unix.max(0) as u64,
                updated_unix: updated_unix.max(0) as u64,
                generation: generation.max(0) as u64,
                pid: pid.and_then(|pid| u32::try_from(pid).ok()),
            });
        }
        Ok(holders)
    }

    /// Read one durable demand row.
    pub fn demand(&self, holder: &str) -> Result<Option<PermitDemand>, LedgerError> {
        read_demand(&self.conn, holder)
    }

    /// Record that a lane currently observes eligible demand.
    ///
    /// On first observation, `first_seen_unix` and a host-wide immutable
    /// sequence are persisted. Redelivery preserves both and refreshes only
    /// `updated_unix`; a parked ([`DemandState::Waiting`]) row rejoins the
    /// queue at its original age. If the holder already owns a permit, a
    /// newly created row starts granted so it cannot block younger demand.
    pub fn observe_demand(
        &mut self,
        holder: &str,
        lane: PermitLane,
        scope: &str,
        first_seen_unix: u64,
        observed_unix: u64,
    ) -> Result<PermitDemand, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let held_lane: Option<String> = tx
            .query_row(
                "SELECT lane FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(held_lane) = held_lane.as_deref()
            && held_lane != lane.as_str()
        {
            return Err(LedgerError::DemandLaneMismatch {
                holder: holder.to_owned(),
                expected: lane,
                seen: held_lane.to_owned(),
            });
        }
        let initial_state = if held_lane.is_some() {
            DemandState::Granted
        } else {
            DemandState::Eligible
        };
        let existing = ensure_demand_tx(
            &tx,
            holder,
            lane,
            scope,
            first_seen_unix,
            observed_unix,
            initial_state,
        )?;
        if existing.state == DemandState::Waiting {
            tx.execute(
                "UPDATE permit_demands SET state = 'eligible', updated_unix = ?1
                 WHERE holder = ?2",
                params![i64::try_from(observed_unix).unwrap_or(i64::MAX), holder],
            )?;
        } else if matches!(existing.state, DemandState::Eligible | DemandState::Granted) {
            tx.execute(
                "UPDATE permit_demands SET updated_unix = ?1 WHERE holder = ?2",
                params![i64::try_from(observed_unix).unwrap_or(i64::MAX), holder],
            )?;
        }
        let demand = read_demand(&tx, holder)?
            .ok_or_else(|| LedgerError::UnknownHolder(holder.to_owned()))?;
        tx.commit()?;
        Ok(demand)
    }

    /// Park a departed waiter's demand: it keeps its queue ticket
    /// (`first_seen_unix`, `sequence`) but no longer head-blocks younger
    /// eligible demand. Redelivery ([`Self::observe_demand`],
    /// [`Self::acquire`]) revives it at its original age; without
    /// redelivery the row is inert. Only an eligible row parks; a permit
    /// holder's granted row and closed rows are untouched.
    pub fn park_demand(&mut self, holder: &str) -> Result<bool, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE permit_demands SET state = 'waiting', updated_unix = ?1
             WHERE holder = ?2 AND state = 'eligible'",
            params![unix_now() as i64, holder],
        )?;
        tx.commit()?;
        Ok(changed > 0)
    }

    /// Cancel an eligible demand that its lane has confirmed is no longer
    /// available upstream. A held permit cannot be cancelled through this
    /// path; cleanup must release it first.
    pub fn cancel_demand(&mut self, holder: &str) -> Result<bool, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE permit_demands SET state = 'cancelled', updated_unix = ?1
             WHERE holder = ?2 AND state IN ('eligible', 'waiting')
               AND NOT EXISTS (SELECT 1 FROM permits WHERE holder = ?2)",
            params![unix_now() as i64, holder],
        )?;
        tx.commit()?;
        Ok(changed > 0)
    }

    /// Current state of one holder's permit, if held.
    pub fn holder_state(&self, holder: &str) -> Result<Option<PermitState>, LedgerError> {
        let state: Option<String> = self
            .conn
            .query_row(
                "SELECT state FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?;
        state
            .map(|state| PermitState::parse(&state).ok_or_else(|| LedgerError::UnknownState(state)))
            .transpose()
    }

    /// Acquire one permit for `holder`, fenced on `generation`.
    ///
    /// Idempotent: a holder that already holds keeps its permit (its state
    /// is left untouched) and reports [`AcquireOutcome::AlreadyHeld`]. A
    /// fresh holder acquires only when capacity is available and it is the
    /// oldest eligible demand. Permit insertion and the eligible-to-granted
    /// transition commit together. A parked ([`DemandState::Waiting`])
    /// holder revives at its original age before the admission decision.
    ///
    /// `pid` records the acquiring host process as diagnostic recovery
    /// evidence; lanes whose holders are not host processes pass `None`.
    pub fn acquire(
        &mut self,
        holder: &str,
        lane: PermitLane,
        state: PermitState,
        generation: u64,
        pid: Option<u32>,
    ) -> Result<AcquireOutcome, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if current.max(0) as u64 != generation {
            return Ok(AcquireOutcome::StaleGeneration);
        }
        let now = unix_now();
        let held_lane: Option<String> = tx
            .query_row(
                "SELECT lane FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(held_lane) = held_lane {
            if held_lane != lane.as_str() {
                return Err(LedgerError::DemandLaneMismatch {
                    holder: holder.to_owned(),
                    expected: lane,
                    seen: held_lane,
                });
            }
            let demand = ensure_demand_tx(&tx, holder, lane, "", now, now, DemandState::Granted)?;
            if demand.state == DemandState::Eligible {
                tx.execute(
                    "UPDATE permit_demands SET state = 'granted', updated_unix = ?1
                     WHERE holder = ?2",
                    params![i64::try_from(now).unwrap_or(i64::MAX), holder],
                )?;
            }
            tx.commit()?;
            return Ok(AcquireOutcome::AlreadyHeld);
        }
        let max: Option<i64> =
            tx.query_row("SELECT max_jobs FROM permit_meta WHERE id = 1", [], |row| {
                row.get(0)
            })?;
        let Some(max) = max.and_then(|max| u32::try_from(max).ok()) else {
            return Ok(AcquireOutcome::NotConfigured);
        };

        let demand = ensure_demand_tx(&tx, holder, lane, "", now, now, DemandState::Eligible)?;
        if demand.state == DemandState::Terminal || demand.state == DemandState::Cancelled {
            return Ok(AcquireOutcome::Closed);
        }
        if demand.state == DemandState::Granted {
            // A granted demand without its permit can only come from an
            // interrupted older release path. It is eligible again because
            // no capacity is currently held for it.
            tx.execute(
                "UPDATE permit_demands SET state = 'eligible', updated_unix = ?1
                 WHERE holder = ?2",
                params![i64::try_from(now).unwrap_or(i64::MAX), holder],
            )?;
        } else if demand.state == DemandState::Waiting {
            // Redelivery revives a parked demand at its original age.
            tx.execute(
                "UPDATE permit_demands SET state = 'eligible', updated_unix = ?1
                 WHERE holder = ?2",
                params![i64::try_from(now).unwrap_or(i64::MAX), holder],
            )?;
        } else {
            tx.execute(
                "UPDATE permit_demands SET updated_unix = ?1 WHERE holder = ?2",
                params![i64::try_from(now).unwrap_or(i64::MAX), holder],
            )?;
        }
        let occupied: i64 = tx.query_row("SELECT COUNT(*) FROM permits", [], |row| row.get(0))?;
        if occupied.max(0) as u64 >= u64::from(max) {
            tx.commit()?;
            return Ok(AcquireOutcome::Full);
        }
        let fresh_after = now.saturating_sub(DEMAND_STALE_AFTER_SECS);
        let oldest: String = tx.query_row(
            "SELECT holder FROM permit_demands WHERE state = 'eligible'
               AND updated_unix > ?1
             ORDER BY first_seen_unix, sequence LIMIT 1",
            params![i64::try_from(fresh_after).unwrap_or(i64::MAX)],
            |row| row.get(0),
        )?;
        if oldest != holder {
            tx.commit()?;
            return Ok(AcquireOutcome::Deferred);
        }
        let now_i64 = i64::try_from(now).unwrap_or(i64::MAX);
        tx.execute(
            "INSERT INTO permits (holder, lane, state, acquired_unix, updated_unix, generation, pid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                holder,
                lane.as_str(),
                state.as_str(),
                now_i64,
                now_i64,
                i64::try_from(generation).unwrap_or(i64::MAX),
                pid.map(i64::from),
            ],
        )?;
        tx.execute(
            "UPDATE permit_demands SET state = 'granted', updated_unix = ?1
             WHERE holder = ?2",
            params![now_i64, holder],
        )?;
        tx.commit()?;
        Ok(AcquireOutcome::Acquired)
    }

    /// Adopt one holder's row when its acquiring process is dead and the
    /// row belongs to `lane`, fenced on `generation`.
    ///
    /// Crash-redelivery convergence: the attempt that acquired the permit
    /// died, and the redelivered attempt takes over the same row (same
    /// holder, new pid and state) instead of spending a second permit or
    /// executing rowless. Occupancy is unchanged. A live pid, a missing
    /// pid, a row in another lane, or a reused pid (which reads as alive)
    /// all refuse the adoption: the error direction is retention.
    pub fn adopt_if_pid_dead(
        &mut self,
        holder: &str,
        lane: PermitLane,
        state: PermitState,
        generation: u64,
        pid: u32,
        is_alive: &dyn Fn(u32) -> bool,
    ) -> Result<AdoptOutcome, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if current.max(0) as u64 != generation {
            return Ok(AdoptOutcome::StaleGeneration);
        }
        let row: Option<(String, Option<i64>)> = tx
            .query_row(
                "SELECT lane, pid FROM permits WHERE holder = ?1",
                params![holder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((row_lane, row_pid)) = row else {
            return Ok(AdoptOutcome::Missing);
        };
        if row_lane != lane.as_str() {
            return Ok(AdoptOutcome::LiveHolder);
        }
        let Some(row_pid) = row_pid.and_then(|pid| u32::try_from(pid).ok()) else {
            return Ok(AdoptOutcome::LiveHolder);
        };
        if is_alive(row_pid) {
            return Ok(AdoptOutcome::LiveHolder);
        }
        let now = unix_now();
        tx.execute(
            "UPDATE permits SET state = ?1, updated_unix = ?2, generation = ?3, pid = ?4
             WHERE holder = ?5",
            params![
                state.as_str(),
                i64::try_from(now).unwrap_or(i64::MAX),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(pid),
                holder,
            ],
        )?;
        let demand = ensure_demand_tx(&tx, holder, lane, "", now, now, DemandState::Granted)?;
        if demand.state == DemandState::Eligible {
            tx.execute(
                "UPDATE permit_demands SET state = 'granted', updated_unix = ?1
                 WHERE holder = ?2",
                params![i64::try_from(now).unwrap_or(i64::MAX), holder],
            )?;
        }
        tx.commit()?;
        Ok(AdoptOutcome::Adopted)
    }

    /// Move one held permit to a new state, fenced on `generation`.
    /// Occupancy is unchanged: every state counts.
    pub fn transition(
        &mut self,
        holder: &str,
        state: PermitState,
        generation: u64,
    ) -> Result<(), LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if current.max(0) as u64 != generation {
            return Err(LedgerError::StaleGeneration {
                expected: current.max(0) as u64,
                seen: generation,
            });
        }
        let recorded: Option<i64> = tx
            .query_row(
                "SELECT generation FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?;
        let Some(recorded) = recorded else {
            return Err(LedgerError::UnknownHolder(holder.to_string()));
        };
        if recorded.max(0) as u64 != generation {
            return Err(LedgerError::StaleGeneration {
                expected: recorded.max(0) as u64,
                seen: generation,
            });
        }
        let now = unix_now() as i64;
        let updated = tx.execute(
            "UPDATE permits SET state = ?1, updated_unix = ?2, generation = ?3
             WHERE holder = ?4 AND generation = ?3",
            params![
                state.as_str(),
                now,
                i64::try_from(generation).unwrap_or(i64::MAX),
                holder,
            ],
        )?;
        if updated == 0 {
            return Err(LedgerError::UnknownHolder(holder.to_string()));
        }
        tx.commit()?;
        Ok(())
    }

    /// Confirm terminal owned cleanup, then atomically release the permit
    /// and close its demand. Unfenced by design: a worker from an earlier
    /// daemon epoch must be able to release its own completed hold.
    pub fn release(&mut self, holder: &str) -> Result<bool, LedgerError> {
        self.release_with_demand_state(holder, DemandState::Terminal)
    }

    /// Release a permit after confirmed handoff/retry cleanup and return
    /// its demand to the queue without changing its original age.
    pub fn release_to_eligible(&mut self, holder: &str) -> Result<bool, LedgerError> {
        self.release_with_demand_state(holder, DemandState::Eligible)
    }

    /// Release a permit after confirmed upstream cancellation and ensure
    /// the demand cannot block later work.
    pub fn release_cancelled(&mut self, holder: &str) -> Result<bool, LedgerError> {
        self.release_with_demand_state(holder, DemandState::Cancelled)
    }

    /// Confirm terminal owned cleanup, then release only the permit owned by
    /// `generation`. This is the worker-lifecycle release primitive: a stale
    /// worker cannot free a permit adopted by a newer epoch or close its
    /// demand row.
    pub fn release_fenced(&mut self, holder: &str, generation: u64) -> Result<bool, LedgerError> {
        self.release_with_demand_state_fenced(holder, DemandState::Terminal, generation)
    }

    /// Fenced retry/handoff release. The demand is returned to the queue
    /// only when the permit row belongs to the supplied epoch.
    pub fn release_to_eligible_fenced(
        &mut self,
        holder: &str,
        generation: u64,
    ) -> Result<bool, LedgerError> {
        self.release_with_demand_state_fenced(holder, DemandState::Eligible, generation)
    }

    /// Fenced cancellation release. A stale cancellation cannot close a new
    /// epoch's demand row.
    pub fn release_cancelled_fenced(
        &mut self,
        holder: &str,
        generation: u64,
    ) -> Result<bool, LedgerError> {
        self.release_with_demand_state_fenced(holder, DemandState::Cancelled, generation)
    }

    fn release_with_demand_state(
        &mut self,
        holder: &str,
        next_demand_state: DemandState,
    ) -> Result<bool, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        let removed = tx.execute("DELETE FROM permits WHERE holder = ?1", params![holder])?;
        match next_demand_state {
            DemandState::Terminal | DemandState::Cancelled => {
                tx.execute(
                    "UPDATE permit_demands SET state = ?1, updated_unix = ?2
                     WHERE holder = ?3",
                    params![next_demand_state.as_str(), now, holder],
                )?;
            }
            DemandState::Eligible => {
                // Never revive work that has already been closed.
                tx.execute(
                    "UPDATE permit_demands SET state = 'eligible', updated_unix = ?1
                     WHERE holder = ?2 AND state IN ('eligible', 'granted')",
                    params![now, holder],
                )?;
            }
            // A release never parks: a held permit implies a granted
            // demand, which a waiting row cannot accompany.
            DemandState::Granted | DemandState::Waiting => {}
        }
        tx.commit()?;
        Ok(removed > 0)
    }

    fn release_with_demand_state_fenced(
        &mut self,
        holder: &str,
        next_demand_state: DemandState,
        generation: u64,
    ) -> Result<bool, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let current = current.max(0) as u64;
        if current != generation {
            return Err(LedgerError::StaleGeneration {
                expected: current,
                seen: generation,
            });
        }

        let recorded: Option<i64> = tx
            .query_row(
                "SELECT generation FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?;
        let Some(recorded) = recorded else {
            // Idempotent release. Crucially, do not mutate demand when the
            // permit is absent: a stale terminal event must not close a new
            // demand with the same holder string.
            tx.commit()?;
            return Ok(false);
        };
        let recorded = recorded.max(0) as u64;
        if recorded != generation {
            return Err(LedgerError::StaleGeneration {
                expected: recorded,
                seen: generation,
            });
        }

        let now = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        let removed = tx.execute(
            "DELETE FROM permits WHERE holder = ?1 AND generation = ?2",
            params![holder, i64::try_from(generation).unwrap_or(i64::MAX)],
        )?;
        if removed == 0 {
            tx.commit()?;
            return Ok(false);
        }
        match next_demand_state {
            DemandState::Terminal | DemandState::Cancelled => {
                tx.execute(
                    "UPDATE permit_demands SET state = ?1, updated_unix = ?2
                     WHERE holder = ?3",
                    params![next_demand_state.as_str(), now, holder],
                )?;
            }
            DemandState::Eligible => {
                tx.execute(
                    "UPDATE permit_demands SET state = 'eligible', updated_unix = ?1
                     WHERE holder = ?2 AND state IN ('eligible', 'granted')",
                    params![now, holder],
                )?;
            }
            DemandState::Granted | DemandState::Waiting => {}
        }
        tx.commit()?;
        Ok(true)
    }

    /// Retain an uncertain permit after cleanup could not be confirmed.
    /// The permit and demand transition commit together so failure cannot
    /// expose false capacity or leave a served demand blocking the queue.
    pub fn retain_uncertain(&mut self, holder: &str, generation: u64) -> Result<(), LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if current.max(0) as u64 != generation {
            return Err(LedgerError::StaleGeneration {
                expected: current.max(0) as u64,
                seen: generation,
            });
        }
        let now = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        let updated = tx.execute(
            "UPDATE permits SET state = 'uncertain', updated_unix = ?1, generation = ?2
             WHERE holder = ?3",
            params![now, i64::try_from(generation).unwrap_or(i64::MAX), holder],
        )?;
        if updated == 0 {
            return Err(LedgerError::UnknownHolder(holder.to_owned()));
        }
        tx.execute(
            "UPDATE permit_demands SET state = 'terminal', updated_unix = ?1
             WHERE holder = ?2 AND state IN ('eligible', 'granted')",
            params![now, holder],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Reconcile durable occupancy against observed live work, and mark this
    /// epoch reconciled so capacity may be advertised.
    ///
    /// `alive` is the caller's attested live set: `(holder, lane, state)`.
    /// Observed holders without a row are adopted as counted occupancy;
    /// recorded holders outside the set are marked uncertain (still
    /// counted). Nothing is ever deleted here.
    pub fn reconcile(
        &mut self,
        alive: &[(&str, PermitLane, PermitState)],
    ) -> Result<ReconcileReport, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let generation: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let mut report = ReconcileReport::default();
        let now = unix_now() as i64;
        for (holder, lane, state) in alive {
            let held: Option<String> = tx
                .query_row(
                    "SELECT holder FROM permits WHERE holder = ?1",
                    params![*holder],
                    |row| row.get(0),
                )
                .optional()?;
            if held.is_none() {
                tx.execute(
                    "INSERT INTO permits (holder, lane, state, acquired_unix, updated_unix, generation)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![*holder, lane.as_str(), state.as_str(), now, now, generation],
                )?;
                report.adopted.push((*holder).to_string());
            } else {
                tx.execute(
                    "UPDATE permits SET lane = ?1, state = ?2, updated_unix = ?3,
                            generation = ?4 WHERE holder = ?5",
                    params![lane.as_str(), state.as_str(), now, generation, *holder,],
                )?;
                report.confirmed.push((*holder).to_string());
            }
            let demand = ensure_demand_tx(
                &tx,
                holder,
                *lane,
                "",
                now.max(0) as u64,
                now.max(0) as u64,
                DemandState::Granted,
            )?;
            if demand.state == DemandState::Eligible {
                tx.execute(
                    "UPDATE permit_demands SET state = 'granted', updated_unix = ?1
                     WHERE holder = ?2",
                    params![now, holder],
                )?;
            }
        }
        let mut select = tx.prepare("SELECT holder, lane FROM permits")?;
        let recorded: Vec<(String, String)> = select
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        drop(select);
        for (holder, raw_lane) in &recorded {
            if alive.iter().any(|(live, _, _)| live == holder) {
                continue;
            }
            let lane = PermitLane::parse(raw_lane)
                .ok_or_else(|| LedgerError::UnknownLane(raw_lane.clone()))?;
            tx.execute(
                "UPDATE permits SET state = 'uncertain', updated_unix = ?1, generation = ?2
                 WHERE holder = ?3",
                params![now, generation, holder],
            )?;
            let demand = ensure_demand_tx(
                &tx,
                holder,
                lane,
                "",
                now.max(0) as u64,
                now.max(0) as u64,
                DemandState::Granted,
            )?;
            if demand.state == DemandState::Eligible {
                tx.execute(
                    "UPDATE permit_demands SET state = 'granted', updated_unix = ?1
                     WHERE holder = ?2",
                    params![now, holder],
                )?;
            }
            report.marked_uncertain.push(holder.clone());
        }
        tx.execute(
            "UPDATE permit_meta SET reconciled_generation = generation WHERE id = 1",
            [],
        )?;
        tx.commit()?;
        report.adopted.sort();
        report.marked_uncertain.sort();
        report.confirmed.sort();
        Ok(report)
    }

    /// Release uncertain native permits whose acquiring process is dead.
    ///
    /// This is the only path that deletes without an explicit release, and
    /// it is narrow on purpose: only `uncertain` rows in the native lane
    /// with a recorded pid for which `is_alive` returns false, and never a
    /// holder in `protected` (the caller's in-flight set — a cleanup
    /// failure retains its visible reservation until lifecycle
    /// reconciliation converges it, even across restarts).
    ///
    /// A reused pid reads as alive and skips the sweep: the error direction
    /// is retention, never a double-spend. Returns the swept holders.
    pub fn sweep_dead_uncertain(
        &mut self,
        is_alive: &dyn Fn(u32) -> bool,
        protected: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<String>, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let candidates: Vec<(String, i64)> = {
            let mut select = tx.prepare(
                "SELECT holder, pid FROM permits
                 WHERE state = 'uncertain' AND lane = 'native' AND pid IS NOT NULL",
            )?;
            select
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<_, _>>()?
        };
        let mut swept = Vec::new();
        for (holder, pid) in candidates {
            if protected.contains(&holder) {
                continue;
            }
            let Ok(pid) = u32::try_from(pid) else {
                continue;
            };
            if is_alive(pid) {
                continue;
            }
            tx.execute("DELETE FROM permits WHERE holder = ?1", params![holder])?;
            swept.push(holder);
        }
        tx.commit()?;
        swept.sort();
        Ok(swept)
    }

    /// Number of permits in `state` (observability; every state counts).
    pub fn occupied_in_state(&self, state: PermitState) -> Result<u32, LedgerError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM permits WHERE state = ?1",
            params![state.as_str()],
            |row| row.get(0),
        )?;
        Ok(u32::try_from(count.max(0)).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    fn temp_ledger(name: &str) -> (PermitLedger, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "velnor-permit-ledger-{name}-{}-{}",
            std::process::id(),
            unix_now(),
        ));
        let path = dir.join("permit-ledger.db");
        let ledger = PermitLedger::open(&path).unwrap();
        (ledger, dir)
    }

    #[test]
    fn acquire_grants_until_full_then_refuses() {
        let (mut ledger, dir) = temp_ledger("full");
        ledger.set_max_jobs(2).unwrap();
        let generation = ledger.generation().unwrap();

        assert_eq!(
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            ledger
                .acquire(
                    "b",
                    PermitLane::Native,
                    PermitState::Running,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(ledger.occupied().unwrap(), 2);
        // A third holder is refused; nothing was spent.
        assert_eq!(
            ledger
                .acquire(
                    "c",
                    PermitLane::Native,
                    PermitState::Reserved,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Full
        );
        assert_eq!(ledger.occupied().unwrap(), 2);

        // Lanes share the one N: a Scale Set holder is refused too.
        assert_eq!(
            ledger
                .acquire(
                    "d",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Full
        );
        assert_eq!(ledger.occupied_by_lane(PermitLane::ScaleSet).unwrap(), 0);

        assert!(ledger.release("a").unwrap());
        // The older queued native demand gets the newly freed permit first.
        assert_eq!(
            ledger
                .acquire(
                    "d",
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Deferred
        );
        assert_eq!(
            ledger
                .acquire(
                    "c",
                    PermitLane::Native,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert!(ledger.release("b").unwrap());
        assert_eq!(
            ledger
                .acquire(
                    "d",
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(ledger.occupied_by_lane(PermitLane::ScaleSet).unwrap(), 1);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parked_demand_yields_queue_and_revives_with_age() {
        let (mut ledger, dir) = temp_ledger("park");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();
        let now = unix_now();

        // Older demand parks (its waiter departed): it keeps its ticket
        // but no longer head-blocks younger demand.
        ledger
            .observe_demand("older", PermitLane::Native, "", now, now)
            .unwrap();
        let ticket = ledger.demand("older").unwrap().unwrap();
        assert!(ledger.park_demand("older").unwrap());
        let parked = ledger.demand("older").unwrap().unwrap();
        assert_eq!(parked.state, DemandState::Waiting);
        assert_eq!(parked.first_seen_unix, ticket.first_seen_unix);
        assert_eq!(parked.sequence, ticket.sequence);

        // A younger holder grants past the parked head with free capacity.
        assert_eq!(
            ledger
                .acquire(
                    "younger",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );

        // Redelivery revives the parked demand at its original age: it
        // heads the queue again once capacity frees.
        assert!(ledger.release("younger").unwrap());
        assert_eq!(
            ledger
                .acquire(
                    "older",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        let revived = ledger.demand("older").unwrap().unwrap();
        assert_eq!(revived.state, DemandState::Granted);
        assert_eq!(revived.first_seen_unix, ticket.first_seen_unix);
        assert_eq!(revived.sequence, ticket.sequence);

        // A granted row never parks; a parked row still cancels; a
        // cancelled row never revives.
        assert!(!ledger.park_demand("older").unwrap());
        ledger
            .observe_demand("gone", PermitLane::Native, "", now, now)
            .unwrap();
        assert!(ledger.park_demand("gone").unwrap());
        assert!(ledger.cancel_demand("gone").unwrap());
        assert_eq!(
            ledger.demand("gone").unwrap().unwrap().state,
            DemandState::Cancelled
        );
        assert_eq!(
            ledger
                .acquire(
                    "gone",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Closed
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn duplicate_delivery_holds_once() {
        let (mut ledger, dir) = temp_ledger("dup");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();

        assert_eq!(
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        // Same holder, redelivered: no second permit.
        assert_eq!(
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::AlreadyHeld
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
        // ... and the duplicate does not evict the other waiter either.
        assert_eq!(
            ledger
                .acquire(
                    "b",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Full
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn every_state_counts_and_transitions_keep_occupancy() {
        let (mut ledger, dir) = temp_ledger("states");
        ledger.set_max_jobs(8).unwrap();
        let generation = ledger.generation().unwrap();
        let states = [
            PermitState::Reserved,
            PermitState::Acquiring,
            PermitState::Provisioning,
            PermitState::Assignable,
            PermitState::Running,
            PermitState::Cleaning,
            PermitState::Uncertain,
        ];
        for (index, state) in states.iter().enumerate() {
            let holder = format!("job-{index}");
            assert_eq!(
                ledger
                    .acquire(&holder, PermitLane::Native, *state, generation, None)
                    .unwrap(),
                AcquireOutcome::Acquired
            );
        }
        assert_eq!(ledger.occupied().unwrap(), 7);
        ledger
            .transition("job-0", PermitState::Running, generation)
            .unwrap();
        assert_eq!(ledger.occupied().unwrap(), 7);
        assert_eq!(
            ledger.holder_state("job-0").unwrap(),
            Some(PermitState::Running)
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_generations_are_rejected_but_release_is_not_fenced() {
        let (mut ledger, dir) = temp_ledger("generation");
        ledger.set_max_jobs(2).unwrap();
        let stale = ledger.generation().unwrap();
        let current = ledger.begin_epoch().unwrap();
        assert!(current > stale);

        assert_eq!(
            ledger
                .acquire("a", PermitLane::Native, PermitState::Acquiring, stale, None)
                .unwrap(),
            AcquireOutcome::StaleGeneration
        );
        assert_eq!(
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    current,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert!(matches!(
            ledger.transition("a", PermitState::Running, stale),
            Err(LedgerError::StaleGeneration { .. })
        ));
        // Release always frees, whatever epoch the hold came from.
        assert!(ledger.release("a").unwrap());
        assert!(!ledger.release("a").unwrap());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fenced_release_rejects_stale_epoch_and_preserves_new_owner() {
        let (mut ledger, dir) = temp_ledger("fenced-release");
        ledger.set_max_jobs(1).unwrap();
        let stale = ledger.generation().unwrap();
        let holder = "scaleset/7/44";
        assert_eq!(
            ledger
                .acquire(
                    holder,
                    PermitLane::ScaleSet,
                    PermitState::Running,
                    stale,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );

        let current = ledger.begin_epoch().unwrap();
        // Startup reconciliation transfers the durable live owner to the
        // current epoch before the new lane can mutate it.
        ledger
            .reconcile(&[(holder, PermitLane::ScaleSet, PermitState::Running)])
            .unwrap();

        assert!(matches!(
            ledger.release_fenced(holder, stale),
            Err(LedgerError::StaleGeneration { .. })
        ));
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.holder_state(holder).unwrap(),
            Some(PermitState::Running)
        );
        assert!(ledger.release_fenced(holder, current).unwrap());
        assert_eq!(ledger.occupied().unwrap(), 0);
        assert_eq!(
            ledger.demand(holder).unwrap().unwrap().state,
            DemandState::Terminal
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oldest_eligible_is_granted_across_lanes_and_redelivery_keeps_age() {
        let (mut ledger, dir) = temp_ledger("global-order");
        ledger.set_max_jobs(2).unwrap();
        let generation = ledger.generation().unwrap();
        let now = unix_now();
        ledger
            .observe_demand("native/older", PermitLane::Native, "scope-a", now, now)
            .unwrap();
        let younger = ledger
            .observe_demand(
                "scaleset/7/younger",
                PermitLane::ScaleSet,
                "set-7",
                now + 1,
                now + 1,
            )
            .unwrap();
        let original = (younger.first_seen_unix, younger.sequence);

        // A younger Scale Set lane has spare capacity but cannot pass the
        // older native demand before the shared grant transaction commits.
        assert_eq!(
            ledger
                .acquire(
                    "scaleset/7/younger",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Deferred
        );
        let redelivered = ledger
            .observe_demand(
                "scaleset/7/younger",
                PermitLane::ScaleSet,
                "set-7",
                now.saturating_sub(100),
                now + 2,
            )
            .unwrap();
        assert_eq!(
            (redelivered.first_seen_unix, redelivered.sequence),
            original
        );

        assert_eq!(
            ledger
                .acquire(
                    "native/older",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            ledger.demand("native/older").unwrap().unwrap().state,
            DemandState::Granted
        );
        // Once the oldest row owns a permit, it leaves the eligible queue;
        // another free host slot is available to the next demand.
        assert_eq!(
            ledger
                .acquire(
                    "scaleset/7/younger",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_acquire_cannot_let_younger_cross_older() {
        use std::sync::{Arc, Barrier};

        let (mut ledger, dir) = temp_ledger("concurrent-order");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();
        let now = unix_now();
        ledger
            .observe_demand("native/older", PermitLane::Native, "", now, now)
            .unwrap();
        ledger
            .observe_demand(
                "scaleset/7/younger",
                PermitLane::ScaleSet,
                "",
                now + 1,
                now + 1,
            )
            .unwrap();
        drop(ledger);

        let barrier = Arc::new(Barrier::new(3));
        let older_barrier = Arc::clone(&barrier);
        let older_path = dir.join("permit-ledger.db");
        let older = std::thread::spawn(move || {
            let mut ledger = PermitLedger::open(&older_path).unwrap();
            older_barrier.wait();
            ledger
                .acquire(
                    "native/older",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None,
                )
                .unwrap()
        });
        let younger_barrier = Arc::clone(&barrier);
        let younger_path = dir.join("permit-ledger.db");
        let younger = std::thread::spawn(move || {
            let mut ledger = PermitLedger::open(&younger_path).unwrap();
            younger_barrier.wait();
            ledger
                .acquire(
                    "scaleset/7/younger",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap()
        });
        barrier.wait();

        assert_eq!(older.join().unwrap(), AcquireOutcome::Acquired);
        assert!(matches!(
            younger.join().unwrap(),
            AcquireOutcome::Full | AcquireOutcome::Deferred
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retry_release_preserves_age_and_uncertain_cleanup_keeps_occupancy() {
        let (mut ledger, dir) = temp_ledger("release-order");
        ledger.set_max_jobs(2).unwrap();
        let generation = ledger.generation().unwrap();
        let now = unix_now();
        let original = ledger
            .observe_demand("native/first", PermitLane::Native, "", now, now)
            .unwrap();
        ledger
            .observe_demand(
                "scaleset/7/second",
                PermitLane::ScaleSet,
                "",
                now + 1,
                now + 1,
            )
            .unwrap();
        assert_eq!(
            ledger
                .acquire(
                    "native/first",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert!(ledger.release_to_eligible("native/first").unwrap());
        assert_eq!(
            ledger
                .demand("native/first")
                .unwrap()
                .unwrap()
                .first_seen_unix,
            original.first_seen_unix
        );
        assert_eq!(
            ledger
                .acquire(
                    "scaleset/7/second",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Deferred
        );
        assert_eq!(
            ledger
                .acquire(
                    "native/first",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert!(ledger.retain_uncertain("native/first", generation).is_ok());
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.demand("native/first").unwrap().unwrap().state,
            DemandState::Terminal
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_migrates_native_demand_into_global_sequence() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-permit-ledger-migrate-{}-{}",
            std::process::id(),
            unix_now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("permit-ledger.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE native_demand (
                    request_id TEXT PRIMARY KEY,
                    scope TEXT NOT NULL,
                    first_seen_unix INTEGER NOT NULL,
                    sequence INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    updated_unix INTEGER NOT NULL
                );
                INSERT INTO native_demand VALUES
                    ('legacy-request', 'scope-a', 100, 8, 'eligible', 120);",
            )
            .unwrap();
        }
        let ledger = PermitLedger::open(&path).unwrap();
        let demand = ledger.demand("native/legacy-request").unwrap().unwrap();
        assert_eq!(demand.lane, PermitLane::Native);
        assert_eq!(demand.scope, "scope-a");
        assert_eq!(demand.first_seen_unix, 100);
        assert_eq!(demand.state, DemandState::Eligible);
        let legacy_exists: bool = ledger
            .conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'native_demand'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!legacy_exists);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unconfigured_ledgers_grant_nothing() {
        let (mut ledger, dir) = temp_ledger("unconfigured");
        let generation = ledger.generation().unwrap();
        assert_eq!(ledger.max_jobs().unwrap(), None);
        assert_eq!(
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::NotConfigured
        );
        assert_eq!(ledger.occupied().unwrap(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reconcile_adopts_marks_and_gates_advertisement() {
        let (mut ledger, dir) = temp_ledger("reconcile");
        ledger.set_max_jobs(4).unwrap();
        let generation = ledger.generation().unwrap();
        ledger
            .acquire(
                "old",
                PermitLane::Native,
                PermitState::Running,
                generation,
                None,
            )
            .unwrap();
        // Nothing advertised before the first reconcile.
        assert_eq!(ledger.advertised_free().unwrap(), None);

        let report = ledger
            .reconcile(&[("new", PermitLane::Native, PermitState::Running)])
            .unwrap();
        assert_eq!(report.adopted, vec!["new".to_string()]);
        assert_eq!(report.marked_uncertain, vec!["old".to_string()]);
        assert!(report.confirmed.is_empty());
        // Adopted and uncertain rows both count; nothing was erased.
        assert_eq!(ledger.occupied().unwrap(), 2);
        assert_eq!(
            ledger.holder_state("old").unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(ledger.advertised_free().unwrap(), Some(2));

        // A new epoch requires a fresh reconcile before advertising.
        ledger.begin_epoch().unwrap();
        assert_eq!(ledger.advertised_free().unwrap(), None);
        let report = ledger
            .reconcile(&[
                ("new", PermitLane::Native, PermitState::Running),
                ("old", PermitLane::Native, PermitState::Cleaning),
            ])
            .unwrap();
        assert!(report.adopted.is_empty());
        assert!(report.marked_uncertain.is_empty());
        assert_eq!(report.confirmed.len(), 2);
        assert_eq!(ledger.advertised_free().unwrap(), Some(2));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adopt_takes_over_dead_attempts_and_refuses_the_rest() {
        let (mut ledger, dir) = temp_ledger("adopt");
        ledger.set_max_jobs(4).unwrap();
        let generation = ledger.generation().unwrap();
        ledger
            .acquire(
                "dead",
                PermitLane::Native,
                PermitState::Running,
                generation,
                Some(101),
            )
            .unwrap();
        ledger
            .acquire(
                "live",
                PermitLane::Native,
                PermitState::Running,
                generation,
                Some(102),
            )
            .unwrap();
        ledger
            .acquire(
                "noid",
                PermitLane::Native,
                PermitState::Running,
                generation,
                None,
            )
            .unwrap();
        ledger
            .acquire(
                "official",
                PermitLane::ScaleSet,
                PermitState::Running,
                generation,
                Some(103),
            )
            .unwrap();

        let is_alive = |pid: u32| pid == 102;
        assert_eq!(
            ledger
                .adopt_if_pid_dead(
                    "dead",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    999,
                    &is_alive,
                )
                .unwrap(),
            AdoptOutcome::Adopted
        );
        // Same row, new pid and state; occupancy unchanged.
        assert_eq!(ledger.occupied().unwrap(), 4);
        assert_eq!(
            ledger.holder_state("dead").unwrap(),
            Some(PermitState::Acquiring)
        );
        let holders = ledger.holders().unwrap();
        assert_eq!(
            holders
                .iter()
                .find(|holder| holder.holder == "dead")
                .unwrap()
                .pid,
            Some(999)
        );
        // Live pid, missing pid, foreign lane, and missing row all refuse.
        assert_eq!(
            ledger
                .adopt_if_pid_dead(
                    "live",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    999,
                    &is_alive,
                )
                .unwrap(),
            AdoptOutcome::LiveHolder
        );
        assert_eq!(
            ledger
                .adopt_if_pid_dead(
                    "noid",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    999,
                    &is_alive,
                )
                .unwrap(),
            AdoptOutcome::LiveHolder
        );
        assert_eq!(
            ledger
                .adopt_if_pid_dead(
                    "official",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    999,
                    &is_alive,
                )
                .unwrap(),
            AdoptOutcome::LiveHolder
        );
        assert_eq!(
            ledger
                .adopt_if_pid_dead(
                    "gone",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    999,
                    &is_alive,
                )
                .unwrap(),
            AdoptOutcome::Missing
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sweep_releases_only_dead_unprotected_uncertain_natives() {
        use std::collections::BTreeSet;
        let (mut ledger, dir) = temp_ledger("sweep");
        ledger.set_max_jobs(8).unwrap();
        let generation = ledger.generation().unwrap();
        // Dead pid does not prove owned teardown completed.
        ledger
            .acquire(
                "dead",
                PermitLane::Native,
                PermitState::Uncertain,
                generation,
                Some(1),
            )
            .unwrap();
        // Live pid: retained even though uncertain.
        ledger
            .acquire(
                "live",
                PermitLane::Native,
                PermitState::Uncertain,
                generation,
                Some(2),
            )
            .unwrap();
        // Dead pid but protected (in-flight elsewhere): retained.
        ledger
            .acquire(
                "kept",
                PermitLane::Native,
                PermitState::Uncertain,
                generation,
                Some(3),
            )
            .unwrap();
        // Dead pid but still running (never marked uncertain): retained.
        ledger
            .acquire(
                "running",
                PermitLane::Native,
                PermitState::Running,
                generation,
                Some(4),
            )
            .unwrap();
        // Dead pid but no pid recorded: retained.
        ledger
            .acquire(
                "noid",
                PermitLane::Native,
                PermitState::Uncertain,
                generation,
                None,
            )
            .unwrap();
        // Dead pid but Scale Set lane: never swept here.
        ledger
            .acquire(
                "official",
                PermitLane::ScaleSet,
                PermitState::Uncertain,
                generation,
                Some(5),
            )
            .unwrap();

        let is_alive = |pid: u32| pid == 2;
        let protected: BTreeSet<String> = ["kept".to_string()].into_iter().collect();
        assert_eq!(
            ledger.sweep_dead_uncertain(&is_alive, &protected).unwrap(),
            vec!["dead".to_string()]
        );
        assert_eq!(ledger.occupied().unwrap(), 5);
        assert!(ledger.holder_state("dead").unwrap().is_none());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn occupancy_survives_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-permit-ledger-reopen-{}-{}",
            std::process::id(),
            unix_now()
        ));
        let path = dir.join("permit-ledger.db");
        let generation = {
            let mut ledger = PermitLedger::open(&path).unwrap();
            ledger.set_max_jobs(3).unwrap();
            let generation = ledger.generation().unwrap();
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Running,
                    generation,
                    None,
                )
                .unwrap();
            generation
        };
        let mut ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.max_jobs().unwrap(), Some(3));
        assert_eq!(ledger.generation().unwrap(), generation);
        assert_eq!(ledger.occupied().unwrap(), 1);
        // Capacity cannot exceed N after crash recovery: the surviving row
        // still counts against fresh acquisitions.
        assert_eq!(
            ledger
                .acquire(
                    "b",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            ledger
                .acquire(
                    "c",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            ledger
                .acquire(
                    "d",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Full
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

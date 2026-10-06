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
//! * Grants and transitions are generation-fenced: [`PermitLedger::begin_epoch`]
//!   bumps the generation at daemon startup, and stale callers are rejected.
//!   Guard-owned transitions and releases use a per-attempt token, so delayed
//!   work from an old holder cannot mutate a redelivery's adopted row.
//!   Recorded-job recovery persists and uses that same token.
//! * Capacity is advertised only after reconciliation:
//!   [`PermitLedger::advertised_free`] returns `None` until
//!   [`PermitLedger::reconcile_attempts`] has run in the current epoch.
//!   Reconciliation never deletes permit rows. Live observations require an
//!   existing permit row with the exact owner
//!   token; observed-but-unrecorded work fails stale and is not adopted.
//!   Recorded-but-unobserved work stays counted and is marked
//!   [`PermitState::Uncertain`] unless an active cleanup claim protects it.
//!   Reconciliation creates a missing demand as granted and promotes an
//!   eligible demand to granted; cleanup failures retain visible reservations.
//! * Acquisition returns an owner token only for a fresh grant. A duplicate
//!   delivery returns [`AcquireAttemptOutcome::AlreadyHeld`] without granting
//!   mutation authority. A fresh grant atomically changes the oldest eligible
//!   demand to granted while inserting its permit.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, Transaction};

/// One `permits` row read for attempt rotation: lane, attempt token, owner
/// pid, generation, recovery claim token, previous pid, previous token.
type RotationPermitRow = (
    String,
    String,
    Option<i64>,
    i64,
    Option<String>,
    Option<i64>,
    Option<String>,
);

/// One `permits` row read for recovery verification: lane, attempt token,
/// recovery claim token, previous pid, previous token.
type HeldPermitRow = (String, String, Option<String>, Option<i64>, Option<String>);

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

/// First pause between [`AcquireAttemptOutcome::Deferred`] retries; doubles per
/// consecutive wait.
pub const DEFERRED_WAIT_MIN: Duration = Duration::from_millis(1);

/// Backoff cap between [`AcquireAttemptOutcome::Deferred`] retries.
pub const DEFERRED_WAIT_MAX: Duration = Duration::from_millis(50);

/// Pause before retrying after `attempt` consecutive [`AcquireAttemptOutcome::Deferred`]
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
    /// Host pid of the current owner process, when the lane records one.
    /// Same-holder redelivery may adopt a dead attempt or a terminal uncertain
    /// attempt retained by this process; startup never uses local pid or root
    /// evidence alone to erase another daemon's row.
    pub pid: Option<u32>,
}

/// Shared Scale Set recovery ownership for one ledger. A dead owner may be
/// taken over without changing the claim token, so a staged ledger rotation
/// can prove that its prior commit belongs to this same recovery claim.
#[derive(Debug)]
pub struct ScaleSetRecoveryClaim {
    path: PathBuf,
    token: String,
    owner_pid: u32,
}

impl ScaleSetRecoveryClaim {
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Release only this claim after recovery and supervision have completed.
    /// Dropping a claim without this explicit call leaves a dead-pid-reclaimable
    /// row, preserving the fencing token across a crash cut.
    pub fn release(&mut self) -> Result<(), LedgerError> {
        let mut ledger = PermitLedger::open(&self.path)?;
        let tx = ledger
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "DELETE FROM scaleset_recovery_claims
             WHERE id = 1 AND claim_token = ?1 AND owner_pid = ?2",
            params![self.token, i64::from(self.owner_pid)],
        )?;
        if changed != 1 {
            return Err(LedgerError::StaleAttempt(
                "Scale Set recovery claim".to_owned(),
            ));
        }
        tx.commit()?;
        self.token.clear();
        Ok(())
    }
}

/// Outcome of [`PermitLedger::adopt_for_redelivery`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptOutcome {
    /// The previous attempt's row was adopted with a fresh owner token.
    Adopted { attempt_token: String },
    /// The holding attempt may still be active (or belongs to another
    /// lane): not adopted.
    LiveHolder,
    /// The row vanished between calls; retry the acquire.
    Missing,
    /// The caller fenced on a stale generation; re-read and retry.
    StaleGeneration,
}

/// Result of fencing a recorded terminal cleanup against a concurrently
/// arriving acquisition for the same holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupClaimOutcome {
    /// The recorded attempt still owned the permit, which is now claimed by
    /// this process for cleanup.
    Claimed,
    /// No permit row existed and its demand was already closed. Cleanup may
    /// release local reservations and remove the exact marker.
    ClosedAbsent,
    /// No permit row existed, so an open demand was atomically closed to stop
    /// a concurrent fresh acquire. Keep local reservations and the marker on
    /// this pass; a later recovery can continue from the closed state.
    DemandClosed,
    /// A different attempt token already owns the holder. Cleanup has made
    /// no changes and must retain its local reservation and marker.
    StaleAttempt,
}

/// Result of a staged Scale Set token rotation during proven worker recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptRotationOutcome {
    /// The current token matched the staged old token and was rotated.
    Rotated,
    /// The ledger contains the staged target plus exact prior-owner and
    /// claim proof; the owning recovery process can finish its state-database
    /// transaction idempotently.
    AlreadyRotated,
    /// The permit row no longer exists. Recovery must retain its durable
    /// stage and resources instead of recreating the permit.
    Missing,
    /// A different attempt token currently owns this holder.
    StaleAttempt,
    /// The caller supplied a stale generation and must reread.
    StaleGeneration,
}

/// Atomic result of releasing one token-owned permit attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnedReleaseOutcome {
    /// The exact token owned a permit row, which is now released.
    Released,
    /// No row remained and the demand already matches the requested closed
    /// target, or staged Scale Set recovery proved the release committed.
    AlreadyAbsent,
    /// A different attempt token owns the row. No demand or permit changed.
    StaleAttempt,
}

/// Result of a fresh owner-bearing acquisition. `AlreadyHeld` deliberately
/// does not expose the current token: an existing row is not proof that this
/// caller owns its attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireAttemptOutcome {
    /// A fresh permit was granted to this attempt.
    Acquired { attempt_token: String },
    /// The holder already has a permit; this caller did not acquire ownership.
    AlreadyHeld,
    /// `occupied >= max_jobs`; no permit was granted.
    Full,
    /// Capacity is available, but an older eligible demand must acquire first.
    Deferred,
    /// This demand was already terminal or cancelled and cannot be revived.
    Closed,
    /// The caller fenced on a stale generation; re-read and retry.
    StaleGeneration,
    /// No `max_jobs` was ever configured; refusing rather than guessing.
    NotConfigured,
}

/// What [`PermitLedger::reconcile_attempts`] confirmed or marked uncertain.
/// It never recreates a missing permit or deletes a recorded permit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
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
    StaleAttempt(String),
    RecoveryClaimed(u32),
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
            Self::StaleAttempt(holder) => write!(
                f,
                "permit attempt token for {holder:?} is missing or no longer current"
            ),
            Self::RecoveryClaimed(pid) => {
                write!(f, "Scale Set recovery is owned by live process {pid}")
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
    let legacy_exists: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'native_demand'
         )",
        [],
        |row| row.get(0),
    )?;
    if !legacy_exists {
        return Ok(());
    }

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

/// Add and backfill the per-attempt owner token on pre-token ledgers.
/// Serialize inspection, alteration, backfill, and uniqueness enforcement so
/// simultaneous daemon opens cannot observe a partially migrated schema.
fn ensure_permit_attempt_token_column(conn: &mut Connection) -> Result<(), LedgerError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS permit_token_migrations (
            holder TEXT PRIMARY KEY,
            attempt_token TEXT NOT NULL UNIQUE,
            acquired_unix INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS scaleset_recovery_claims (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            claim_token TEXT NOT NULL,
            owner_pid INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS recorded_terminal_cleanup_releases (
            holder TEXT NOT NULL,
            attempt_token TEXT NOT NULL,
            PRIMARY KEY (holder, attempt_token)
        );",
    )?;
    let (
        has_attempt_token,
        has_recovery_claim_token,
        has_recovery_previous_pid,
        has_recovery_previous_token,
    ) = {
        let mut statement = tx.prepare("PRAGMA table_info(permits)")?;
        let mut rows = statement.query([])?;
        let mut has_attempt_token = false;
        let mut has_recovery_claim_token = false;
        let mut has_recovery_previous_pid = false;
        let mut has_recovery_previous_token = false;
        while let Some(row) = rows.next()? {
            match row.get::<_, String>(1)?.as_str() {
                "attempt_token" => has_attempt_token = true,
                "recovery_claim_token" => has_recovery_claim_token = true,
                "recovery_previous_pid" => has_recovery_previous_pid = true,
                "recovery_previous_token" => has_recovery_previous_token = true,
                _ => {}
            }
        }
        (
            has_attempt_token,
            has_recovery_claim_token,
            has_recovery_previous_pid,
            has_recovery_previous_token,
        )
    };
    if !has_attempt_token {
        tx.execute_batch("ALTER TABLE permits ADD COLUMN attempt_token TEXT;")?;
    }
    if !has_recovery_claim_token {
        tx.execute_batch("ALTER TABLE permits ADD COLUMN recovery_claim_token TEXT;")?;
    }
    if !has_recovery_previous_pid {
        tx.execute_batch("ALTER TABLE permits ADD COLUMN recovery_previous_pid INTEGER;")?;
    }
    if !has_recovery_previous_token {
        tx.execute_batch("ALTER TABLE permits ADD COLUMN recovery_previous_token TEXT;")?;
    }
    let missing_tokens: Vec<(String, i64)> = {
        let mut statement = tx.prepare(
            "SELECT holder, acquired_unix FROM permits
             WHERE attempt_token IS NULL OR attempt_token = ''",
        )?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?
    };
    for (holder, acquired_unix) in missing_tokens {
        let attempt_token = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "UPDATE permits SET attempt_token = ?1 WHERE holder = ?2",
            params![attempt_token, holder],
        )?;
        // Only rows whose token column did not exist are eligible for a
        // one-time marker migration. A NULL/empty token in an already
        // tokenized schema has no trustworthy attempt provenance.
        if !has_attempt_token {
            tx.execute(
                "INSERT OR IGNORE INTO permit_token_migrations
                 (holder, attempt_token, acquired_unix) VALUES (?1, ?2, ?3)",
                params![holder, attempt_token, acquired_unix],
            )?;
        }
    }
    tx.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_permits_attempt_token
         ON permits (attempt_token);",
    )?;
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

fn read_owned_attempt_state(
    conn: &Connection,
    holder: &str,
    attempt_token: &str,
) -> Result<String, LedgerError> {
    let current: Option<(String, String)> = conn
        .query_row(
            "SELECT attempt_token, state FROM permits WHERE holder = ?1",
            params![holder],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((current_token, current_state)) = current {
        return if current_token == attempt_token {
            Ok(current_state)
        } else {
            Err(LedgerError::StaleAttempt(holder.to_owned()))
        };
    }

    let known_holder: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM permit_demands WHERE holder = ?1)",
        params![holder],
        |row| row.get(0),
    )?;
    Err(if known_holder {
        LedgerError::StaleAttempt(holder.to_owned())
    } else {
        LedgerError::UnknownHolder(holder.to_owned())
    })
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
        let schema_objects: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE (type = 'table' AND name IN (
                    'permit_meta', 'permits', 'permit_demands', 'permit_token_migrations',
                    'scaleset_recovery_claims',
                    'recorded_terminal_cleanup_releases'
                ))
                OR (type = 'index' AND name IN (
                    'idx_permit_demands_oldest', 'idx_permits_attempt_token'
                ))",
            [],
            |row| row.get(0),
        )?;
        // Avoid recreating tables and indexes when their schema and singleton
        // metadata row are complete. Counting only sqlite_master entries is
        // insufficient: another process may have removed the permit_meta
        // seed row while all eight schema objects remain.
        let meta_seeded = schema_objects == 8
            && conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM permit_meta WHERE id = 1)",
                [],
                |row| row.get(0),
            )?;
        let schema_repair_needed = schema_objects != 8 || !meta_seeded;
        if schema_repair_needed {
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
                    pid INTEGER,
                    attempt_token TEXT NOT NULL,
                    recovery_claim_token TEXT,
                    recovery_previous_pid INTEGER,
                    recovery_previous_token TEXT
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
                CREATE TABLE IF NOT EXISTS permit_token_migrations (
                    holder TEXT PRIMARY KEY,
                    attempt_token TEXT NOT NULL UNIQUE,
                    acquired_unix INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS scaleset_recovery_claims (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    claim_token TEXT NOT NULL,
                    owner_pid INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS recorded_terminal_cleanup_releases (
                    holder TEXT NOT NULL,
                    attempt_token TEXT NOT NULL,
                    PRIMARY KEY (holder, attempt_token)
                );
                CREATE INDEX IF NOT EXISTS idx_permit_demands_oldest
                    ON permit_demands (state, first_seen_unix, sequence);",
            )?;
        }
        // Run the idempotent transactional backfill on every open. A ledger
        // can have all expected schema objects while still containing NULL
        // or empty tokens from an interrupted/older migration; `sqlite_master`
        // shape alone does not prove every extant permit has an owner token.
        conn.execute_batch("DROP TABLE IF EXISTS unresolved_native_permit_protections;")?;
        ensure_permit_attempt_token_column(&mut conn)?;
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

    /// Claim the shared Scale Set recovery role for this ledger. Claims are
    /// serialized in SQLite, survive process crashes, and keep their token
    /// when a proven-dead pid is replaced. A live or same-process owner blocks
    /// another daemon from recovering or supervising the same ledger.
    pub fn claim_scaleset_recovery(
        &mut self,
        is_alive: &dyn Fn(u32) -> bool,
    ) -> Result<ScaleSetRecoveryClaim, LedgerError> {
        let current_pid = std::process::id();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<(String, i64)> = tx
            .query_row(
                "SELECT claim_token, owner_pid FROM scaleset_recovery_claims WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let token = match existing {
            Some((token, raw_pid)) => {
                if token.is_empty() {
                    return Err(LedgerError::StaleAttempt(
                        "Scale Set recovery claim token".to_owned(),
                    ));
                }
                let old_pid = u32::try_from(raw_pid).map_err(|_| {
                    LedgerError::StaleAttempt("Scale Set recovery claim owner pid".to_owned())
                })?;
                if old_pid == 0 {
                    return Err(LedgerError::StaleAttempt(
                        "Scale Set recovery claim owner pid".to_owned(),
                    ));
                }
                if is_alive(old_pid) {
                    return Err(LedgerError::RecoveryClaimed(old_pid));
                }
                let changed = tx.execute(
                    "UPDATE scaleset_recovery_claims SET owner_pid = ?1
                     WHERE id = 1 AND claim_token = ?2 AND owner_pid = ?3",
                    params![i64::from(current_pid), token, raw_pid],
                )?;
                if changed != 1 {
                    return Err(LedgerError::StaleAttempt(
                        "Scale Set recovery claim".to_owned(),
                    ));
                }
                token
            }
            None => {
                let token = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO scaleset_recovery_claims (id, claim_token, owner_pid)
                     VALUES (1, ?1, ?2)",
                    params![token, i64::from(current_pid)],
                )?;
                token
            }
        };
        tx.commit()?;
        Ok(ScaleSetRecoveryClaim {
            path: self.path.clone(),
            token,
            owner_pid: current_pid,
        })
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
    /// a fresh [`Self::reconcile_attempts`] before capacity is advertised.
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

    /// Whether [`Self::reconcile_attempts`] ran in the current generation.
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

    /// Whether a holder currently has any permit row. This observation does
    /// not grant ownership and cannot be used to mutate the row.
    pub fn has_permit(&self, holder: &str) -> Result<bool, LedgerError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM permits WHERE holder = ?1)",
            params![holder],
            |row| row.get(0),
        )?)
    }

    /// Check an attempt token without returning the current token to a caller
    /// that may not own it. Mutations must still use the token-fenced method;
    /// this read is observational only.
    pub fn is_current_attempt(
        &self,
        holder: &str,
        attempt_token: &str,
    ) -> Result<bool, LedgerError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(
                    SELECT 1 FROM permits WHERE holder = ?1 AND attempt_token = ?2
                )",
            params![holder, attempt_token],
            |row| row.get(0),
        )?)
    }

    /// Internal verification/test access. Owner tokens are returned only by
    /// successful acquire/adoption; external callers cannot recover one from
    /// a holder string after an `AlreadyHeld` result.
    #[cfg(test)]
    fn attempt_token(&self, holder: &str) -> Result<Option<String>, LedgerError> {
        Ok(self
            .conn
            .query_row(
                "SELECT attempt_token FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
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
    /// [`Self::acquire_attempt`]) revives it at its original age; without
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

    /// Acquire one permit and return its token atomically with the ownership
    /// decision. Existing holders never receive a token from this method;
    /// they must use a separate, proof-bearing recovery/adoption path.
    pub fn acquire_attempt(
        &mut self,
        holder: &str,
        lane: PermitLane,
        state: PermitState,
        generation: u64,
        pid: Option<u32>,
    ) -> Result<AcquireAttemptOutcome, LedgerError> {
        self.acquire_attempt_with_token(holder, lane, state, generation, pid, None)
    }

    /// Acquire a Scale Set permit under a token already persisted in its
    /// cross-database acquisition stage. This lets startup distinguish a
    /// committed ledger insert from the state projection that may have been
    /// interrupted. Replaying the same stage token is idempotent; another
    /// holder token remains unowned and returns `AlreadyHeld`.
    pub fn acquire_scaleset_attempt_with_token(
        &mut self,
        holder: &str,
        state: PermitState,
        generation: u64,
        pid: Option<u32>,
        attempt_token: &str,
    ) -> Result<AcquireAttemptOutcome, LedgerError> {
        if attempt_token.is_empty() {
            return Err(LedgerError::StaleAttempt(holder.to_owned()));
        }
        self.acquire_attempt_with_token(
            holder,
            PermitLane::ScaleSet,
            state,
            generation,
            pid,
            Some(attempt_token),
        )
    }

    fn acquire_attempt_with_token(
        &mut self,
        holder: &str,
        lane: PermitLane,
        state: PermitState,
        generation: u64,
        pid: Option<u32>,
        staged_attempt_token: Option<&str>,
    ) -> Result<AcquireAttemptOutcome, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if current.max(0) as u64 != generation {
            return Ok(AcquireAttemptOutcome::StaleGeneration);
        }
        let now = unix_now();
        let held: Option<(String, String, String)> = tx
            .query_row(
                "SELECT lane, attempt_token, state FROM permits WHERE holder = ?1",
                params![holder],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((held_lane, held_token, held_state)) = held {
            if held_lane != lane.as_str() {
                return Err(LedgerError::DemandLaneMismatch {
                    holder: holder.to_owned(),
                    expected: lane,
                    seen: held_lane,
                });
            }
            if staged_attempt_token == Some(held_token.as_str()) && held_state == state.as_str() {
                let demand = read_demand(&tx, holder)?
                    .ok_or_else(|| LedgerError::UnknownHolder(holder.to_owned()))?;
                check_demand_lane(holder, &demand, lane)?;
                if demand.state != DemandState::Granted {
                    return Ok(AcquireAttemptOutcome::AlreadyHeld);
                }
                tx.commit()?;
                return Ok(AcquireAttemptOutcome::Acquired {
                    attempt_token: held_token,
                });
            }
            // A duplicate has no attempt token, so it cannot repair demand
            // state or otherwise mutate the existing owner's row.
            tx.commit()?;
            return Ok(AcquireAttemptOutcome::AlreadyHeld);
        }
        let max: Option<i64> =
            tx.query_row("SELECT max_jobs FROM permit_meta WHERE id = 1", [], |row| {
                row.get(0)
            })?;
        let Some(max) = max.and_then(|max| u32::try_from(max).ok()) else {
            return Ok(AcquireAttemptOutcome::NotConfigured);
        };

        let demand = ensure_demand_tx(&tx, holder, lane, "", now, now, DemandState::Eligible)?;
        if demand.state == DemandState::Terminal || demand.state == DemandState::Cancelled {
            return Ok(AcquireAttemptOutcome::Closed);
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
            return Ok(AcquireAttemptOutcome::Full);
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
            return Ok(AcquireAttemptOutcome::Deferred);
        }
        let now_i64 = i64::try_from(now).unwrap_or(i64::MAX);
        let attempt_token = staged_attempt_token
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        tx.execute(
            "INSERT INTO permits
             (holder, lane, state, acquired_unix, updated_unix, generation, pid, attempt_token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                holder,
                lane.as_str(),
                state.as_str(),
                now_i64,
                now_i64,
                i64::try_from(generation).unwrap_or(i64::MAX),
                pid.map(i64::from),
                attempt_token,
            ],
        )?;
        tx.execute(
            "UPDATE permit_demands SET state = 'granted', updated_unix = ?1
             WHERE holder = ?2",
            params![now_i64, holder],
        )?;
        tx.commit()?;
        Ok(AcquireAttemptOutcome::Acquired { attempt_token })
    }

    /// Adopt one holder's row for redelivery when its acquiring process is
    /// dead, or when this process previously retained the row as uncertain
    /// with a terminal demand. A `Cleaning` row is an active cleanup claim:
    /// same-process uncertainty retention cannot demote it, so timed-out
    /// teardown remains protected until its owner releases it. The row must
    /// belong to `lane` and match `generation`.
    ///
    /// Crash-redelivery convergence: the redelivered attempt takes over the
    /// same row (same holder, updated pid and state) instead of spending a
    /// second permit or executing rowless. A same-process retry can also
    /// take over its own uncertain terminal row: its old guard has dropped,
    /// so PID liveness no longer identifies an active attempt. Active
    /// states, uncertain rows from another live process, missing pids, and
    /// foreign lanes refuse adoption. Occupancy is unchanged.
    pub fn adopt_for_redelivery(
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
        let row: Option<(String, Option<i64>, String, Option<String>)> = tx
            .query_row(
                "SELECT permits.lane, permits.pid, permits.state, permit_demands.state
                 FROM permits LEFT JOIN permit_demands USING (holder)
                 WHERE permits.holder = ?1",
                params![holder],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((row_lane, row_pid, row_state, demand_state)) = row else {
            return Ok(AdoptOutcome::Missing);
        };
        if row_lane != lane.as_str() {
            return Ok(AdoptOutcome::LiveHolder);
        }
        let Some(row_pid) = row_pid.and_then(|pid| u32::try_from(pid).ok()) else {
            return Ok(AdoptOutcome::LiveHolder);
        };
        // `retain_uncertain` marks the demand terminal when its old guard
        // exits without a terminal service result. That state pair marks a
        // dropped attempt only when no cleanup worker has claimed it:
        // `Cleaning` is preserved across the old guard's drop. The pid check
        // still protects uncertain rows owned by another live daemon.
        let same_process_retained_native_attempt = lane == PermitLane::Native
            && row_pid == pid
            && row_state == PermitState::Uncertain.as_str()
            && demand_state.as_deref() == Some(DemandState::Terminal.as_str());
        if is_alive(row_pid) && !same_process_retained_native_attempt {
            return Ok(AdoptOutcome::LiveHolder);
        }
        let now = unix_now();
        let attempt_token = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "UPDATE permits
             SET state = ?1, updated_unix = ?2, generation = ?3, pid = ?4, attempt_token = ?5,
                 recovery_claim_token = NULL, recovery_previous_pid = NULL,
                 recovery_previous_token = NULL
             WHERE holder = ?6",
            params![
                state.as_str(),
                i64::try_from(now).unwrap_or(i64::MAX),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(pid),
                attempt_token,
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
        Ok(AdoptOutcome::Adopted { attempt_token })
    }

    /// Apply or finish a durable Scale Set recovery token rotation.
    ///
    /// The caller must hold the ledger-wide recovery claim and prove the
    /// worker/resource is no longer live. The state-database stage supplies
    /// the exact prior pid and token. `None` matches only a token recorded by
    /// this ledger's one-time legacy migration. The existing permit row is
    /// mandatory: this method never inserts or reacquires one. A retry after
    /// a crash between ledger and state-database commits is accepted only when
    /// the current claim owns the exact rotation recorded on that row. For
    /// this idempotent path, `generation` is the rotation row's exact committed
    /// generation; the ledger's current epoch may have advanced since then.
    #[allow(
        clippy::too_many_arguments,
        reason = "the rotation binds holder, state, generation, prior proof, target, and claim atomically"
    )]
    pub fn rotate_scaleset_attempt_for_recovery(
        &mut self,
        holder: &str,
        state: PermitState,
        generation: u64,
        previous_pid: u32,
        previous_token: Option<&str>,
        target_token: &str,
        claim_token: &str,
    ) -> Result<AttemptRotationOutcome, LedgerError> {
        if target_token.is_empty()
            || claim_token.is_empty()
            || previous_pid == 0
            || previous_token.is_some_and(str::is_empty)
            || previous_token == Some(target_token)
        {
            return Ok(AttemptRotationOutcome::StaleAttempt);
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current_pid = std::process::id();
        let claim_owner: Option<(String, i64)> = tx
            .query_row(
                "SELECT claim_token, owner_pid FROM scaleset_recovery_claims WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if claim_owner.as_ref().is_none_or(|(token, owner_pid)| {
            token != claim_token || *owner_pid != i64::from(current_pid)
        }) {
            return Ok(AttemptRotationOutcome::StaleAttempt);
        }
        let current_generation: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let row: Option<RotationPermitRow> = tx
            .query_row(
                "SELECT lane, attempt_token, pid, generation, recovery_claim_token,
                        recovery_previous_pid, recovery_previous_token
                 FROM permits WHERE holder = ?1",
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
        let Some((
            lane,
            current_token,
            raw_pid,
            row_generation,
            row_claim,
            row_previous_pid,
            row_previous_token,
        )) = row
        else {
            return Ok(AttemptRotationOutcome::Missing);
        };
        if lane != PermitLane::ScaleSet.as_str() {
            return Err(LedgerError::DemandLaneMismatch {
                holder: holder.to_owned(),
                expected: PermitLane::ScaleSet,
                seen: lane,
            });
        }
        if current_token == target_token {
            if (current_generation.max(0) as u64) < generation {
                return Ok(AttemptRotationOutcome::StaleGeneration);
            }
            let same_previous_token = match previous_token {
                Some(previous) => row_previous_token.as_deref() == Some(previous),
                None => row_previous_token.is_none(),
            };
            if row_claim.as_deref() == Some(claim_token)
                && row_previous_pid == Some(i64::from(previous_pid))
                && same_previous_token
                && u64::try_from(row_generation).is_ok_and(|seen| seen == generation)
            {
                tx.commit()?;
                return Ok(AttemptRotationOutcome::AlreadyRotated);
            }
            return Ok(AttemptRotationOutcome::StaleAttempt);
        }
        if current_generation.max(0) as u64 != generation {
            return Ok(AttemptRotationOutcome::StaleGeneration);
        }
        let current_row_pid = raw_pid.and_then(|pid| u32::try_from(pid).ok());
        let previous_token_matches = match previous_token {
            Some(previous) => previous == current_token,
            None => tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM permit_token_migrations
                    WHERE holder = ?1 AND attempt_token = ?2
                )",
                params![holder, current_token],
                |row| row.get(0),
            )?,
        };
        if current_token.is_empty()
            || current_row_pid != Some(previous_pid)
            || !previous_token_matches
        {
            return Ok(AttemptRotationOutcome::StaleAttempt);
        }
        let now = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        let changed = tx.execute(
            "UPDATE permits SET state = ?1, updated_unix = ?2, generation = ?3,
                 pid = ?4, attempt_token = ?5, recovery_claim_token = ?6,
                 recovery_previous_pid = ?7, recovery_previous_token = ?8
             WHERE holder = ?9 AND lane = ?10 AND attempt_token = ?11 AND pid = ?12",
            params![
                state.as_str(),
                now,
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(current_pid),
                target_token,
                claim_token,
                i64::from(previous_pid),
                previous_token,
                holder,
                PermitLane::ScaleSet.as_str(),
                current_token,
                i64::from(previous_pid),
            ],
        )?;
        if changed != 1 {
            return Ok(AttemptRotationOutcome::StaleAttempt);
        }
        let demand = ensure_demand_tx(
            &tx,
            holder,
            PermitLane::ScaleSet,
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
        tx.commit()?;
        Ok(AttemptRotationOutcome::Rotated)
    }

    /// Transition only if the permit still belongs to this exact attempt.
    /// Adoption rotates the token, so delayed work from the previous attempt
    /// cannot mutate the redelivery's row. The successful owner is recorded
    /// as the current process, allowing later recovery to distinguish a live
    /// cleanup claim from a dead attempt.
    pub fn transition_owned(
        &mut self,
        holder: &str,
        state: PermitState,
        generation: u64,
        attempt_token: &str,
    ) -> Result<(), LedgerError> {
        self.transition_for_attempt(holder, state, generation, attempt_token)
    }

    /// Claim local cleanup for a marker whose exact job is proven terminal.
    ///
    /// This is a recovery authority, not a substitute for an attempt owner's
    /// normal transition. When the token still owns a row, move that row to
    /// `Cleaning` and record this process in the same transaction. When the
    /// row is absent, close its demand atomically: a concurrent fresh acquire
    /// either inserts first and makes this token stale, or observes the closed
    /// demand and cannot create a permit while local storage is released.
    ///
    /// An open demand is closed on this call but returns [`CleanupClaimOutcome::DemandClosed`]
    /// so the caller retains storage and the marker until a later recovery
    /// pass confirms the closed state. A different current token returns
    /// [`CleanupClaimOutcome::StaleAttempt`] without changing either row.
    pub fn claim_recorded_terminal_cleanup(
        &mut self,
        holder: &str,
        lane: PermitLane,
        attempt_token: &str,
    ) -> Result<CleanupClaimOutcome, LedgerError> {
        if attempt_token.is_empty() {
            return Ok(CleanupClaimOutcome::StaleAttempt);
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = unix_now();
        let now_i64 = i64::try_from(now).unwrap_or(i64::MAX);
        let generation: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let permit: Option<(String, String)> = tx
            .query_row(
                "SELECT lane, attempt_token FROM permits WHERE holder = ?1",
                params![holder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((permit_lane, current_token)) = permit {
            if permit_lane != lane.as_str() {
                return Err(LedgerError::DemandLaneMismatch {
                    holder: holder.to_owned(),
                    expected: lane,
                    seen: permit_lane,
                });
            }
            if let Some(demand) = read_demand(&tx, holder)? {
                check_demand_lane(holder, &demand, lane)?;
            }
            if current_token != attempt_token {
                tx.commit()?;
                return Ok(CleanupClaimOutcome::StaleAttempt);
            }
            tx.execute(
                "UPDATE permits
                 SET state = 'cleaning', updated_unix = ?1, generation = ?2, pid = ?3
                 WHERE holder = ?4 AND attempt_token = ?5",
                params![
                    now_i64,
                    generation,
                    i64::from(std::process::id()),
                    holder,
                    attempt_token,
                ],
            )?;
            tx.commit()?;
            return Ok(CleanupClaimOutcome::Claimed);
        }

        let existing_demand = read_demand(&tx, holder)?;
        let demand_was_closed = if let Some(demand) = &existing_demand {
            check_demand_lane(holder, demand, lane)?;
            matches!(demand.state, DemandState::Terminal | DemandState::Cancelled)
        } else {
            false
        };
        if lane == PermitLane::Native
            && demand_was_closed
            && existing_demand
                .as_ref()
                .is_some_and(|demand| demand.state == DemandState::Cancelled)
        {
            // The following release call is separated from this recorded
            // cleanup proof by local-storage teardown. Persist the exact
            // marker token so that it can acknowledge this already-closed
            // cancellation after reopening the ledger.
            tx.execute(
                "INSERT OR IGNORE INTO recorded_terminal_cleanup_releases
                 (holder, attempt_token) VALUES (?1, ?2)",
                params![holder, attempt_token],
            )?;
        }
        if !demand_was_closed {
            ensure_demand_tx(&tx, holder, lane, "", now, now, DemandState::Terminal)?;
            tx.execute(
                "UPDATE permit_demands SET state = 'terminal', updated_unix = ?1
                 WHERE holder = ?2",
                params![now_i64, holder],
            )?;
        }
        tx.commit()?;
        Ok(if demand_was_closed {
            CleanupClaimOutcome::ClosedAbsent
        } else {
            CleanupClaimOutcome::DemandClosed
        })
    }

    fn transition_for_attempt(
        &mut self,
        holder: &str,
        state: PermitState,
        generation: u64,
        attempt_token: &str,
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
        let current_state = read_owned_attempt_state(&tx, holder, attempt_token)?;
        if current_state == PermitState::Cleaning.as_str() {
            // A cleanup worker's claim is final for this attempt. Even an
            // idempotent transition to Cleaning must not refresh its row.
            tx.commit()?;
            return Ok(());
        }
        let now = unix_now() as i64;
        tx.execute(
            "UPDATE permits SET state = ?1, updated_unix = ?2, generation = ?3, pid = ?4
             WHERE holder = ?5 AND attempt_token = ?6
               AND (state <> 'cleaning' OR ?1 = 'cleaning')",
            params![
                state.as_str(),
                now,
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(std::process::id()),
                holder,
                attempt_token,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Release and terminalize a permit only if this attempt still owns it.
    pub fn release_owned(
        &mut self,
        holder: &str,
        attempt_token: &str,
    ) -> Result<bool, LedgerError> {
        self.release_with_demand_state(holder, DemandState::Terminal, attempt_token)
    }

    /// Terminally release a Native attempt. If its row is already gone, an
    /// already-closed demand makes a completed release idempotent;
    /// absence alone never creates or closes a demand.
    pub fn release_native_terminal_owned(
        &mut self,
        holder: &str,
        attempt_token: &str,
    ) -> Result<OwnedReleaseOutcome, LedgerError> {
        self.release_native_owned_with_terminal_demand(holder, attempt_token, DemandState::Terminal)
    }

    /// Replay a durable Scale Set release stage with exact-token outcomes.
    ///
    /// A present permit is deleted only when its lane and token match. Its
    /// demand transition commits in the same transaction. If the permit is
    /// already absent, return `AlreadyAbsent` only when the existing demand
    /// proves that the same release target committed; absence never inserts
    /// or mutates a demand. This distinguishes a completed release from a
    /// newer token without a racy holder-only lookup.
    pub fn release_scaleset_staged_owned(
        &mut self,
        holder: &str,
        attempt_token: &str,
        next_demand_state: DemandState,
    ) -> Result<OwnedReleaseOutcome, LedgerError> {
        if attempt_token.is_empty()
            || !matches!(
                next_demand_state,
                DemandState::Eligible | DemandState::Cancelled | DemandState::Terminal
            )
        {
            return Ok(OwnedReleaseOutcome::StaleAttempt);
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now_i64 = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        let current: Option<(String, String)> = tx
            .query_row(
                "SELECT lane, attempt_token FROM permits WHERE holder = ?1",
                params![holder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((lane, current_token)) = current else {
            let demand = read_demand(&tx, holder)?;
            let Some(demand) = demand else {
                tx.commit()?;
                return Ok(OwnedReleaseOutcome::StaleAttempt);
            };
            check_demand_lane(holder, &demand, PermitLane::ScaleSet)?;
            if demand.state == next_demand_state {
                tx.commit()?;
                return Ok(OwnedReleaseOutcome::AlreadyAbsent);
            }
            tx.commit()?;
            return Ok(OwnedReleaseOutcome::StaleAttempt);
        };
        if lane != PermitLane::ScaleSet.as_str() {
            return Err(LedgerError::DemandLaneMismatch {
                holder: holder.to_owned(),
                expected: PermitLane::ScaleSet,
                seen: lane,
            });
        }
        if current_token != attempt_token {
            tx.commit()?;
            return Ok(OwnedReleaseOutcome::StaleAttempt);
        }
        let demand = read_demand(&tx, holder)?
            .ok_or_else(|| LedgerError::UnknownHolder(holder.to_owned()))?;
        check_demand_lane(holder, &demand, PermitLane::ScaleSet)?;
        if next_demand_state == DemandState::Eligible
            && !matches!(demand.state, DemandState::Eligible | DemandState::Granted)
        {
            tx.commit()?;
            return Ok(OwnedReleaseOutcome::StaleAttempt);
        }
        tx.execute(
            "DELETE FROM permits WHERE holder = ?1 AND attempt_token = ?2",
            params![holder, attempt_token],
        )?;
        match next_demand_state {
            DemandState::Terminal | DemandState::Cancelled => {
                tx.execute(
                    "UPDATE permit_demands SET state = ?1, updated_unix = ?2
                     WHERE holder = ?3",
                    params![next_demand_state.as_str(), now_i64, holder],
                )?;
            }
            DemandState::Eligible => {
                tx.execute(
                    "UPDATE permit_demands SET state = 'eligible', updated_unix = ?1
                     WHERE holder = ?2",
                    params![now_i64, holder],
                )?;
            }
            DemandState::Granted | DemandState::Waiting => {
                // Roll the delete back if a future call path slips past the
                // target-state check above.
                return Ok(OwnedReleaseOutcome::StaleAttempt);
            }
        }
        tx.commit()?;
        Ok(OwnedReleaseOutcome::Released)
    }

    /// Release to the queue only if this attempt still owns the permit.
    pub fn release_to_eligible_owned(
        &mut self,
        holder: &str,
        attempt_token: &str,
    ) -> Result<bool, LedgerError> {
        self.release_with_demand_state(holder, DemandState::Eligible, attempt_token)
    }

    /// Release and cancel only if this attempt still owns the permit.
    pub fn release_cancelled_owned(
        &mut self,
        holder: &str,
        attempt_token: &str,
    ) -> Result<bool, LedgerError> {
        self.release_with_demand_state(holder, DemandState::Cancelled, attempt_token)
    }

    /// Cancel a Native attempt with the same absent-row behavior as
    /// [`Self::release_native_terminal_owned`].
    pub fn release_native_cancelled_owned(
        &mut self,
        holder: &str,
        attempt_token: &str,
    ) -> Result<OwnedReleaseOutcome, LedgerError> {
        self.release_native_owned_with_terminal_demand(
            holder,
            attempt_token,
            DemandState::Cancelled,
        )
    }

    fn release_native_owned_with_terminal_demand(
        &mut self,
        holder: &str,
        attempt_token: &str,
        next_demand_state: DemandState,
    ) -> Result<OwnedReleaseOutcome, LedgerError> {
        debug_assert!(matches!(
            next_demand_state,
            DemandState::Terminal | DemandState::Cancelled
        ));
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: Option<(String, String)> = tx
            .query_row(
                "SELECT lane, attempt_token FROM permits WHERE holder = ?1",
                params![holder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((lane, current_token)) = current else {
            let Some(demand) = read_demand(&tx, holder)? else {
                tx.execute(
                    "DELETE FROM recorded_terminal_cleanup_releases
                     WHERE holder = ?1 AND attempt_token = ?2",
                    params![holder, attempt_token],
                )?;
                tx.commit()?;
                return Ok(OwnedReleaseOutcome::StaleAttempt);
            };
            check_demand_lane(holder, &demand, PermitLane::Native)?;
            // Cancellation is accepted as terminal only after the exact
            // recorded marker token claimed cleanup; ordinary owner releases
            // still need the demand to match their requested target.
            let recorded_cancelled_cleanup = next_demand_state == DemandState::Terminal
                && demand.state == DemandState::Cancelled
                && tx.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM recorded_terminal_cleanup_releases
                        WHERE holder = ?1 AND attempt_token = ?2
                    )",
                    params![holder, attempt_token],
                    |row| row.get::<_, bool>(0),
                )?;
            let outcome = if demand.state == next_demand_state || recorded_cancelled_cleanup {
                OwnedReleaseOutcome::AlreadyAbsent
            } else {
                OwnedReleaseOutcome::StaleAttempt
            };
            tx.execute(
                "DELETE FROM recorded_terminal_cleanup_releases
                 WHERE holder = ?1 AND attempt_token = ?2",
                params![holder, attempt_token],
            )?;
            tx.commit()?;
            return Ok(outcome);
        };
        if lane != PermitLane::Native.as_str() {
            return Err(LedgerError::DemandLaneMismatch {
                holder: holder.to_owned(),
                expected: PermitLane::Native,
                seen: lane,
            });
        }
        if current_token != attempt_token {
            tx.execute(
                "DELETE FROM recorded_terminal_cleanup_releases
                 WHERE holder = ?1 AND attempt_token = ?2",
                params![holder, attempt_token],
            )?;
            tx.commit()?;
            return Ok(OwnedReleaseOutcome::StaleAttempt);
        }
        let now = unix_now();
        ensure_demand_tx(
            &tx,
            holder,
            PermitLane::Native,
            "",
            now,
            now,
            next_demand_state,
        )?;
        tx.execute(
            "DELETE FROM permits WHERE holder = ?1 AND attempt_token = ?2",
            params![holder, attempt_token],
        )?;
        tx.execute(
            "UPDATE permit_demands SET state = ?1, updated_unix = ?2
             WHERE holder = ?3",
            params![
                next_demand_state.as_str(),
                i64::try_from(now).unwrap_or(i64::MAX),
                holder
            ],
        )?;
        tx.execute(
            "DELETE FROM recorded_terminal_cleanup_releases
             WHERE holder = ?1 AND attempt_token = ?2",
            params![holder, attempt_token],
        )?;
        tx.commit()?;
        Ok(OwnedReleaseOutcome::Released)
    }

    fn release_with_demand_state(
        &mut self,
        holder: &str,
        next_demand_state: DemandState,
        attempt_token: &str,
    ) -> Result<bool, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        let removed = tx.execute(
            "DELETE FROM permits WHERE holder = ?1 AND attempt_token = ?2",
            params![holder, attempt_token],
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

    /// Retain an uncertain permit only if it still belongs to this exact
    /// attempt. A redelivery rotates the token and fences a delayed old drop.
    pub fn retain_uncertain_owned(
        &mut self,
        holder: &str,
        generation: u64,
        attempt_token: &str,
    ) -> Result<(), LedgerError> {
        self.retain_uncertain_for_attempt(holder, generation, attempt_token)
    }

    /// Finish confirmed local cleanup while preserving a Native permit for
    /// service-response recovery. Exact token, Uncertain state, and terminal
    /// demand commit together; a later same-process redelivery can adopt only
    /// after this worker relinquishes its active cleanup claim.
    pub fn retain_uncertain_after_cleanup_owned(
        &mut self,
        holder: &str,
        attempt_token: &str,
    ) -> Result<(), LedgerError> {
        if attempt_token.is_empty() {
            return Err(LedgerError::StaleAttempt(holder.to_owned()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT lane, attempt_token FROM permits WHERE holder = ?1",
                params![holder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((lane, current_token)) = row else {
            return Err(LedgerError::StaleAttempt(holder.to_owned()));
        };
        if lane != PermitLane::Native.as_str() {
            return Err(LedgerError::DemandLaneMismatch {
                holder: holder.to_owned(),
                expected: PermitLane::Native,
                seen: lane,
            });
        }
        if current_token != attempt_token {
            return Err(LedgerError::StaleAttempt(holder.to_owned()));
        }
        let now = unix_now();
        let generation: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "UPDATE permits SET state = 'uncertain', updated_unix = ?1,
                 generation = ?2, pid = ?3
             WHERE holder = ?4 AND attempt_token = ?5",
            params![
                i64::try_from(now).unwrap_or(i64::MAX),
                generation,
                i64::from(std::process::id()),
                holder,
                attempt_token
            ],
        )?;
        ensure_demand_tx(
            &tx,
            holder,
            PermitLane::Native,
            "",
            now,
            now,
            DemandState::Granted,
        )?;
        tx.execute(
            "UPDATE permit_demands SET state = 'terminal', updated_unix = ?1
             WHERE holder = ?2 AND state <> 'cancelled'",
            params![i64::try_from(now).unwrap_or(i64::MAX), holder],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn retain_uncertain_for_attempt(
        &mut self,
        holder: &str,
        generation: u64,
        attempt_token: &str,
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
        let current_state = read_owned_attempt_state(&tx, holder, attempt_token)?;
        if current_state == PermitState::Cleaning.as_str() {
            // Do not demote an active cleanup claim or terminalize its demand.
            tx.commit()?;
            return Ok(());
        }
        let now = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        tx.execute(
            "UPDATE permits SET state = 'uncertain', updated_unix = ?1, generation = ?2
             WHERE holder = ?3 AND attempt_token = ?4 AND state <> 'cleaning'",
            params![
                now,
                i64::try_from(generation).unwrap_or(i64::MAX),
                holder,
                attempt_token,
            ],
        )?;
        tx.execute(
            "UPDATE permit_demands SET state = 'terminal', updated_unix = ?1
             WHERE holder = ?2 AND state IN ('eligible', 'granted')",
            params![now, holder],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Reconcile durable occupancy against exact-token observations, and mark
    /// this epoch reconciled so capacity may be advertised.
    ///
    /// `alive` is the caller's attested live set: `(holder, lane,
    /// attempt_token)`. All observations are checked and applied in one
    /// immediate transaction. A missing row or token mismatch fails closed;
    /// observations never recreate a released permit or mint an unrecorded
    /// owner token. Recorded holders outside the set are marked uncertain
    /// (still counted), while active `Cleaning` claims remain protected.
    pub fn reconcile_attempts(
        &mut self,
        alive: &[(&str, PermitLane, &str)],
    ) -> Result<ReconcileReport, LedgerError> {
        self.reconcile_attempts_with_staged(alive, &[])
    }

    /// Reconcile exact live observations and proof-bearing staged Scale Set
    /// token rotations in one ledger transaction.
    ///
    /// A staged tuple is `(holder, lane, previous_token, target_token)`. The
    /// caller must prove the stage came from the durable Scale Set recovery
    /// journal. Before rotation, a stage accepts only the exact previous
    /// token; `None` matches only the exact token recorded by the ledger's
    /// migration journal. After rotation, the target must carry the prior
    /// token-or-legacy-null, prior PID, and recovery-claim proof from the
    /// atomic ledger CAS. This operation never inserts, rotates, or releases
    /// a permit. Recovery must still prove the old worker and resources are
    /// no longer live before finishing the staged rotation.
    pub fn reconcile_attempts_with_staged(
        &mut self,
        alive: &[(&str, PermitLane, &str)],
        staged: &[(&str, PermitLane, Option<&str>, &str)],
    ) -> Result<ReconcileReport, LedgerError> {
        self.reconcile_attempts_with_staged_releases(alive, staged, &[])
    }

    /// Reconcile exact live observations plus durable Scale Set token
    /// rotation and release stages in one ledger transaction.
    ///
    /// `staged_releases` contains `(holder, lane, attempt_token,
    /// target_demand_state)` tuples from a durable Scale Set release journal.
    /// The lane must be `ScaleSet`, and an extant row must still have that
    /// lane and exact token. An absent row is accepted only when its durable
    /// demand already has the staged target state, proving the ledger release
    /// transaction committed before its state-database projection did. Staged
    /// releases never recreate or mutate permits or demand; recovery must
    /// finish the projection while still holding its serialized recovery
    /// claim. A mismatched token, lane, or absent-row demand state fails
    /// closed.
    pub fn reconcile_attempts_with_staged_releases(
        &mut self,
        alive: &[(&str, PermitLane, &str)],
        staged_rotations: &[(&str, PermitLane, Option<&str>, &str)],
        staged_releases: &[(&str, PermitLane, &str, DemandState)],
    ) -> Result<ReconcileReport, LedgerError> {
        self.reconcile_attempts_with_staged_acquisitions(
            alive,
            staged_rotations,
            staged_releases,
            &[],
        )
    }

    /// Reconcile exact live observations and durable Scale Set rotation,
    /// release, and acquisition stages in one ledger transaction.
    ///
    /// `staged_acquisitions` contains `(holder, lane, attempt_token)` tuples
    /// from the state database, persisted before the ledger insert. An exact
    /// extant row is attested; an absent row is allowed because the stage may
    /// have been written before the ledger transaction began. Reconciliation
    /// never creates, releases, or rotates a permit row; it may mark
    /// unobserved rows uncertain and create their missing demand as granted
    /// or promote an eligible demand to granted. Exact live and staged-rotation
    /// rows do the same demand reconciliation. Recovery may discard an absent
    /// acquisition stage, or finish the state projection when its exact token
    /// row exists.
    pub fn reconcile_attempts_with_staged_acquisitions(
        &mut self,
        alive: &[(&str, PermitLane, &str)],
        staged_rotations: &[(&str, PermitLane, Option<&str>, &str)],
        staged_releases: &[(&str, PermitLane, &str, DemandState)],
        staged_acquisitions: &[(&str, PermitLane, &str)],
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
        let mut observed_holders = std::collections::BTreeSet::<String>::new();
        for (holder, lane, attempt_token) in alive {
            if attempt_token.is_empty() || !observed_holders.insert((*holder).to_owned()) {
                return Err(LedgerError::StaleAttempt((*holder).to_owned()));
            }
            let held: Option<HeldPermitRow> = tx
                .query_row(
                    "SELECT lane, attempt_token, recovery_claim_token,
                            recovery_previous_pid, recovery_previous_token
                     FROM permits WHERE holder = ?1",
                    params![*holder],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((recorded_lane, recorded_token, _, _, _)) = held else {
                return Err(LedgerError::StaleAttempt((*holder).to_owned()));
            };
            if recorded_lane != lane.as_str() {
                return Err(LedgerError::DemandLaneMismatch {
                    holder: (*holder).to_owned(),
                    expected: *lane,
                    seen: recorded_lane,
                });
            }
            if recorded_token != *attempt_token {
                return Err(LedgerError::StaleAttempt((*holder).to_owned()));
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
            report.confirmed.push((*holder).to_string());
        }
        for (holder, lane, previous_token, target_token) in staged_rotations {
            if *lane != PermitLane::ScaleSet
                || target_token.is_empty()
                || previous_token.is_some_and(str::is_empty)
                || *previous_token == Some(*target_token)
                || !observed_holders.insert((*holder).to_owned())
            {
                return Err(LedgerError::StaleAttempt((*holder).to_owned()));
            }
            let held: Option<HeldPermitRow> = tx
                .query_row(
                    "SELECT lane, attempt_token, recovery_claim_token,
                            recovery_previous_pid, recovery_previous_token
                     FROM permits WHERE holder = ?1",
                    params![*holder],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((
                recorded_lane,
                recorded_token,
                recovery_claim,
                recovery_previous_pid,
                recovery_previous_token,
            )) = held
            else {
                return Err(LedgerError::StaleAttempt((*holder).to_owned()));
            };
            if recorded_lane != PermitLane::ScaleSet.as_str() {
                return Err(LedgerError::DemandLaneMismatch {
                    holder: (*holder).to_owned(),
                    expected: PermitLane::ScaleSet,
                    seen: recorded_lane,
                });
            }
            let completed_rotation_matches = recovery_claim
                .as_deref()
                .is_some_and(|token| !token.is_empty())
                && recovery_previous_pid.is_some_and(|pid| pid > 0)
                && match previous_token {
                    Some(previous) => recovery_previous_token.as_deref() == Some(previous),
                    None => recovery_previous_token.is_none(),
                };
            let previous_matches = match previous_token {
                Some(previous) if *previous == recorded_token => true,
                Some(_) | None if recorded_token == *target_token => completed_rotation_matches,
                None => tx.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM permit_token_migrations
                        WHERE holder = ?1 AND attempt_token = ?2
                    )",
                    params![*holder, recorded_token],
                    |row| row.get(0),
                )?,
                Some(_) => false,
            };
            if recorded_token.is_empty() || !previous_matches {
                return Err(LedgerError::StaleAttempt((*holder).to_owned()));
            }
            let demand = ensure_demand_tx(
                &tx,
                holder,
                PermitLane::ScaleSet,
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
            report.confirmed.push((*holder).to_string());
        }
        for (holder, lane, attempt_token, target_demand_state) in staged_releases {
            if *lane != PermitLane::ScaleSet {
                return Err(LedgerError::DemandLaneMismatch {
                    holder: (*holder).to_owned(),
                    expected: PermitLane::ScaleSet,
                    seen: lane.as_str().to_owned(),
                });
            }
            if !matches!(
                *target_demand_state,
                DemandState::Eligible | DemandState::Cancelled | DemandState::Terminal
            ) || attempt_token.is_empty()
                || !observed_holders.insert((*holder).to_owned())
            {
                return Err(LedgerError::StaleAttempt((*holder).to_owned()));
            }
            let held: Option<(String, String)> = tx
                .query_row(
                    "SELECT lane, attempt_token FROM permits WHERE holder = ?1",
                    params![*holder],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((recorded_lane, recorded_token)) = held else {
                // An exact desired demand state proves the token-fenced
                // release transaction committed; the owner token itself is
                // no longer present to compare. Never mint a replacement.
                let demand: Option<(String, String)> = tx
                    .query_row(
                        "SELECT lane, state FROM permit_demands WHERE holder = ?1",
                        params![*holder],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((demand_lane, demand_state)) = demand else {
                    return Err(LedgerError::StaleAttempt((*holder).to_owned()));
                };
                if demand_lane != lane.as_str() {
                    return Err(LedgerError::DemandLaneMismatch {
                        holder: (*holder).to_owned(),
                        expected: *lane,
                        seen: demand_lane,
                    });
                }
                if DemandState::parse(&demand_state) != Some(*target_demand_state) {
                    return Err(LedgerError::StaleAttempt((*holder).to_owned()));
                }
                continue;
            };
            if recorded_lane != lane.as_str() {
                return Err(LedgerError::DemandLaneMismatch {
                    holder: (*holder).to_owned(),
                    expected: PermitLane::ScaleSet,
                    seen: recorded_lane,
                });
            }
            if recorded_token != *attempt_token {
                return Err(LedgerError::StaleAttempt((*holder).to_owned()));
            }
            // Keep an extant staged release untouched. Its owner will retry
            // the token-fenced release after the cross-database recovery
            // stage has been inspected.
            report.confirmed.push((*holder).to_string());
        }
        for (holder, lane, attempt_token) in staged_acquisitions {
            if *lane != PermitLane::ScaleSet
                || attempt_token.is_empty()
                || !observed_holders.insert((*holder).to_owned())
            {
                return Err(LedgerError::StaleAttempt((*holder).to_owned()));
            }
            let held: Option<(String, String)> = tx
                .query_row(
                    "SELECT lane, attempt_token FROM permits WHERE holder = ?1",
                    params![*holder],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((recorded_lane, recorded_token)) = held else {
                // The state journal precedes the ledger insert. Absence means
                // its ledger half did not commit; recovery can discard it.
                continue;
            };
            if recorded_lane != lane.as_str() {
                return Err(LedgerError::DemandLaneMismatch {
                    holder: (*holder).to_owned(),
                    expected: *lane,
                    seen: recorded_lane,
                });
            }
            if recorded_token != *attempt_token {
                return Err(LedgerError::StaleAttempt((*holder).to_owned()));
            }
            report.confirmed.push((*holder).to_string());
        }
        let mut select = tx.prepare("SELECT holder, lane FROM permits")?;
        let recorded: Vec<(String, String)> = select
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        drop(select);
        for (holder, raw_lane) in &recorded {
            if observed_holders.contains(holder) {
                continue;
            }
            let lane = PermitLane::parse(raw_lane)
                .ok_or_else(|| LedgerError::UnknownLane(raw_lane.clone()))?;
            let changed = tx.execute(
                "UPDATE permits SET state = 'uncertain', updated_unix = ?1, generation = ?2
                 WHERE holder = ?3 AND state <> 'cleaning'",
                params![now, generation, holder],
            )?;
            if changed == 0 {
                // A worker may have claimed cleanup after this process took
                // its observation snapshot. Keep its active claim intact.
                continue;
            }
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
        report.marked_uncertain.sort();
        report.confirmed.sort();
        Ok(report)
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

    /// Acquisition assertions intentionally ignore the fresh token when a
    /// test only checks admission. Tests that mutate a permit retain and pass
    /// the token from `AcquireAttemptOutcome::Acquired`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum AcquireExpected {
        Acquired,
        AlreadyHeld,
        Full,
        Deferred,
        Closed,
        StaleGeneration,
        NotConfigured,
    }

    impl PartialEq<AcquireExpected> for AcquireAttemptOutcome {
        fn eq(&self, expected: &AcquireExpected) -> bool {
            matches!(
                (self, expected),
                (
                    AcquireAttemptOutcome::Acquired { .. },
                    AcquireExpected::Acquired
                ) | (
                    AcquireAttemptOutcome::AlreadyHeld,
                    AcquireExpected::AlreadyHeld
                ) | (AcquireAttemptOutcome::Full, AcquireExpected::Full)
                    | (AcquireAttemptOutcome::Deferred, AcquireExpected::Deferred)
                    | (AcquireAttemptOutcome::Closed, AcquireExpected::Closed)
                    | (
                        AcquireAttemptOutcome::StaleGeneration,
                        AcquireExpected::StaleGeneration
                    )
                    | (
                        AcquireAttemptOutcome::NotConfigured,
                        AcquireExpected::NotConfigured
                    )
            )
        }
    }

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

        let AcquireAttemptOutcome::Acquired {
            attempt_token: a_token,
        } = ledger
            .acquire_attempt(
                "a",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                None,
            )
            .unwrap()
        else {
            panic!("first permit was not acquired");
        };
        let AcquireAttemptOutcome::Acquired {
            attempt_token: b_token,
        } = ledger
            .acquire_attempt(
                "b",
                PermitLane::Native,
                PermitState::Running,
                generation,
                None,
            )
            .unwrap()
        else {
            panic!("second permit was not acquired");
        };
        assert_eq!(ledger.occupied().unwrap(), 2);
        // A third holder is refused; nothing was spent.
        assert_eq!(
            ledger
                .acquire_attempt(
                    "c",
                    PermitLane::Native,
                    PermitState::Reserved,
                    generation,
                    None
                )
                .unwrap(),
            AcquireExpected::Full
        );
        assert_eq!(ledger.occupied().unwrap(), 2);

        // Lanes share the one N: a Scale Set holder is refused too.
        assert_eq!(
            ledger
                .acquire_attempt(
                    "d",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None
                )
                .unwrap(),
            AcquireExpected::Full
        );
        assert_eq!(ledger.occupied_by_lane(PermitLane::ScaleSet).unwrap(), 0);

        assert!(ledger.release_owned("a", &a_token).unwrap());
        // The older queued native demand gets the newly freed permit first.
        assert_eq!(
            ledger
                .acquire_attempt(
                    "d",
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireExpected::Deferred
        );
        assert_eq!(
            ledger
                .acquire_attempt(
                    "c",
                    PermitLane::Native,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireExpected::Acquired
        );
        assert!(ledger.release_owned("b", &b_token).unwrap());
        assert_eq!(
            ledger
                .acquire_attempt(
                    "d",
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireExpected::Acquired
        );
        assert_eq!(ledger.occupied_by_lane(PermitLane::ScaleSet).unwrap(), 1);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn acquire_attempt_returns_token_only_to_fresh_owner() {
        let (mut ledger, dir) = temp_ledger("acquire-attempt-token");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();

        let AcquireAttemptOutcome::Acquired { attempt_token } = ledger
            .acquire_attempt(
                "owned",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(std::process::id()),
            )
            .unwrap()
        else {
            panic!("fresh attempt was not acquired");
        };
        assert!(!attempt_token.is_empty());
        assert_eq!(
            ledger.attempt_token("owned").unwrap(),
            Some(attempt_token.clone())
        );
        ledger
            .conn
            .execute(
                "UPDATE permit_demands SET state = 'eligible' WHERE holder = 'owned'",
                [],
            )
            .unwrap();
        assert_eq!(
            ledger
                .acquire_attempt(
                    "owned",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(std::process::id()),
                )
                .unwrap(),
            AcquireAttemptOutcome::AlreadyHeld
        );
        assert_eq!(
            ledger.demand("owned").unwrap().unwrap().state,
            DemandState::Eligible,
            "an unowned duplicate cannot mutate the held attempt's demand"
        );
        ledger
            .transition_owned("owned", PermitState::Cleaning, generation, &attempt_token)
            .unwrap();
        assert_eq!(
            ledger
                .holders()
                .unwrap()
                .into_iter()
                .find(|holder| holder.holder == "owned")
                .unwrap()
                .pid,
            Some(std::process::id())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn staged_scale_set_acquisition_replays_exact_token_after_crash_cut() {
        let (mut ledger, dir) = temp_ledger("staged-scaleset-acquisition");
        ledger.set_max_jobs(2).unwrap();
        let holder = "scaleset/7/acquire-stage";
        let token = uuid::Uuid::new_v4().to_string();

        // A crash after the state journal write but before the ledger insert
        // leaves no row. Startup may attest the stage without creating one.
        ledger.begin_epoch().unwrap();
        let report = ledger
            .reconcile_attempts_with_staged_acquisitions(
                &[],
                &[],
                &[],
                &[(holder, PermitLane::ScaleSet, &token)],
            )
            .unwrap();
        assert!(report.confirmed.is_empty());
        assert!(!ledger.has_permit(holder).unwrap());
        assert!(ledger.demand(holder).unwrap().is_none());

        // The same stage token is used by the ledger write, so a crash after
        // that commit but before state projection can be resumed safely.
        let generation = ledger.generation().unwrap();
        assert_eq!(
            ledger
                .acquire_scaleset_attempt_with_token(
                    holder,
                    PermitState::Acquiring,
                    generation,
                    None,
                    &token,
                )
                .unwrap(),
            AcquireAttemptOutcome::Acquired {
                attempt_token: token.clone(),
            }
        );
        assert_eq!(
            ledger
                .acquire_scaleset_attempt_with_token(
                    holder,
                    PermitState::Acquiring,
                    generation,
                    None,
                    &token,
                )
                .unwrap(),
            AcquireAttemptOutcome::Acquired {
                attempt_token: token.clone(),
            }
        );
        ledger.begin_epoch().unwrap();
        let report = ledger
            .reconcile_attempts_with_staged_acquisitions(
                &[],
                &[],
                &[],
                &[(holder, PermitLane::ScaleSet, &token)],
            )
            .unwrap();
        assert_eq!(report.confirmed, vec![holder.to_owned()]);
        assert_eq!(ledger.attempt_token(holder).unwrap(), Some(token.clone()));

        // Stage replay never adopts a different live owner token.
        let generation = ledger.generation().unwrap();
        assert_eq!(
            ledger
                .acquire_scaleset_attempt_with_token(
                    holder,
                    PermitState::Acquiring,
                    generation,
                    None,
                    "newer-stage-token",
                )
                .unwrap(),
            AcquireAttemptOutcome::AlreadyHeld
        );
        assert_eq!(
            ledger
                .acquire_scaleset_attempt_with_token(
                    holder,
                    PermitState::Running,
                    generation,
                    None,
                    &token,
                )
                .unwrap(),
            AcquireAttemptOutcome::AlreadyHeld
        );
        assert!(matches!(
            ledger.reconcile_attempts_with_staged_acquisitions(
                &[],
                &[],
                &[],
                &[(holder, PermitLane::ScaleSet, "newer-stage-token")],
            ),
            Err(LedgerError::StaleAttempt(seen)) if seen == holder
        ));
        assert!(matches!(
            ledger.reconcile_attempts_with_staged_acquisitions(
                &[],
                &[],
                &[],
                &[(holder, PermitLane::Native, &token)],
            ),
            Err(LedgerError::StaleAttempt(seen)) if seen == holder
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn staged_scaleset_attempt_rotation_is_token_fenced_and_restart_finishable() {
        let (mut ledger, dir) = temp_ledger("staged-scaleset-rotation");
        ledger.set_max_jobs(2).unwrap();
        let generation = ledger.generation().unwrap();
        let holder = "scaleset/7/worker-rotation";
        let previous_pid = std::process::id();
        let AcquireAttemptOutcome::Acquired {
            attempt_token: old_token,
        } = ledger
            .acquire_attempt(
                holder,
                PermitLane::ScaleSet,
                PermitState::Running,
                generation,
                Some(previous_pid),
            )
            .unwrap()
        else {
            panic!("Scale Set permit was not acquired");
        };
        let target_token = uuid::Uuid::new_v4().to_string();
        let recovery_claim = ledger.claim_scaleset_recovery(&|_| false).unwrap();

        assert_eq!(
            ledger
                .rotate_scaleset_attempt_for_recovery(
                    holder,
                    PermitState::Provisioning,
                    generation,
                    previous_pid,
                    Some(&old_token),
                    &target_token,
                    recovery_claim.token(),
                )
                .unwrap(),
            AttemptRotationOutcome::Rotated
        );
        assert_eq!(
            ledger.attempt_token(holder).unwrap(),
            Some(target_token.clone())
        );
        assert!(!ledger.release_owned(holder, &old_token).unwrap());

        // A crash after ledger commit but before the state-database token
        // transaction resumes from the stage without rotating a second time.
        assert_eq!(
            ledger
                .rotate_scaleset_attempt_for_recovery(
                    holder,
                    PermitState::Provisioning,
                    generation,
                    previous_pid,
                    Some(&old_token),
                    &target_token,
                    recovery_claim.token(),
                )
                .unwrap(),
            AttemptRotationOutcome::AlreadyRotated
        );
        assert_eq!(
            ledger.attempt_token(holder).unwrap(),
            Some(target_token.clone())
        );
        assert!(ledger.release_owned(holder, &target_token).unwrap());
        assert_eq!(
            ledger
                .rotate_scaleset_attempt_for_recovery(
                    holder,
                    PermitState::Provisioning,
                    generation,
                    previous_pid,
                    None,
                    &uuid::Uuid::new_v4().to_string(),
                    recovery_claim.token(),
                )
                .unwrap(),
            AttemptRotationOutcome::Missing
        );
        assert!(!ledger.has_permit(holder).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recovery_claim_excludes_competitors_and_dead_owner_takeover_finishes_rotation() {
        let (mut first, dir) = temp_ledger("scaleset-recovery-claim-takeover");
        first.set_max_jobs(1).unwrap();
        let generation = first.generation().unwrap();
        let holder = "scaleset/7/claim-takeover";
        let previous_pid = u32::MAX;
        let AcquireAttemptOutcome::Acquired { attempt_token } = first
            .acquire_attempt(
                holder,
                PermitLane::ScaleSet,
                PermitState::Running,
                generation,
                Some(previous_pid),
            )
            .unwrap()
        else {
            panic!("Scale Set permit was not acquired");
        };
        let original_claim = first.claim_scaleset_recovery(&|_| false).unwrap();

        let mut competing = PermitLedger::open(first.path()).unwrap();
        assert!(matches!(
            competing.claim_scaleset_recovery(&|pid| pid == std::process::id()),
            Err(LedgerError::RecoveryClaimed(pid)) if pid == std::process::id()
        ));

        let target_token = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            first
                .rotate_scaleset_attempt_for_recovery(
                    holder,
                    PermitState::Provisioning,
                    generation,
                    previous_pid,
                    Some(&attempt_token),
                    &target_token,
                    original_claim.token(),
                )
                .unwrap(),
            AttemptRotationOutcome::Rotated
        );

        // Model process death after the ledger rotation but before the state
        // database projection: another claimant takes over the same durable
        // token and proves ownership of the already-committed rotation.
        let simulated_dead_owner = u32::MAX - 1;
        competing
            .conn
            .execute(
                "UPDATE scaleset_recovery_claims SET owner_pid = ?1 WHERE id = 1",
                params![i64::from(simulated_dead_owner)],
            )
            .unwrap();
        let mut replacement_claim = competing.claim_scaleset_recovery(&|_| false).unwrap();
        assert_eq!(replacement_claim.token(), original_claim.token());
        assert_eq!(
            competing
                .rotate_scaleset_attempt_for_recovery(
                    holder,
                    PermitState::Provisioning,
                    generation,
                    previous_pid,
                    Some(&attempt_token),
                    &target_token,
                    replacement_claim.token(),
                )
                .unwrap(),
            AttemptRotationOutcome::AlreadyRotated
        );
        replacement_claim.release().unwrap();
        drop(competing);
        drop(first);
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
        let AcquireAttemptOutcome::Acquired {
            attempt_token: younger_token,
        } = ledger
            .acquire_attempt(
                "younger",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                None,
            )
            .unwrap()
        else {
            panic!("younger permit was not acquired");
        };

        // Redelivery revives the parked demand at its original age: it
        // heads the queue again once capacity frees.
        assert!(ledger.release_owned("younger", &younger_token).unwrap());
        assert_eq!(
            ledger
                .acquire_attempt(
                    "older",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireExpected::Acquired
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
                .acquire_attempt(
                    "gone",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireExpected::Closed
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
                .acquire_attempt(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireExpected::Acquired
        );
        // Same holder, redelivered: no second permit.
        assert_eq!(
            ledger
                .acquire_attempt(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireExpected::AlreadyHeld
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
        // ... and the duplicate does not evict the other waiter either.
        assert_eq!(
            ledger
                .acquire_attempt(
                    "b",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireExpected::Full
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
        let mut job_zero_token = None;
        for (index, state) in states.iter().enumerate() {
            let holder = format!("job-{index}");
            let AcquireAttemptOutcome::Acquired { attempt_token } = ledger
                .acquire_attempt(&holder, PermitLane::Native, *state, generation, None)
                .unwrap()
            else {
                panic!("permit for {holder} was not acquired");
            };
            if index == 0 {
                job_zero_token = Some(attempt_token);
            }
        }
        assert_eq!(ledger.occupied().unwrap(), 7);
        ledger
            .transition_owned(
                "job-0",
                PermitState::Running,
                generation,
                job_zero_token.as_deref().unwrap(),
            )
            .unwrap();
        assert_eq!(ledger.occupied().unwrap(), 7);
        assert_eq!(
            ledger.holder_state("job-0").unwrap(),
            Some(PermitState::Running)
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_generations_are_rejected_and_release_uses_the_owner_token() {
        let (mut ledger, dir) = temp_ledger("generation");
        ledger.set_max_jobs(2).unwrap();
        let stale = ledger.generation().unwrap();
        let current = ledger.begin_epoch().unwrap();
        assert!(current > stale);

        assert_eq!(
            ledger
                .acquire_attempt("a", PermitLane::Native, PermitState::Acquiring, stale, None)
                .unwrap(),
            AcquireExpected::StaleGeneration
        );
        let AcquireAttemptOutcome::Acquired { attempt_token } = ledger
            .acquire_attempt(
                "a",
                PermitLane::Native,
                PermitState::Acquiring,
                current,
                None,
            )
            .unwrap()
        else {
            panic!("current-generation permit was not acquired");
        };
        assert!(matches!(
            ledger.transition_owned("a", PermitState::Running, stale, &attempt_token),
            Err(LedgerError::StaleGeneration { .. })
        ));
        // The owner token fences release; generation changes do not rotate it.
        assert!(ledger.release_owned("a", &attempt_token).unwrap());
        assert!(!ledger.release_owned("a", &attempt_token).unwrap());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn attempt_mutations_distinguish_stale_missing_and_cleaning_without_mutation() {
        let (mut ledger, dir) = temp_ledger("attempt-mutation-errors");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();
        let holder = "native/attempt-mutation-errors";
        let AcquireAttemptOutcome::Acquired { attempt_token } = ledger
            .acquire_attempt(
                holder,
                PermitLane::Native,
                PermitState::Running,
                generation,
                Some(std::process::id()),
            )
            .unwrap()
        else {
            panic!("attempt was not acquired");
        };
        ledger
            .transition_owned(holder, PermitState::Cleaning, generation, &attempt_token)
            .unwrap();
        let cleaning_holders = ledger.holders().unwrap();
        let cleaning_demand = ledger.demand(holder).unwrap();

        // The exact owner may finish late, but cannot mutate or demote a
        // cleanup claim through either transition path.
        ledger
            .transition_owned(holder, PermitState::Running, generation, &attempt_token)
            .unwrap();
        ledger
            .retain_uncertain_owned(holder, generation, &attempt_token)
            .unwrap();
        assert_eq!(ledger.holders().unwrap(), cleaning_holders);
        assert_eq!(ledger.demand(holder).unwrap(), cleaning_demand);

        // A different token is stale even while the current attempt is
        // Cleaning. Neither operation may touch the permit or its demand.
        let stale_token = "old-attempt-token";
        assert!(matches!(
            ledger.transition_owned(holder, PermitState::Running, generation, stale_token),
            Err(LedgerError::StaleAttempt(seen)) if seen == holder
        ));
        assert_eq!(ledger.holders().unwrap(), cleaning_holders);
        assert_eq!(ledger.demand(holder).unwrap(), cleaning_demand);
        assert!(matches!(
            ledger.retain_uncertain_owned(holder, generation, stale_token),
            Err(LedgerError::StaleAttempt(seen)) if seen == holder
        ));
        assert_eq!(ledger.holders().unwrap(), cleaning_holders);
        assert_eq!(ledger.demand(holder).unwrap(), cleaning_demand);

        // A missing holder remains a distinct condition and must not create
        // either a permit or a demand.
        let missing_holder = "native/absent-attempt";
        assert!(matches!(
            ledger.transition_owned(
                missing_holder,
                PermitState::Running,
                generation,
                &attempt_token,
            ),
            Err(LedgerError::UnknownHolder(seen)) if seen == missing_holder
        ));
        assert!(matches!(
            ledger.retain_uncertain_owned(missing_holder, generation, &attempt_token),
            Err(LedgerError::UnknownHolder(seen)) if seen == missing_holder
        ));
        assert!(!ledger.has_permit(missing_holder).unwrap());
        assert!(ledger.demand(missing_holder).unwrap().is_none());
        assert_eq!(ledger.holders().unwrap(), cleaning_holders);
        assert_eq!(ledger.demand(holder).unwrap(), cleaning_demand);

        // Once release removes the permit, its durable demand still proves
        // this holder existed; delayed calls with the former token are stale,
        // while a holder with no permit or demand remains unknown.
        assert!(ledger.release_owned(holder, &attempt_token).unwrap());
        let released_holders = ledger.holders().unwrap();
        let released_demand = ledger.demand(holder).unwrap();
        assert!(matches!(
            ledger.transition_owned(holder, PermitState::Running, generation, &attempt_token),
            Err(LedgerError::StaleAttempt(seen)) if seen == holder
        ));
        assert!(matches!(
            ledger.retain_uncertain_owned(holder, generation, &attempt_token),
            Err(LedgerError::StaleAttempt(seen)) if seen == holder
        ));
        assert!(!ledger.has_permit(holder).unwrap());
        assert_eq!(ledger.holders().unwrap(), released_holders);
        assert_eq!(ledger.demand(holder).unwrap(), released_demand);

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
                .acquire_attempt(
                    "scaleset/7/younger",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireExpected::Deferred
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
                .acquire_attempt(
                    "native/older",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireExpected::Acquired
        );
        assert_eq!(
            ledger.demand("native/older").unwrap().unwrap().state,
            DemandState::Granted
        );
        // Once the oldest row owns a permit, it leaves the eligible queue;
        // another free host slot is available to the next demand.
        assert_eq!(
            ledger
                .acquire_attempt(
                    "scaleset/7/younger",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireExpected::Acquired
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
                .acquire_attempt(
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
                .acquire_attempt(
                    "scaleset/7/younger",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap()
        });
        barrier.wait();

        assert_eq!(older.join().unwrap(), AcquireExpected::Acquired);
        assert!(matches!(
            younger.join().unwrap(),
            AcquireAttemptOutcome::Full | AcquireAttemptOutcome::Deferred
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
        let AcquireAttemptOutcome::Acquired {
            attempt_token: first_token,
        } = ledger
            .acquire_attempt(
                "native/first",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                None,
            )
            .unwrap()
        else {
            panic!("first permit was not acquired");
        };
        assert!(ledger
            .release_to_eligible_owned("native/first", &first_token)
            .unwrap());
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
                .acquire_attempt(
                    "scaleset/7/second",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireExpected::Deferred
        );
        let AcquireAttemptOutcome::Acquired { attempt_token } = ledger
            .acquire_attempt(
                "native/first",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                None,
            )
            .unwrap()
        else {
            panic!("retried permit was not acquired");
        };
        assert!(ledger
            .retain_uncertain_owned("native/first", generation, &attempt_token)
            .is_ok());
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
    fn open_backfills_unique_attempt_tokens_transactionally_and_idempotently() {
        let (ledger, dir) = temp_ledger("attempt-token-backfill");
        let path = ledger.path().to_owned();
        drop(ledger);

        // Recreate the pre-token permit table with extant occupants.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE permits;
             CREATE TABLE permits (
                holder TEXT PRIMARY KEY,
                lane TEXT NOT NULL,
                state TEXT NOT NULL,
                acquired_unix INTEGER NOT NULL,
                updated_unix INTEGER NOT NULL,
                generation INTEGER NOT NULL,
                pid INTEGER
             );
             INSERT INTO permits VALUES
                ('legacy-a', 'native', 'running', 1, 1, 0, 10),
                ('legacy-b', 'native', 'uncertain', 2, 2, 0, 11);",
        )
        .unwrap();
        drop(conn);

        let ledger = PermitLedger::open(&path).unwrap();
        let first_a = ledger.attempt_token("legacy-a").unwrap().unwrap();
        let first_b = ledger.attempt_token("legacy-b").unwrap().unwrap();
        assert!(!first_a.is_empty());
        assert!(!first_b.is_empty());
        assert_ne!(first_a, first_b);
        drop(ledger);

        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            ledger.attempt_token("legacy-a").unwrap(),
            Some(first_a.clone())
        );
        assert_eq!(
            ledger.attempt_token("legacy-b").unwrap(),
            Some(first_b.clone())
        );
        drop(ledger);

        // The complete schema can still contain a bad token value (for
        // example after an interrupted migration). Backfill must run even
        // when every schema object already exists.
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE permits SET attempt_token = '' WHERE holder = 'legacy-a'",
            [],
        )
        .unwrap();
        drop(conn);

        let ledger = PermitLedger::open(&path).unwrap();
        let repaired_a = ledger.attempt_token("legacy-a").unwrap().unwrap();
        assert!(!repaired_a.is_empty());
        assert_ne!(repaired_a, first_a);
        assert_eq!(ledger.attempt_token("legacy-b").unwrap(), Some(first_b));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_repairs_missing_permit_meta_seed_row() {
        let (ledger, dir) = temp_ledger("missing-meta-seed");
        let path = ledger.path().to_owned();
        drop(ledger);

        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.execute("DELETE FROM permit_meta WHERE id = 1", [])
                .unwrap(),
            1
        );
        drop(conn);

        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.max_jobs().unwrap(), None);
        assert_eq!(ledger.generation().unwrap(), 0);
        let seeded: bool = ledger
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM permit_meta WHERE id = 1)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(seeded);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unconfigured_ledgers_grant_nothing() {
        let (mut ledger, dir) = temp_ledger("unconfigured");
        let generation = ledger.generation().unwrap();
        assert_eq!(ledger.max_jobs().unwrap(), None);
        assert_eq!(
            ledger
                .acquire_attempt(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireExpected::NotConfigured
        );
        assert_eq!(ledger.occupied().unwrap(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reconcile_requires_exact_attempts_marks_and_gates_advertisement() {
        let (mut ledger, dir) = temp_ledger("reconcile");
        ledger.set_max_jobs(4).unwrap();
        let generation = ledger.generation().unwrap();
        let AcquireAttemptOutcome::Acquired {
            attempt_token: old_token,
        } = ledger
            .acquire_attempt(
                "old",
                PermitLane::Native,
                PermitState::Running,
                generation,
                None,
            )
            .unwrap()
        else {
            panic!("old permit was not acquired");
        };
        // Nothing advertised before the first reconcile.
        assert_eq!(ledger.advertised_free().unwrap(), None);

        // An observed holder without an existing exact-token row is not
        // reacquired during reconciliation.
        assert!(matches!(
            ledger.reconcile_attempts(&[(
                "new",
                PermitLane::Native,
                "unissued-token",
            )]),
            Err(LedgerError::StaleAttempt(holder)) if holder == "new"
        ));
        assert_eq!(ledger.occupied().unwrap(), 1);

        let report = ledger.reconcile_attempts(&[]).unwrap();
        assert_eq!(report.marked_uncertain, vec!["old".to_string()]);
        assert!(report.confirmed.is_empty());
        // Uncertain rows count; nothing was erased.
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.holder_state("old").unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(ledger.advertised_free().unwrap(), Some(3));

        // A new epoch requires a fresh reconcile before advertising.
        ledger.begin_epoch().unwrap();
        assert_eq!(ledger.advertised_free().unwrap(), None);
        let report = ledger
            .reconcile_attempts(&[("old", PermitLane::Native, &old_token)])
            .unwrap();
        assert!(report.marked_uncertain.is_empty());
        assert_eq!(report.confirmed, vec!["old".to_string()]);
        assert_eq!(ledger.advertised_free().unwrap(), Some(3));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn staged_reconcile_accepts_only_the_pre_or_post_rotation_row() {
        let (mut ledger, dir) = temp_ledger("staged-reconcile");
        ledger.set_max_jobs(2).unwrap();
        let generation = ledger.generation().unwrap();
        let holder = "scaleset/7/staged";
        let previous_pid = std::process::id();
        let AcquireAttemptOutcome::Acquired {
            attempt_token: previous_token,
        } = ledger
            .acquire_attempt(
                holder,
                PermitLane::ScaleSet,
                PermitState::Running,
                generation,
                Some(previous_pid),
            )
            .unwrap()
        else {
            panic!("staged fixture permit was not acquired");
        };
        let target_token = uuid::Uuid::new_v4().to_string();

        // Crash cut before ledger rotation: accept exact previous token and
        // leave the permit unchanged for proof-bearing recovery to finish.
        let report = ledger
            .reconcile_attempts_with_staged(
                &[],
                &[(
                    holder,
                    PermitLane::ScaleSet,
                    Some(&previous_token),
                    &target_token,
                )],
            )
            .unwrap();
        assert_eq!(report.confirmed, vec![holder.to_owned()]);
        assert_eq!(
            ledger.attempt_token(holder).unwrap(),
            Some(previous_token.clone())
        );
        assert_eq!(
            ledger.holder_state(holder).unwrap(),
            Some(PermitState::Running)
        );

        // Crash cut after ledger rotation but before state DB projection:
        // a fresh epoch still recognizes the exact target token.
        let recovery_claim = ledger.claim_scaleset_recovery(&|_| false).unwrap();
        assert_eq!(
            ledger
                .rotate_scaleset_attempt_for_recovery(
                    holder,
                    PermitState::Provisioning,
                    generation,
                    previous_pid,
                    Some(&previous_token),
                    &target_token,
                    recovery_claim.token(),
                )
                .unwrap(),
            AttemptRotationOutcome::Rotated
        );
        ledger.begin_epoch().unwrap();
        let report = ledger
            .reconcile_attempts_with_staged(
                &[],
                &[(
                    holder,
                    PermitLane::ScaleSet,
                    Some(&previous_token),
                    &target_token,
                )],
            )
            .unwrap();
        assert_eq!(report.confirmed, vec![holder.to_owned()]);
        assert_eq!(
            ledger.attempt_token(holder).unwrap(),
            Some(target_token.clone())
        );

        // A v23 stage had no prior token. Only explicit migration provenance
        // authorizes the exact backfilled token on this extant row.
        let legacy_generation = ledger.generation().unwrap();
        let legacy_pid = std::process::id();
        let AcquireAttemptOutcome::Acquired {
            attempt_token: backfilled_token,
        } = ledger
            .acquire_attempt(
                "scaleset/7/v23-active",
                PermitLane::ScaleSet,
                PermitState::Uncertain,
                legacy_generation,
                Some(legacy_pid),
            )
            .unwrap()
        else {
            panic!("v23 fixture permit was not acquired");
        };
        ledger
            .conn
            .execute(
                "INSERT INTO permit_token_migrations (holder, attempt_token, acquired_unix)
                 VALUES (?1, ?2, ?3)",
                params![
                    "scaleset/7/v23-active",
                    backfilled_token,
                    i64::try_from(unix_now()).unwrap_or(i64::MAX)
                ],
            )
            .unwrap();
        let legacy_target = uuid::Uuid::new_v4().to_string();
        ledger.begin_epoch().unwrap();
        let report = ledger
            .reconcile_attempts_with_staged(
                &[],
                &[
                    (
                        holder,
                        PermitLane::ScaleSet,
                        Some(&previous_token),
                        &target_token,
                    ),
                    (
                        "scaleset/7/v23-active",
                        PermitLane::ScaleSet,
                        None,
                        &legacy_target,
                    ),
                ],
            )
            .unwrap();
        assert_eq!(report.confirmed.len(), 2);
        assert_eq!(
            ledger.attempt_token("scaleset/7/v23-active").unwrap(),
            Some(backfilled_token)
        );
        assert!(matches!(
            ledger.reconcile_attempts_with_staged(
                &[],
                &[(
                    "scaleset/7/missing",
                    PermitLane::ScaleSet,
                    None,
                    "target",
                )],
            ),
            Err(LedgerError::StaleAttempt(seen)) if seen == "scaleset/7/missing"
        ));
        assert!(!ledger.has_permit("scaleset/7/missing").unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn staged_scale_set_release_reconcile_accepts_exact_or_absent_without_recreating() {
        let (mut ledger, dir) = temp_ledger("staged-scaleset-release");
        ledger.set_max_jobs(2).unwrap();
        let generation = ledger.generation().unwrap();
        let holder = "scaleset/7/release-stage";
        let AcquireAttemptOutcome::Acquired { attempt_token } = ledger
            .acquire_attempt(
                holder,
                PermitLane::ScaleSet,
                PermitState::Cleaning,
                generation,
                None,
            )
            .unwrap()
        else {
            panic!("staged release fixture permit was not acquired");
        };

        // Before the cross-database release commits, the exact row is
        // attested and left untouched for Scale Set recovery.
        let report = ledger
            .reconcile_attempts_with_staged_releases(
                &[],
                &[],
                &[(
                    holder,
                    PermitLane::ScaleSet,
                    &attempt_token,
                    DemandState::Terminal,
                )],
            )
            .unwrap();
        assert_eq!(report.confirmed, vec![holder.to_owned()]);
        assert_eq!(
            ledger.attempt_token(holder).unwrap(),
            Some(attempt_token.clone())
        );
        assert_eq!(
            ledger.holder_state(holder).unwrap(),
            Some(PermitState::Cleaning)
        );

        // Crash after the ledger release but before the state database
        // projection: a new startup accepts the absent exact staged row and
        // does not reacquire it or alter the terminal demand.
        assert_eq!(
            ledger
                .release_scaleset_staged_owned(holder, &attempt_token, DemandState::Terminal,)
                .unwrap(),
            OwnedReleaseOutcome::Released
        );
        ledger.begin_epoch().unwrap();
        let report = ledger
            .reconcile_attempts_with_staged_releases(
                &[],
                &[],
                &[(
                    holder,
                    PermitLane::ScaleSet,
                    &attempt_token,
                    DemandState::Terminal,
                )],
            )
            .unwrap();
        assert!(report.confirmed.is_empty());
        assert!(!ledger.has_permit(holder).unwrap());
        assert_eq!(
            ledger.demand(holder).unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert_eq!(
            ledger
                .release_scaleset_staged_owned(holder, &attempt_token, DemandState::Terminal,)
                .unwrap(),
            OwnedReleaseOutcome::AlreadyAbsent
        );
        assert_eq!(
            ledger
                .release_scaleset_staged_owned(holder, &attempt_token, DemandState::Eligible,)
                .unwrap(),
            OwnedReleaseOutcome::StaleAttempt
        );
        assert!(matches!(
            ledger.reconcile_attempts_with_staged_releases(
                &[],
                &[],
                &[(holder, PermitLane::ScaleSet, &attempt_token, DemandState::Eligible)],
            ),
            Err(LedgerError::StaleAttempt(seen)) if seen == holder
        ));
        assert!(matches!(
            ledger.reconcile_attempts_with_staged_releases(
                &[],
                &[],
                &[(
                    holder,
                    PermitLane::Native,
                    &attempt_token,
                    DemandState::Terminal,
                )],
            ),
            Err(LedgerError::DemandLaneMismatch {
                expected: PermitLane::ScaleSet,
                ..
            })
        ));

        // A newer token on the same holder may never be mistaken for the
        // staged release owner.
        let replacement_holder = "scaleset/7/replacement";
        let generation = ledger.generation().unwrap();
        let replacement_pid = std::process::id();
        let AcquireAttemptOutcome::Acquired {
            attempt_token: replacement_token,
        } = ledger
            .acquire_attempt(
                replacement_holder,
                PermitLane::ScaleSet,
                PermitState::Running,
                generation,
                Some(replacement_pid),
            )
            .unwrap()
        else {
            panic!("replacement fixture permit was not acquired");
        };
        let replacement_target = uuid::Uuid::new_v4().to_string();
        let recovery_claim = ledger.claim_scaleset_recovery(&|_| false).unwrap();
        assert_eq!(
            ledger
                .rotate_scaleset_attempt_for_recovery(
                    replacement_holder,
                    PermitState::Provisioning,
                    generation,
                    replacement_pid,
                    Some(&replacement_token),
                    &replacement_target,
                    recovery_claim.token(),
                )
                .unwrap(),
            AttemptRotationOutcome::Rotated
        );
        ledger.begin_epoch().unwrap();
        assert!(matches!(
            ledger.reconcile_attempts_with_staged_releases(
                &[],
                &[],
                &[(
                    replacement_holder,
                    PermitLane::ScaleSet,
                    &replacement_token,
                    DemandState::Terminal,
                )],
            ),
            Err(LedgerError::StaleAttempt(seen)) if seen == replacement_holder
        ));
        assert_eq!(
            ledger.attempt_token(replacement_holder).unwrap(),
            Some(replacement_target)
        );
        assert_eq!(
            ledger
                .release_scaleset_staged_owned(
                    replacement_holder,
                    &replacement_token,
                    DemandState::Terminal,
                )
                .unwrap(),
            OwnedReleaseOutcome::StaleAttempt
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
        drop(ledger);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_terminal_release_requires_owner_when_absent_and_retry_is_idempotent() {
        let (mut ledger, dir) = temp_ledger("native-terminal-release-absent");
        ledger.set_max_jobs(1).unwrap();
        let now = unix_now();
        ledger
            .observe_demand("native/absent", PermitLane::Native, "test", now, now)
            .unwrap();
        let generation = ledger.generation().unwrap();
        let AcquireAttemptOutcome::Acquired {
            attempt_token: stale_token,
        } = ledger
            .acquire_attempt(
                "native/absent",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(std::process::id()),
            )
            .unwrap()
        else {
            panic!("initial demand must acquire its permit");
        };
        assert!(ledger
            .release_to_eligible_owned("native/absent", &stale_token)
            .unwrap());

        assert_eq!(
            ledger
                .release_native_terminal_owned("native/absent", &stale_token)
                .unwrap(),
            OwnedReleaseOutcome::StaleAttempt
        );
        assert_eq!(
            ledger.demand("native/absent").unwrap().unwrap().state,
            DemandState::Eligible
        );
        assert_eq!(
            ledger
                .release_native_cancelled_owned("native/absent", &stale_token)
                .unwrap(),
            OwnedReleaseOutcome::StaleAttempt
        );
        assert_eq!(
            ledger.demand("native/absent").unwrap().unwrap().state,
            DemandState::Eligible
        );
        let AcquireAttemptOutcome::Acquired { attempt_token } = ledger
            .acquire_attempt(
                "native/absent",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(std::process::id()),
            )
            .unwrap()
        else {
            panic!("open demand must remain acquirable after an unowned release");
        };
        assert_eq!(
            ledger
                .release_native_terminal_owned("native/absent", &attempt_token)
                .unwrap(),
            OwnedReleaseOutcome::Released
        );
        assert_eq!(
            ledger.demand("native/absent").unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert_eq!(
            ledger
                .release_native_terminal_owned("native/absent", &attempt_token)
                .unwrap(),
            OwnedReleaseOutcome::AlreadyAbsent
        );
        assert_eq!(
            ledger
                .acquire_attempt(
                    "native/absent",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(std::process::id()),
                )
                .unwrap(),
            AcquireAttemptOutcome::Closed
        );
        assert_eq!(ledger.occupied().unwrap(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cleanup_preservation_releases_only_exact_attempt_to_redelivery() {
        let (mut ledger, dir) = temp_ledger("cleanup-preservation-token");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();
        let holder = "native/cleanup-preservation";
        let AcquireAttemptOutcome::Acquired { attempt_token } = ledger
            .acquire_attempt(
                holder,
                PermitLane::Native,
                PermitState::Cleaning,
                generation,
                Some(std::process::id()),
            )
            .unwrap()
        else {
            panic!("cleanup attempt was not acquired");
        };

        assert!(matches!(
            ledger.retain_uncertain_after_cleanup_owned(holder, "stale-token"),
            Err(LedgerError::StaleAttempt(seen)) if seen == holder
        ));
        assert_eq!(
            ledger.holder_state(holder).unwrap(),
            Some(PermitState::Cleaning)
        );
        assert_eq!(
            ledger.demand(holder).unwrap().unwrap().state,
            DemandState::Granted
        );

        ledger
            .retain_uncertain_after_cleanup_owned(holder, &attempt_token)
            .unwrap();
        assert_eq!(
            ledger.holder_state(holder).unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(
            ledger.demand(holder).unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert_eq!(ledger.attempt_token(holder).unwrap(), Some(attempt_token));
        assert!(matches!(
            ledger
                .adopt_for_redelivery(
                    holder,
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    std::process::id(),
                    &|_| true,
                )
                .unwrap(),
            AdoptOutcome::Adopted { .. }
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exact_token_reconcile_races_release_without_reacquiring_orphaned_row() {
        use std::sync::{Arc, Barrier};

        let (mut setup, dir) = temp_ledger("reconcile-release-race");
        setup.set_max_jobs(32).unwrap();
        let generation = setup.generation().unwrap();
        drop(setup);

        for iteration in 0..16 {
            let holder = format!("native/reconcile-race-{iteration}");
            let mut setup = PermitLedger::open(&dir.join("permit-ledger.db")).unwrap();
            let AcquireAttemptOutcome::Acquired { attempt_token } = setup
                .acquire_attempt(
                    &holder,
                    PermitLane::Native,
                    PermitState::Running,
                    generation,
                    Some(std::process::id()),
                )
                .unwrap()
            else {
                panic!("race fixture permit was not acquired");
            };
            let path = setup.path().to_owned();
            drop(setup);

            let barrier = Arc::new(Barrier::new(3));
            let release_barrier = Arc::clone(&barrier);
            let release_path = path.clone();
            let release_holder = holder.clone();
            let release_token = attempt_token.clone();
            let release = std::thread::spawn(move || {
                let mut ledger = PermitLedger::open(&release_path).unwrap();
                release_barrier.wait();
                ledger
                    .release_owned(&release_holder, &release_token)
                    .unwrap()
            });

            let reconcile_barrier = Arc::clone(&barrier);
            let reconcile_path = path.clone();
            let reconcile_holder = holder.clone();
            let reconcile_token = attempt_token.clone();
            let reconcile = std::thread::spawn(move || {
                let mut ledger = PermitLedger::open(&reconcile_path).unwrap();
                reconcile_barrier.wait();
                ledger.reconcile_attempts(&[(
                    &reconcile_holder,
                    PermitLane::Native,
                    &reconcile_token,
                )])
            });

            barrier.wait();
            assert!(release.join().unwrap());
            let reconcile = reconcile.join().unwrap();
            assert!(
                reconcile.is_ok()
                    || matches!(reconcile, Err(LedgerError::StaleAttempt(ref seen)) if seen == &holder)
            );
            let ledger = PermitLedger::open(&path).unwrap();
            assert!(!ledger.has_permit(&holder).unwrap());
            assert_eq!(
                ledger.demand(&holder).unwrap().unwrap().state,
                DemandState::Terminal
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adopt_dead_and_same_process_uncertain_attempts_but_refuse_live_attempts() {
        let (mut ledger, dir) = temp_ledger("adopt");
        ledger.set_max_jobs(5).unwrap();
        let generation = ledger.generation().unwrap();
        ledger
            .acquire_attempt(
                "dead",
                PermitLane::Native,
                PermitState::Running,
                generation,
                Some(101),
            )
            .unwrap();
        ledger
            .acquire_attempt(
                "live",
                PermitLane::Native,
                PermitState::Running,
                generation,
                Some(102),
            )
            .unwrap();
        ledger
            .acquire_attempt(
                "noid",
                PermitLane::Native,
                PermitState::Running,
                generation,
                None,
            )
            .unwrap();
        ledger
            .acquire_attempt(
                "official",
                PermitLane::ScaleSet,
                PermitState::Running,
                generation,
                Some(103),
            )
            .unwrap();

        let AcquireAttemptOutcome::Acquired {
            attempt_token: stale_attempt_token,
        } = ledger
            .acquire_attempt(
                "uncertain",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(102),
            )
            .unwrap()
        else {
            panic!("uncertain fixture attempt was not acquired");
        };
        // This same-process attempt dropped after an ambiguous acquire.
        // Its state and terminal demand distinguish it from active work.
        ledger
            .retain_uncertain_owned("uncertain", generation, &stale_attempt_token)
            .unwrap();

        let is_alive = |pid: u32| pid == 102;
        let AdoptOutcome::Adopted {
            attempt_token: dead_attempt_token,
        } = ledger
            .adopt_for_redelivery(
                "dead",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                999,
                &is_alive,
            )
            .unwrap()
        else {
            panic!("dead attempt was not adopted");
        };
        assert!(!dead_attempt_token.is_empty());
        // Same row, new pid and state; occupancy unchanged.
        assert_eq!(ledger.occupied().unwrap(), 5);
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
        // Same-process redelivery takes over only the uncertain terminal
        // attempt. Its active neighbor retains its running state and pid.
        let AdoptOutcome::Adopted {
            attempt_token: adopted_attempt_token,
        } = ledger
            .adopt_for_redelivery(
                "uncertain",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                102,
                &is_alive,
            )
            .unwrap()
        else {
            panic!("same-process uncertain attempt was not adopted");
        };
        assert_eq!(
            ledger.holder_state("uncertain").unwrap(),
            Some(PermitState::Acquiring)
        );
        assert_ne!(stale_attempt_token, adopted_attempt_token);
        let adopted_holders = ledger.holders().unwrap();
        let adopted_demand = ledger.demand("uncertain").unwrap();
        // Teardown from the previous owner is fenced after adoption. Its
        // state and terminal-demand updates must not leak through stale
        // transitions, retention, or release.
        assert!(matches!(
            ledger.transition_owned(
                "uncertain",
                PermitState::Cleaning,
                generation,
                &stale_attempt_token,
            ),
            Err(LedgerError::StaleAttempt(seen)) if seen == "uncertain"
        ));
        assert_eq!(ledger.holders().unwrap(), adopted_holders);
        assert_eq!(ledger.demand("uncertain").unwrap(), adopted_demand);
        assert!(matches!(
            ledger.retain_uncertain_owned("uncertain", generation, &stale_attempt_token),
            Err(LedgerError::StaleAttempt(seen)) if seen == "uncertain"
        ));
        assert_eq!(ledger.holders().unwrap(), adopted_holders);
        assert_eq!(ledger.demand("uncertain").unwrap(), adopted_demand);
        assert!(!ledger
            .release_owned("uncertain", &stale_attempt_token)
            .unwrap());
        assert_eq!(ledger.holders().unwrap(), adopted_holders);
        assert_eq!(ledger.demand("uncertain").unwrap(), adopted_demand);
        assert_eq!(ledger.occupied().unwrap(), 5);
        assert_eq!(
            ledger.holder_state("uncertain").unwrap(),
            Some(PermitState::Acquiring)
        );
        assert_eq!(
            ledger.demand("uncertain").unwrap().unwrap().state,
            DemandState::Terminal
        );
        // A concurrent redelivery sees the acquired active state and cannot
        // take the same row again.
        assert_eq!(
            ledger
                .adopt_for_redelivery(
                    "uncertain",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    102,
                    &is_alive,
                )
                .unwrap(),
            AdoptOutcome::LiveHolder
        );
        assert_eq!(
            ledger
                .adopt_for_redelivery(
                    "live",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    102,
                    &is_alive,
                )
                .unwrap(),
            AdoptOutcome::LiveHolder
        );
        assert_eq!(
            ledger.holder_state("live").unwrap(),
            Some(PermitState::Running)
        );
        // Missing pid, foreign lane, and missing row all refuse.
        assert_eq!(
            ledger
                .adopt_for_redelivery(
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
                .adopt_for_redelivery(
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
                .adopt_for_redelivery(
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
    fn same_process_scaleset_uncertain_attempt_is_not_adopted_while_live() {
        let (mut ledger, dir) = temp_ledger("scaleset-live-uncertain");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();
        let holder = "scaleset/7/worker-1";
        let AcquireAttemptOutcome::Acquired { attempt_token } = ledger
            .acquire_attempt(
                holder,
                PermitLane::ScaleSet,
                PermitState::Provisioning,
                generation,
                Some(std::process::id()),
            )
            .unwrap()
        else {
            panic!("Scale Set attempt was not acquired");
        };
        ledger
            .retain_uncertain_owned(holder, generation, &attempt_token)
            .unwrap();

        assert_eq!(
            ledger
                .adopt_for_redelivery(
                    holder,
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    std::process::id(),
                    &|_| true,
                )
                .unwrap(),
            AdoptOutcome::LiveHolder
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert!(ledger.has_permit(holder).unwrap());
        assert!(ledger.release_owned(holder, &attempt_token).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn timed_out_cleanup_claim_blocks_same_process_redelivery_until_worker_finishes() {
        use std::sync::{Arc, Barrier};

        let (mut setup, dir) = temp_ledger("timed-out-cleanup-live-redelivery");
        setup.set_max_jobs(2).unwrap();
        let generation = setup.generation().unwrap();
        let holder = "native/timed-out-cleanup";
        let AcquireAttemptOutcome::Acquired { attempt_token } = setup
            .acquire_attempt(
                holder,
                PermitLane::Native,
                PermitState::Running,
                generation,
                Some(std::process::id()),
            )
            .unwrap()
        else {
            panic!("attempt was not acquired");
        };
        let path = setup.path().to_owned();
        drop(setup);

        // The detached teardown worker owns the Cleaning state while it
        // retries local cleanup after the handler's timeout.
        let claimed = Arc::new(Barrier::new(2));
        let timeout_drop = Arc::new(Barrier::new(2));
        let retained = Arc::new(Barrier::new(2));
        let finish_cleanup = Arc::new(Barrier::new(2));
        let worker_path = path.clone();
        let worker_holder = holder.to_owned();
        let worker_token = attempt_token.clone();
        let worker_claimed = Arc::clone(&claimed);
        let worker_timeout_drop = Arc::clone(&timeout_drop);
        let worker_retained = Arc::clone(&retained);
        let worker_finish = Arc::clone(&finish_cleanup);
        let worker = std::thread::spawn(move || {
            let mut ledger = PermitLedger::open(&worker_path).unwrap();
            let generation = ledger.generation().unwrap();
            ledger
                .transition_owned(
                    &worker_holder,
                    PermitState::Cleaning,
                    generation,
                    &worker_token,
                )
                .unwrap();
            worker_claimed.wait();
            // Simulate the timed-out request guard dropping while the
            // detached cleanup worker is still live.
            worker_timeout_drop.wait();
            ledger
                .retain_uncertain_owned(&worker_holder, generation, &worker_token)
                .unwrap();
            assert_eq!(
                ledger.holder_state(&worker_holder).unwrap(),
                Some(PermitState::Cleaning)
            );
            worker_retained.wait();
            worker_finish.wait();
            assert!(ledger.release_owned(&worker_holder, &worker_token).unwrap());
        });

        claimed.wait();
        let is_alive = |pid| pid == std::process::id();
        let mut redelivery = PermitLedger::open(&path).unwrap();
        assert_eq!(
            redelivery
                .adopt_for_redelivery(
                    holder,
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    std::process::id(),
                    &is_alive,
                )
                .unwrap(),
            AdoptOutcome::LiveHolder
        );
        timeout_drop.wait();
        retained.wait();
        assert_eq!(
            redelivery.holder_state(holder).unwrap(),
            Some(PermitState::Cleaning)
        );
        assert_eq!(
            redelivery
                .adopt_for_redelivery(
                    holder,
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    std::process::id(),
                    &is_alive,
                )
                .unwrap(),
            AdoptOutcome::LiveHolder
        );
        finish_cleanup.wait();
        worker.join().unwrap();
        assert!(!redelivery.has_permit(holder).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn absent_row_cleanup_claim_races_fresh_acquire_without_releasing_new_attempt() {
        use std::sync::{Arc, Barrier};

        let (mut setup, dir) = temp_ledger("cleanup-claim-acquire-race");
        setup.set_max_jobs(32).unwrap();
        let generation = setup.begin_epoch().unwrap();
        setup.reconcile_attempts(&[]).unwrap();
        drop(setup);

        for attempt in 0..16 {
            let holder = format!("native/cleanup-race-{attempt}");
            let previous_token = format!("previous-attempt-{attempt}");
            let mut setup = PermitLedger::open(&dir.join("permit-ledger.db")).unwrap();
            let now = unix_now();
            setup
                .observe_demand(&holder, PermitLane::Native, "test", now, now)
                .unwrap();
            drop(setup);

            let gate = Arc::new(Barrier::new(2));
            let acquire_gate = Arc::clone(&gate);
            let acquire_path = dir.join("permit-ledger.db");
            let acquire_holder = holder.clone();
            let acquire = std::thread::spawn(move || {
                let mut ledger = PermitLedger::open(&acquire_path).unwrap();
                acquire_gate.wait();
                ledger
                    .acquire_attempt(
                        &acquire_holder,
                        PermitLane::Native,
                        PermitState::Acquiring,
                        generation,
                        Some(std::process::id()),
                    )
                    .unwrap()
            });

            let cleanup_gate = Arc::clone(&gate);
            let cleanup_path = dir.join("permit-ledger.db");
            let cleanup_holder = holder.clone();
            let cleanup_token = previous_token.clone();
            let cleanup = std::thread::spawn(move || {
                let mut ledger = PermitLedger::open(&cleanup_path).unwrap();
                cleanup_gate.wait();
                ledger
                    .claim_recorded_terminal_cleanup(
                        &cleanup_holder,
                        PermitLane::Native,
                        &cleanup_token,
                    )
                    .unwrap()
            });

            let acquired = acquire.join().unwrap();
            let claimed = cleanup.join().unwrap();
            match acquired {
                AcquireAttemptOutcome::Acquired { attempt_token } => {
                    assert_eq!(claimed, CleanupClaimOutcome::StaleAttempt);
                    let mut ledger = PermitLedger::open(&dir.join("permit-ledger.db")).unwrap();
                    assert!(!ledger.release_owned(&holder, &previous_token).unwrap());
                    assert!(ledger.is_current_attempt(&holder, &attempt_token).unwrap());
                }
                AcquireAttemptOutcome::Closed => {
                    assert_eq!(claimed, CleanupClaimOutcome::DemandClosed);
                    let mut ledger = PermitLedger::open(&dir.join("permit-ledger.db")).unwrap();
                    assert_eq!(
                        ledger
                            .claim_recorded_terminal_cleanup(
                                &holder,
                                PermitLane::Native,
                                &previous_token,
                            )
                            .unwrap(),
                        CleanupClaimOutcome::ClosedAbsent
                    );
                    assert!(!ledger.has_permit(&holder).unwrap());
                    assert_eq!(
                        ledger.demand(&holder).unwrap().unwrap().state,
                        DemandState::Terminal
                    );
                    assert!(!ledger.release_owned(&holder, &previous_token).unwrap());
                }
                outcome => panic!("unexpected fresh-acquire result: {outcome:?}"),
            }
        }

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recorded_cleanup_retry_accepts_cancelled_redelivery_after_token_rotation() {
        let (mut ledger, dir) = temp_ledger("recorded-cleanup-cancelled-redelivery");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();
        let holder = "native/recorded-cleanup-cancelled";
        let AcquireAttemptOutcome::Acquired {
            attempt_token: recorded_token,
        } = ledger
            .acquire_attempt(
                holder,
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(std::process::id()),
            )
            .unwrap()
        else {
            panic!("recorded attempt was not acquired");
        };

        ledger
            .retain_uncertain_after_cleanup_owned(holder, &recorded_token)
            .unwrap();
        let AdoptOutcome::Adopted {
            attempt_token: redelivered_token,
        } = ledger
            .adopt_for_redelivery(
                holder,
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                std::process::id(),
                &|_| true,
            )
            .unwrap()
        else {
            panic!("preserved attempt was not adopted");
        };
        assert_ne!(recorded_token, redelivered_token);
        assert_eq!(
            ledger
                .release_native_cancelled_owned(holder, &redelivered_token)
                .unwrap(),
            OwnedReleaseOutcome::Released
        );
        assert_eq!(
            ledger.demand(holder).unwrap().unwrap().state,
            DemandState::Cancelled
        );

        // The ordinary token-owner API still rejects an absent cancellation
        // until recorded cleanup proves the old marker's exact token.
        assert_eq!(
            ledger
                .release_native_terminal_owned(holder, &recorded_token)
                .unwrap(),
            OwnedReleaseOutcome::StaleAttempt
        );
        assert_eq!(
            ledger
                .claim_recorded_terminal_cleanup(holder, PermitLane::Native, &recorded_token)
                .unwrap(),
            CleanupClaimOutcome::ClosedAbsent
        );
        let other_holder = "native/recorded-cleanup-other";
        let now = unix_now();
        ledger
            .observe_demand(other_holder, PermitLane::Native, "test", now, now)
            .unwrap();
        assert!(ledger.cancel_demand(other_holder).unwrap());
        assert_eq!(
            ledger
                .release_native_terminal_owned(other_holder, &recorded_token)
                .unwrap(),
            OwnedReleaseOutcome::StaleAttempt
        );
        assert_eq!(
            ledger
                .release_native_terminal_owned(holder, "wrong-recorded-token")
                .unwrap(),
            OwnedReleaseOutcome::StaleAttempt
        );
        assert_eq!(
            ledger.demand(holder).unwrap().unwrap().state,
            DemandState::Cancelled
        );
        assert_eq!(
            ledger.demand(other_holder).unwrap().unwrap().state,
            DemandState::Cancelled
        );

        // The caller releases local storage between the claim and release,
        // reopening the ledger on the second operation.
        drop(ledger);
        let mut retry = PermitLedger::open(&dir.join("permit-ledger.db")).unwrap();
        assert_eq!(
            retry
                .release_native_terminal_owned(holder, &recorded_token)
                .unwrap(),
            OwnedReleaseOutcome::AlreadyAbsent
        );
        assert_eq!(
            retry
                .release_native_terminal_owned(holder, &recorded_token)
                .unwrap(),
            OwnedReleaseOutcome::StaleAttempt
        );

        // Simulate a crash after release commits but before the caller clears
        // its marker. The retry must reclaim and recreate the consumed proof.
        drop(retry);
        let mut crashed_retry = PermitLedger::open(&dir.join("permit-ledger.db")).unwrap();
        assert_eq!(
            crashed_retry
                .claim_recorded_terminal_cleanup(holder, PermitLane::Native, &recorded_token)
                .unwrap(),
            CleanupClaimOutcome::ClosedAbsent
        );
        drop(crashed_retry);
        let mut final_retry = PermitLedger::open(&dir.join("permit-ledger.db")).unwrap();
        assert_eq!(
            final_retry
                .release_native_terminal_owned(holder, &recorded_token)
                .unwrap(),
            OwnedReleaseOutcome::AlreadyAbsent
        );
        assert!(!final_retry.has_permit(holder).unwrap());
        assert_eq!(
            final_retry.demand(holder).unwrap().unwrap().state,
            DemandState::Cancelled
        );
        assert_eq!(
            final_retry
                .release_native_terminal_owned(holder, &recorded_token)
                .unwrap(),
            OwnedReleaseOutcome::StaleAttempt
        );
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
                .acquire_attempt(
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
                .acquire_attempt(
                    "b",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireExpected::Acquired
        );
        assert_eq!(
            ledger
                .acquire_attempt(
                    "c",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireExpected::Acquired
        );
        assert_eq!(
            ledger
                .acquire_attempt(
                    "d",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireExpected::Full
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

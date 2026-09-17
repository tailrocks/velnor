//! Host-wide `max_jobs=N` permit ledger shared by every local lane.
//!
//! One top-level job lifecycle holds exactly one permit, from the point
//! capacity is committed for acquisition (or assignable readiness) until
//! terminal work and owned cleanup are confirmed. Reserved, acquiring,
//! provisioning, assignable, running, cleaning, and uncertain states are
//! all included in the single count: row presence is occupancy, whatever
//! the state. Offered but unacquired work is queued demand, not occupied
//! capacity, and never appears here.
//!
//! Lanes ([`PermitLane`]) share the one `N`: native acquisitions and the
//! future Scale Set adapter ([`PermitLane::ScaleSet`], the D1 hook point)
//! gate at this authority. There is no per-lane reservation.
//!
//! Durability and crash recovery:
//!
//! * The ledger is a host-wide SQLite database. Every daemon on the host
//!   must resolve to the same file; multi-process contention is bounded by
//!   a busy timeout, and every mutation runs in an immediate transaction.
//! * Grants are generation-fenced: [`PermitLedger::begin_epoch`] bumps the
//!   generation at daemon startup, and acquire/transition calls carrying a
//!   stale generation are rejected. Release is intentionally unfenced —
//!   freeing capacity is always safe, and fencing it would leak permits
//!   held by a previous epoch's workers.
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
//!   a second permit.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

/// SQLite busy timeout for multi-process ledger contention.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// A local lane sharing the one host-wide `N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermitLane {
    /// Velnor-native daemon/slot acquisitions.
    Native,
    /// Official-runner Scale Set acquisitions (D1 adapter hook point).
    ScaleSet,
}

impl PermitLane {
    fn as_str(self) -> &'static str {
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
    /// Crash-recovery evidence for the sweep: a dead pid proves the attempt
    /// that held the permit is gone. Pids are host-scoped; lanes whose
    /// holders are not host processes pass `None` and are never swept.
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
    UnknownHolder(String),
    StaleGeneration { expected: u64, seen: u64 },
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "permit ledger storage: {error}"),
            Self::UnknownLane(lane) => write!(f, "permit ledger holds unknown lane {lane:?}"),
            Self::UnknownState(state) => write!(f, "permit ledger holds unknown state {state:?}"),
            Self::UnknownHolder(holder) => {
                write!(f, "permit ledger holds no permit for {holder:?}")
            }
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

fn unix_now() -> u64 {
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
        let conn = Connection::open(path)?;
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
            );",
        )?;
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
    /// is left untouched) and reports [`AcquireOutcome::AlreadyHeld`].
    /// Full ledgers and stale generations grant nothing.
    ///
    /// `pid` records the acquiring host process for crash recovery (see
    /// [`Self::sweep_dead_uncertain`]); lanes whose holders are not host
    /// processes pass `None`.
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
        let max: Option<i64> =
            tx.query_row("SELECT max_jobs FROM permit_meta WHERE id = 1", [], |row| {
                row.get(0)
            })?;
        let Some(max) = max.and_then(|max| u32::try_from(max).ok()) else {
            return Ok(AcquireOutcome::NotConfigured);
        };
        let held: Option<String> = tx
            .query_row(
                "SELECT holder FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?;
        if held.is_some() {
            return Ok(AcquireOutcome::AlreadyHeld);
        }
        let occupied: i64 = tx.query_row("SELECT COUNT(*) FROM permits", [], |row| row.get(0))?;
        if occupied.max(0) as u64 >= u64::from(max) {
            return Ok(AcquireOutcome::Full);
        }
        let now = unix_now() as i64;
        tx.execute(
            "INSERT INTO permits (holder, lane, state, acquired_unix, updated_unix, generation, pid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                holder,
                lane.as_str(),
                state.as_str(),
                now,
                now,
                i64::try_from(generation).unwrap_or(i64::MAX),
                pid.map(i64::from),
            ],
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
        let now = unix_now() as i64;
        tx.execute(
            "UPDATE permits SET state = ?1, updated_unix = ?2, generation = ?3, pid = ?4
             WHERE holder = ?5",
            params![
                state.as_str(),
                now,
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(pid),
                holder,
            ],
        )?;
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
        let now = unix_now() as i64;
        let updated = tx.execute(
            "UPDATE permits SET state = ?1, updated_unix = ?2, generation = ?3 WHERE holder = ?4",
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

    /// Release one holder's permit. Unfenced by design (see module docs);
    /// returns whether a row was removed.
    pub fn release(&self, holder: &str) -> Result<bool, LedgerError> {
        let removed = self
            .conn
            .execute("DELETE FROM permits WHERE holder = ?1", params![holder])?;
        Ok(removed > 0)
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
                report.confirmed.push((*holder).to_string());
            }
        }
        let mut select = tx.prepare("SELECT holder FROM permits")?;
        let recorded: Vec<String> = select
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        drop(select);
        for holder in &recorded {
            if alive.iter().any(|(live, _, _)| live == holder) {
                continue;
            }
            tx.execute(
                "UPDATE permits SET state = 'uncertain', updated_unix = ?1, generation = ?2
                 WHERE holder = ?3",
                params![now, generation, holder],
            )?;
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
        // Dead pid, unprotected: swept.
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

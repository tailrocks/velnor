//! Native oldest-observed demand queue: the admission-order half of the
//! host-wide `max_jobs=N` authority.
//!
//! The permit ledger answers "is there capacity"; this table answers "whose
//! turn is it". Every native broker offer is submitted under its GitHub
//! `runner_request_id` with an immutable `(first_seen_unix, sequence)` age.
//! Redelivery retains the original age, so a re-offered job never jumps the
//! queue. Scopes share the one sequence: ordering (and the single `N`) range
//! across every scope on the host, with no per-scope reservation.
//!
//! The table lives in the permit-ledger database file so every daemon on the
//! host resolves to the same queue, exactly like the ledger itself. It is
//! opened over its own connection (the [`DemandStore`][scaleset] precedent):
//! ordering reads never join the ledger's immediate transactions.
//!
//! Layering: ordering is advisory, capacity is authoritative. A younger
//! offer defers while older fresh eligible demand exists and free permits
//! cannot cover both; every demand-store failure degrades to [`FenceOutcome::Blind`]
//! (proceed to the ledger fence, which still fails closed). A blind grant
//! can overtake, but it can never overspend: `occupied <= N` is enforced
//! atomically by the ledger, not by this queue.
//!
//! No global FIFO is promised: GitHub controls delivery and assignment, so
//! observed order is not submission order, and two offers racing inside one
//! submit window resolve by ledger commit order. Deferral is bounded by
//! freshness, not by a counter: a row untouched for [`STALE_AFTER_SECS`]
//! is presumed dead upstream (its slot would otherwise re-touch it on every
//! redelivery) and no longer blocks younger demand. Stale rows are kept for
//! forensics; a later redelivery revives them with the original age.
//!
//! [scaleset]: crate::scaleset::demand::DemandStore

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

/// SQLite busy timeout for multi-process demand contention. Matches the
/// ledger: submit/check/mark are one immediate transaction each.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// An eligible row untouched this long is presumed dead upstream and no
/// longer defers younger demand. Skipped offers are re-touched on every
/// redelivery, so a live demand cannot go stale while any slot is offered
/// it; 300s matches the host's queue-wait patience scale.
pub const STALE_AFTER_SECS: u64 = 300;

/// Lifecycle of one native demand row. Only [`DemandState::Eligible`] rows
/// can defer a younger offer; `granted` rows hold a ledger permit and
/// `terminal` rows were served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DemandState {
    Eligible,
    Granted,
    Terminal,
}

impl DemandState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Granted => "granted",
            Self::Terminal => "terminal",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "eligible" => Ok(Self::Eligible),
            "granted" => Ok(Self::Granted),
            "terminal" => Ok(Self::Terminal),
            unknown => anyhow::bail!("native_demand holds unknown state {unknown:?}"),
        }
    }
}

/// One `native_demand` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDemand {
    pub request_id: String,
    pub scope: String,
    pub first_seen_unix: u64,
    pub sequence: i64,
    pub state: DemandState,
    pub updated_unix: u64,
}

/// Outcome of submitting one broker offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// Fresh row; carries the allocated immutable sequence.
    Inserted { sequence: i64 },
    /// Redelivery: the existing row (original age) stands; its
    /// `updated_unix` is refreshed so live demand never goes stale.
    Redelivered { state: DemandState },
    /// Re-offer of a served request; the terminal row is never revived.
    ReofferedTerminal,
}

/// Pure oldest-first rule evaluated at the permit fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferDecision {
    /// Grant: free permits cover every older fresh eligible row plus this
    /// offer, or there is no older fresh demand.
    Grant,
    /// Defer: `older` fresh eligible rows are older than this offer and
    /// free permits cannot cover them all. The broker redelivers.
    Defer { older: u32 },
}

/// Evaluate the oldest-first rule. `free_permits` is `None` when the ledger
/// has no configured `N`: capacity is then undecided and the caller
/// proceeds to the ledger fence, which fails closed on its own.
#[must_use]
pub fn grant_or_defer(older_fresh_eligible: u32, free_permits: Option<u32>) -> DeferDecision {
    match free_permits {
        // Defer means "older demand exists and capacity cannot cover it
        // plus this offer". With no older demand the fence always grants
        // and the ledger behind it fails closed on Full.
        Some(free) if older_fresh_eligible > 0 && free <= older_fresh_eligible => {
            DeferDecision::Defer {
                older: older_fresh_eligible,
            }
        }
        _ => DeferDecision::Grant,
    }
}

/// Outcome of [`fence_admission`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceOutcome {
    /// Submitted (or re-submitted) and oldest-first grants this offer.
    Grant,
    /// Submitted, but `older` fresh eligible rows are older and free
    /// permits cannot cover them all. Skip; the broker redelivers.
    Defer { older: u32 },
    /// Ordering is unavailable (`reason`): the demand store or the ledger
    /// read failed. Proceed to the ledger fence, which still fails closed
    /// on capacity. A blind grant can overtake, never overspend.
    Blind { reason: String },
}

/// Durable demand store over `native_demand` in the ledger database file.
///
/// Opens its own connection; [`Self::open`] ensures the schema so the first
/// submit on a fresh ledger file just works.
#[derive(Debug)]
pub struct NativeDemandStore {
    conn: Connection,
}

impl NativeDemandStore {
    /// Open the demand store at the ledger database path.
    pub fn open(ledger_path: &Path) -> Result<Self> {
        // The ledger owns directory creation; a demand-only open on a fresh
        // path must not conjure directories the ledger never approved.
        let conn = Connection::open(ledger_path).context("open native demand database")?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .context("set demand store busy timeout")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS native_demand (
                request_id TEXT PRIMARY KEY,
                scope TEXT NOT NULL,
                first_seen_unix INTEGER NOT NULL,
                sequence INTEGER NOT NULL,
                state TEXT NOT NULL,
                updated_unix INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_native_demand_order
                ON native_demand (state, first_seen_unix, sequence);",
        )
        .context("ensure native demand schema")?;
        Ok(Self { conn })
    }

    fn row_to_demand(row: &rusqlite::Row<'_>) -> rusqlite::Result<NativeDemand> {
        let state_raw: String = row.get(4)?;
        let state = DemandState::parse(&state_raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, error.into())
        })?;
        let first_seen_raw: i64 = row.get(2)?;
        let updated_raw: i64 = row.get(5)?;
        Ok(NativeDemand {
            request_id: row.get(0)?,
            scope: row.get(1)?,
            first_seen_unix: first_seen_raw.max(0) as u64,
            sequence: row.get(3)?,
            state,
            updated_unix: updated_raw.max(0) as u64,
        })
    }

    /// Insert one offer, or retain the existing row on redelivery.
    ///
    /// Fresh rows start `eligible`. Redeliveries refresh `updated_unix` but
    /// never touch `first_seen_unix` or `sequence`, and never revive
    /// terminal rows.
    pub fn submit_offer(
        &mut self,
        request_id: &str,
        scope: &str,
        now_unix: u64,
    ) -> Result<SubmitOutcome> {
        if let Some(existing) = self.get(request_id)? {
            if existing.state == DemandState::Terminal {
                return Ok(SubmitOutcome::ReofferedTerminal);
            }
            self.conn
                .execute(
                    "UPDATE native_demand SET updated_unix = ?1 WHERE request_id = ?2",
                    params![i64::try_from(now_unix).unwrap_or(i64::MAX), request_id],
                )
                .context("touch redelivered demand row")?;
            return Ok(SubmitOutcome::Redelivered {
                state: existing.state,
            });
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin demand submit transaction")?;
        let sequence: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM native_demand",
                [],
                |row| row.get(0),
            )
            .context("allocate demand sequence")?;
        let now = i64::try_from(now_unix).unwrap_or(i64::MAX);
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO native_demand
                 (request_id, scope, first_seen_unix, sequence, state, updated_unix)
                 VALUES (?1, ?2, ?3, ?4, 'eligible', ?3)",
                params![request_id, scope, now, sequence],
            )
            .context("insert demand row")?;
        tx.commit().context("commit demand submit")?;
        if inserted == 0 {
            // Lost a submit race; the winner's row (original age) stands.
            let state = self
                .get(request_id)?
                .map_or(DemandState::Eligible, |row| row.state);
            if state == DemandState::Terminal {
                return Ok(SubmitOutcome::ReofferedTerminal);
            }
            return Ok(SubmitOutcome::Redelivered { state });
        }
        Ok(SubmitOutcome::Inserted { sequence })
    }

    /// Fetch one demand row by request ID.
    pub fn get(&self, request_id: &str) -> Result<Option<NativeDemand>> {
        self.conn
            .query_row(
                "SELECT request_id, scope, first_seen_unix, sequence, state, updated_unix
                 FROM native_demand WHERE request_id = ?1",
                params![request_id],
                Self::row_to_demand,
            )
            .optional()
            .context("fetch demand row")
    }

    /// Count `eligible` rows strictly older than `(first_seen_unix,
    /// sequence)` and touched after `fresh_after_unix`. Stale rows are
    /// presumed dead upstream and do not defer.
    pub fn count_older_fresh_eligible(
        &self,
        first_seen_unix: u64,
        sequence: i64,
        fresh_after_unix: u64,
    ) -> Result<u32> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM native_demand
                 WHERE state = 'eligible'
                   AND (first_seen_unix, sequence) < (?1, ?2)
                   AND updated_unix > ?3",
                params![
                    i64::try_from(first_seen_unix).unwrap_or(i64::MAX),
                    sequence,
                    i64::try_from(fresh_after_unix).unwrap_or(i64::MAX),
                ],
                |row| row.get(0),
            )
            .context("count older fresh eligible demand")?;
        Ok(u32::try_from(count.max(0)).unwrap_or(u32::MAX))
    }

    /// Oldest-first `eligible` rows across every scope, bounded by `limit`.
    /// Tests and the future cross-mode union query; the fence counts
    /// instead of listing, so production builds omit this helper.
    #[cfg(any(test, feature = "test-support"))]
    pub fn oldest_eligible(&self, limit: usize) -> Result<Vec<NativeDemand>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT request_id, scope, first_seen_unix, sequence, state, updated_unix
                 FROM native_demand WHERE state = 'eligible'
                 ORDER BY first_seen_unix, sequence LIMIT ?1",
            )
            .context("prepare oldest-eligible query")?;
        let rows = stmt
            .query_map(
                params![i64::try_from(limit).unwrap_or(i64::MAX)],
                Self::row_to_demand,
            )
            .context("query oldest-eligible rows")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("read oldest-eligible rows")
    }

    fn upsert_state(
        &mut self,
        request_id: &str,
        scope: &str,
        state: DemandState,
        now_unix: u64,
    ) -> Result<()> {
        let now = i64::try_from(now_unix).unwrap_or(i64::MAX);
        let updated = self
            .conn
            .execute(
                "UPDATE native_demand SET state = ?1, updated_unix = ?2 WHERE request_id = ?3",
                params![state.as_str(), now, request_id],
            )
            .context("update demand row state")?;
        if updated == 0 {
            // No submitted row (a permit path that never fenced, or a unit
            // test driving the guard directly): record the demand late with
            // now-age rather than leaving the permit rowless.
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .context("begin late demand submit transaction")?;
            let sequence: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(sequence), 0) + 1 FROM native_demand",
                    [],
                    |row| row.get(0),
                )
                .context("allocate late demand sequence")?;
            tx.execute(
                "INSERT OR IGNORE INTO native_demand
                 (request_id, scope, first_seen_unix, sequence, state, updated_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?3)",
                params![request_id, scope, now, sequence, state.as_str()],
            )
            .context("insert late demand row")?;
            tx.commit().context("commit late demand submit")?;
        }
        Ok(())
    }

    /// The offer's permit was granted: the row holds a permit now.
    pub fn mark_granted(&mut self, request_id: &str, scope: &str, now_unix: u64) -> Result<()> {
        self.upsert_state(request_id, scope, DemandState::Granted, now_unix)
    }

    /// The offer's permit was released before terminal work: the demand is
    /// re-grantable and keeps its original age.
    pub fn mark_eligible(&mut self, request_id: &str, scope: &str, now_unix: u64) -> Result<()> {
        self.upsert_state(request_id, scope, DemandState::Eligible, now_unix)
    }

    /// The offer was served (terminal work done, whatever the ledger
    /// retention): it never defers again and is never revived.
    pub fn mark_terminal(&mut self, request_id: &str, scope: &str, now_unix: u64) -> Result<()> {
        self.upsert_state(request_id, scope, DemandState::Terminal, now_unix)
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Current time for production fence calls. Tests pass explicit instants.
#[must_use]
pub fn now_unix() -> u64 {
    unix_now()
}

/// Submit one broker offer and evaluate the oldest-first rule at the
/// permit fence, in one call for the slot hot path.
///
/// `stale_after_secs` bounds deferral: eligible rows untouched longer than
/// that are presumed dead upstream. Every failure degrades to
/// [`FenceOutcome::Blind`] with a reason: ordering is advisory, and the
/// ledger fence behind this call still fails closed on capacity.
pub fn fence_admission(
    ledger_path: &Path,
    request_id: &str,
    scope: &str,
    now_unix: u64,
    stale_after_secs: u64,
) -> FenceOutcome {
    if request_id.is_empty() {
        return FenceOutcome::Blind {
            reason: "empty runner request id cannot key demand".to_owned(),
        };
    }
    let mut store = match NativeDemandStore::open(ledger_path) {
        Ok(store) => store,
        Err(error) => {
            return FenceOutcome::Blind {
                reason: format!("demand store open failed: {error:#}"),
            }
        }
    };
    let submitted = match store.submit_offer(request_id, scope, now_unix) {
        Ok(submitted) => submitted,
        Err(error) => {
            return FenceOutcome::Blind {
                reason: format!("demand submit failed: {error:#}"),
            }
        }
    };
    if submitted == SubmitOutcome::ReofferedTerminal {
        // A served request re-offered: proceed exactly as before demand
        // existed; the ledger and run-service idempotency decide.
        return FenceOutcome::Grant;
    }
    let mine = match store.get(request_id) {
        Ok(Some(mine)) => mine,
        Ok(None) => {
            return FenceOutcome::Blind {
                reason: "demand row vanished after submit".to_owned(),
            }
        }
        Err(error) => {
            return FenceOutcome::Blind {
                reason: format!("demand fetch failed: {error:#}"),
            }
        }
    };
    let fresh_after = now_unix.saturating_sub(stale_after_secs);
    let older =
        match store.count_older_fresh_eligible(mine.first_seen_unix, mine.sequence, fresh_after) {
            Ok(older) => older,
            Err(error) => {
                return FenceOutcome::Blind {
                    reason: format!("demand count failed: {error:#}"),
                }
            }
        };
    let free = match free_permits(ledger_path) {
        Ok(free) => free,
        Err(error) => {
            return FenceOutcome::Blind {
                reason: format!("ledger free-capacity read failed: {error:#}"),
            }
        }
    };
    match grant_or_defer(older, free) {
        DeferDecision::Grant => FenceOutcome::Grant,
        DeferDecision::Defer { older } => FenceOutcome::Defer { older },
    }
}

/// Free permits (`N - occupied`), or `None` when no `N` is configured.
/// Raw, not advertisement-gated: `acquire` does not check reconciliation,
/// so the fence must predict what `acquire` will do.
fn free_permits(ledger_path: &Path) -> Result<Option<u32>> {
    use velnor_control::permit_ledger::PermitLedger;
    let ledger = PermitLedger::open(ledger_path).context("open ledger for fence read")?;
    let max = ledger.max_jobs().context("read max_jobs for fence")?;
    let Some(max) = max else {
        return Ok(None);
    };
    let occupied = ledger.occupied().context("read occupancy for fence")?;
    Ok(Some(max.saturating_sub(occupied)))
}

/// Best-effort demand transition for permit-guard lifecycle hooks. Never
/// fails the caller: ordering follows capacity, not the reverse. Unknown
/// holders (no `native/` prefix) are not demand and are ignored.
pub(crate) fn transition_best_effort(
    ledger_path: &Path,
    holder: &str,
    scope: &str,
    state: DemandState,
    now_unix: u64,
) {
    let Some(request_id) = holder.strip_prefix("native/") else {
        return;
    };
    if request_id.is_empty() {
        return;
    }
    let result = (|| -> Result<()> {
        let mut store = NativeDemandStore::open(ledger_path)?;
        match state {
            DemandState::Granted => store.mark_granted(request_id, scope, now_unix),
            DemandState::Eligible => store.mark_eligible(request_id, scope, now_unix),
            DemandState::Terminal => store.mark_terminal(request_id, scope, now_unix),
        }
    })();
    if let Err(error) = result {
        eprintln!(
            "Warning: native demand transition to {} failed for {request_id}: {error:#}",
            state.as_str(),
        );
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

    fn temp_ledger_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-native-demand-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("permit-ledger.db")
    }

    fn configure_ledger(path: &Path, max_jobs: u32) {
        use velnor_control::permit_ledger::PermitLedger;
        let mut ledger = PermitLedger::open(path).unwrap();
        ledger.set_max_jobs(max_jobs).unwrap();
        ledger.begin_epoch().unwrap();
        ledger.reconcile(&[]).unwrap();
    }

    #[test]
    fn submit_allocates_monotonic_sequences_across_scopes() {
        let path = temp_ledger_path("sequences");
        configure_ledger(&path, 4);
        let mut store = NativeDemandStore::open(&path).unwrap();
        let first = store.submit_offer("req-a", "scope-a", 1_000).unwrap();
        let second = store.submit_offer("req-b", "scope-b", 1_000).unwrap();
        assert_eq!(first, SubmitOutcome::Inserted { sequence: 1 });
        assert_eq!(second, SubmitOutcome::Inserted { sequence: 2 });
        // One sequence across scopes: no per-scope reservation to hide in.
        let oldest = store.oldest_eligible(16).unwrap();
        assert_eq!(oldest.len(), 2);
        assert_eq!(oldest[0].request_id, "req-a");
        assert_eq!(oldest[1].request_id, "req-b");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn redelivery_retains_age_and_refreshes_liveness() {
        let path = temp_ledger_path("redeliver");
        configure_ledger(&path, 4);
        let mut store = NativeDemandStore::open(&path).unwrap();
        assert_eq!(
            store.submit_offer("req-1", "scope-a", 1_000).unwrap(),
            SubmitOutcome::Inserted { sequence: 1 }
        );
        assert_eq!(
            store.submit_offer("req-1", "scope-a", 9_999).unwrap(),
            SubmitOutcome::Redelivered {
                state: DemandState::Eligible
            }
        );
        let row = store.get("req-1").unwrap().unwrap();
        assert_eq!(row.first_seen_unix, 1_000);
        assert_eq!(row.sequence, 1);
        assert_eq!(row.updated_unix, 9_999);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn terminal_rows_are_never_revived() {
        let path = temp_ledger_path("terminal");
        configure_ledger(&path, 4);
        let mut store = NativeDemandStore::open(&path).unwrap();
        store.submit_offer("req-1", "scope-a", 1_000).unwrap();
        store.mark_terminal("req-1", "scope-a", 1_001).unwrap();
        assert_eq!(
            store.submit_offer("req-1", "scope-a", 2_000).unwrap(),
            SubmitOutcome::ReofferedTerminal
        );
        let row = store.get("req-1").unwrap().unwrap();
        assert_eq!(row.state, DemandState::Terminal);
        assert_eq!(row.updated_unix, 1_001);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn older_count_ignores_stale_younger_and_noneligible_rows() {
        let path = temp_ledger_path("count");
        configure_ledger(&path, 4);
        let mut store = NativeDemandStore::open(&path).unwrap();
        // Old but stale (untouched for 301s): presumed dead, never counts.
        store.submit_offer("stale", "scope-a", 1_000).unwrap();
        // Old and fresh: counts.
        store.submit_offer("old", "scope-b", 1_100).unwrap();
        store.submit_offer("old", "scope-b", 1_390).unwrap();
        // Granted (holds a permit) and terminal (served): never count.
        store.submit_offer("holds", "scope-a", 1_200).unwrap();
        store.mark_granted("holds", "scope-a", 1_390).unwrap();
        store.submit_offer("served", "scope-a", 1_300).unwrap();
        store.mark_terminal("served", "scope-a", 1_390).unwrap();
        // Mine, youngest.
        store.submit_offer("mine", "scope-a", 1_400).unwrap();
        let mine = store.get("mine").unwrap().unwrap();
        // fresh_after = 1400 - 300 = 1100: `old` (touched 1390) counts,
        // `stale` (touched 1000) does not.
        assert_eq!(
            store
                .count_older_fresh_eligible(mine.first_seen_unix, mine.sequence, 1_100)
                .unwrap(),
            1
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn grant_or_defer_covers_the_rule_matrix() {
        // No older demand: grant whatever the capacity (even zero free —
        // the ledger fence behind the rule fails closed on Full).
        assert_eq!(grant_or_defer(0, Some(0)), DeferDecision::Grant);
        assert_eq!(grant_or_defer(0, None), DeferDecision::Grant);
        // Free permits cover every older row plus this offer: grant.
        assert_eq!(grant_or_defer(2, Some(3)), DeferDecision::Grant);
        // Free permits cannot cover the older rows plus this offer: defer.
        assert_eq!(
            grant_or_defer(2, Some(2)),
            DeferDecision::Defer { older: 2 }
        );
        assert_eq!(
            grant_or_defer(1, Some(0)),
            DeferDecision::Defer { older: 1 }
        );
        // Unconfigured ledger: proceed; the ledger fence fails closed.
        assert_eq!(grant_or_defer(5, None), DeferDecision::Grant);
    }

    #[test]
    fn fence_defers_young_behind_old_and_grants_after_service() {
        use velnor_control::permit_ledger::{AcquireOutcome, PermitLane, PermitState};
        let path = temp_ledger_path("fence");
        configure_ledger(&path, 1);
        // Old submits first (different scope: ordering crosses scopes).
        assert_eq!(
            fence_admission(&path, "old", "scope-a", 1_000, STALE_AFTER_SECS),
            FenceOutcome::Grant
        );
        // Young submits while one free permit cannot cover old + young.
        assert_eq!(
            fence_admission(&path, "young", "scope-b", 1_001, STALE_AFTER_SECS),
            FenceOutcome::Defer { older: 1 }
        );
        // Old takes the only permit; young still defers (full AND older).
        let mut ledger = velnor_control::permit_ledger::PermitLedger::open(&path).unwrap();
        let generation = ledger.generation().unwrap();
        assert_eq!(
            ledger
                .acquire(
                    "native/old",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            fence_admission(&path, "young", "scope-b", 1_002, STALE_AFTER_SECS),
            FenceOutcome::Defer { older: 1 }
        );
        // Old is served and released; young is now oldest and grants.
        assert!(ledger.release("native/old").unwrap());
        let mut store = NativeDemandStore::open(&path).unwrap();
        store.mark_terminal("old", "scope-a", 1_003).unwrap();
        assert_eq!(
            fence_admission(&path, "young", "scope-b", 1_004, STALE_AFTER_SECS),
            FenceOutcome::Grant
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn fence_grants_young_when_capacity_covers_every_older_row() {
        let path = temp_ledger_path("plenty");
        configure_ledger(&path, 4);
        assert_eq!(
            fence_admission(&path, "old", "scope-a", 1_000, STALE_AFTER_SECS),
            FenceOutcome::Grant
        );
        // Two free permits cover old + young: no pointless deferral.
        assert_eq!(
            fence_admission(&path, "young", "scope-b", 1_001, STALE_AFTER_SECS),
            FenceOutcome::Grant
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn fence_ignores_stale_older_rows() {
        let path = temp_ledger_path("stale-fence");
        configure_ledger(&path, 1);
        assert_eq!(
            fence_admission(&path, "old", "scope-a", 1_000, STALE_AFTER_SECS),
            FenceOutcome::Grant
        );
        // 301s later with no redelivery touch: old is presumed dead
        // upstream and no longer blocks young.
        assert_eq!(
            fence_admission(&path, "young", "scope-b", 1_301, STALE_AFTER_SECS),
            FenceOutcome::Grant
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn fence_is_blind_never_failed_on_broken_paths() {
        // A directory is not a database: every read degrades to Blind.
        let dir = std::env::temp_dir().join(format!(
            "velnor-native-demand-broken-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let outcome = fence_admission(&dir, "req-1", "scope-a", 1_000, STALE_AFTER_SECS);
        assert!(matches!(outcome, FenceOutcome::Blind { .. }), "{outcome:?}");
        assert!(matches!(
            fence_admission(
                &dir.join("permit-ledger.db"),
                "",
                "scope-a",
                1_000,
                STALE_AFTER_SECS
            ),
            FenceOutcome::Blind { .. }
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn transitions_upsert_late_rows_and_ignore_foreign_holders() {
        let path = temp_ledger_path("upsert");
        configure_ledger(&path, 2);
        // No prior submit: the transition records the demand late.
        transition_best_effort(&path, "native/late", "scope-a", DemandState::Granted, 1_000);
        let store = NativeDemandStore::open(&path).unwrap();
        let row = store.get("late").unwrap().unwrap();
        assert_eq!(row.state, DemandState::Granted);
        assert_eq!(row.first_seen_unix, 1_000);
        // Foreign holders are not native demand: silently ignored.
        transition_best_effort(
            &path,
            "scaleset/7/9",
            "scope-a",
            DemandState::Terminal,
            1_001,
        );
        assert!(store.get("scaleset/7/9").unwrap().is_none());
        assert!(store.get("7/9").unwrap().is_none());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

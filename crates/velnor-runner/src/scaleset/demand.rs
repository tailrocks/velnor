//! Global oldest-observed demand queue (§5.1 step 2) + trust-before-grant.
//!
//! Every `JobAvailable` lands in `scaleset_demand` keyed by GitHub
//! `runnerRequestId`. `first_seen_at` + `sequence` are immutable:
//! redelivery retains the original age, so a re-offered job never jumps the
//! queue. Grants are generation-fenced: a `granted` row from a stale epoch
//! is void and returns to `eligible` (age kept) before any new grant.
//!
//! Trust gate: [`classify_offer`] mirrors the [`TrustClass::derive`][tc]
//! decision structure over the fields an offer actually carries
//! (`event_name`, `owner/repo`). Scale-set offers never carry the head-repo
//! signals `derive` needs for `pull_request`/`workflow_run` events, so
//! those offers stay `observed` with a reason — never granted blind. The
//! enrichment that could promote them (job fetch at acquire time) is a
//! follow-up; failing closed here is the specified behavior.
//!
//! [tc]: crate::trust_class::TrustClass::derive

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, ToSql};
use velnor_model::ScaleSetJobAvailable;

/// Demand-row lifecycle. Stored verbatim in `scaleset_demand.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DemandState {
    /// Seen but not yet grantable (structurally incomplete or trust-unknown).
    Observed,
    /// Structurally complete offer awaiting the trust gate.
    Eligible,
    /// Trust passed in the recorded generation; awaiting reserve+acquire.
    Granted,
    /// Acquire intent persisted; `acquirejobs` call outstanding or crashed.
    AcquireIntent,
    /// Server returned this ID from `acquirejobs`.
    Acquired,
    /// `acquirejobs` transport failed after send; may-or-may-not be ours.
    Uncertain,
    /// Provision intent persisted; worker lane owns creation.
    ProvisionIntent,
    /// Terminally refused with a reason; never re-offered by us.
    Declined,
    /// Externally resolved (`JobCompleted` observed) or fully released.
    Terminal,
}

impl DemandState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Eligible => "eligible",
            Self::Granted => "granted",
            Self::AcquireIntent => "acquire_intent",
            Self::Acquired => "acquired",
            Self::Uncertain => "uncertain",
            Self::ProvisionIntent => "provision_intent",
            Self::Declined => "declined",
            Self::Terminal => "terminal",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "observed" => Ok(Self::Observed),
            "eligible" => Ok(Self::Eligible),
            "granted" => Ok(Self::Granted),
            "acquire_intent" => Ok(Self::AcquireIntent),
            "acquired" => Ok(Self::Acquired),
            "uncertain" => Ok(Self::Uncertain),
            "provision_intent" => Ok(Self::ProvisionIntent),
            "declined" => Ok(Self::Declined),
            "terminal" => Ok(Self::Terminal),
            unknown => anyhow::bail!("scaleset_demand holds unknown state {unknown:?}"),
        }
    }

    /// States that hold (or are owed) a ledger permit for this set.
    #[must_use]
    pub const fn holds_permit(self) -> bool {
        matches!(
            self,
            Self::Granted
                | Self::AcquireIntent
                | Self::Acquired
                | Self::Uncertain
                | Self::ProvisionIntent
        )
    }
}

/// One `scaleset_demand` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Demand {
    pub request_id: i64,
    pub scale_set_id: i32,
    pub first_seen_at: String,
    pub sequence: i64,
    pub state: DemandState,
    pub decline_reason: Option<String>,
    pub repo_owner: String,
    pub repo_name: String,
    /// Stable i64 projection of the wire `jobId` GUID: the v21 column is
    /// `INTEGER` but the wire value is a string, so the exact GUID is not
    /// storable there. `request_id` stays the exact key; this hash is
    /// correlation-only.
    pub job_id_hash: i64,
    pub generation: u64,
    pub updated_at: String,
    /// Durable deciding input for the grant-pass trust gate (v23).
    pub event_name: String,
}

/// Outcome of submitting one offered job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// Fresh row; carries the allocated immutable sequence.
    Inserted { sequence: i64 },
    /// Redelivery: existing row kept with its original age.
    Redelivered { state: DemandState },
    /// Re-offer of a terminally resolved request; stays terminal.
    ReofferedTerminal,
}

/// Trust verdict for one offer, mirroring `TrustClass::derive` over
/// offer-visible fields only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferTrust {
    Trusted,
    Unknown { reason: &'static str },
}

/// The classifiable slice of an offer: the only fields the verdict reads.
#[derive(Debug, Clone, Copy)]
pub struct OfferProbe<'a> {
    pub event_name: &'a str,
    pub owner_name: &'a str,
    pub repository_name: &'a str,
}

impl<'a> From<&'a ScaleSetJobAvailable> for OfferProbe<'a> {
    fn from(offer: &'a ScaleSetJobAvailable) -> Self {
        Self {
            event_name: &offer.base.event_name,
            owner_name: &offer.base.owner_name,
            repository_name: &offer.base.repository_name,
        }
    }
}

/// `pull_request` prefix length, mirroring `trust_class.rs`: `pull_request`,
/// `pull_request_target`, `pull_request_review(_comment)` all share it.
const PULL_REQUEST_PREFIX: &[u8; 12] = b"pull_request";

fn is_pull_request_event(event: &str) -> bool {
    event.len() >= PULL_REQUEST_PREFIX.len()
        && event.as_bytes()[..PULL_REQUEST_PREFIX.len()].eq_ignore_ascii_case(PULL_REQUEST_PREFIX)
}

fn is_workflow_run_event(event: &str) -> bool {
    event.eq_ignore_ascii_case("workflow_run")
}

/// Classify one offer. Pure over the offer: no I/O, no clock.
///
/// Mirrors the `TrustClass::derive` structure: unparseable event → unknown;
/// missing base repo → unknown; fork-sensitive events → unknown (the head
/// repo `derive` would compare against is not carried by any scale-set
/// offer, so the comparison is impossible, not merely skipped); anything
/// else with a base repo → trusted.
#[must_use]
pub fn classify_offer(offer: &ScaleSetJobAvailable) -> OfferTrust {
    classify_probe(&OfferProbe::from(offer))
}

/// Classify stored offer fields. The grant pass re-runs the gate over the
/// durable row (not the submit-time verdict) so stale-reset rows and
/// policy changes re-evaluate from the same inputs.
#[must_use]
pub fn classify_probe(probe: &OfferProbe<'_>) -> OfferTrust {
    let event = probe.event_name.trim();
    if event.is_empty()
        || !event
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return OfferTrust::Unknown {
            reason: "event-unparseable",
        };
    }
    if probe.owner_name.trim().is_empty() || probe.repository_name.trim().is_empty() {
        return OfferTrust::Unknown {
            reason: "repo-missing",
        };
    }
    if is_pull_request_event(event) || is_workflow_run_event(event) {
        return OfferTrust::Unknown {
            reason: "trust-inputs-missing",
        };
    }
    OfferTrust::Trusted
}

/// Durable demand store over `scaleset_demand`.
///
/// Opens its own connection to the state database; [`Store::open`][store]
/// runs first so the v21/v22 schema is guaranteed before any statement.
/// Single-writer discipline comes from SQLite immediate transactions +
/// the daemon's one-adapter-per-set topology, same as the ledger.
///
/// [store]: velnor_control::store::Store::open
#[derive(Debug)]
pub struct DemandStore {
    conn: Connection,
}

impl DemandStore {
    /// Open the demand store at the state database path.
    pub fn open(path: &Path) -> Result<Self> {
        velnor_control::store::Store::open(path).context("migrate scale-set demand schema")?;
        let conn = Connection::open(path).context("open scale-set demand database")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("set demand store busy timeout")?;
        Ok(Self { conn })
    }

    fn now_rfc3339() -> String {
        velnor_model::Timestamp::now()
            .to_rfc3339()
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
    }

    fn row_to_demand(row: &rusqlite::Row<'_>) -> rusqlite::Result<Demand> {
        let state_raw: String = row.get(4)?;
        let state = DemandState::parse(&state_raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, error.into())
        })?;
        let generation_raw: i64 = row.get(9)?;
        Ok(Demand {
            request_id: row.get(0)?,
            scale_set_id: row.get(1)?,
            first_seen_at: row.get(2)?,
            sequence: row.get(3)?,
            state,
            decline_reason: row.get(5)?,
            repo_owner: row.get(6)?,
            repo_name: row.get(7)?,
            job_id_hash: row.get(8)?,
            generation: generation_raw.max(0) as u64,
            updated_at: row.get(10)?,
            event_name: row.get(11)?,
        })
    }

    /// Insert one offer, or retain the existing row on redelivery.
    ///
    /// Fresh rows start `eligible` when structurally classifiable and
    /// `observed` otherwise; redeliveries never touch `first_seen_at` or
    /// `sequence`, and never revive terminal rows.
    pub fn submit_offer(
        &mut self,
        scale_set_id: i32,
        offer: &ScaleSetJobAvailable,
        generation: u64,
    ) -> Result<SubmitOutcome> {
        let request_id = offer.base.runner_request_id;
        if let Some(existing) = self.get(request_id)? {
            if existing.state == DemandState::Terminal {
                return Ok(SubmitOutcome::ReofferedTerminal);
            }
            return Ok(SubmitOutcome::Redelivered {
                state: existing.state,
            });
        }
        let initial = match classify_offer(offer) {
            OfferTrust::Trusted => DemandState::Eligible,
            OfferTrust::Unknown { .. } => DemandState::Observed,
        };
        let reason: Option<String> = match classify_offer(offer) {
            OfferTrust::Trusted => None,
            OfferTrust::Unknown { reason } => Some(reason.to_owned()),
        };
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin demand submit transaction")?;
        let sequence: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM scaleset_demand",
                [],
                |row| row.get(0),
            )
            .context("allocate demand sequence")?;
        let now = Self::now_rfc3339();
        let labels_hash = crate::scaleset::intents::labels_hash(&offer.base.request_labels);
        let job_id_hash = crate::scaleset::intents::stable_i64(&offer.base.job_id);
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO scaleset_demand
                 (request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
                  repo_owner, repo_name, job_id, labels_hash, generation, updated_at, event_name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    request_id,
                    scale_set_id,
                    now,
                    sequence,
                    initial.as_str(),
                    reason,
                    offer.base.owner_name,
                    offer.base.repository_name,
                    job_id_hash,
                    labels_hash,
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    now,
                    offer.base.event_name,
                ],
            )
            .context("insert demand row")?;
        tx.commit().context("commit demand submit")?;
        if inserted == 0 {
            // Lost a submit race; the winner's row (original age) stands.
            let state = self
                .get(request_id)?
                .map_or(DemandState::Observed, |row| row.state);
            if state == DemandState::Terminal {
                return Ok(SubmitOutcome::ReofferedTerminal);
            }
            return Ok(SubmitOutcome::Redelivered { state });
        }
        Ok(SubmitOutcome::Inserted { sequence })
    }

    /// Fetch one demand row by request ID.
    pub fn get(&self, request_id: i64) -> Result<Option<Demand>> {
        self.conn
            .query_row(
                "SELECT request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
                        repo_owner, repo_name, job_id, generation, updated_at, event_name
                 FROM scaleset_demand WHERE request_id = ?1",
                params![request_id],
                Self::row_to_demand,
            )
            .optional()
            .context("fetch demand row")
    }

    /// Oldest-first `eligible` rows for one set, bounded by `limit`.
    pub fn oldest_eligible(&self, scale_set_id: i32, limit: usize) -> Result<Vec<Demand>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
                        repo_owner, repo_name, job_id, generation, updated_at, event_name
                 FROM scaleset_demand
                 WHERE scale_set_id = ?1 AND state = 'eligible'
                 ORDER BY first_seen_at, sequence LIMIT ?2",
            )
            .context("prepare oldest-eligible query")?;
        let rows = stmt
            .query_map(
                params![scale_set_id, i64::try_from(limit).unwrap_or(i64::MAX)],
                Self::row_to_demand,
            )
            .context("query oldest-eligible rows")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("read oldest-eligible rows")
    }

    /// Oldest-first `granted` rows for one set, bounded by `limit`.
    pub fn oldest_granted(&self, scale_set_id: i32, limit: usize) -> Result<Vec<Demand>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
                        repo_owner, repo_name, job_id, generation, updated_at, event_name
                 FROM scaleset_demand
                 WHERE scale_set_id = ?1 AND state = 'granted'
                 ORDER BY first_seen_at, sequence LIMIT ?2",
            )
            .context("prepare oldest-granted query")?;
        let rows = stmt
            .query_map(
                params![scale_set_id, i64::try_from(limit).unwrap_or(i64::MAX)],
                Self::row_to_demand,
            )
            .context("query oldest-granted rows")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("read oldest-granted rows")
    }

    /// Move one row to a new state, recording the acting generation.
    pub fn set_state(
        &mut self,
        request_id: i64,
        state: DemandState,
        decline_reason: Option<&str>,
        generation: u64,
    ) -> Result<()> {
        let now = Self::now_rfc3339();
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_demand
                 SET state = ?1, decline_reason = ?2, generation = ?3, updated_at = ?4
                 WHERE request_id = ?5",
                params![
                    state.as_str(),
                    decline_reason,
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    now,
                    request_id,
                ],
            )
            .context("update demand state")?;
        if updated == 0 {
            anyhow::bail!("demand holds no row for request {request_id}");
        }
        Ok(())
    }

    /// Void stale-epoch grants: `granted`/`acquire_intent` rows recorded
    /// under any other generation return to `eligible` with age preserved.
    /// Returns the number of rows reset.
    pub fn reset_stale_grants(&mut self, scale_set_id: i32, generation: u64) -> Result<u64> {
        let now = Self::now_rfc3339();
        let reset = self
            .conn
            .execute(
                "UPDATE scaleset_demand
                 SET state = 'eligible', decline_reason = NULL, generation = ?1, updated_at = ?2
                 WHERE scale_set_id = ?3 AND generation != ?1
                   AND state IN ('granted', 'acquire_intent')",
                params![
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    now,
                    scale_set_id,
                ],
            )
            .context("reset stale demand grants")?;
        Ok(u64::try_from(reset).unwrap_or(u64::MAX))
    }

    /// Count rows of one set currently in any of `states`.
    pub fn count_in_states(&self, scale_set_id: i32, states: &[DemandState]) -> Result<u64> {
        if states.is_empty() {
            return Ok(0);
        }
        let placeholders = states.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT COUNT(*) FROM scaleset_demand WHERE scale_set_id = ?1 AND state IN ({placeholders})"
        );
        let state_names: Vec<&str> = states.iter().map(|state| state.as_str()).collect();
        let mut refs: Vec<&dyn ToSql> = Vec::with_capacity(states.len() + 1);
        refs.push(&scale_set_id);
        for name in &state_names {
            refs.push(name);
        }
        let count: i64 = self
            .conn
            .query_row(&sql, refs.as_slice(), |row| row.get(0))
            .context("count demand rows in states")?;
        Ok(count.max(0) as u64)
    }

    /// `(scale_set_id, request_id, state)` of every set's rows in any of
    /// `states`, ordered by set then request. Feeds daemon-startup
    /// attestation, which is host-global (the ledger holds every set).
    pub fn list_in_states_all(
        &self,
        states: &[DemandState],
    ) -> Result<Vec<(i32, i64, DemandState)>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = states.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT scale_set_id, request_id, state FROM scaleset_demand
             WHERE state IN ({placeholders}) ORDER BY scale_set_id, request_id"
        );
        let state_names: Vec<&str> = states.iter().map(|state| state.as_str()).collect();
        let mut refs: Vec<&dyn ToSql> = Vec::with_capacity(states.len());
        for name in &state_names {
            refs.push(name);
        }
        let mut stmt = self.conn.prepare(&sql).context("prepare demand list")?;
        let rows = stmt
            .query_map(refs.as_slice(), |row| {
                let scale_set_id: i32 = row.get(0)?;
                let request_id: i64 = row.get(1)?;
                let state_raw: String = row.get(2)?;
                let state = DemandState::parse(&state_raw).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        error.into(),
                    )
                })?;
                Ok((scale_set_id, request_id, state))
            })
            .context("query demand list")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("read demand list")
    }

    /// `(request_id, state)` of one set's rows in any of `states`, ordered
    /// by request ID. Feeds the reconcile alive-set.
    pub fn list_in_states(
        &self,
        scale_set_id: i32,
        states: &[DemandState],
    ) -> Result<Vec<(i64, DemandState)>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = states.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT request_id, state FROM scaleset_demand
             WHERE scale_set_id = ?1 AND state IN ({placeholders}) ORDER BY request_id"
        );
        let state_names: Vec<&str> = states.iter().map(|state| state.as_str()).collect();
        let mut refs: Vec<&dyn ToSql> = Vec::with_capacity(states.len() + 1);
        refs.push(&scale_set_id);
        for name in &state_names {
            refs.push(name);
        }
        let mut stmt = self.conn.prepare(&sql).context("prepare demand list")?;
        let rows = stmt
            .query_map(refs.as_slice(), |row| {
                let request_id: i64 = row.get(0)?;
                let state_raw: String = row.get(1)?;
                let state = DemandState::parse(&state_raw).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        error.into(),
                    )
                })?;
                Ok((request_id, state))
            })
            .context("query demand list")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("read demand list")
    }
}

/// Run the trust gate over the oldest `eligible` offers: trusted offers are
/// granted in the current generation, trust-unknown offers are demoted to
/// `observed` with their reason (age kept, head-of-line blocking avoided).
/// Returns the newly granted rows, oldest first.
pub fn grant_oldest(
    store: &mut DemandStore,
    scale_set_id: i32,
    generation: u64,
    metrics: &crate::scaleset::metrics::Metrics,
) -> Result<Vec<Demand>> {
    let reset = store.reset_stale_grants(scale_set_id, generation)?;
    metrics.add_stale_grants_reset(reset);
    // `eligible` rows only exist between submit and grant, so the table scan
    // stays small; cap the pass to keep one message bounded regardless.
    let candidates = store.oldest_eligible(scale_set_id, 512)?;
    let mut granted = Vec::new();
    for candidate in candidates {
        let probe = OfferProbe {
            event_name: &candidate.event_name,
            owner_name: &candidate.repo_owner,
            repository_name: &candidate.repo_name,
        };
        match classify_probe(&probe) {
            OfferTrust::Trusted => {
                store.set_state(candidate.request_id, DemandState::Granted, None, generation)?;
                metrics.add_offers_granted(1);
                granted.push(candidate);
            }
            OfferTrust::Unknown { reason } => {
                store.set_state(
                    candidate.request_id,
                    DemandState::Observed,
                    Some(reason),
                    generation,
                )?;
            }
        }
    }
    Ok(granted)
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

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-scaleset-demand-{name}-{}",
            std::process::id()
        ));
        // Drop stale state from pid-reusing earlier runs: every test starts
        // from an empty database, deterministically.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.db")
    }

    fn offer(request_id: i64, event: &str) -> ScaleSetJobAvailable {
        ScaleSetJobAvailable {
            acquire_job_url: "https://scaleset-fixture.invalid/acquire".to_owned(),
            base: velnor_model::ScaleSetJobMessage {
                message_type: velnor_model::ScaleSetJobMessageType::JobAvailable,
                runner_request_id: request_id,
                repository_name: "velnor".to_owned(),
                owner_name: "tailrocks".to_owned(),
                job_id: format!("job-{request_id}"),
                job_workflow_ref: "tailrocks/velnor/.github/workflows/ci.yml@main".to_owned(),
                job_display_name: "build".to_owned(),
                workflow_run_id: 9001,
                event_name: event.to_owned(),
                request_labels: vec!["velnor".to_owned()],
                queue_time: "2026-09-17T00:00:01Z".to_owned(),
                scale_set_assign_time: String::new(),
                runner_assign_time: String::new(),
                finish_time: String::new(),
            },
        }
    }

    #[test]
    fn submit_assigns_sequence_and_redelivery_keeps_age() {
        let path = temp_path("submit");
        let mut store = DemandStore::open(&path).unwrap();
        let first = store.submit_offer(7, &offer(101, "push"), 3).unwrap();
        let sequence = match first {
            SubmitOutcome::Inserted { sequence } => sequence,
            other => panic!("expected insert, got {other:?}"),
        };
        let before = store.get(101).unwrap().unwrap();
        let again = store.submit_offer(7, &offer(101, "push"), 3).unwrap();
        assert_eq!(
            again,
            SubmitOutcome::Redelivered {
                state: DemandState::Eligible,
            }
        );
        let after = store.get(101).unwrap().unwrap();
        assert_eq!(after.first_seen_at, before.first_seen_at);
        assert_eq!(after.sequence, sequence);
        assert_eq!(after.sequence, before.sequence);
    }

    #[test]
    fn trust_gate_grants_push_and_parks_pr_without_blocking() {
        let path = temp_path("grant");
        let mut store = DemandStore::open(&path).unwrap();
        // PR offer submitted first: it must not head-of-line block the push.
        store
            .submit_offer(7, &offer(201, "pull_request"), 5)
            .unwrap();
        store.submit_offer(7, &offer(202, "push"), 5).unwrap();
        let metrics = crate::scaleset::metrics::Metrics::new();
        let granted = grant_oldest(&mut store, 7, 5, &metrics).unwrap();
        assert_eq!(granted.len(), 1);
        assert_eq!(granted[0].request_id, 202);
        assert_eq!(
            store.get(201).unwrap().unwrap().state,
            DemandState::Observed
        );
        assert_eq!(
            store.get(201).unwrap().unwrap().decline_reason.as_deref(),
            Some("trust-inputs-missing")
        );
        assert_eq!(store.get(202).unwrap().unwrap().state, DemandState::Granted);
        assert_eq!(metrics.snapshot().offers_granted, 1);
    }

    #[test]
    fn stale_generation_grants_reset_to_eligible_with_age_kept() {
        let path = temp_path("stale");
        let mut store = DemandStore::open(&path).unwrap();
        store.submit_offer(7, &offer(301, "push"), 5).unwrap();
        let metrics = crate::scaleset::metrics::Metrics::new();
        assert_eq!(grant_oldest(&mut store, 7, 5, &metrics).unwrap().len(), 1);
        let before = store.get(301).unwrap().unwrap();
        assert_eq!(before.state, DemandState::Granted);
        let reset = store.reset_stale_grants(7, 6).unwrap();
        assert_eq!(reset, 1);
        let after = store.get(301).unwrap().unwrap();
        assert_eq!(after.state, DemandState::Eligible);
        assert_eq!(after.first_seen_at, before.first_seen_at);
        assert_eq!(after.sequence, before.sequence);
        // Re-granted fresh in the new epoch.
        assert_eq!(grant_oldest(&mut store, 7, 6, &metrics).unwrap().len(), 1);
        assert_eq!(store.get(301).unwrap().unwrap().generation, 6);
    }

    #[test]
    fn terminal_rows_never_revive_on_reoffer() {
        let path = temp_path("terminal");
        let mut store = DemandStore::open(&path).unwrap();
        store.submit_offer(7, &offer(401, "push"), 5).unwrap();
        store
            .set_state(401, DemandState::Terminal, None, 5)
            .unwrap();
        assert_eq!(
            store.submit_offer(7, &offer(401, "push"), 5).unwrap(),
            SubmitOutcome::ReofferedTerminal
        );
        assert_eq!(
            store.get(401).unwrap().unwrap().state,
            DemandState::Terminal
        );
    }

    #[test]
    fn classify_offer_fails_closed_on_unknown_inputs() {
        assert_eq!(classify_offer(&offer(1, "push")), OfferTrust::Trusted);
        assert_eq!(
            classify_offer(&offer(1, "workflow_dispatch")),
            OfferTrust::Trusted
        );
        assert_eq!(
            classify_offer(&offer(1, "pull_request")),
            OfferTrust::Unknown {
                reason: "trust-inputs-missing"
            }
        );
        assert_eq!(
            classify_offer(&offer(1, "pull_request_target")),
            OfferTrust::Unknown {
                reason: "trust-inputs-missing"
            }
        );
        assert_eq!(
            classify_offer(&offer(1, "workflow_run")),
            OfferTrust::Unknown {
                reason: "trust-inputs-missing"
            }
        );
        assert_eq!(
            classify_offer(&offer(1, "")),
            OfferTrust::Unknown {
                reason: "event-unparseable"
            }
        );
        assert_eq!(
            classify_offer(&offer(1, "push;rm")),
            OfferTrust::Unknown {
                reason: "event-unparseable"
            }
        );
        let mut no_repo = offer(1, "push");
        no_repo.base.owner_name.clear();
        assert_eq!(
            classify_offer(&no_repo),
            OfferTrust::Unknown {
                reason: "repo-missing"
            }
        );
    }
}

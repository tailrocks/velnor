//! Global oldest-observed demand queue (§5.1 step 2) + trust-before-grant.
//!
//! Every `JobAvailable` lands in `scaleset_demand` keyed by its resolved
//! request identity (`runnerRequestId`, falling back to stable job/run IDs).
//! `first_seen_at` + `sequence` are immutable:
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
use velnor_control::permit_ledger::DemandState as PermitDemandState;
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
    /// Upstream canceled the assignment while an acquire batch is still
    /// in flight or has an uncertain result. Keep occupancy until acquire
    /// reconciliation proves whether the runner took ownership.
    CanceledPending,
    /// A pending cancellation was reconciled as successfully acquired;
    /// terminal cleanup must run before releasing the held permit.
    CanceledAcquired,
    /// A canceled attempt was confirmed unacquired. It cannot be offered
    /// again under this request ID; finish closing its global demand.
    CanceledDone,
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
            Self::CanceledPending => "canceled_pending",
            Self::CanceledAcquired => "canceled_acquired",
            Self::CanceledDone => "canceled_done",
            Self::ProvisionIntent => "provision_intent",
            Self::Declined => "declined",
            Self::Terminal => "terminal",
        }
    }

    pub(crate) fn parse(raw: &str) -> Result<Self> {
        match raw {
            "observed" => Ok(Self::Observed),
            "eligible" => Ok(Self::Eligible),
            "granted" => Ok(Self::Granted),
            "acquire_intent" => Ok(Self::AcquireIntent),
            "acquired" => Ok(Self::Acquired),
            "uncertain" => Ok(Self::Uncertain),
            "canceled_pending" => Ok(Self::CanceledPending),
            "canceled_acquired" => Ok(Self::CanceledAcquired),
            "canceled_done" => Ok(Self::CanceledDone),
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
                | Self::CanceledPending
                | Self::CanceledAcquired
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
    /// Captured owner token for the current permit attempt. Historical rows
    /// stay empty and cannot mutate a holder until an authorized recovery
    /// boundary establishes a new owner.
    pub permit_attempt_token: Option<String>,
}

/// Cross-database release record. The state DB journal is written before
/// deleting the host permit; replay then finishes these exact projections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StagedAttemptRelease {
    pub holder: String,
    pub scale_set_id: i32,
    pub request_id: i64,
    pub attempt_token: String,
    pub ledger_demand_state: PermitDemandState,
    pub next_demand_state: Option<DemandState>,
    pub worker_ownership_id: Option<String>,
    /// Optional durable acquire batch whose resolution depends on this
    /// release. The batch closes only after its final staged member does.
    pub batch_id: Option<String>,
    pub batch_uncertain: bool,
}

/// Cross-database fresh-acquire record. The target token is written here
/// before the permit database can insert its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StagedAttemptAcquire {
    pub holder: String,
    pub scale_set_id: i32,
    pub request_id: i64,
    pub previous_attempt_token: Option<String>,
    pub target_attempt_token: String,
}

fn permit_demand_state_name(state: PermitDemandState) -> &'static str {
    match state {
        PermitDemandState::Eligible => "eligible",
        PermitDemandState::Cancelled => "cancelled",
        PermitDemandState::Terminal => "terminal",
        PermitDemandState::Granted => "granted",
        PermitDemandState::Waiting => "waiting",
    }
}

fn parse_permit_demand_state(raw: &str) -> Result<PermitDemandState> {
    match raw {
        "eligible" => Ok(PermitDemandState::Eligible),
        "cancelled" => Ok(PermitDemandState::Cancelled),
        "terminal" => Ok(PermitDemandState::Terminal),
        other => anyhow::bail!("staged permit release has invalid ledger demand state {other:?}"),
    }
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

/// Resolve a stable non-negative 64-bit request ID from a job message.
///
/// Upstream Actions Service can transmit `runnerRequestId` as 0. Project it
/// when non-zero; otherwise hash `jobId`, use `workflowRunId` when `jobId` is
/// absent, and reserve 1 as the final zero-value fallback.
#[must_use]
pub fn resolve_job_request_id(base: &velnor_model::ScaleSetJobMessage) -> i64 {
    if base.runner_request_id != 0 {
        base.runner_request_id
    } else if !base.job_id.is_empty() {
        let val = crate::scaleset::intents::stable_i64(&base.job_id);
        if val != 0 {
            val
        } else {
            1
        }
    } else if base.workflow_run_id != 0 {
        base.workflow_run_id
    } else {
        1
    }
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

    /// Persist the state-DB half of a permit release before the separate host
    /// permit transaction runs. Replays must name the identical attempt and
    /// projections.
    pub(crate) fn stage_attempt_release(
        &mut self,
        holder: &str,
        scale_set_id: i32,
        request_id: i64,
        attempt_token: &str,
        ledger_demand_state: PermitDemandState,
        next_demand_state: Option<DemandState>,
        worker_ownership_id: Option<&str>,
    ) -> Result<StagedAttemptRelease> {
        self.stage_attempt_release_inner(
            holder,
            scale_set_id,
            request_id,
            attempt_token,
            ledger_demand_state,
            next_demand_state,
            worker_ownership_id,
            None,
        )
    }

    pub(crate) fn stage_attempt_release_for_batch(
        &mut self,
        holder: &str,
        scale_set_id: i32,
        request_id: i64,
        attempt_token: &str,
        ledger_demand_state: PermitDemandState,
        next_demand_state: Option<DemandState>,
        batch_id: &str,
    ) -> Result<StagedAttemptRelease> {
        if batch_id.is_empty() {
            anyhow::bail!("release batch id cannot be empty");
        }
        self.stage_attempt_release_inner(
            holder,
            scale_set_id,
            request_id,
            attempt_token,
            ledger_demand_state,
            next_demand_state,
            None,
            Some(batch_id),
        )
    }

    /// Atomically stage every known missing member before the first permit
    /// delete. This lets batch resolution wait on the complete member set
    /// even if the daemon dies during release replay.
    pub(crate) fn stage_attempt_releases_for_batch(
        &mut self,
        scale_set_id: i32,
        batch_id: &str,
        releases: &[(i64, String)],
    ) -> Result<Vec<StagedAttemptRelease>> {
        if batch_id.is_empty() {
            anyhow::bail!("release batch id cannot be empty");
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin batch release staging")?;
        let mut stages = Vec::with_capacity(releases.len());
        let mut seen = std::collections::BTreeSet::new();
        for (request_id, attempt_token) in releases {
            if !seen.insert(*request_id) {
                anyhow::bail!("duplicate missing request {request_id} in release batch");
            }
            let holder = format!("scaleset/{scale_set_id}/{request_id}");
            if attempt_token.is_empty() {
                anyhow::bail!("release token for {holder:?} cannot be empty");
            }
            let pending_acquire: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM scaleset_attempt_acquires WHERE holder = ?1)",
                [&holder],
                |row| row.get(0),
            )?;
            let pending_rotation: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM scaleset_attempt_rotations WHERE holder = ?1)",
                [&holder],
                |row| row.get(0),
            )?;
            if pending_acquire || pending_rotation {
                anyhow::bail!("permit transition for {holder:?} already has another stage");
            }
            let current: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT state, permit_attempt_token FROM scaleset_demand
                     WHERE scale_set_id = ?1 AND request_id = ?2",
                    params![scale_set_id, request_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((state, current_token)) = current else {
                anyhow::bail!("missing request {holder:?} has no demand row");
            };
            if DemandState::parse(&state)? != DemandState::AcquireIntent
                || current_token.as_deref() != Some(attempt_token.as_str())
            {
                anyhow::bail!("missing request {holder:?} changed before release staging");
            }
            Self::verify_release_batch_member(
                &tx,
                batch_id,
                scale_set_id,
                *request_id,
                attempt_token,
            )?;
            let existing: Option<(String, Option<String>, Option<String>)> = tx
                .query_row(
                    "SELECT attempt_token, next_demand_state, worker_ownership_id
                     FROM scaleset_attempt_releases WHERE holder = ?1",
                    [&holder],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            match existing {
                Some((recorded, Some(next), None))
                    if recorded == attempt_token.as_str() && next == "eligible" => {}
                None => {
                    tx.execute(
                        "INSERT INTO scaleset_attempt_releases
                         (holder, scale_set_id, request_id, attempt_token,
                          ledger_demand_state, next_demand_state,
                          worker_ownership_id, created_at)
                         VALUES (?1, ?2, ?3, ?4, 'eligible', 'eligible', NULL, ?5)",
                        params![
                            holder,
                            scale_set_id,
                            request_id,
                            attempt_token,
                            Self::now_rfc3339(),
                        ],
                    )?;
                }
                _ => anyhow::bail!("release stage for {holder:?} changed"),
            }
            let sidecar: Option<(String, i64)> = tx
                .query_row(
                    "SELECT batch_id, resolve_uncertain
                     FROM scaleset_attempt_release_batches WHERE holder = ?1",
                    [&holder],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            match sidecar {
                Some((recorded, 0)) if recorded == batch_id => {}
                None => {
                    tx.execute(
                        "INSERT INTO scaleset_attempt_release_batches
                         (holder, batch_id, resolve_uncertain, created_at)
                         VALUES (?1, ?2, 0, ?3)",
                        params![holder, batch_id, Self::now_rfc3339()],
                    )?;
                }
                _ => anyhow::bail!("release batch stage for {holder:?} changed"),
            }
            stages.push(StagedAttemptRelease {
                holder,
                scale_set_id,
                request_id: *request_id,
                attempt_token: attempt_token.clone(),
                ledger_demand_state: PermitDemandState::Eligible,
                next_demand_state: Some(DemandState::Eligible),
                worker_ownership_id: None,
                batch_id: Some(batch_id.to_owned()),
                batch_uncertain: false,
            });
        }
        tx.commit().context("commit batch release staging")?;
        Ok(stages)
    }

    fn stage_attempt_release_inner(
        &mut self,
        holder: &str,
        scale_set_id: i32,
        request_id: i64,
        attempt_token: &str,
        ledger_demand_state: PermitDemandState,
        next_demand_state: Option<DemandState>,
        worker_ownership_id: Option<&str>,
        batch_id: Option<&str>,
    ) -> Result<StagedAttemptRelease> {
        if holder != format!("scaleset/{scale_set_id}/{request_id}")
            || attempt_token.is_empty()
            || !matches!(
                ledger_demand_state,
                PermitDemandState::Eligible
                    | PermitDemandState::Cancelled
                    | PermitDemandState::Terminal
            )
            || next_demand_state.is_some_and(|state| {
                !matches!(
                    state,
                    DemandState::Eligible | DemandState::CanceledDone | DemandState::Terminal
                )
            })
            || worker_ownership_id.is_some_and(str::is_empty)
            || batch_id.is_some_and(|_| {
                ledger_demand_state != PermitDemandState::Eligible
                    || next_demand_state != Some(DemandState::Eligible)
                    || worker_ownership_id.is_some()
            })
        {
            anyhow::bail!("invalid staged Scale Set permit release for {holder:?}");
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin staged permit release")?;
        let acquire_staged: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM scaleset_attempt_acquires WHERE holder = ?1)",
            [holder],
            |row| row.get(0),
        )?;
        if acquire_staged {
            anyhow::bail!("fresh acquire for {holder:?} is pending; release is fenced");
        }
        let rotation_staged: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM scaleset_attempt_rotations WHERE holder = ?1)",
            [holder],
            |row| row.get(0),
        )?;
        if rotation_staged {
            anyhow::bail!(
                "Scale Set recovery rotation for {holder:?} is pending; release is fenced"
            );
        }
        if let Some(_) = next_demand_state {
            let recorded: Option<String> = tx
                .query_row(
                    "SELECT permit_attempt_token FROM scaleset_demand
                     WHERE scale_set_id = ?1 AND request_id = ?2",
                    params![scale_set_id, request_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if recorded.as_deref() != Some(attempt_token) {
                anyhow::bail!("demand projection for {holder:?} has another attempt token");
            }
        }
        if let Some(ownership_id) = worker_ownership_id {
            let recorded: Option<(Option<String>, String)> = tx
                .query_row(
                    "SELECT permit_attempt_token, worker_state FROM scaleset_workers
                     WHERE ownership_id = ?1 AND request_id = ?2",
                    params![ownership_id, request_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((recorded_token, state)) = recorded else {
                anyhow::bail!("worker projection for {holder:?} is missing");
            };
            if recorded_token.as_deref() != Some(attempt_token)
                || !matches!(state.as_str(), "owned_cleanup" | "permit_released")
            {
                anyhow::bail!("worker projection for {holder:?} is not cleanup-ready");
            }
        }
        if let Some(batch_id) = batch_id {
            Self::verify_release_batch_member(
                &tx,
                batch_id,
                scale_set_id,
                request_id,
                attempt_token,
            )?;
        }
        let next_state = next_demand_state.map(DemandState::as_str);
        let existing: Option<(
            String,
            i32,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
        )> = tx
            .query_row(
                "SELECT holder, scale_set_id, request_id, attempt_token, ledger_demand_state,
                        next_demand_state, worker_ownership_id
                 FROM scaleset_attempt_releases WHERE holder = ?1",
                [holder],
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
        if let Some(existing) = existing {
            if existing
                != (
                    holder.to_owned(),
                    scale_set_id,
                    request_id,
                    attempt_token.to_owned(),
                    permit_demand_state_name(ledger_demand_state).to_owned(),
                    next_state.map(str::to_owned),
                    worker_ownership_id.map(str::to_owned),
                )
            {
                anyhow::bail!("staged release for {holder:?} belongs to another attempt");
            }
        } else {
            tx.execute(
                "INSERT INTO scaleset_attempt_releases
                 (holder, scale_set_id, request_id, attempt_token, ledger_demand_state,
                  next_demand_state, worker_ownership_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    holder,
                    scale_set_id,
                    request_id,
                    attempt_token,
                    permit_demand_state_name(ledger_demand_state),
                    next_state,
                    worker_ownership_id,
                    Self::now_rfc3339(),
                ],
            )?;
        }
        let existing_batch: Option<(String, i64)> = tx
            .query_row(
                "SELECT batch_id, resolve_uncertain
                 FROM scaleset_attempt_release_batches WHERE holder = ?1",
                [holder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match (batch_id, existing_batch) {
            (Some(batch_id), Some((recorded, uncertain)))
                if recorded == batch_id && uncertain == 0 => {}
            (Some(batch_id), None) => {
                tx.execute(
                    "INSERT INTO scaleset_attempt_release_batches
                     (holder, batch_id, resolve_uncertain, created_at)
                     VALUES (?1, ?2, 0, ?3)",
                    params![holder, batch_id, Self::now_rfc3339()],
                )?;
            }
            (None, None) => {}
            _ => anyhow::bail!("release batch stage for {holder:?} changed"),
        }
        tx.commit().context("commit staged permit release")?;
        Ok(StagedAttemptRelease {
            holder: holder.to_owned(),
            scale_set_id,
            request_id,
            attempt_token: attempt_token.to_owned(),
            ledger_demand_state,
            next_demand_state,
            worker_ownership_id: worker_ownership_id.map(str::to_owned),
            batch_id: batch_id.map(str::to_owned),
            batch_uncertain: false,
        })
    }

    /// Durable releases awaiting replay, ordered by holder.
    pub(crate) fn pending_attempt_releases(&self) -> Result<Vec<StagedAttemptRelease>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.holder, r.scale_set_id, r.request_id, r.attempt_token,
                    r.ledger_demand_state, r.next_demand_state, r.worker_ownership_id,
                    b.batch_id, COALESCE(b.resolve_uncertain, 0)
             FROM scaleset_attempt_releases r
             LEFT JOIN scaleset_attempt_release_batches b ON b.holder = r.holder
             ORDER BY r.holder",
        )?;
        let rows = stmt.query_map([], |row| {
            let ledger_state: String = row.get(4)?;
            let ledger_state = match parse_permit_demand_state(&ledger_state) {
                Ok(state) => state,
                Err(error) => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        error.into(),
                    ));
                }
            };
            let state: Option<String> = row.get(5)?;
            let state = state
                .map(|value| {
                    DemandState::parse(&value).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            5,
                            rusqlite::types::Type::Text,
                            error.into(),
                        )
                    })
                })
                .transpose()?;
            Ok(StagedAttemptRelease {
                holder: row.get(0)?,
                scale_set_id: row.get(1)?,
                request_id: row.get(2)?,
                attempt_token: row.get(3)?,
                ledger_demand_state: ledger_state,
                next_demand_state: state,
                worker_ownership_id: row.get(6)?,
                batch_id: row.get(7)?,
                batch_uncertain: row.get::<_, i64>(8)? != 0,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("list staged Scale Set permit releases")
    }

    /// Apply all state projections and remove one release stage in one local
    /// transaction. The caller first performs the token-fenced ledger release.
    pub(crate) fn finish_attempt_release(&mut self, holder: &str, generation: u64) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin staged release projection completion")?;
        let stage: Option<StagedAttemptRelease> = tx
            .query_row(
                "SELECT r.holder, r.scale_set_id, r.request_id, r.attempt_token,
                        r.ledger_demand_state, r.next_demand_state, r.worker_ownership_id,
                        b.batch_id, COALESCE(b.resolve_uncertain, 0)
                 FROM scaleset_attempt_releases r
                 LEFT JOIN scaleset_attempt_release_batches b ON b.holder = r.holder
                 WHERE r.holder = ?1",
                [holder],
                |row| {
                    let ledger_state: String = row.get(4)?;
                    let ledger_state =
                        parse_permit_demand_state(&ledger_state).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                4,
                                rusqlite::types::Type::Text,
                                error.into(),
                            )
                        })?;
                    let state: Option<String> = row.get(5)?;
                    let state = state
                        .map(|value| {
                            DemandState::parse(&value).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    5,
                                    rusqlite::types::Type::Text,
                                    error.into(),
                                )
                            })
                        })
                        .transpose()?;
                    Ok(StagedAttemptRelease {
                        holder: row.get(0)?,
                        scale_set_id: row.get(1)?,
                        request_id: row.get(2)?,
                        attempt_token: row.get(3)?,
                        ledger_demand_state: ledger_state,
                        next_demand_state: state,
                        worker_ownership_id: row.get(6)?,
                        batch_id: row.get(7)?,
                        batch_uncertain: row.get::<_, i64>(8)? != 0,
                    })
                },
            )
            .optional()?;
        let Some(stage) = stage else {
            tx.commit().context("finish absent staged permit release")?;
            return Ok(false);
        };
        if let Some(next_state) = stage.next_demand_state {
            let current: Option<(Option<String>, String)> = tx
                .query_row(
                    "SELECT permit_attempt_token, state FROM scaleset_demand
                     WHERE scale_set_id = ?1 AND request_id = ?2",
                    params![stage.scale_set_id, stage.request_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((token, state)) = current else {
                anyhow::bail!("demand projection for {} vanished", stage.holder);
            };
            if token.as_deref() != Some(stage.attempt_token.as_str()) {
                anyhow::bail!("demand projection for {} changed attempt", stage.holder);
            }
            let current_state = DemandState::parse(&state)?;
            if current_state != next_state {
                let allowed = match next_state {
                    DemandState::Eligible => current_state.holds_permit(),
                    DemandState::CanceledDone => current_state == DemandState::CanceledPending,
                    DemandState::Terminal => true,
                    _ => false,
                };
                if !allowed {
                    anyhow::bail!(
                        "demand projection for {} cannot move from {current_state:?} to {next_state:?}",
                        stage.holder
                    );
                }
                if tx.execute(
                    "UPDATE scaleset_demand SET state = ?1, decline_reason = NULL,
                         generation = ?2, updated_at = ?3
                     WHERE scale_set_id = ?4 AND request_id = ?5
                       AND permit_attempt_token = ?6 AND state = ?7",
                    params![
                        next_state.as_str(),
                        i64::try_from(generation).unwrap_or(i64::MAX),
                        Self::now_rfc3339(),
                        stage.scale_set_id,
                        stage.request_id,
                        stage.attempt_token,
                        state,
                    ],
                )? != 1
                {
                    anyhow::bail!(
                        "demand projection for {} moved during release",
                        stage.holder
                    );
                }
            }
        }
        if let Some(ownership_id) = stage.worker_ownership_id.as_deref() {
            let current: Option<(Option<String>, String)> = tx
                .query_row(
                    "SELECT permit_attempt_token, worker_state FROM scaleset_workers
                     WHERE ownership_id = ?1 AND request_id = ?2",
                    params![ownership_id, stage.request_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((token, state)) = current else {
                anyhow::bail!("worker projection for {} vanished", stage.holder);
            };
            if token.as_deref() != Some(stage.attempt_token.as_str())
                || !matches!(state.as_str(), "owned_cleanup" | "permit_released")
            {
                anyhow::bail!(
                    "worker projection for {} changed during release",
                    stage.holder
                );
            }
            if state != "permit_released"
                && tx.execute(
                    "UPDATE scaleset_workers SET worker_state = 'permit_released', generation = ?1,
                         updated_at = ?2
                     WHERE ownership_id = ?3 AND request_id = ?4
                       AND permit_attempt_token = ?5 AND worker_state = 'owned_cleanup'",
                    params![
                        i64::try_from(generation).unwrap_or(i64::MAX),
                        Self::now_rfc3339(),
                        ownership_id,
                        stage.request_id,
                        stage.attempt_token,
                    ],
                )? != 1
            {
                anyhow::bail!(
                    "worker projection for {} moved during release",
                    stage.holder
                );
            }
        }
        if let Some(batch_id) = stage.batch_id.as_deref() {
            Self::verify_release_batch_member(
                &tx,
                batch_id,
                stage.scale_set_id,
                stage.request_id,
                &stage.attempt_token,
            )?;
            if tx.execute(
                "DELETE FROM scaleset_attempt_release_batches
                 WHERE holder = ?1 AND batch_id = ?2 AND resolve_uncertain = ?3",
                params![
                    stage.holder,
                    batch_id,
                    if stage.batch_uncertain { 1_i64 } else { 0_i64 }
                ],
            )? != 1
            {
                anyhow::bail!("release batch stage for {} moved", stage.holder);
            }
            let batch_has_remaining_stages: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM scaleset_attempt_release_batches WHERE batch_id = ?1
                 )",
                [batch_id],
                |row| row.get(0),
            )?;
            if !batch_has_remaining_stages {
                let state = if stage.batch_uncertain {
                    "uncertain"
                } else {
                    "resolved"
                };
                let changed = tx.execute(
                    "UPDATE scaleset_acquire_batches SET state = ?1, uncertain = ?2,
                         updated_at = ?3 WHERE batch_id = ?4",
                    params![
                        state,
                        if stage.batch_uncertain { 1_i64 } else { 0_i64 },
                        Self::now_rfc3339(),
                        batch_id,
                    ],
                )?;
                if changed != 1 {
                    anyhow::bail!("release batch {batch_id:?} vanished during completion");
                }
            }
        }
        if tx.execute(
            "DELETE FROM scaleset_attempt_releases WHERE holder = ?1 AND attempt_token = ?2",
            params![stage.holder, stage.attempt_token],
        )? != 1
        {
            anyhow::bail!("staged permit release for {} moved", stage.holder);
        }
        tx.commit()
            .context("commit staged release projection completion")?;
        Ok(true)
    }

    fn verify_release_batch_member(
        conn: &Connection,
        batch_id: &str,
        scale_set_id: i32,
        request_id: i64,
        attempt_token: &str,
    ) -> Result<()> {
        let member: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT request_ids_json, attempt_tokens_json FROM scaleset_acquire_batches
                 WHERE batch_id = ?1 AND scale_set_id = ?2",
                params![batch_id, scale_set_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((requests_json, tokens_json)) = member else {
            anyhow::bail!("release batch {batch_id:?} is missing");
        };
        let requests: Vec<i64> =
            serde_json::from_str(&requests_json).context("decode release batch requests")?;
        let Some(index) = requests.iter().position(|request| *request == request_id) else {
            anyhow::bail!("release request {request_id} is not in batch {batch_id:?}");
        };
        let tokens: Vec<Option<String>> = tokens_json
            .as_deref()
            .context("release batch has no attempt tokens")
            .and_then(|json| serde_json::from_str(json).context("decode release batch tokens"))?;
        if tokens.len() != requests.len() || tokens[index].as_deref() != Some(attempt_token) {
            anyhow::bail!("release request {request_id} token differs from batch {batch_id:?}");
        }
        Ok(())
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
            permit_attempt_token: row.get(12)?,
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
        let request_id = resolve_job_request_id(&offer.base);
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

    /// Record an assigned job directly into `scaleset_demand`.
    ///
    /// When a direct `JobAssigned` arrives without a prior offer, record an
    /// unconfirmed Acquired projection so the observation is durable. It has
    /// no attempt token; the processor must fail closed until acquirejobs or
    /// another exact owner source proves permit ownership.
    pub fn submit_assigned(
        &mut self,
        scale_set_id: i32,
        assigned: &velnor_model::ScaleSetJobAssigned,
        generation: u64,
    ) -> Result<(i64, DemandState)> {
        let request_id = resolve_job_request_id(&assigned.base);
        if let Some(existing) = self.get(request_id)? {
            return Ok((request_id, existing.state));
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin demand submit_assigned transaction")?;
        let sequence: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM scaleset_demand",
                [],
                |row| row.get(0),
            )
            .context("allocate demand sequence")?;
        let now = Self::now_rfc3339();
        let labels_hash = crate::scaleset::intents::labels_hash(&assigned.base.request_labels);
        let job_id_hash = crate::scaleset::intents::stable_i64(&assigned.base.job_id);
        tx.execute(
            "INSERT OR IGNORE INTO scaleset_demand
             (request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
              repo_owner, repo_name, job_id, labels_hash, generation, updated_at, event_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                request_id,
                scale_set_id,
                now,
                sequence,
                DemandState::Acquired.as_str(),
                Option::<String>::None,
                assigned.base.owner_name,
                assigned.base.repository_name,
                job_id_hash,
                labels_hash,
                i64::try_from(generation).unwrap_or(i64::MAX),
                now,
                assigned.base.event_name,
            ],
        )
        .context("insert demand row for assigned job")?;
        tx.commit().context("commit demand submit_assigned")?;
        Ok((request_id, DemandState::Acquired))
    }

    /// Fetch one demand row by request ID.
    pub fn get(&self, request_id: i64) -> Result<Option<Demand>> {
        self.conn
            .query_row(
                "SELECT request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
                        repo_owner, repo_name, job_id, generation, updated_at, event_name,
                        permit_attempt_token
                 FROM scaleset_demand WHERE request_id = ?1",
                params![request_id],
                Self::row_to_demand,
            )
            .optional()
            .context("fetch demand row")
    }

    /// Persist the token returned by the permit ledger. `expected_previous`
    /// makes delayed acquisition/recovery writes unable to replace a newer
    /// token already recorded for this request.
    pub fn set_permit_attempt_token(
        &mut self,
        request_id: i64,
        expected_previous: Option<&str>,
        attempt_token: &str,
    ) -> Result<()> {
        if attempt_token.is_empty() {
            anyhow::bail!("permit attempt token cannot be empty");
        }
        let updated = self.conn.execute(
            "UPDATE scaleset_demand SET permit_attempt_token = ?1
             WHERE request_id = ?2
               AND ((?3 IS NULL AND permit_attempt_token IS NULL)
                    OR permit_attempt_token = ?3)",
            params![attempt_token, request_id, expected_previous],
        )?;
        if updated == 1 {
            return Ok(());
        }
        if self
            .get(request_id)?
            .is_some_and(|row| row.permit_attempt_token.as_deref() == Some(attempt_token))
        {
            return Ok(());
        }
        anyhow::bail!(
            "permit attempt token for request {request_id} changed before the owner token was persisted"
        )
    }

    /// Persist the caller-generated token before the separate permit-ledger
    /// transaction inserts a fresh Scale Set row.
    pub(crate) fn stage_attempt_acquire(
        &mut self,
        holder: &str,
        scale_set_id: i32,
        request_id: i64,
        previous_attempt_token: Option<&str>,
        target_attempt_token: &str,
    ) -> Result<StagedAttemptAcquire> {
        if holder != format!("scaleset/{scale_set_id}/{request_id}")
            || target_attempt_token.is_empty()
            || previous_attempt_token == Some(target_attempt_token)
        {
            anyhow::bail!("invalid staged Scale Set acquire for {holder:?}");
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin staged Scale Set acquire")?;
        let release_staged: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM scaleset_attempt_releases WHERE holder = ?1
                 UNION ALL
                 SELECT 1 FROM scaleset_attempt_release_batches WHERE holder = ?1
             )",
            [holder],
            |row| row.get(0),
        )?;
        if release_staged {
            anyhow::bail!("Scale Set release for {holder:?} is pending; acquire is fenced");
        }
        let rotation_staged: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM scaleset_attempt_rotations WHERE holder = ?1)",
            [holder],
            |row| row.get(0),
        )?;
        if rotation_staged {
            anyhow::bail!(
                "Scale Set recovery rotation for {holder:?} is pending; acquire is fenced"
            );
        }
        let current: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT state, permit_attempt_token FROM scaleset_demand
                 WHERE scale_set_id = ?1 AND request_id = ?2",
                params![scale_set_id, request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, token)) = current else {
            anyhow::bail!("acquire demand {holder:?} is missing");
        };
        if DemandState::parse(&state)? != DemandState::Granted
            || token.as_deref() != previous_attempt_token
        {
            anyhow::bail!("acquire projection for {holder:?} changed before claim");
        }
        let existing: Option<(i32, i64, Option<String>, String)> = tx
            .query_row(
                "SELECT scale_set_id, request_id, previous_attempt_token, target_attempt_token
                 FROM scaleset_attempt_acquires WHERE holder = ?1",
                [holder],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let stage = StagedAttemptAcquire {
            holder: holder.to_owned(),
            scale_set_id,
            request_id,
            previous_attempt_token: previous_attempt_token.map(str::to_owned),
            target_attempt_token: target_attempt_token.to_owned(),
        };
        if let Some(existing) = existing {
            if existing
                != (
                    scale_set_id,
                    request_id,
                    stage.previous_attempt_token.clone(),
                    target_attempt_token.to_owned(),
                )
            {
                anyhow::bail!("staged acquire for {holder:?} belongs to another attempt");
            }
        } else {
            tx.execute(
                "INSERT INTO scaleset_attempt_acquires
                 (holder, scale_set_id, request_id, previous_attempt_token,
                  target_attempt_token, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    holder,
                    scale_set_id,
                    request_id,
                    previous_attempt_token,
                    target_attempt_token,
                    Self::now_rfc3339(),
                ],
            )?;
        }
        tx.commit().context("commit staged Scale Set acquire")?;
        Ok(stage)
    }

    pub(crate) fn pending_attempt_acquires(&self) -> Result<Vec<StagedAttemptAcquire>> {
        let mut stmt = self.conn.prepare(
            "SELECT holder, scale_set_id, request_id, previous_attempt_token,
                    target_attempt_token
             FROM scaleset_attempt_acquires ORDER BY holder",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StagedAttemptAcquire {
                holder: row.get(0)?,
                scale_set_id: row.get(1)?,
                request_id: row.get(2)?,
                previous_attempt_token: row.get(3)?,
                target_attempt_token: row.get(4)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("list staged Scale Set permit acquisitions")
    }

    pub(crate) fn has_acquire_batch_attempt(
        &self,
        scale_set_id: i32,
        request_id: i64,
        attempt_token: &str,
    ) -> Result<bool> {
        Self::acquire_batch_contains_attempt(&self.conn, scale_set_id, request_id, attempt_token)
    }

    /// Project an acquired token while retaining its journal until the
    /// matching acquire batch is durable. An absent insert clears the stage.
    pub(crate) fn finish_attempt_acquire(
        &mut self,
        stage: &StagedAttemptAcquire,
        acquired: bool,
    ) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin staged Scale Set acquire completion")?;
        let recorded: Option<(Option<String>, String)> = tx
            .query_row(
                "SELECT previous_attempt_token, target_attempt_token
                 FROM scaleset_attempt_acquires WHERE holder = ?1",
                [&stage.holder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if recorded
            != Some((
                stage.previous_attempt_token.clone(),
                stage.target_attempt_token.clone(),
            ))
        {
            anyhow::bail!("staged acquire {} changed before completion", stage.holder);
        }
        let current: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT state, permit_attempt_token FROM scaleset_demand
                 WHERE scale_set_id = ?1 AND request_id = ?2",
                params![stage.scale_set_id, stage.request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, token)) = current else {
            anyhow::bail!("acquire demand {} vanished", stage.holder);
        };
        if DemandState::parse(&state)? != DemandState::Granted {
            anyhow::bail!(
                "acquire demand {} changed state during recovery",
                stage.holder
            );
        }
        if acquired {
            if token.as_deref() == Some(stage.target_attempt_token.as_str()) {
                // The projection half already committed. Keep the journal
                // until its acquire batch is durable, or startup must release
                // this pre-network reservation back to Eligible.
            } else if token.as_deref() == stage.previous_attempt_token.as_deref() {
                if tx.execute(
                    "UPDATE scaleset_demand SET permit_attempt_token = ?1, updated_at = ?2
                     WHERE scale_set_id = ?3 AND request_id = ?4 AND state = 'granted'
                       AND permit_attempt_token IS ?5",
                    params![
                        stage.target_attempt_token,
                        Self::now_rfc3339(),
                        stage.scale_set_id,
                        stage.request_id,
                        stage.previous_attempt_token,
                    ],
                )? != 1
                {
                    anyhow::bail!(
                        "acquire demand {} changed during token projection",
                        stage.holder
                    );
                }
            } else {
                anyhow::bail!("acquire demand {} belongs to another attempt", stage.holder);
            }
        } else if token.as_deref() != stage.previous_attempt_token.as_deref() {
            anyhow::bail!("absent acquire {} changed its previous token", stage.holder);
        }
        if !acquired {
            if Self::acquire_batch_contains_attempt(
                &tx,
                stage.scale_set_id,
                stage.request_id,
                &stage.target_attempt_token,
            )? {
                anyhow::bail!("batch for staged acquire {} already exists", stage.holder);
            }
            if tx.execute(
                "DELETE FROM scaleset_attempt_acquires
                 WHERE holder = ?1 AND target_attempt_token = ?2",
                params![stage.holder, stage.target_attempt_token],
            )? != 1
            {
                anyhow::bail!("staged acquire {} moved during completion", stage.holder);
            }
        }
        tx.commit()
            .context("commit staged Scale Set acquire completion")?;
        Ok(true)
    }

    /// Once the batch is durable, it becomes the replay boundary for the
    /// held token. Remove the acquire stage only after exact batch membership
    /// and token projection both agree.
    pub(crate) fn finish_attempt_acquire_batch(
        &mut self,
        stage: &StagedAttemptAcquire,
    ) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin staged acquire batch completion")?;
        let token: Option<String> = tx
            .query_row(
                "SELECT permit_attempt_token FROM scaleset_demand
                 WHERE scale_set_id = ?1 AND request_id = ?2 AND state = 'granted'",
                params![stage.scale_set_id, stage.request_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if token.as_deref() != Some(stage.target_attempt_token.as_str()) {
            anyhow::bail!(
                "acquire batch projection for {} has another token",
                stage.holder
            );
        }
        if !Self::acquire_batch_contains_attempt(
            &tx,
            stage.scale_set_id,
            stage.request_id,
            &stage.target_attempt_token,
        )? {
            anyhow::bail!("acquire batch for {} is not durable", stage.holder);
        }
        if tx.execute(
            "DELETE FROM scaleset_attempt_acquires
             WHERE holder = ?1 AND target_attempt_token = ?2",
            params![stage.holder, stage.target_attempt_token],
        )? != 1
        {
            anyhow::bail!(
                "staged acquire {} moved during batch completion",
                stage.holder
            );
        }
        tx.commit()
            .context("commit staged acquire batch completion")?;
        Ok(true)
    }

    /// Convert an exact staged acquisition with no durable network batch into
    /// a staged Eligible release. This closes the cross-database gap without
    /// exposing an intermediate holder-only state to another acquire.
    pub(crate) fn finish_staged_acquire_as_release(
        &mut self,
        stage: &StagedAttemptAcquire,
    ) -> Result<StagedAttemptRelease> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin staged acquire rollback")?;
        let recorded: Option<(Option<String>, String)> = tx
            .query_row(
                "SELECT previous_attempt_token, target_attempt_token
                 FROM scaleset_attempt_acquires WHERE holder = ?1",
                [&stage.holder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if recorded
            != Some((
                stage.previous_attempt_token.clone(),
                stage.target_attempt_token.clone(),
            ))
        {
            anyhow::bail!("staged acquire {} changed before rollback", stage.holder);
        }
        if Self::acquire_batch_contains_attempt(
            &tx,
            stage.scale_set_id,
            stage.request_id,
            &stage.target_attempt_token,
        )? {
            anyhow::bail!(
                "acquire {} has a durable batch; cannot roll back",
                stage.holder
            );
        }
        let current: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT state, permit_attempt_token FROM scaleset_demand
                 WHERE scale_set_id = ?1 AND request_id = ?2",
                params![stage.scale_set_id, stage.request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, token)) = current else {
            anyhow::bail!("acquire demand {} vanished", stage.holder);
        };
        if DemandState::parse(&state)? != DemandState::Granted
            || (token.as_deref() != stage.previous_attempt_token.as_deref()
                && token.as_deref() != Some(stage.target_attempt_token.as_str()))
        {
            anyhow::bail!("acquire demand {} changed before rollback", stage.holder);
        }
        if token.as_deref() != Some(stage.target_attempt_token.as_str())
            && tx.execute(
                "UPDATE scaleset_demand SET permit_attempt_token = ?1, updated_at = ?2
                 WHERE scale_set_id = ?3 AND request_id = ?4 AND state = 'granted'
                   AND permit_attempt_token IS ?5",
                params![
                    stage.target_attempt_token,
                    Self::now_rfc3339(),
                    stage.scale_set_id,
                    stage.request_id,
                    stage.previous_attempt_token,
                ],
            )? != 1
        {
            anyhow::bail!("acquire demand {} changed during rollback", stage.holder);
        }
        tx.execute(
            "INSERT INTO scaleset_attempt_releases
             (holder, scale_set_id, request_id, attempt_token, ledger_demand_state,
              next_demand_state, worker_ownership_id, created_at)
             VALUES (?1, ?2, ?3, ?4, 'eligible', 'eligible', NULL, ?5)",
            params![
                stage.holder,
                stage.scale_set_id,
                stage.request_id,
                stage.target_attempt_token,
                Self::now_rfc3339(),
            ],
        )?;
        if tx.execute(
            "DELETE FROM scaleset_attempt_acquires
             WHERE holder = ?1 AND target_attempt_token = ?2",
            params![stage.holder, stage.target_attempt_token],
        )? != 1
        {
            anyhow::bail!("staged acquire {} moved during rollback", stage.holder);
        }
        tx.commit().context("commit staged acquire rollback")?;
        Ok(StagedAttemptRelease {
            holder: stage.holder.clone(),
            scale_set_id: stage.scale_set_id,
            request_id: stage.request_id,
            attempt_token: stage.target_attempt_token.clone(),
            ledger_demand_state: PermitDemandState::Eligible,
            next_demand_state: Some(DemandState::Eligible),
            worker_ownership_id: None,
            batch_id: None,
            batch_uncertain: false,
        })
    }

    fn acquire_batch_contains_attempt(
        conn: &Connection,
        scale_set_id: i32,
        request_id: i64,
        attempt_token: &str,
    ) -> Result<bool> {
        let mut stmt = conn.prepare(
            "SELECT request_ids_json, attempt_tokens_json FROM scaleset_acquire_batches
             WHERE scale_set_id = ?1",
        )?;
        let rows = stmt.query_map([scale_set_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (request_ids_json, attempt_tokens_json) = row?;
            let request_ids: Vec<i64> =
                serde_json::from_str(&request_ids_json).context("decode acquire batch ids")?;
            let Some(index) = request_ids.iter().position(|id| *id == request_id) else {
                continue;
            };
            let tokens: Vec<Option<String>> = attempt_tokens_json
                .as_deref()
                .context("acquire batch has no attempt tokens")
                .and_then(|json| {
                    serde_json::from_str(json).context("decode acquire batch attempt tokens")
                })?;
            if tokens.len() != request_ids.len() {
                anyhow::bail!("acquire batch tokens are misaligned with requests");
            }
            if tokens[index].as_deref() != Some(attempt_token) {
                anyhow::bail!("acquire batch request {request_id} belongs to another attempt");
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Oldest-first `eligible` rows for one set, bounded by `limit`.
    pub fn oldest_eligible(&self, scale_set_id: i32, limit: usize) -> Result<Vec<Demand>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
                        repo_owner, repo_name, job_id, generation, updated_at, event_name,
                        permit_attempt_token
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
                        repo_owner, repo_name, job_id, generation, updated_at, event_name,
                        permit_attempt_token
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

    /// Move an unowned row to a new state, recording the acting generation.
    /// A row with a permit token must use [`set_state_owned`].
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
                 WHERE request_id = ?5 AND permit_attempt_token IS NULL",
                params![
                    state.as_str(),
                    decline_reason,
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    now,
                    request_id,
                ],
            )
            .context("update unowned demand state")?;
        if updated == 0 {
            if self.get(request_id)?.is_some() {
                anyhow::bail!(
                    "demand {request_id} has a permit attempt token; state mutation requires the owner token"
                );
            }
            anyhow::bail!("demand holds no row for request {request_id}");
        }
        Ok(())
    }

    /// Move one demand row only when `attempt_token` still matches its
    /// captured permit attempt. This fences delayed worker and acquire
    /// callbacks after a serialized recovery has rotated ownership.
    pub fn set_state_owned(
        &mut self,
        request_id: i64,
        state: DemandState,
        decline_reason: Option<&str>,
        generation: u64,
        attempt_token: &str,
    ) -> Result<()> {
        if attempt_token.is_empty() {
            anyhow::bail!("demand permit attempt token cannot be empty");
        }
        let now = Self::now_rfc3339();
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_demand
                 SET state = ?1, decline_reason = ?2, generation = ?3, updated_at = ?4
                 WHERE request_id = ?5 AND permit_attempt_token = ?6",
                params![
                    state.as_str(),
                    decline_reason,
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    now,
                    request_id,
                    attempt_token,
                ],
            )
            .context("update owned demand state")?;
        if updated != 1 {
            anyhow::bail!(
                "demand {request_id} is missing or belongs to a different permit attempt"
            );
        }
        Ok(())
    }

    /// Void stale-epoch grants: `granted`/`acquire_intent` rows recorded
    /// under any other generation return to `eligible` with age preserved.
    /// Returns the number of rows reset.
    pub fn reset_stale_grants(&mut self, scale_set_id: i32, generation: u64) -> Result<u64> {
        let mut stmt = self.conn.prepare(
            "SELECT request_id, permit_attempt_token FROM scaleset_demand
             WHERE scale_set_id = ?1 AND generation != ?2
               AND state IN ('granted', 'acquire_intent')",
        )?;
        let rows = stmt.query_map(
            params![scale_set_id, i64::try_from(generation).unwrap_or(i64::MAX)],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
        )?;
        let stale: Vec<(i64, Option<String>)> = rows.collect::<Result<_, _>>()?;
        drop(stmt);
        for (request_id, token) in &stale {
            if let Some(token) = token.as_deref() {
                self.set_state_owned(*request_id, DemandState::Eligible, None, generation, token)?;
            } else {
                self.set_state(*request_id, DemandState::Eligible, None, generation)?;
            }
        }
        Ok(u64::try_from(stale.len()).unwrap_or(u64::MAX))
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
                if let Some(attempt_token) = candidate.permit_attempt_token.as_deref() {
                    store.set_state_owned(
                        candidate.request_id,
                        DemandState::Granted,
                        None,
                        generation,
                        attempt_token,
                    )?;
                } else {
                    store.set_state(
                        candidate.request_id,
                        DemandState::Granted,
                        None,
                        generation,
                    )?;
                }
                metrics.add_offers_granted(1);
                granted.push(candidate);
            }
            OfferTrust::Unknown { reason } => {
                if let Some(attempt_token) = candidate.permit_attempt_token.as_deref() {
                    store.set_state_owned(
                        candidate.request_id,
                        DemandState::Observed,
                        Some(reason),
                        generation,
                        attempt_token,
                    )?;
                } else {
                    store.set_state(
                        candidate.request_id,
                        DemandState::Observed,
                        Some(reason),
                        generation,
                    )?;
                }
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
    fn staged_acquire_stays_until_exact_batch_is_durable() {
        let path = temp_path("acquire-batch-boundary");
        let mut store = DemandStore::open(&path).unwrap();
        store.submit_offer(7, &offer(601, "push"), 3).unwrap();
        store.set_state(601, DemandState::Granted, None, 3).unwrap();
        let holder = "scaleset/7/601";
        let stage = store
            .stage_attempt_acquire(holder, 7, 601, None, "attempt-601")
            .unwrap();
        store.finish_attempt_acquire(&stage, true).unwrap();
        assert_eq!(
            store.pending_attempt_acquires().unwrap(),
            vec![stage.clone()]
        );

        let mut batches = crate::scaleset::intents::AcquireBatchStore::open(&path).unwrap();
        batches
            .record_intended(
                "batch-601",
                7,
                &[601],
                &[holder.to_owned()],
                &["attempt-601".to_owned()],
                3,
            )
            .unwrap();
        store.finish_attempt_acquire_batch(&stage).unwrap();
        assert!(store.pending_attempt_acquires().unwrap().is_empty());
        assert_eq!(
            store
                .get(601)
                .unwrap()
                .unwrap()
                .permit_attempt_token
                .as_deref(),
            Some("attempt-601")
        );
    }

    #[test]
    fn acquire_restart_cuts_clear_absent_or_stage_release_for_exact_row() {
        let absent_path = temp_path("acquire-absent-restart");
        let stage = {
            let mut store = DemandStore::open(&absent_path).unwrap();
            store.submit_offer(7, &offer(602, "push"), 3).unwrap();
            store.set_state(602, DemandState::Granted, None, 3).unwrap();
            store
                .stage_attempt_acquire("scaleset/7/602", 7, 602, None, "attempt-602")
                .unwrap()
        };
        let mut store = DemandStore::open(&absent_path).unwrap();
        store.finish_attempt_acquire(&stage, false).unwrap();
        assert!(store.pending_attempt_acquires().unwrap().is_empty());
        assert_eq!(store.get(602).unwrap().unwrap().permit_attempt_token, None);

        let exact_path = temp_path("acquire-exact-restart");
        let stage = {
            let mut store = DemandStore::open(&exact_path).unwrap();
            store.submit_offer(7, &offer(603, "push"), 3).unwrap();
            store.set_state(603, DemandState::Granted, None, 3).unwrap();
            let stage = store
                .stage_attempt_acquire("scaleset/7/603", 7, 603, None, "attempt-603")
                .unwrap();
            store.finish_attempt_acquire(&stage, true).unwrap();
            stage
        };
        let mut store = DemandStore::open(&exact_path).unwrap();
        let release = store.finish_staged_acquire_as_release(&stage).unwrap();
        assert!(store.pending_attempt_acquires().unwrap().is_empty());
        assert_eq!(store.pending_attempt_releases().unwrap(), vec![release]);
        store.finish_attempt_release("scaleset/7/603", 4).unwrap();
        let row = store.get(603).unwrap().unwrap();
        assert_eq!(row.state, DemandState::Eligible);
        assert_eq!(row.permit_attempt_token.as_deref(), Some("attempt-603"));
    }

    #[test]
    fn all_missing_batch_members_stage_before_batch_resolution() {
        let path = temp_path("batch-release-stage");
        let mut demand = DemandStore::open(&path).unwrap();
        let request_ids = [701, 702];
        let holders = ["scaleset/7/701".to_owned(), "scaleset/7/702".to_owned()];
        let tokens = ["attempt-701".to_owned(), "attempt-702".to_owned()];
        for ((request_id, holder), token) in request_ids.iter().zip(&holders).zip(&tokens) {
            demand
                .submit_offer(7, &offer(*request_id, "push"), 3)
                .unwrap();
            demand
                .set_state(*request_id, DemandState::Granted, None, 3)
                .unwrap();
            demand
                .set_permit_attempt_token(*request_id, None, token)
                .unwrap();
            demand
                .set_state_owned(*request_id, DemandState::AcquireIntent, None, 3, token)
                .unwrap();
            assert_eq!(holder, &format!("scaleset/7/{request_id}"));
        }
        let mut batches = crate::scaleset::intents::AcquireBatchStore::open(&path).unwrap();
        batches
            .record_intended("batch-701", 7, &request_ids, &holders, &tokens, 3)
            .unwrap();
        let releases = request_ids
            .iter()
            .copied()
            .zip(tokens.iter().cloned())
            .collect::<Vec<_>>();
        let mut invalid_releases = releases.clone();
        invalid_releases[1].1 = "wrong-attempt".to_owned();
        assert!(demand
            .stage_attempt_releases_for_batch(7, "batch-701", &invalid_releases)
            .is_err());
        assert!(demand.pending_attempt_releases().unwrap().is_empty());
        assert_eq!(
            demand
                .stage_attempt_releases_for_batch(7, "batch-701", &releases)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(demand.pending_attempt_releases().unwrap().len(), 2);

        drop(demand);
        drop(batches);
        let mut demand = DemandStore::open(&path).unwrap();
        let batches = crate::scaleset::intents::AcquireBatchStore::open(&path).unwrap();

        demand.finish_attempt_release(&holders[0], 4).unwrap();
        assert_eq!(
            batches.get("batch-701").unwrap().unwrap().state,
            crate::scaleset::intents::BatchState::Intended,
            "one completed member must not close a multi-member batch"
        );
        assert_eq!(demand.pending_attempt_releases().unwrap().len(), 1);

        drop(demand);
        drop(batches);
        let mut demand = DemandStore::open(&path).unwrap();
        let batches = crate::scaleset::intents::AcquireBatchStore::open(&path).unwrap();
        demand.finish_attempt_release(&holders[1], 4).unwrap();
        assert_eq!(demand.pending_attempt_releases().unwrap().len(), 0);
        assert_eq!(
            batches.get("batch-701").unwrap().unwrap().state,
            crate::scaleset::intents::BatchState::Resolved
        );
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
    fn null_request_ids_use_distinct_stable_job_and_run_identity() {
        let path = temp_path("null-request-identity");
        let mut store = DemandStore::open(&path).unwrap();

        let mut first = offer(0, "push");
        first.base.job_id = "job-null-a".to_owned();
        first.base.workflow_run_id = 0;
        let mut second = offer(0, "push");
        second.base.job_id = "job-null-b".to_owned();
        second.base.workflow_run_id = 0;
        let mut run_fallback = offer(0, "push");
        run_fallback.base.job_id.clear();
        run_fallback.base.workflow_run_id = 90_031;

        let first_id = resolve_job_request_id(&first.base);
        let second_id = resolve_job_request_id(&second.base);
        let run_id = resolve_job_request_id(&run_fallback.base);
        assert_ne!(first_id, second_id);
        assert_ne!(first_id, run_id);
        assert_ne!(second_id, run_id);

        for candidate in [&first, &second, &run_fallback] {
            store.submit_offer(7, candidate, 3).unwrap();
        }

        assert!(store.get(first_id).unwrap().is_some());
        assert!(store.get(second_id).unwrap().is_some());
        assert!(store.get(run_id).unwrap().is_some());
        assert!(store.get(0).unwrap().is_none());
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
    fn demand_state_writes_require_the_captured_attempt_token() {
        let path = temp_path("attempt-token-state-fence");
        let mut store = DemandStore::open(&path).unwrap();
        store.submit_offer(7, &offer(302, "push"), 5).unwrap();
        store
            .set_permit_attempt_token(302, None, "attempt-old")
            .unwrap();

        store
            .set_state_owned(302, DemandState::Acquired, None, 5, "attempt-old")
            .unwrap();
        store
            .set_permit_attempt_token(302, Some("attempt-old"), "attempt-new")
            .unwrap();

        assert!(store
            .set_state_owned(302, DemandState::Terminal, None, 5, "attempt-old")
            .is_err());
        assert!(store
            .set_state(302, DemandState::Terminal, None, 5)
            .is_err());
        let row = store.get(302).unwrap().unwrap();
        assert_eq!(row.state, DemandState::Acquired);
        assert_eq!(row.permit_attempt_token.as_deref(), Some("attempt-new"));
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

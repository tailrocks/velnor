//! Global oldest-observed demand queue (§5.1 step 2) + trust-before-grant.
//!
//! Every `JobAvailable` lands in `scaleset_demand` keyed by its scale set and
//! GitHub `runnerRequestId`. `first_seen_at` + `sequence` are immutable:
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
use serde::Deserialize;
use velnor_model::{ScaleSetJobAssigned, ScaleSetJobAvailable};

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

    fn parse(raw: &str) -> Result<Self> {
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
    /// storable there. `(scale_set_id, request_id)` stays the exact key; this
    /// hash is correlation-only.
    pub job_id_hash: i64,
    pub generation: u64,
    pub updated_at: String,
    /// Durable deciding input for the grant-pass trust gate (v23).
    pub event_name: String,
    /// Durable workflow identity input for the configured admission gate.
    pub job_workflow_ref: String,
    /// Exact wire identity used to detect a fallback-ID collision. An empty
    /// value is historical/incomplete and is never treated as a redelivery.
    pub request_identity: String,
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

/// Explicit scale-set admission allowlists.
///
/// Every dimension is required. An absent or incomplete policy never grants
/// trusted treatment. `repository` and `source` are full `owner/repository`
/// names; `workflow` is the `.github/workflows/...` path; `ref` is normalized
/// to a qualified ref before comparison.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OfferAdmission {
    #[serde(default)]
    pub owner: Vec<String>,
    #[serde(default)]
    pub repository: Vec<String>,
    #[serde(rename = "ref", default)]
    pub git_ref: Vec<String>,
    #[serde(default)]
    pub source: Vec<String>,
    #[serde(default)]
    pub workflow: Vec<String>,
    #[serde(default)]
    pub event: Vec<String>,
}

impl OfferAdmission {
    /// Build an exact policy for one workflow identity.
    #[must_use]
    pub fn exact(
        owner: &str,
        repository: &str,
        git_ref: &str,
        source: &str,
        workflow: &str,
        event: &str,
    ) -> Self {
        Self {
            owner: vec![owner.to_owned()],
            repository: vec![repository.to_owned()],
            git_ref: vec![git_ref.to_owned()],
            source: vec![source.to_owned()],
            workflow: vec![workflow.to_owned()],
            event: vec![event.to_owned()],
        }
    }

    /// Validate the policy shape before it can reach the grant path.
    pub fn validate(&self) -> Result<()> {
        for (name, values) in [
            ("owner", &self.owner),
            ("repository", &self.repository),
            ("ref", &self.git_ref),
            ("source", &self.source),
            ("workflow", &self.workflow),
            ("event", &self.event),
        ] {
            if values.is_empty() {
                anyhow::bail!("scale-set admission: {name} allowlist is required");
            }
            if values.iter().any(|value| value.trim().is_empty()) {
                anyhow::bail!("scale-set admission: {name} allowlist contains an empty value");
            }
        }
        if self
            .git_ref
            .iter()
            .any(|value| normalize_workflow_ref(value).is_none())
        {
            anyhow::bail!("scale-set admission: ref allowlist contains an invalid ref");
        }
        if self
            .workflow
            .iter()
            .any(|value| !valid_workflow_path(value.trim()))
        {
            anyhow::bail!("scale-set admission: workflow allowlist contains an invalid path");
        }
        if self
            .event
            .iter()
            .any(|value| !valid_event_name(value.trim()))
        {
            anyhow::bail!("scale-set admission: event allowlist contains an invalid event");
        }
        Ok(())
    }

    fn allows(&self, identity: &OfferIdentity) -> bool {
        self.owner
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&identity.owner))
            && self
                .repository
                .iter()
                .any(|value| value.trim().eq_ignore_ascii_case(&identity.repository))
            && self.git_ref.iter().any(|value| {
                normalize_workflow_ref(value).is_some_and(|value| value == identity.git_ref)
            })
            && self
                .source
                .iter()
                .any(|value| value.trim().eq_ignore_ascii_case(&identity.source))
            && self
                .workflow
                .iter()
                .any(|value| value.trim() == identity.workflow)
            && self
                .event
                .iter()
                .any(|value| value.trim().eq_ignore_ascii_case(&identity.event))
    }
}

impl Default for OfferAdmission {
    fn default() -> Self {
        Self {
            owner: Vec::new(),
            repository: Vec::new(),
            git_ref: Vec::new(),
            source: Vec::new(),
            workflow: Vec::new(),
            event: Vec::new(),
        }
    }
}

/// The classifiable slice of an offer: the only fields the verdict reads.
#[derive(Debug, Clone, Copy)]
pub struct OfferProbe<'a> {
    pub event_name: &'a str,
    pub owner_name: &'a str,
    pub repository_name: &'a str,
    pub job_workflow_ref: &'a str,
}

impl<'a> From<&'a ScaleSetJobAvailable> for OfferProbe<'a> {
    fn from(offer: &'a ScaleSetJobAvailable) -> Self {
        Self {
            event_name: &offer.base.event_name,
            owner_name: &offer.base.owner_name,
            repository_name: &offer.base.repository_name,
            job_workflow_ref: &offer.base.job_workflow_ref,
        }
    }
}

impl<'a> From<&'a ScaleSetJobAssigned> for OfferProbe<'a> {
    fn from(assigned: &'a ScaleSetJobAssigned) -> Self {
        Self {
            event_name: &assigned.base.event_name,
            owner_name: &assigned.base.owner_name,
            repository_name: &assigned.base.repository_name,
            job_workflow_ref: &assigned.base.job_workflow_ref,
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

fn valid_event_name(event: &str) -> bool {
    !event.is_empty()
        && event
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_workflow_path(path: &str) -> bool {
    path.starts_with(".github/workflows/")
        && path.len() > ".github/workflows/".len()
        && !path.contains("..")
        && !path.chars().any(char::is_control)
}

fn normalize_workflow_ref(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return None;
    }
    if raw.starts_with("refs/")
        || (raw.len() == 40 && raw.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Some(raw.to_owned());
    }
    Some(format!("refs/heads/{raw}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OfferIdentity {
    owner: String,
    repository: String,
    git_ref: String,
    source: String,
    workflow: String,
    event: String,
}

fn parse_offer_identity(probe: &OfferProbe<'_>) -> Option<OfferIdentity> {
    let event = probe.event_name.trim();
    if !valid_event_name(event) {
        return None;
    }
    let owner = probe.owner_name.trim();
    let repository_name = probe.repository_name.trim();
    if owner.is_empty()
        || repository_name.is_empty()
        || owner.contains('/')
        || repository_name.contains('/')
        || owner.chars().any(char::is_control)
        || repository_name.chars().any(char::is_control)
    {
        return None;
    }
    let (source_path, raw_ref) = probe.job_workflow_ref.trim().rsplit_once('@')?;
    let mut source_parts = source_path.splitn(3, '/');
    let source_owner = source_parts.next()?.trim();
    let source_repository = source_parts.next()?.trim();
    let workflow = source_parts.next()?.trim();
    if source_owner.is_empty()
        || source_repository.is_empty()
        || source_owner.contains('/')
        || source_repository.contains('/')
        || !valid_workflow_path(workflow)
    {
        return None;
    }
    Some(OfferIdentity {
        owner: owner.to_ascii_lowercase(),
        repository: format!(
            "{}/{}",
            owner.to_ascii_lowercase(),
            repository_name.to_ascii_lowercase()
        ),
        git_ref: normalize_workflow_ref(raw_ref)?,
        source: format!(
            "{}/{}",
            source_owner.to_ascii_lowercase(),
            source_repository.to_ascii_lowercase()
        ),
        workflow: workflow.to_owned(),
        event: event.to_ascii_lowercase(),
    })
}

/// Resolve the canonical durable request ID from a job message.
///
/// Upstream Actions Service transmits `runnerRequestId` on job offers.
/// When direct scale-set assignment is active, `runnerRequestId` may be 0,
/// but `jobId` (GUID) is consistently transmitted across `JobAssigned`,
/// `JobStarted`, and `JobCompleted`. We project non-zero `runnerRequestId`
/// when present, else hash `jobId` stably. A missing job ID is rejected
/// instead of collapsing unrelated offers onto a sentinel key.
#[must_use]
pub fn resolve_job_request_id(base: &velnor_model::ScaleSetJobMessage) -> Option<i64> {
    resolve_job_request_identity(base).map(|(request_id, _)| request_id)
}

/// Resolve the durable ID plus the exact source identity used to derive it.
/// The i64 fallback remains a lookup key for the existing schema, but the
/// original job ID is stored beside it so a hash collision fails closed.
#[must_use]
pub fn resolve_job_request_identity(
    base: &velnor_model::ScaleSetJobMessage,
) -> Option<(i64, String)> {
    if base.runner_request_id != 0 {
        Some((
            base.runner_request_id,
            format!("runner-request:{}", base.runner_request_id),
        ))
    } else if !base.job_id.is_empty() {
        let request_id = crate::scaleset::intents::stable_i64(&base.job_id);
        (request_id != 0).then(|| (request_id, format!("job-id:{}", base.job_id)))
    } else {
        None
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
pub fn classify_offer(
    offer: &ScaleSetJobAvailable,
    admission: Option<&OfferAdmission>,
) -> OfferTrust {
    classify_probe(&OfferProbe::from(offer), admission)
}

/// Classify stored offer fields. The grant pass re-runs the gate over the
/// durable row (not the submit-time verdict) so stale-reset rows and
/// policy changes re-evaluate from the same inputs.
#[must_use]
pub fn classify_probe(probe: &OfferProbe<'_>, admission: Option<&OfferAdmission>) -> OfferTrust {
    let event = probe.event_name.trim();
    if !valid_event_name(event) {
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
    let Some(admission) = admission else {
        return OfferTrust::Unknown {
            reason: "admission-missing",
        };
    };
    if admission.validate().is_err() {
        return OfferTrust::Unknown {
            reason: "admission-missing",
        };
    }
    let Some(identity) = parse_offer_identity(probe) else {
        return OfferTrust::Unknown {
            reason: "workflow-inputs-missing",
        };
    };
    if admission.allows(&identity) {
        OfferTrust::Trusted
    } else {
        OfferTrust::Unknown {
            reason: "admission-denied",
        }
    }
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
    admission: Option<OfferAdmission>,
}

impl DemandStore {
    /// Open the demand store at the state database path.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_inner(path, None)
    }

    /// Open the demand store with the explicit trusted-offer admission policy.
    pub fn open_with_admission(path: &Path, admission: OfferAdmission) -> Result<Self> {
        admission
            .validate()
            .context("validate scale-set admission")?;
        Self::open_inner(path, Some(admission))
    }

    fn open_inner(path: &Path, admission: Option<OfferAdmission>) -> Result<Self> {
        velnor_control::store::Store::open(path).context("migrate scale-set demand schema")?;
        let conn = Connection::open(path).context("open scale-set demand database")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("set demand store busy timeout")?;
        Ok(Self { conn, admission })
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
            job_workflow_ref: row.get(12)?,
            request_identity: row.get(13)?,
        })
    }

    fn classify_offer(&self, offer: &ScaleSetJobAvailable) -> OfferTrust {
        classify_offer(offer, self.admission.as_ref())
    }

    fn classify_demand(&self, demand: &Demand) -> OfferTrust {
        classify_probe(
            &OfferProbe {
                event_name: &demand.event_name,
                owner_name: &demand.repo_owner,
                repository_name: &demand.repo_name,
                job_workflow_ref: &demand.job_workflow_ref,
            },
            self.admission.as_ref(),
        )
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
        let (request_id, request_identity) = resolve_job_request_identity(&offer.base)
            .context("scale-set offer has no canonical request identity")?;
        if let Some(existing) = self.get(scale_set_id, request_id)? {
            if existing.request_identity.is_empty() || existing.request_identity != request_identity
            {
                anyhow::bail!(
                    "request identity collision for scale-set request {request_id}; refusing to merge offers"
                );
            }
            if existing.state == DemandState::Terminal {
                return Ok(SubmitOutcome::ReofferedTerminal);
            }
            return Ok(SubmitOutcome::Redelivered {
                state: existing.state,
            });
        }
        let trust = self.classify_offer(offer);
        let initial = match trust {
            OfferTrust::Trusted => DemandState::Eligible,
            OfferTrust::Unknown { .. } => DemandState::Observed,
        };
        let reason: Option<String> = match trust {
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
                  repo_owner, repo_name, job_id, labels_hash, generation, updated_at, event_name,
                  job_workflow_ref, request_identity)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                    offer.base.job_workflow_ref,
                    request_identity,
                ],
            )
            .context("insert demand row")?;
        tx.commit().context("commit demand submit")?;
        if inserted == 0 {
            // Lost a submit race; the winner's row (original age) stands.
            let row = self
                .get(scale_set_id, request_id)?
                .context("demand row vanished after offer race")?;
            if row.request_identity.is_empty() || row.request_identity != request_identity {
                anyhow::bail!(
                    "request identity collision for scale-set request {request_id}; refusing to merge offers"
                );
            }
            let state = row.state;
            if state == DemandState::Terminal {
                return Ok(SubmitOutcome::ReofferedTerminal);
            }
            return Ok(SubmitOutcome::Redelivered { state });
        }
        Ok(SubmitOutcome::Inserted { sequence })
    }

    /// Record an assigned job directly into `scaleset_demand`.
    ///
    /// When GitHub directly assigns a job (`JobAssigned`) without a prior
    /// `JobAvailable` offer, the row is recorded as acquired only after the
    /// same admission gate used by offers passes. A denied or incomplete
    /// assignment is durable observation only and never owns a permit.
    pub fn submit_assigned(
        &mut self,
        scale_set_id: i32,
        assigned: &velnor_model::ScaleSetJobAssigned,
        generation: u64,
    ) -> Result<(i64, DemandState)> {
        let (request_id, request_identity) = resolve_job_request_identity(&assigned.base)
            .context("scale-set assignment has no canonical request identity")?;
        if let Some(existing) = self.get(scale_set_id, request_id)? {
            if existing.request_identity.is_empty() || existing.request_identity != request_identity
            {
                anyhow::bail!(
                    "request identity collision for scale-set request {request_id}; refusing to merge assignment"
                );
            }
            return Ok((request_id, existing.state));
        }
        let trust = classify_probe(&OfferProbe::from(assigned), self.admission.as_ref());
        let (state, decline_reason) = match trust {
            OfferTrust::Trusted => (DemandState::Acquired, None),
            OfferTrust::Unknown { reason } => (DemandState::Observed, Some(reason.to_owned())),
        };
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
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO scaleset_demand
             (request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
              repo_owner, repo_name, job_id, labels_hash, generation, updated_at, event_name,
              job_workflow_ref, request_identity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    request_id,
                    scale_set_id,
                    now,
                    sequence,
                    state.as_str(),
                    decline_reason,
                    assigned.base.owner_name,
                    assigned.base.repository_name,
                    job_id_hash,
                    labels_hash,
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    now,
                    assigned.base.event_name,
                    assigned.base.job_workflow_ref,
                    request_identity,
                ],
            )
            .context("insert demand row for assigned job")?;
        tx.commit().context("commit demand submit_assigned")?;
        if inserted == 0 {
            let row = self
                .get(scale_set_id, request_id)?
                .context("demand row vanished after assignment race")?;
            if row.request_identity.is_empty() || row.request_identity != request_identity {
                anyhow::bail!(
                    "request identity collision for scale-set request {request_id}; refusing to merge assignment"
                );
            }
            let state = row.state;
            return Ok((request_id, state));
        }
        Ok((request_id, state))
    }

    /// Fetch one demand row by its scale-set-scoped request identity.
    pub fn get(&self, scale_set_id: i32, request_id: i64) -> Result<Option<Demand>> {
        self.conn
            .query_row(
                "SELECT request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
                        repo_owner, repo_name, job_id, generation, updated_at, event_name,
                        job_workflow_ref, request_identity
                 FROM scaleset_demand
                 WHERE scale_set_id = ?1 AND request_id = ?2",
                params![scale_set_id, request_id],
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
                        repo_owner, repo_name, job_id, generation, updated_at, event_name,
                        job_workflow_ref, request_identity
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
                        job_workflow_ref, request_identity
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
    ///
    /// This compatibility API is itself fenced: a stale generation or a
    /// terminal/declined row is a durable no-op. New lifecycle code should
    /// use [`compare_and_set_state`] so the observed state is part of the
    /// precondition too.
    pub fn set_state(
        &mut self,
        scale_set_id: i32,
        request_id: i64,
        state: DemandState,
        decline_reason: Option<&str>,
        generation: u64,
    ) -> Result<()> {
        let Some(current) = self.get(scale_set_id, request_id)? else {
            anyhow::bail!("demand holds no row for scale set {scale_set_id}, request {request_id}");
        };
        if current.generation > generation
            || matches!(current.state, DemandState::Terminal | DemandState::Declined)
        {
            return Ok(());
        }
        if current.state == state && current.generation == generation {
            return Ok(());
        }
        let _ = self.compare_and_set_state(
            scale_set_id,
            request_id,
            current.state,
            current.generation,
            state,
            decline_reason,
            generation,
        )?;
        Ok(())
    }

    /// Compare-and-set one demand row. A request can advance its recorded
    /// generation, but never move backwards; the observed state must still be
    /// exactly the state read by the caller. This is the durable fence used by
    /// the acquire path to reject stale processors without overwriting newer
    /// or terminal work.
    pub fn compare_and_set_state(
        &mut self,
        scale_set_id: i32,
        request_id: i64,
        expected_state: DemandState,
        expected_generation: u64,
        state: DemandState,
        decline_reason: Option<&str>,
        generation: u64,
    ) -> Result<bool> {
        if generation < expected_generation {
            return Ok(false);
        }
        if matches!(
            expected_state,
            DemandState::Terminal | DemandState::Declined
        ) && expected_state != state
        {
            return Ok(false);
        }
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_demand
                 SET state = ?1, decline_reason = ?2, generation = ?3, updated_at = ?4
                 WHERE scale_set_id = ?5 AND request_id = ?6
                   AND state = ?7 AND generation = ?8 AND generation <= ?3",
                params![
                    state.as_str(),
                    decline_reason,
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    Self::now_rfc3339(),
                    scale_set_id,
                    request_id,
                    expected_state.as_str(),
                    i64::try_from(expected_generation).unwrap_or(i64::MAX),
                ],
            )
            .context("compare-and-set demand state")?;
        Ok(updated == 1)
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
        match store.classify_demand(&candidate) {
            OfferTrust::Trusted => {
                if store.compare_and_set_state(
                    candidate.scale_set_id,
                    candidate.request_id,
                    DemandState::Eligible,
                    candidate.generation,
                    DemandState::Granted,
                    None,
                    generation,
                )? {
                    metrics.add_offers_granted(1);
                    if let Some(row) = store.get(candidate.scale_set_id, candidate.request_id)? {
                        granted.push(row);
                    }
                }
            }
            OfferTrust::Unknown { reason } => {
                let _ = store.compare_and_set_state(
                    candidate.scale_set_id,
                    candidate.request_id,
                    DemandState::Eligible,
                    candidate.generation,
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

    fn admission() -> OfferAdmission {
        OfferAdmission {
            owner: vec!["tailrocks".to_owned()],
            repository: vec!["tailrocks/velnor".to_owned()],
            git_ref: vec!["main".to_owned()],
            source: vec!["tailrocks/velnor".to_owned()],
            workflow: vec![".github/workflows/ci.yml".to_owned()],
            event: vec![
                "push".to_owned(),
                "workflow_dispatch".to_owned(),
                "pull_request".to_owned(),
                "pull_request_target".to_owned(),
                "workflow_run".to_owned(),
            ],
        }
    }

    fn assigned(request_id: i64, event: &str) -> ScaleSetJobAssigned {
        let mut base = offer(request_id, event).base;
        base.message_type = velnor_model::ScaleSetJobMessageType::JobAssigned;
        ScaleSetJobAssigned { base }
    }

    #[test]
    fn submit_assigns_sequence_and_redelivery_keeps_age() {
        let path = temp_path("submit");
        let mut store = DemandStore::open_with_admission(&path, admission()).unwrap();
        let first = store.submit_offer(7, &offer(101, "push"), 3).unwrap();
        let sequence = match first {
            SubmitOutcome::Inserted { sequence } => sequence,
            other => panic!("expected insert, got {other:?}"),
        };
        let before = store.get(7, 101).unwrap().unwrap();
        let again = store.submit_offer(7, &offer(101, "push"), 3).unwrap();
        assert_eq!(
            again,
            SubmitOutcome::Redelivered {
                state: DemandState::Eligible,
            }
        );
        let after = store.get(7, 101).unwrap().unwrap();
        assert_eq!(after.first_seen_at, before.first_seen_at);
        assert_eq!(after.sequence, sequence);
        assert_eq!(after.sequence, before.sequence);
    }

    #[test]
    fn trust_gate_grants_push_and_parks_pr_without_blocking() {
        let path = temp_path("grant");
        let mut store = DemandStore::open_with_admission(&path, admission()).unwrap();
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
            store.get(7, 201).unwrap().unwrap().state,
            DemandState::Observed
        );
        assert_eq!(
            store
                .get(7, 201)
                .unwrap()
                .unwrap()
                .decline_reason
                .as_deref(),
            Some("trust-inputs-missing")
        );
        assert_eq!(
            store.get(7, 202).unwrap().unwrap().state,
            DemandState::Granted
        );
        assert_eq!(metrics.snapshot().offers_granted, 1);
    }

    #[test]
    fn stale_generation_grants_reset_to_eligible_with_age_kept() {
        let path = temp_path("stale");
        let mut store = DemandStore::open_with_admission(&path, admission()).unwrap();
        store.submit_offer(7, &offer(301, "push"), 5).unwrap();
        let metrics = crate::scaleset::metrics::Metrics::new();
        assert_eq!(grant_oldest(&mut store, 7, 5, &metrics).unwrap().len(), 1);
        let before = store.get(7, 301).unwrap().unwrap();
        assert_eq!(before.state, DemandState::Granted);
        let reset = store.reset_stale_grants(7, 6).unwrap();
        assert_eq!(reset, 1);
        let after = store.get(7, 301).unwrap().unwrap();
        assert_eq!(after.state, DemandState::Eligible);
        assert_eq!(after.first_seen_at, before.first_seen_at);
        assert_eq!(after.sequence, before.sequence);
        // Re-granted fresh in the new epoch.
        assert_eq!(grant_oldest(&mut store, 7, 6, &metrics).unwrap().len(), 1);
        assert_eq!(store.get(7, 301).unwrap().unwrap().generation, 6);
    }

    #[test]
    fn terminal_rows_never_revive_on_reoffer() {
        let path = temp_path("terminal");
        let mut store = DemandStore::open_with_admission(&path, admission()).unwrap();
        store.submit_offer(7, &offer(401, "push"), 5).unwrap();
        store
            .set_state(7, 401, DemandState::Terminal, None, 5)
            .unwrap();
        assert_eq!(
            store.submit_offer(7, &offer(401, "push"), 5).unwrap(),
            SubmitOutcome::ReofferedTerminal
        );
        assert_eq!(
            store.get(7, 401).unwrap().unwrap().state,
            DemandState::Terminal
        );
    }

    #[test]
    fn stale_state_cas_cannot_overwrite_terminal_or_new_generation() {
        let path = temp_path("state-cas");
        let mut store = DemandStore::open_with_admission(&path, admission()).unwrap();
        store.submit_offer(7, &offer(450, "push"), 4).unwrap();
        assert!(store
            .compare_and_set_state(
                7,
                450,
                DemandState::Eligible,
                4,
                DemandState::Granted,
                None,
                4,
            )
            .unwrap());
        assert!(store
            .compare_and_set_state(
                7,
                450,
                DemandState::Granted,
                4,
                DemandState::Terminal,
                None,
                5,
            )
            .unwrap());
        assert!(!store
            .compare_and_set_state(
                7,
                450,
                DemandState::Granted,
                4,
                DemandState::Eligible,
                None,
                4,
            )
            .unwrap());
        store
            .set_state(7, 450, DemandState::Eligible, None, 4)
            .unwrap();
        let row = store.get(7, 450).unwrap().unwrap();
        assert_eq!(row.state, DemandState::Terminal);
        assert_eq!(row.generation, 5);
    }

    #[test]
    fn incomplete_request_identity_fails_closed_on_redelivery() {
        let path = temp_path("identity-fence");
        let mut store = DemandStore::open_with_admission(&path, admission()).unwrap();
        store.submit_offer(7, &offer(451, "push"), 4).unwrap();
        store
            .conn
            .execute(
                "UPDATE scaleset_demand SET request_identity = ''
                 WHERE scale_set_id = 7 AND request_id = 451",
                [],
            )
            .unwrap();
        assert!(store.submit_offer(7, &offer(451, "push"), 4).is_err());
    }

    #[test]
    fn classify_offer_fails_closed_on_unknown_inputs() {
        let policy = admission();
        assert_eq!(
            classify_offer(&offer(1, "push"), None),
            OfferTrust::Unknown {
                reason: "admission-missing"
            }
        );
        assert_eq!(
            classify_offer(&offer(1, "push"), Some(&policy)),
            OfferTrust::Trusted
        );
        assert_eq!(
            classify_offer(&offer(1, "workflow_dispatch"), Some(&policy)),
            OfferTrust::Trusted
        );
        assert_eq!(
            classify_offer(&offer(1, "pull_request"), Some(&policy)),
            OfferTrust::Unknown {
                reason: "trust-inputs-missing"
            }
        );
        assert_eq!(
            classify_offer(&offer(1, "pull_request_target"), Some(&policy)),
            OfferTrust::Unknown {
                reason: "trust-inputs-missing"
            }
        );
        assert_eq!(
            classify_offer(&offer(1, "workflow_run"), Some(&policy)),
            OfferTrust::Unknown {
                reason: "trust-inputs-missing"
            }
        );
        assert_eq!(
            classify_offer(&offer(1, ""), Some(&policy)),
            OfferTrust::Unknown {
                reason: "event-unparseable"
            }
        );
        assert_eq!(
            classify_offer(&offer(1, "push;rm"), Some(&policy)),
            OfferTrust::Unknown {
                reason: "event-unparseable"
            }
        );
        let mut no_repo = offer(1, "push");
        no_repo.base.owner_name.clear();
        assert_eq!(
            classify_offer(&no_repo, Some(&policy)),
            OfferTrust::Unknown {
                reason: "repo-missing"
            }
        );
    }

    #[test]
    fn assigned_path_applies_admission_before_acquired_state() {
        let path = temp_path("assigned-admission");
        let mut store = DemandStore::open_with_admission(&path, admission()).unwrap();

        let mut denied = assigned(501, "push");
        denied.base.repository_name = "other-repository".to_owned();
        let (request_id, state) = store.submit_assigned(7, &denied, 1).unwrap();
        assert_eq!(request_id, 501);
        assert_eq!(state, DemandState::Observed);
        let row = store.get(7, request_id).unwrap().unwrap();
        assert_eq!(row.state, DemandState::Observed);
        assert_eq!(row.decline_reason.as_deref(), Some("admission-denied"));
        assert_eq!(
            store
                .count_in_states(7, &[DemandState::Acquired, DemandState::Granted])
                .unwrap(),
            0
        );

        let (_, state) = store.submit_assigned(7, &assigned(502, "push"), 1).unwrap();
        assert_eq!(state, DemandState::Acquired);
    }

    #[test]
    fn same_request_id_is_independent_per_scale_set() {
        let path = temp_path("composite-identity");
        let mut store = DemandStore::open_with_admission(&path, admission()).unwrap();

        assert!(matches!(
            store.submit_offer(7, &offer(777, "push"), 1).unwrap(),
            SubmitOutcome::Inserted { .. }
        ));
        assert!(matches!(
            store.submit_offer(8, &offer(777, "push"), 1).unwrap(),
            SubmitOutcome::Inserted { .. }
        ));

        let first = store.get(7, 777).unwrap().unwrap();
        let second = store.get(8, 777).unwrap().unwrap();
        assert_eq!(first.scale_set_id, 7);
        assert_eq!(second.scale_set_id, 8);
        assert_ne!(first.sequence, second.sequence);

        assert!(store
            .compare_and_set_state(
                7,
                777,
                DemandState::Eligible,
                1,
                DemandState::Granted,
                None,
                1,
            )
            .unwrap());
        assert_eq!(
            store.get(7, 777).unwrap().unwrap().state,
            DemandState::Granted
        );
        assert_eq!(
            store.get(8, 777).unwrap().unwrap().state,
            DemandState::Eligible
        );
    }

    #[test]
    fn assignment_without_identity_fails_before_persisting_or_mutating_permits() {
        let path = temp_path("assigned-no-identity");
        let mut store = DemandStore::open_with_admission(&path, admission()).unwrap();
        let mut missing = assigned(0, "push");
        missing.base.job_id.clear();
        assert!(store.submit_assigned(7, &missing, 1).is_err());
        assert!(store
            .list_in_states_all(&[DemandState::Acquired])
            .unwrap()
            .is_empty());
    }
}

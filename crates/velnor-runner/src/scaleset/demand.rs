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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, ToSql};
use time::format_description::well_known::Rfc3339;
use velnor_control::permit_ledger::PendingScaleSetDemandPublication;
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
    /// storable there. `request_id` stays the exact key; this hash is
    /// correlation-only.
    pub job_id_hash: i64,
    pub generation: u64,
    pub updated_at: String,
    /// Durable deciding input for the grant-pass trust gate (v23).
    pub event_name: String,
    /// Immutable identity of the current global permit acquisition. It stays
    /// with the durable request through crash/replay cleanup so stale
    /// callbacks cannot release a later same-holder lease.
    pub permit_lease_generation: Option<u64>,
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
    path: PathBuf,
}

fn ensure_permit_lease_generation_column(conn: &mut Connection) -> Result<()> {
    if DemandStore::table_has_column(conn, "scaleset_demand", "permit_lease_generation")? {
        return Ok(());
    }
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .context("begin scale-set permit lease migration")?;
    if !DemandStore::table_has_column(&tx, "scaleset_demand", "permit_lease_generation")? {
        tx.execute_batch("ALTER TABLE scaleset_demand ADD COLUMN permit_lease_generation INTEGER;")
            .context("add scale-set permit lease identity")?;
    }
    tx.commit()
        .context("commit scale-set permit lease migration")?;
    Ok(())
}

impl DemandStore {
    /// Open the demand store at the state database path.
    pub fn open(path: &Path) -> Result<Self> {
        velnor_control::store::Store::open(path).context("migrate scale-set demand schema")?;
        let mut conn = Connection::open(path).context("open scale-set demand database")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("set demand store busy timeout")?;
        ensure_permit_lease_generation_column(&mut conn)?;
        let path = path
            .canonicalize()
            .with_context(|| format!("resolve scale-set demand database {}", path.display()))?;
        Ok(Self { conn, path })
    }

    /// Canonical durable database backing this demand store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
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
            permit_lease_generation: row
                .get::<_, Option<i64>>(12)?
                .map(|generation| generation.max(0) as u64),
        })
    }

    fn table_has_column(conn: &Connection, table: &str, wanted: &str) -> Result<bool> {
        let mut statement = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .context("inspect scale-set demand columns")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(1))
            .context("query scale-set demand columns")?;
        for column in rows {
            if column.context("read scale-set demand column")? == wanted {
                return Ok(true);
            }
        }
        Ok(false)
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
        let first_seen_at = Self::now_rfc3339();
        self.submit_offer_at(scale_set_id, offer, generation, &first_seen_at)
    }

    /// Insert one offer with a timestamp allocated by the host-wide queue
    /// transaction. Global-first ingress uses this so local eligibility
    /// cannot receive a younger or older clock sample than its mirror.
    pub fn submit_offer_at(
        &mut self,
        scale_set_id: i32,
        offer: &ScaleSetJobAvailable,
        generation: u64,
        first_seen_at: &str,
    ) -> Result<SubmitOutcome> {
        let request_id = offer.base.runner_request_id;
        if let Some(existing) = self.get(request_id)? {
            if existing.state == DemandState::Terminal {
                return Ok(SubmitOutcome::ReofferedTerminal);
            }
            self.conn
                .execute(
                    "UPDATE scaleset_demand SET updated_at = ?1
                     WHERE request_id = ?2 AND state != 'terminal'",
                    params![Self::now_rfc3339(), request_id],
                )
                .context("refresh demand redelivery time")?;
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
                    first_seen_at,
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
                        repo_owner, repo_name, job_id, generation, updated_at, event_name,
                        permit_lease_generation
                 FROM scaleset_demand WHERE request_id = ?1",
                params![request_id],
                Self::row_to_demand,
            )
            .optional()
            .context("fetch demand row")
    }

    /// Immutable fields needed to replay a global-first publication for an
    /// already persisted eligible row. Redelivery must publish the durable
    /// row's identity, even when the server repeats changed offer fields.
    pub fn publication_for_request(
        &self,
        request_id: i64,
    ) -> Result<Option<velnor_control::permit_ledger::ScaleSetDemandPublication>> {
        self.conn
            .query_row(
                "SELECT request_id, scale_set_id, repo_owner, repo_name, job_id,
                        labels_hash, event_name
                 FROM scaleset_demand WHERE request_id = ?1",
                params![request_id],
                |row| {
                    Ok(velnor_control::permit_ledger::ScaleSetDemandPublication {
                        request_id: row.get(0)?,
                        scale_set_id: row.get(1)?,
                        repo_owner: row.get(2)?,
                        repo_name: row.get(3)?,
                        job_id_hash: row.get(4)?,
                        labels_hash: row.get(5)?,
                        event_name: row.get(6)?,
                    })
                },
            )
            .optional()
            .context("fetch durable Scale Set publication fields")
    }

    /// Oldest-first `eligible` rows for one set, bounded by `limit`.
    pub fn oldest_eligible(&self, scale_set_id: i32, limit: usize) -> Result<Vec<Demand>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
                        repo_owner, repo_name, job_id, generation, updated_at, event_name,
                        permit_lease_generation
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

    /// Every eligible row across all sets, oldest-first. Startup uses this
    /// to restore the host-wide permit queue from durable demand age.
    pub fn list_eligible_all(&self) -> Result<Vec<Demand>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
                        repo_owner, repo_name, job_id, generation, updated_at, event_name,
                        permit_lease_generation
                 FROM scaleset_demand
                 WHERE state = 'eligible'
                 ORDER BY first_seen_at, sequence",
            )
            .context("prepare all eligible demand query")?;
        stmt.query_map([], Self::row_to_demand)
            .context("query all eligible demand")?
            .collect::<Result<Vec<_>, _>>()
            .context("read all eligible demand")
    }

    /// Recreate a locally missing eligible row from the global-first
    /// publication record. This repairs a crash after the host queue marker
    /// committed but before the source database insert. Existing rows must
    /// agree with the publication; replay never overwrites age or sequence.
    pub fn restore_pending_offer(
        &mut self,
        pending: &PendingScaleSetDemandPublication,
    ) -> Result<()> {
        let canonical_source = pending
            .source_db
            .canonicalize()
            .with_context(|| format!("resolve pending source {}", pending.source_db.display()))?;
        anyhow::ensure!(
            canonical_source == self.path,
            "pending Scale Set publication source does not match opened database"
        );
        let (holder_scale_set, holder_request) =
            crate::scaleset::intents::parse_permit_holder(&pending.holder)
                .context("parse pending Scale Set holder")?;
        anyhow::ensure!(
            holder_scale_set == pending.publication.scale_set_id
                && holder_request == pending.publication.request_id,
            "pending Scale Set holder and publication IDs disagree"
        );
        anyhow::ensure!(
            matches!(
                classify_probe(&OfferProbe {
                    event_name: &pending.publication.event_name,
                    owner_name: &pending.publication.repo_owner,
                    repository_name: &pending.publication.repo_name,
                }),
                OfferTrust::Trusted
            ),
            "pending Scale Set publication no longer satisfies the trusted-offer gate"
        );

        let first_seen_at = time::OffsetDateTime::from_unix_timestamp(
            i64::try_from(pending.first_seen_unix).context("pending timestamp exceeds i64")?,
        )
        .context("construct pending first-seen timestamp")?
        .replace_nanosecond(pending.first_seen_subsec_nanos.min(999_999_999))
        .context("set pending timestamp precision")?
        .format(&Rfc3339)
        .context("format pending first-seen timestamp")?;
        let generation = i64::try_from(pending.generation)
            .context("pending generation exceeds database range")?;
        let now = Self::now_rfc3339();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin pending Scale Set publication replay")?;
        let existing = tx
            .query_row(
                "SELECT request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
                        repo_owner, repo_name, job_id, generation, updated_at, event_name,
                        permit_lease_generation
                 FROM scaleset_demand WHERE request_id = ?1",
                params![pending.publication.request_id],
                Self::row_to_demand,
            )
            .optional()
            .context("read existing Scale Set row during publication replay")?;
        if let Some(existing) = existing {
            let existing_time = velnor_model::Timestamp::parse(&existing.first_seen_at)
                .context("parse existing first-seen time during publication replay")?
                .as_offset_datetime();
            let (labels_hash, event_name): (String, String) = tx
                .query_row(
                    "SELECT labels_hash, event_name FROM scaleset_demand WHERE request_id = ?1",
                    params![pending.publication.request_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .context("read immutable offer fields during publication replay")?;
            let pending_seconds =
                i64::try_from(pending.first_seen_unix).context("pending timestamp exceeds i64")?;
            anyhow::ensure!(
                existing.state == DemandState::Eligible
                    && existing.scale_set_id == pending.publication.scale_set_id
                    && existing_time.unix_timestamp() == pending_seconds
                    && existing_time.nanosecond() == pending.first_seen_subsec_nanos
                    && existing.repo_owner == pending.publication.repo_owner
                    && existing.repo_name == pending.publication.repo_name
                    && existing.job_id_hash == pending.publication.job_id_hash
                    && labels_hash == pending.publication.labels_hash
                    && event_name == pending.publication.event_name,
                "existing Scale Set row conflicts with pending global publication"
            );
            // The durable global marker proves this offer has an
            // uncommitted publication, even if the source row predates the
            // stale-demand window. Refresh only liveness here; its original
            // age and sequence remain the queue order.
            tx.execute(
                "UPDATE scaleset_demand SET updated_at = ?1
                 WHERE request_id = ?2 AND state = 'eligible'",
                params![now, pending.publication.request_id],
            )
            .context("refresh replayed eligible demand liveness")?;
            tx.commit()
                .context("finish idempotent Scale Set publication replay")?;
            return Ok(());
        }

        let sequence: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM scaleset_demand",
                [],
                |row| row.get(0),
            )
            .context("allocate replayed demand sequence")?;
        tx.execute(
            "INSERT INTO scaleset_demand
             (request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
              repo_owner, repo_name, job_id, labels_hash, generation, updated_at, event_name)
             VALUES (?1, ?2, ?3, ?4, 'eligible', NULL, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                pending.publication.request_id,
                pending.publication.scale_set_id,
                first_seen_at,
                sequence,
                pending.publication.repo_owner,
                pending.publication.repo_name,
                pending.publication.job_id_hash,
                pending.publication.labels_hash,
                generation,
                now,
                pending.publication.event_name,
            ],
        )
        .context("restore missing eligible Scale Set demand")?;
        tx.commit()
            .context("commit replayed Scale Set publication")?;
        Ok(())
    }

    /// Oldest-first `granted` rows for one set, bounded by `limit`.
    pub fn oldest_granted(&self, scale_set_id: i32, limit: usize) -> Result<Vec<Demand>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT request_id, scale_set_id, first_seen_at, sequence, state, decline_reason,
                        repo_owner, repo_name, job_id, generation, updated_at, event_name,
                        permit_lease_generation
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

    /// Advance one durable state only if the row still has `expected`.
    /// Completion and provisioning can run in different processes, so a
    /// read followed by an unconditional write can resurrect a terminal
    /// request after cleanup has started.
    pub fn transition_state_if(
        &mut self,
        request_id: i64,
        expected: DemandState,
        state: DemandState,
        decline_reason: Option<&str>,
        generation: u64,
    ) -> Result<bool> {
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_demand
                 SET state = ?1, decline_reason = ?2, generation = ?3, updated_at = ?4
                 WHERE request_id = ?5 AND state = ?6",
                params![
                    state.as_str(),
                    decline_reason,
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    Self::now_rfc3339(),
                    request_id,
                    expected.as_str(),
                ],
            )
            .context("conditionally transition demand state")?;
        Ok(updated == 1)
    }

    /// Atomically persist the pre-network acquire intent with the immutable
    /// lease identity returned by the host ledger. A crash/replay may then
    /// release only this attempt even if the same request ID is later
    /// reacquired.
    pub fn set_state_with_permit_lease(
        &mut self,
        request_id: i64,
        state: DemandState,
        decline_reason: Option<&str>,
        generation: u64,
        permit_lease_generation: u64,
    ) -> Result<()> {
        anyhow::ensure!(
            permit_lease_generation > 0,
            "permit lease generation must be positive"
        );
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin demand state/lease update")?;
        let updated = tx
            .execute(
                "UPDATE scaleset_demand
                 SET state = ?1, decline_reason = ?2, generation = ?3, updated_at = ?4,
                     permit_lease_generation = ?5
                 WHERE request_id = ?6",
                params![
                    state.as_str(),
                    decline_reason,
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    Self::now_rfc3339(),
                    i64::try_from(permit_lease_generation).unwrap_or(i64::MAX),
                    request_id,
                ],
            )
            .context("persist demand state and permit lease")?;
        if updated == 0 {
            anyhow::bail!("demand holds no row for request {request_id}");
        }
        tx.commit().context("commit demand state/lease update")?;
        Ok(())
    }

    /// Backfill lease identity during startup attestation for an older
    /// durable request whose held global row predates local lease recording.
    /// Never overwrite a different lease outside the fresh Granted edge.
    pub fn record_permit_lease_generation(
        &mut self,
        request_id: i64,
        permit_lease_generation: u64,
    ) -> Result<()> {
        anyhow::ensure!(
            permit_lease_generation > 0,
            "permit lease generation must be positive"
        );
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin demand permit lease backfill")?;
        let existing: Option<(String, Option<i64>)> = tx
            .query_row(
                "SELECT state, permit_lease_generation FROM scaleset_demand
                 WHERE request_id = ?1",
                params![request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("read demand permit lease before backfill")?;
        let Some((state_raw, existing_generation)) = existing else {
            anyhow::bail!("demand holds no row for request {request_id}");
        };
        let state = DemandState::parse(&state_raw)?;
        anyhow::ensure!(
            state.holds_permit() || state == DemandState::Terminal,
            "demand {request_id} in state {state:?} cannot bind a held permit lease"
        );
        if let Some(existing_generation) = existing_generation {
            anyhow::ensure!(
                existing_generation.max(0) as u64 == permit_lease_generation
                    || state == DemandState::Granted,
                "demand {request_id} is already bound to another permit lease"
            );
        }
        tx.execute(
            "UPDATE scaleset_demand SET permit_lease_generation = ?1 WHERE request_id = ?2",
            params![
                i64::try_from(permit_lease_generation).unwrap_or(i64::MAX),
                request_id,
            ],
        )
        .context("backfill demand permit lease")?;
        tx.commit().context("commit demand permit lease backfill")?;
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
    fn conditional_state_transition_does_not_resurrect_terminal_demand() {
        let path = temp_path("conditional-state-transition");
        let mut store = DemandStore::open(&path).unwrap();
        store.submit_offer(7, &offer(103, "push"), 3).unwrap();
        store
            .set_state(103, DemandState::Acquired, None, 3)
            .unwrap();
        store
            .set_state(103, DemandState::Terminal, None, 3)
            .unwrap();

        assert!(!store
            .transition_state_if(
                103,
                DemandState::Acquired,
                DemandState::ProvisionIntent,
                None,
                4,
            )
            .unwrap());
        assert_eq!(
            store.get(103).unwrap().unwrap().state,
            DemandState::Terminal
        );
    }

    #[test]
    fn pending_global_publication_replay_restores_missing_row_once_with_exact_age() {
        let path = temp_path("pending-replay");
        // The journal records the canonical path of an already-created
        // demand database; restore that source before constructing the row.
        drop(DemandStore::open(&path).unwrap());
        let source_db = path.canonicalize().unwrap();
        let request_id = 102;
        let pending = PendingScaleSetDemandPublication {
            holder: crate::scaleset::intents::permit_holder(7, request_id),
            source_db,
            generation: 9,
            first_seen_unix: 1_789_000_000,
            first_seen_subsec_nanos: 123_456_789,
            publication: velnor_control::permit_ledger::ScaleSetDemandPublication {
                request_id,
                scale_set_id: 7,
                repo_owner: "tailrocks".to_owned(),
                repo_name: "velnor".to_owned(),
                job_id_hash: crate::scaleset::intents::stable_i64(&format!("job-{request_id}")),
                labels_hash: crate::scaleset::intents::labels_hash(&["velnor".to_owned()]),
                event_name: "push".to_owned(),
            },
        };

        let mut store = DemandStore::open(&path).unwrap();
        store.restore_pending_offer(&pending).unwrap();
        let restored = store.get(request_id).unwrap().unwrap();
        let restored_time = velnor_model::Timestamp::parse(&restored.first_seen_at)
            .unwrap()
            .as_offset_datetime();
        assert_eq!(restored.state, DemandState::Eligible);
        assert_eq!(restored.scale_set_id, 7);
        assert_eq!(restored.generation, 9);
        assert_eq!(restored_time.unix_timestamp(), 1_789_000_000);
        assert_eq!(restored_time.nanosecond(), 123_456_789);
        store.restore_pending_offer(&pending).unwrap();
        let replayed = store.get(request_id).unwrap().unwrap();
        assert_eq!(replayed.sequence, restored.sequence);
        assert_eq!(replayed.first_seen_at, restored.first_seen_at);
        let original_age = velnor_model::Timestamp::parse(&replayed.first_seen_at)
            .unwrap()
            .as_offset_datetime();
        store
            .conn
            .execute(
                "UPDATE scaleset_demand SET updated_at = '1970-01-01T00:00:00Z'
                 WHERE request_id = ?1",
                params![request_id],
            )
            .unwrap();
        store.restore_pending_offer(&pending).unwrap();
        let refreshed = store.get(request_id).unwrap().unwrap();
        let refreshed_age = velnor_model::Timestamp::parse(&refreshed.first_seen_at)
            .unwrap()
            .as_offset_datetime();
        let refreshed_liveness = velnor_model::Timestamp::parse(&refreshed.updated_at)
            .unwrap()
            .as_offset_datetime();
        assert_eq!(refreshed.state, DemandState::Eligible);
        assert_eq!(refreshed.sequence, restored.sequence);
        assert_eq!(refreshed_age, original_age);
        assert!(refreshed_liveness.unix_timestamp() > 0);
        store
            .conn
            .execute(
                "UPDATE scaleset_demand SET repo_name = 'changed' WHERE request_id = ?1",
                params![request_id],
            )
            .unwrap();
        assert!(store.restore_pending_offer(&pending).is_err());
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

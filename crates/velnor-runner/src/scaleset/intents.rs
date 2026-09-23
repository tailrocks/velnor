//! Durable intents + idempotency keys (§5.1 steps 3–5).
//!
//! Every effectful call is preceded by its intent row: the acquire batch
//! persists BEFORE `acquirejobs`, the provision intent BEFORE Docker. Crash
//! anywhere and the retry adopts the recorded intent instead of minting a
//! second one. Keys:
//!
//! * ledger holder: [`permit_holder`] — deterministic per
//!   `(scale_set_id, request_id)`, so reserve retries and the worker lane's
//!   post-cleanup release address the same row;
//! * acquire batch: [`mint_batch_id`] — unique per attempt; retries read the
//!   recorded open batch rather than minting;
//! * provision: [`provision_operation_id`] per `(set, request, attempt)` +
//!   [`provision_ownership_id`] per `(set, runner_name)`, with the
//!   deterministic [`runner_name`]; retries adopt by ownership.
//!
//! Fingerprints ([`labels_hash`], [`jit_fingerprint`], [`stable_i64`]) are
//! the only secret-adjacent projections the stores keep: hashes, never raw
//! labels, JIT blobs, or URLs.

use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::scaleset::demand::{LocalCorrelationId, RunnerRequestId};

/// Ledger holder for one scale-set acquisition. Deterministic: every retry
/// and the post-cleanup release resolve to this exact string.
///
/// The slash form matches the native lane's `native/<id>` namespace: one
/// ledger, one separator convention, no per-lane parsing.
#[must_use]
pub fn permit_holder(scale_set_id: i32, request_id: i64) -> String {
    format!("scaleset/{scale_set_id}/{request_id}")
}

/// Parse a [`permit_holder`] back into `(scale_set_id, request_id)`.
pub fn parse_permit_holder(holder: &str) -> Result<(i32, i64)> {
    let rest = holder
        .strip_prefix("scaleset/")
        .with_context(|| format!("permit holder {holder:?} is not a scale-set holder"))?;
    let (set, request) = rest
        .split_once('/')
        .with_context(|| format!("permit holder {holder:?} has no request part"))?;
    Ok((
        set.parse()
            .with_context(|| format!("permit holder {holder:?} has a bad scale-set part"))?,
        request
            .parse()
            .with_context(|| format!("permit holder {holder:?} has a bad request part"))?,
    ))
}

/// Mint a unique acquire-batch ID. Uniqueness (not stability) is what
/// minting needs: retries adopt the recorded open batch.
#[must_use]
pub fn mint_batch_id(scale_set_id: i32) -> String {
    format!(
        "acq-{scale_set_id}-{}-{}",
        std::process::id(),
        velnor_model::Timestamp::now()
            .as_offset_datetime()
            .unix_timestamp_nanos()
    )
}

/// Stable runner name per `(scale_set_id, request_id)`: retries and the
/// `GetRunnerByName` oracle resolve the same name, never a random one.
#[must_use]
pub fn runner_name(scale_set_id: i32, request_id: i64) -> String {
    format!("velnor-{scale_set_id}-{request_id}")
}

/// Parse `(scale_set_id, request_id)` from a runner name formatted like
/// `velnor-{scale_set_id}-{request_id}`. Only canonical decimal spellings
/// emitted by [`runner_name`] are accepted. Signed, zero-padded, and
/// non-positive identities are invalid: generated Velnor runner names always
/// carry positive IDs.
#[must_use]
pub fn parse_runner_name(name: &str) -> Option<(i32, i64)> {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() == 3 && parts[0] == "velnor" {
        let set_id = parts[1].parse::<i32>().ok()?;
        let req_id = parts[2].parse::<i64>().ok()?;
        (set_id > 0 && req_id > 0 && runner_name(set_id, req_id) == name)
            .then_some((set_id, req_id))
    } else {
        None
    }
}

/// Resolve the request identity carried by an event's runner name.
///
/// A non-empty runner name is authoritative. Invalid or cross-scale names
/// return `None` instead of falling back to the event's request field, which
/// would let another scale set mutate this processor's demand or permit.
#[must_use]
pub fn request_id_for_runner(
    scale_set_id: i32,
    runner_name: &str,
    fallback_request_id: i64,
) -> Option<i64> {
    if runner_name.is_empty() {
        return Some(fallback_request_id);
    }
    let (event_scale_set_id, request_id) = parse_runner_name(runner_name)?;
    (event_scale_set_id == scale_set_id).then_some(request_id)
}

/// Provision operation idempotency key: stable per attempt, distinct across
/// attempts so a failed attempt never aliases its retry.
#[must_use]
pub fn provision_operation_id(scale_set_id: i32, request_id: i64, attempt: u32) -> String {
    format!("prov-op-{scale_set_id}-{request_id}-{attempt}")
}

/// Provision ownership idempotency key: stable across attempts, so a retry
/// adopts the containers its attempt created.
#[must_use]
pub fn provision_ownership_id(scale_set_id: i32, runner_name: &str) -> String {
    format!("prov-own-{scale_set_id}-{runner_name}")
}

/// Hex SHA-256 over the sorted label set (order-independent).
#[must_use]
pub fn labels_hash(labels: &[String]) -> String {
    let mut sorted = labels.to_vec();
    sorted.sort();
    let mut hasher = Sha256::new();
    for label in &sorted {
        hasher.update(label.len().to_be_bytes());
        hasher.update(label.as_bytes());
    }
    hex_encode(&hasher.finalize())
}

/// Hex SHA-256 fingerprint of a JIT config blob. Recorded AFTER fetch; the
/// blob itself is never persisted.
#[must_use]
pub fn jit_fingerprint(encoded_jit_config: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(encoded_jit_config.as_bytes());
    hex_encode(&hasher.finalize())
}

/// Stable non-negative i63 projection of an opaque string (the v21 demand
/// `job_id` column is `INTEGER` while the wire `jobId` is a GUID string).
#[must_use]
pub fn stable_i64(value: &str) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (i64::from_be_bytes(bytes) & i64::MAX).max(0)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Acquire-batch lifecycle in `scaleset_acquire_batches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchState {
    /// Intent persisted; `acquirejobs` not yet resolved.
    Intended,
    /// Server response reconciled (acquired + missing recorded).
    Resolved,
    /// Transport failed after send; members hold `uncertain` demand rows.
    Uncertain,
}

impl BatchState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intended => "intended",
            Self::Resolved => "resolved",
            Self::Uncertain => "uncertain",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "intended" => Ok(Self::Intended),
            "resolved" => Ok(Self::Resolved),
            "uncertain" => Ok(Self::Uncertain),
            unknown => anyhow::bail!("acquire batch holds unknown state {unknown:?}"),
        }
    }
}

/// One `scaleset_acquire_batches` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquireBatch {
    pub batch_id: String,
    pub scale_set_id: i32,
    pub request_ids: Vec<i64>,
    pub holders: Vec<String>,
    pub state: BatchState,
    pub uncertain: bool,
    pub generation: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// Result of claiming request membership before an `acquirejobs` call.
///
/// `Claimed` owns the requested batch ID. `Contended` adopts the already-open
/// batch and must not issue a second upstream call. The distinction is
/// durable: it comes from the unique request claim table, not from a process
/// mutex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireClaimOutcome {
    Claimed(AcquireBatch),
    Contended(AcquireBatch),
}

/// Durable evidence from one `acquirejobs` response.
///
/// `returned` is retained byte-for-byte as the decoded ID sequence. The
/// `unexpected` projection deliberately retains returned IDs that were not
/// requested as well as duplicate returned IDs. They are protocol anomalies,
/// not permission to mutate a different demand row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquireResponseReconciliation {
    pub requested: Vec<i64>,
    pub returned: Vec<i64>,
    pub acquired: Vec<i64>,
    pub missing: Vec<i64>,
    pub unexpected: Vec<i64>,
}

/// Reconcile one `AcquireJobs` response without inventing or dropping IDs.
///
/// This follows the upstream contract: the request body contains the exact
/// `RunnerRequestID` values from `JobAvailable`; the response is authoritative
/// only for the IDs it actually returns. Anomalies are returned to the caller
/// so it can persist evidence and fail closed before touching permits.
pub fn reconcile_returned_ids_checked(
    requested: &[i64],
    returned: &[i64],
) -> Result<AcquireResponseReconciliation> {
    let mut requested_set = HashSet::with_capacity(requested.len());
    for request_id in requested {
        if *request_id <= 0 || !requested_set.insert(*request_id) {
            anyhow::bail!(
                "acquire request contains a missing or duplicate RunnerRequestID: {request_id}"
            );
        }
    }

    let mut returned_set = HashSet::with_capacity(returned.len());
    let mut unexpected = Vec::new();
    for returned_id in returned {
        if !requested_set.contains(returned_id) || !returned_set.insert(*returned_id) {
            // Preserve order and duplicates. Diagnostics must show the exact
            // anomalous response rather than a lossy set projection.
            unexpected.push(*returned_id);
        }
    }

    let acquired = requested
        .iter()
        .copied()
        .filter(|request_id| returned_set.contains(request_id))
        .collect::<Vec<_>>();
    let missing = requested
        .iter()
        .copied()
        .filter(|request_id| !returned_set.contains(request_id))
        .collect::<Vec<_>>();

    Ok(AcquireResponseReconciliation {
        requested: requested.to_vec(),
        returned: returned.to_vec(),
        acquired,
        missing,
        unexpected,
    })
}

/// Set-reconcile projection retained for the current processor call sites.
///
/// New lifecycle code must use [`reconcile_returned_ids_checked`] so
/// anomalies remain durable evidence. This projection is intentionally only
/// the requested-ID state transition and never turns an unexpected server ID
/// into a local request.
#[must_use]
pub fn reconcile_returned_ids(requested: &[i64], returned: &[i64]) -> (Vec<i64>, Vec<i64>) {
    match reconcile_returned_ids_checked(requested, returned) {
        Ok(reconciled) => (reconciled.acquired, reconciled.missing),
        Err(error) => {
            // The old tuple API cannot represent a malformed request. Keep
            // it fail-closed for its existing callers: no ID is authorized,
            // and every requested ID remains unresolved for redelivery.
            tracing::error!(error = %error, "invalid AcquireJobs request identity");
            (Vec::new(), requested.to_vec())
        }
    }
}

/// One terminal observation bound to one upstream request attempt.
///
/// `RunnerRequestId` and `LocalCorrelationId` are both required and remain
/// separate in storage. A repeated observation for the same tuple is
/// idempotent; a conflicting result is an error rather than an overwrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalObservation {
    pub scale_set_id: i32,
    pub runner_request_id: RunnerRequestId,
    pub job_id: LocalCorrelationId,
    pub result: String,
    pub runner_id: i32,
    pub runner_name: String,
    pub generation: u64,
}

impl TerminalObservation {
    pub fn new(
        scale_set_id: i32,
        runner_request_id: i64,
        job_id: &str,
        result: &str,
        runner_id: i32,
        runner_name: &str,
        generation: u64,
    ) -> Result<Self> {
        if scale_set_id <= 0 {
            anyhow::bail!("terminal observation has invalid scale-set ID {scale_set_id}");
        }
        let runner_request_id = RunnerRequestId::new(runner_request_id)
            .context("terminal observation has no positive RunnerRequestID")?;
        let job_id = LocalCorrelationId::new(job_id)
            .context("terminal observation has no valid local job correlation")?;
        if result.is_empty() || result.chars().any(char::is_control) {
            anyhow::bail!("terminal observation has no valid result");
        }
        if runner_name.chars().any(char::is_control) {
            anyhow::bail!("terminal observation has an invalid runner name");
        }
        Ok(Self {
            scale_set_id,
            runner_request_id,
            job_id,
            result: result.to_owned(),
            runner_id,
            runner_name: runner_name.to_owned(),
            generation,
        })
    }
}

/// Durable terminal observation row, including the first observation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedTerminalObservation {
    pub observation: TerminalObservation,
    pub observed_at: String,
}

/// Durable response evidence for one acquisition attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquireResponseRecord {
    pub batch_id: String,
    pub attempt: u32,
    pub reconciliation: AcquireResponseReconciliation,
    pub observed_at: String,
}

/// Durable acquire-batch store over `scaleset_acquire_batches`.
#[derive(Debug)]
pub struct AcquireBatchStore {
    conn: Connection,
}

impl AcquireBatchStore {
    /// Open the batch store at the state database path.
    pub fn open(path: &Path) -> Result<Self> {
        velnor_control::store::Store::open(path).context("migrate acquire-batch schema")?;
        let conn = Connection::open(path).context("open acquire-batch database")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("set acquire-batch store busy timeout")?;
        Self::ensure_reconciliation_schema(&conn)?;
        let mut store = Self { conn };
        store.backfill_active_claims()?;
        Ok(store)
    }

    fn now_rfc3339() -> String {
        velnor_model::Timestamp::now()
            .to_rfc3339()
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
    }

    fn ensure_reconciliation_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS scaleset_acquire_responses (
                 batch_id TEXT NOT NULL,
                 attempt INTEGER NOT NULL,
                 requested_ids_json TEXT NOT NULL,
                 returned_ids_json TEXT NOT NULL,
                 acquired_ids_json TEXT NOT NULL,
                 missing_ids_json TEXT NOT NULL,
                 unexpected_ids_json TEXT NOT NULL,
                 observed_at TEXT NOT NULL,
                 PRIMARY KEY (batch_id, attempt)
             );
             CREATE INDEX IF NOT EXISTS idx_scaleset_acquire_responses_batch
                 ON scaleset_acquire_responses (batch_id, attempt);
             CREATE TABLE IF NOT EXISTS scaleset_terminal_observations (
                 scale_set_id INTEGER NOT NULL,
                 runner_request_id INTEGER NOT NULL,
                 job_id TEXT NOT NULL,
                 result TEXT NOT NULL,
                 runner_id INTEGER NOT NULL,
                 runner_name TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 observed_at TEXT NOT NULL,
                 PRIMARY KEY (scale_set_id, runner_request_id, job_id)
             );
             CREATE INDEX IF NOT EXISTS idx_scaleset_terminal_observations_request
                 ON scaleset_terminal_observations
                    (scale_set_id, runner_request_id, observed_at);",
        )
        .context("ensure scale-set reconciliation schema")
    }

    fn row_to_batch(row: &rusqlite::Row<'_>) -> rusqlite::Result<AcquireBatch> {
        let request_raw: String = row.get(2)?;
        let request_ids: Vec<i64> = serde_json::from_str(&request_raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, error.into())
        })?;
        let holders_raw: String = row.get(3)?;
        let holders: Vec<String> = serde_json::from_str(&holders_raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, error.into())
        })?;
        let state_raw: String = row.get(4)?;
        let state = BatchState::parse(&state_raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, error.into())
        })?;
        let generation_raw: i64 = row.get(6)?;
        Ok(AcquireBatch {
            batch_id: row.get(0)?,
            scale_set_id: row.get(1)?,
            request_ids,
            holders,
            state,
            uncertain: row.get(5)?,
            generation: generation_raw.max(0) as u64,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    }

    fn decode_ids(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Vec<i64>> {
        let raw: String = row.get(index)?;
        serde_json::from_str(&raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                error.into(),
            )
        })
    }

    fn row_to_response(row: &rusqlite::Row<'_>) -> rusqlite::Result<AcquireResponseRecord> {
        let attempt_raw: i64 = row.get(1)?;
        Ok(AcquireResponseRecord {
            batch_id: row.get(0)?,
            attempt: attempt_raw.max(0) as u32,
            reconciliation: AcquireResponseReconciliation {
                requested: Self::decode_ids(row, 2)?,
                returned: Self::decode_ids(row, 3)?,
                acquired: Self::decode_ids(row, 4)?,
                missing: Self::decode_ids(row, 5)?,
                unexpected: Self::decode_ids(row, 6)?,
            },
            observed_at: row.get(7)?,
        })
    }

    fn validate_request_ids(request_ids: &[i64], holders: &[String]) -> Result<()> {
        if request_ids.is_empty() {
            anyhow::bail!("acquire batch cannot be empty");
        }
        if request_ids.len() != holders.len() {
            anyhow::bail!(
                "acquire batch request/holder cardinality differs: {} != {}",
                request_ids.len(),
                holders.len()
            );
        }
        let mut requests = HashSet::with_capacity(request_ids.len());
        for request_id in request_ids {
            if *request_id == 0 || !requests.insert(*request_id) {
                anyhow::bail!(
                    "acquire batch contains a missing or duplicate request identity: {request_id}"
                );
            }
        }
        let mut holder_set = HashSet::with_capacity(holders.len());
        for holder in holders {
            if holder.is_empty() || !holder_set.insert(holder) {
                anyhow::bail!("acquire batch contains a missing or duplicate holder identity");
            }
        }
        Ok(())
    }

    fn backfill_active_claims(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin acquire-claim backfill")?;
        let batches: Vec<(String, i32, String, u64, String)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT batch_id, scale_set_id, request_ids_json, generation, created_at
                     FROM scaleset_acquire_batches
                     WHERE state IN ('intended', 'uncertain')",
                )
                .context("prepare acquire-claim backfill")?;
            stmt.query_map([], |row| {
                let generation: i64 = row.get(3)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    generation.max(0) as u64,
                    row.get(4)?,
                ))
            })
            .context("query acquire-claim backfill")?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (batch_id, scale_set_id, request_json, generation, created_at) in batches {
            let request_ids: Vec<i64> =
                serde_json::from_str(&request_json).context("decode acquire-claim backfill")?;
            let holders = request_ids
                .iter()
                .map(|request_id| permit_holder(scale_set_id, *request_id))
                .collect::<Vec<_>>();
            Self::validate_request_ids(&request_ids, &holders)?;
            for request_id in request_ids {
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO scaleset_acquire_claims
                     (scale_set_id, request_id, batch_id, generation, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        scale_set_id,
                        request_id,
                        batch_id,
                        i64::try_from(generation).unwrap_or(i64::MAX),
                        created_at,
                    ],
                )?;
                if inserted == 0 {
                    let owner: String = tx.query_row(
                        "SELECT batch_id FROM scaleset_acquire_claims
                         WHERE scale_set_id = ?1 AND request_id = ?2",
                        params![scale_set_id, request_id],
                        |row| row.get(0),
                    )?;
                    if owner != batch_id {
                        anyhow::bail!(
                            "request {scale_set_id}/{request_id} is claimed by both {owner:?} and {batch_id:?}"
                        );
                    }
                }
            }
        }
        tx.commit().context("commit acquire-claim backfill")?;
        Ok(())
    }

    /// Claim every request before the `acquirejobs` call. The unique request
    /// key makes concurrent processors adopt the same open batch rather than
    /// minting separate network operations.
    pub fn claim_intended(
        &mut self,
        batch_id: &str,
        scale_set_id: i32,
        request_ids: &[i64],
        holders: &[String],
        generation: u64,
    ) -> Result<AcquireClaimOutcome> {
        if batch_id.is_empty() {
            anyhow::bail!("acquire batch ID cannot be empty");
        }
        Self::validate_request_ids(request_ids, holders)?;
        let now = Self::now_rfc3339();
        let request_json =
            serde_json::to_string(request_ids).context("encode acquire request ids")?;
        let holders_json = serde_json::to_string(holders).context("encode acquire holders")?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin acquire-claim transaction")?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO scaleset_acquire_batches
                 (batch_id, scale_set_id, request_ids_json, holders_json, state,
                  uncertain, generation, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'intended', 0, ?5, ?6, ?7)",
                params![
                    batch_id,
                    scale_set_id,
                    request_json,
                    holders_json,
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    now,
                    now,
                ],
            )
            .context("record acquire intent")?;
        let recorded = tx
            .query_row(
                "SELECT batch_id, scale_set_id, request_ids_json, holders_json, state,
                        uncertain, generation, created_at, updated_at
                 FROM scaleset_acquire_batches WHERE batch_id = ?1",
                params![batch_id],
                Self::row_to_batch,
            )
            .context("read recorded acquire intent")?;
        if inserted == 0
            && (recorded.scale_set_id != scale_set_id
                || recorded.request_ids != request_ids
                || recorded.holders != holders
                || recorded.generation != generation)
        {
            anyhow::bail!("acquire batch {batch_id:?} identity or generation changed");
        }
        if matches!(recorded.state, BatchState::Resolved) {
            anyhow::bail!("acquire batch {batch_id:?} is already resolved");
        }

        for request_id in request_ids {
            let owner: Option<String> = tx
                .query_row(
                    "SELECT batch_id FROM scaleset_acquire_claims
                     WHERE scale_set_id = ?1 AND request_id = ?2",
                    params![scale_set_id, request_id],
                    |row| row.get(0),
                )
                .optional()
                .context("read acquire request claim")?;
            let Some(owner) = owner else {
                tx.execute(
                    "INSERT INTO scaleset_acquire_claims
                     (scale_set_id, request_id, batch_id, generation, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        scale_set_id,
                        request_id,
                        batch_id,
                        i64::try_from(generation).unwrap_or(i64::MAX),
                        Self::now_rfc3339(),
                    ],
                )
                .context("claim acquire request")?;
                continue;
            };
            if owner == batch_id {
                continue;
            }
            let conflict = tx
                .query_row(
                    "SELECT batch_id, scale_set_id, request_ids_json, holders_json, state,
                            uncertain, generation, created_at, updated_at
                     FROM scaleset_acquire_batches WHERE batch_id = ?1",
                    params![owner],
                    Self::row_to_batch,
                )
                .optional()
                .context("read competing acquire batch")?
                .with_context(|| format!("active acquire claim {owner:?} has no batch row"))?;
            if conflict.scale_set_id != scale_set_id
                || !matches!(conflict.state, BatchState::Intended | BatchState::Uncertain)
            {
                anyhow::bail!(
                    "request {scale_set_id}/{request_id} has an invalid active claim {owner:?}"
                );
            }
            tx.rollback().context("rollback contended acquire claim")?;
            return Ok(AcquireClaimOutcome::Contended(conflict));
        }
        tx.commit().context("commit acquire-claim transaction")?;
        Ok(AcquireClaimOutcome::Claimed(recorded))
    }

    /// Compatibility wrapper for callers that only need the durable row.
    pub fn record_intended(
        &mut self,
        batch_id: &str,
        scale_set_id: i32,
        request_ids: &[i64],
        holders: &[String],
        generation: u64,
    ) -> Result<AcquireBatch> {
        match self.claim_intended(batch_id, scale_set_id, request_ids, holders, generation)? {
            AcquireClaimOutcome::Claimed(batch) | AcquireClaimOutcome::Contended(batch) => {
                Ok(batch)
            }
        }
    }

    /// Fetch one batch by ID.
    pub fn get(&self, batch_id: &str) -> Result<Option<AcquireBatch>> {
        self.conn
            .query_row(
                "SELECT batch_id, scale_set_id, request_ids_json, holders_json, state,
                        uncertain, generation, created_at, updated_at
                 FROM scaleset_acquire_batches WHERE batch_id = ?1",
                params![batch_id],
                Self::row_to_batch,
            )
            .optional()
            .context("fetch acquire batch")
    }

    /// Resolve one batch after the `acquirejobs` round: `resolved` on a
    /// reconciled response, `uncertain` on transport failure after send.
    pub fn resolve(&mut self, batch_id: &str, uncertain: bool) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin resolve acquire batch")?;
        let current: Option<String> = tx
            .query_row(
                "SELECT state FROM scaleset_acquire_batches WHERE batch_id = ?1",
                params![batch_id],
                |row| row.get(0),
            )
            .optional()
            .context("read acquire batch before resolve")?;
        let Some(current) = current else {
            anyhow::bail!("acquire batch {batch_id:?} does not exist");
        };
        if current == BatchState::Resolved.as_str() {
            tx.commit().context("commit idempotent acquire resolve")?;
            return Ok(());
        }
        let state = if uncertain {
            BatchState::Uncertain
        } else {
            BatchState::Resolved
        };
        tx.execute(
            "UPDATE scaleset_acquire_batches
             SET state = ?1, uncertain = ?2, updated_at = ?3
             WHERE batch_id = ?4 AND state IN ('intended', 'uncertain')",
            params![
                state.as_str(),
                i32::from(uncertain),
                Self::now_rfc3339(),
                batch_id
            ],
        )
        .context("resolve acquire batch")?;
        if !uncertain {
            tx.execute(
                "DELETE FROM scaleset_acquire_claims WHERE batch_id = ?1",
                params![batch_id],
            )
            .context("release acquire request claims")?;
        }
        tx.commit().context("commit acquire batch resolution")?;
        Ok(())
    }

    /// Allocate the next durable response-attempt ordinal for one batch.
    ///
    /// A retry after an uncertain transport result is a new observation of
    /// the same upstream request batch. Keeping the ordinal prevents a later
    /// response from overwriting the first response evidence.
    pub fn next_response_attempt(&self, batch_id: &str) -> Result<u32> {
        if self.get(batch_id)?.is_none() {
            anyhow::bail!("acquire batch {batch_id:?} does not exist");
        }
        let next: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(attempt), -1) + 1
                 FROM scaleset_acquire_responses WHERE batch_id = ?1",
                params![batch_id],
                |row| row.get(0),
            )
            .context("read next acquire response attempt")?;
        u32::try_from(next).context("acquire response attempt exceeds u32")
    }

    /// Persist the exact response projection before applying any permit
    /// transition. Replaying the same `(batch, attempt)` is idempotent only
    /// when the full response is identical; a conflicting replay fails
    /// closed.
    pub fn record_response(
        &mut self,
        batch_id: &str,
        attempt: u32,
        reconciliation: &AcquireResponseReconciliation,
    ) -> Result<AcquireResponseRecord> {
        let batch = self
            .get(batch_id)?
            .with_context(|| format!("acquire batch {batch_id:?} does not exist"))?;
        if reconciliation.requested != batch.request_ids {
            anyhow::bail!(
                "acquire response {batch_id:?}/{attempt} request set differs from durable intent"
            );
        }
        let requested_json = serde_json::to_string(&reconciliation.requested)
            .context("encode acquire response requested IDs")?;
        let returned_json = serde_json::to_string(&reconciliation.returned)
            .context("encode acquire response returned IDs")?;
        let acquired_json = serde_json::to_string(&reconciliation.acquired)
            .context("encode acquire response acquired IDs")?;
        let missing_json = serde_json::to_string(&reconciliation.missing)
            .context("encode acquire response missing IDs")?;
        let unexpected_json = serde_json::to_string(&reconciliation.unexpected)
            .context("encode acquire response anomaly IDs")?;
        let observed_at = Self::now_rfc3339();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin acquire response transaction")?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO scaleset_acquire_responses
                 (batch_id, attempt, requested_ids_json, returned_ids_json,
                  acquired_ids_json, missing_ids_json, unexpected_ids_json, observed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    batch_id,
                    i64::from(attempt),
                    requested_json,
                    returned_json,
                    acquired_json,
                    missing_json,
                    unexpected_json,
                    observed_at,
                ],
            )
            .context("persist acquire response")?;
        let recorded = tx
            .query_row(
                "SELECT batch_id, attempt, requested_ids_json, returned_ids_json,
                        acquired_ids_json, missing_ids_json, unexpected_ids_json, observed_at
                 FROM scaleset_acquire_responses
                 WHERE batch_id = ?1 AND attempt = ?2",
                params![batch_id, i64::from(attempt)],
                Self::row_to_response,
            )
            .context("read persisted acquire response")?;
        if inserted == 0 && recorded.reconciliation != *reconciliation {
            anyhow::bail!("acquire response {batch_id:?}/{attempt} changed during replay");
        }
        tx.commit().context("commit acquire response")?;
        Ok(recorded)
    }

    /// Read all response attempts for one batch in wire-observation order.
    pub fn responses(&self, batch_id: &str) -> Result<Vec<AcquireResponseRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT batch_id, attempt, requested_ids_json, returned_ids_json,
                        acquired_ids_json, missing_ids_json, unexpected_ids_json, observed_at
                 FROM scaleset_acquire_responses
                 WHERE batch_id = ?1 ORDER BY attempt ASC",
            )
            .context("prepare acquire response history")?;
        stmt.query_map(params![batch_id], Self::row_to_response)
            .context("query acquire response history")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read acquire response history")
    }

    fn row_to_terminal(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecordedTerminalObservation> {
        let scale_set_id: i32 = row.get(0)?;
        let runner_request_id: i64 = row.get(1)?;
        let job_id: String = row.get(2)?;
        let result: String = row.get(3)?;
        let runner_id: i32 = row.get(4)?;
        let runner_name: String = row.get(5)?;
        let generation_raw: i64 = row.get(6)?;
        let observation = TerminalObservation::new(
            scale_set_id,
            runner_request_id,
            &job_id,
            &result,
            runner_id,
            &runner_name,
            generation_raw.max(0) as u64,
        )
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
        })?;
        Ok(RecordedTerminalObservation {
            observation,
            observed_at: row.get(7)?,
        })
    }

    /// Persist one terminal lifecycle observation under its exact
    /// `(scale_set, RunnerRequestID, jobId)` attempt key.
    ///
    /// A duplicate identical observation returns `false`; a conflicting
    /// result or runner identity is rejected and never overwrites evidence.
    pub fn record_terminal_observation(
        &mut self,
        observation: &TerminalObservation,
    ) -> Result<bool> {
        let now = Self::now_rfc3339();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin terminal observation transaction")?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO scaleset_terminal_observations
                 (scale_set_id, runner_request_id, job_id, result, runner_id,
                  runner_name, generation, observed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    observation.scale_set_id,
                    observation.runner_request_id.get(),
                    observation.job_id.as_str(),
                    observation.result,
                    observation.runner_id,
                    observation.runner_name,
                    i64::try_from(observation.generation).unwrap_or(i64::MAX),
                    now,
                ],
            )
            .context("persist terminal observation")?;
        let recorded = tx
            .query_row(
                "SELECT scale_set_id, runner_request_id, job_id, result, runner_id,
                        runner_name, generation, observed_at
                 FROM scaleset_terminal_observations
                 WHERE scale_set_id = ?1 AND runner_request_id = ?2 AND job_id = ?3",
                params![
                    observation.scale_set_id,
                    observation.runner_request_id.get(),
                    observation.job_id.as_str(),
                ],
                Self::row_to_terminal,
            )
            .context("read persisted terminal observation")?;
        if inserted == 0
            && (recorded.observation.result != observation.result
                || recorded.observation.runner_id != observation.runner_id
                || recorded.observation.runner_name != observation.runner_name)
        {
            anyhow::bail!(
                "terminal observation identity changed for scale-set {}/{} job {:?}",
                observation.scale_set_id,
                observation.runner_request_id.get(),
                observation.job_id.as_str()
            );
        }
        tx.commit().context("commit terminal observation")?;
        Ok(inserted == 1)
    }

    /// Fetch one exact attempt-qualified terminal observation.
    pub fn terminal_observation(
        &self,
        scale_set_id: i32,
        runner_request_id: RunnerRequestId,
        job_id: &LocalCorrelationId,
    ) -> Result<Option<RecordedTerminalObservation>> {
        self.conn
            .query_row(
                "SELECT scale_set_id, runner_request_id, job_id, result, runner_id,
                        runner_name, generation, observed_at
                 FROM scaleset_terminal_observations
                 WHERE scale_set_id = ?1 AND runner_request_id = ?2 AND job_id = ?3",
                params![scale_set_id, runner_request_id.get(), job_id.as_str()],
                Self::row_to_terminal,
            )
            .optional()
            .context("fetch terminal observation")
    }

    /// Batches still awaiting resolution (`intended` or `uncertain`), oldest
    /// first, bounded by `limit`. Crash recovery and idle reconcile work
    /// from this list.
    pub fn open_batches(&self, scale_set_id: i32, limit: usize) -> Result<Vec<AcquireBatch>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT batch_id, scale_set_id, request_ids_json, holders_json, state,
                        uncertain, generation, created_at, updated_at
                 FROM scaleset_acquire_batches
                 WHERE scale_set_id = ?1 AND state IN ('intended', 'uncertain')
                 ORDER BY created_at, batch_id LIMIT ?2",
            )
            .context("prepare open-batches query")?;
        let rows = stmt
            .query_map(
                params![scale_set_id, i64::try_from(limit).unwrap_or(i64::MAX)],
                Self::row_to_batch,
            )
            .context("query open batches")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("read open batches")
    }

    /// Whether any durable acquire batch has ever included `request_id`.
    ///
    /// Startup uses this to distinguish a pre-network orphan `AcquireIntent`
    /// from a request that may already have reached Actions Service. Scan
    /// historical rows too: a resolved batch is still evidence that the
    /// network call may have happened.
    pub fn contains_request(&self, scale_set_id: i32, request_id: i64) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT request_ids_json FROM scaleset_acquire_batches
                 WHERE scale_set_id = ?1",
            )
            .context("prepare acquire-batch membership query")?;
        let rows = stmt
            .query_map(params![scale_set_id], |row| row.get::<_, String>(0))
            .context("query acquire-batch memberships")?;
        for row in rows {
            let encoded = row.context("read acquire-batch membership")?;
            let request_ids: Vec<i64> =
                serde_json::from_str(&encoded).context("decode acquire-batch membership")?;
            if request_ids.contains(&request_id) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// One `scaleset_provision_intents` row: the persisted step-5 intent the
/// worker lane consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionIntent {
    pub operation_id: String,
    pub ownership_id: String,
    pub scale_set_id: i32,
    pub request_id: i64,
    pub runner_name: String,
    pub runner_digest: String,
    pub dind_digest: String,
    pub jit_fingerprint: String,
    pub generation: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// Durable provision-intent store over `scaleset_provision_intents`.
#[derive(Debug)]
pub struct ProvisionIntentStore {
    conn: Connection,
}

impl ProvisionIntentStore {
    /// Open the intent store at the state database path.
    pub fn open(path: &Path) -> Result<Self> {
        velnor_control::store::Store::open(path).context("migrate provision-intent schema")?;
        let conn = Connection::open(path).context("open provision-intent database")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("set provision-intent store busy timeout")?;
        Ok(Self { conn })
    }

    fn now_rfc3339() -> String {
        velnor_model::Timestamp::now()
            .to_rfc3339()
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
    }

    fn row_to_intent(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProvisionIntent> {
        let generation_raw: i64 = row.get(8)?;
        Ok(ProvisionIntent {
            operation_id: row.get(0)?,
            ownership_id: row.get(1)?,
            scale_set_id: row.get(2)?,
            request_id: row.get(3)?,
            runner_name: row.get(4)?,
            runner_digest: row.get(5)?,
            dind_digest: row.get(6)?,
            jit_fingerprint: row.get(7)?,
            generation: generation_raw.max(0) as u64,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    }

    /// Persist the provision intent BEFORE Docker calls. Idempotent on
    /// `operation_id`: retries adopt the recorded row.
    #[allow(clippy::too_many_arguments, reason = "intent row, one call site")]
    pub fn record_intent(
        &mut self,
        operation_id: &str,
        ownership_id: &str,
        scale_set_id: i32,
        request_id: i64,
        runner_name: &str,
        runner_digest: &str,
        dind_digest: &str,
        generation: u64,
    ) -> Result<ProvisionIntent> {
        let now = Self::now_rfc3339();
        self.conn
            .execute(
                "INSERT OR IGNORE INTO scaleset_provision_intents
                 (operation_id, ownership_id, scale_set_id, request_id, runner_name,
                  runner_digest, dind_digest, jit_fingerprint, generation, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '', ?8, ?9, ?10)",
                params![
                    operation_id,
                    ownership_id,
                    scale_set_id,
                    request_id,
                    runner_name,
                    runner_digest,
                    dind_digest,
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    now,
                    now,
                ],
            )
            .context("record provision intent")?;
        self.get(operation_id)?
            .with_context(|| format!("provision intent {operation_id:?} vanished after insert"))
    }

    /// Fetch one intent by operation ID.
    pub fn get(&self, operation_id: &str) -> Result<Option<ProvisionIntent>> {
        self.conn
            .query_row(
                "SELECT operation_id, ownership_id, scale_set_id, request_id, runner_name,
                        runner_digest, dind_digest, jit_fingerprint, generation, created_at, updated_at
                 FROM scaleset_provision_intents WHERE operation_id = ?1",
                params![operation_id],
                Self::row_to_intent,
            )
            .optional()
            .context("fetch provision intent")
    }

    /// Fetch the intent bound to one acquired request, if any.
    pub fn get_by_request(
        &self,
        scale_set_id: i32,
        request_id: i64,
    ) -> Result<Option<ProvisionIntent>> {
        self.conn
            .query_row(
                "SELECT operation_id, ownership_id, scale_set_id, request_id, runner_name,
                        runner_digest, dind_digest, jit_fingerprint, generation, created_at, updated_at
                 FROM scaleset_provision_intents
                 WHERE scale_set_id = ?1 AND request_id = ?2
                 ORDER BY created_at DESC LIMIT 1",
                params![scale_set_id, request_id],
                Self::row_to_intent,
            )
            .optional()
            .context("fetch provision intent by request")
    }

    /// List every intent for one scale set, oldest first. Crash recovery
    /// adopts or fails each recorded worker from this list.
    pub fn list_for_set(&self, scale_set_id: i32) -> Result<Vec<ProvisionIntent>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT operation_id, ownership_id, scale_set_id, request_id, runner_name,
                        runner_digest, dind_digest, jit_fingerprint, generation, created_at, updated_at
                 FROM scaleset_provision_intents
                 WHERE scale_set_id = ?1 ORDER BY created_at ASC",
            )
            .context("list provision intents")?;
        stmt.query_map(params![scale_set_id], Self::row_to_intent)
            .context("list provision intents")?
            .collect::<Result<Vec<_>, _>>()
            .context("list provision intents")
    }

    /// Record the JIT fingerprint after the worker lane fetches the config.
    /// The blob itself is never persisted — only this fingerprint.
    pub fn record_jit_fingerprint(&mut self, operation_id: &str, fingerprint: &str) -> Result<()> {
        let now = Self::now_rfc3339();
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_provision_intents
                 SET jit_fingerprint = ?1, updated_at = ?2 WHERE operation_id = ?3",
                params![fingerprint, now, operation_id],
            )
            .context("record JIT fingerprint")?;
        if updated == 0 {
            anyhow::bail!("provision intent {operation_id:?} does not exist");
        }
        Ok(())
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

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-scaleset-intents-{name}-{}",
            std::process::id()
        ));
        // Drop stale state from pid-reusing earlier runs: every test starts
        // from an empty database, deterministically.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.db")
    }

    #[test]
    fn holder_round_trips() {
        let holder = permit_holder(7, 4242);
        assert_eq!(holder, "scaleset/7/4242");
        assert_eq!(parse_permit_holder(&holder).unwrap(), (7, 4242));
        assert!(parse_permit_holder("native:abc").is_err());
        assert!(parse_permit_holder("scaleset/7").is_err());
    }

    #[test]
    fn provision_keys_are_stable_per_attempt() {
        assert_eq!(runner_name(7, 4242), "velnor-7-4242");
        assert_eq!(parse_runner_name("velnor-7-4242"), Some((7, 4242)));
        assert_eq!(request_id_for_runner(7, "velnor-7-4242", 99), Some(4242));
        assert_eq!(request_id_for_runner(7, "", 99), Some(99));
        assert_eq!(request_id_for_runner(7, "velnor-8-4242", 99), None);
        assert_eq!(request_id_for_runner(7, "runner-7-4242", 99), None);
        assert_eq!(parse_runner_name("velnor-0-4242"), None);
        assert_eq!(parse_runner_name("velnor-7-0"), None);
        for alias in [
            "velnor-07-4242",
            "velnor-7-04242",
            "velnor-+7-4242",
            "velnor-7-+4242",
            "velnor--7-4242",
            "velnor-7--4242",
        ] {
            assert_eq!(parse_runner_name(alias), None, "noncanonical alias {alias}");
        }
        assert_eq!(provision_operation_id(7, 4242, 0), "prov-op-7-4242-0");
        assert_ne!(
            provision_operation_id(7, 4242, 0),
            provision_operation_id(7, 4242, 1)
        );
        assert_eq!(
            provision_ownership_id(7, "velnor-7-4242"),
            "prov-own-7-velnor-7-4242"
        );
    }

    #[test]
    fn fingerprints_are_stable_and_order_free() {
        let labels = vec!["b".to_owned(), "a".to_owned()];
        assert_eq!(labels_hash(&labels), labels_hash(&labels));
        assert_eq!(
            labels_hash(&labels),
            labels_hash(&["a".to_owned(), "b".to_owned()])
        );
        assert_ne!(labels_hash(&labels), labels_hash(&["a".to_owned()]));
        assert!(stable_i64("job-guid") >= 0);
        assert_eq!(stable_i64("job-guid"), stable_i64("job-guid"));
        assert_eq!(jit_fingerprint("blob").len(), 64);
    }

    #[test]
    fn returned_id_reconcile_never_assumes_full_grant() {
        let (acquired, missing) = reconcile_returned_ids(&[1, 2, 3], &[2, 9]);
        assert_eq!(acquired, vec![2]);
        assert_eq!(missing, vec![1, 3]);
        let (acquired, missing) = reconcile_returned_ids(&[1], &[]);
        assert!(acquired.is_empty());
        assert_eq!(missing, vec![1]);
    }

    #[test]
    fn checked_response_preserves_anomalous_ids_without_authorizing_them() {
        let response = reconcile_returned_ids_checked(&[1, 2, 3], &[2, 9, 2, 0]).unwrap();
        assert_eq!(response.requested, vec![1, 2, 3]);
        assert_eq!(response.returned, vec![2, 9, 2, 0]);
        assert_eq!(response.acquired, vec![2]);
        assert_eq!(response.missing, vec![1, 3]);
        assert_eq!(response.unexpected, vec![9, 2, 0]);
        assert!(reconcile_returned_ids_checked(&[1, 1], &[1]).is_err());
        assert!(reconcile_returned_ids_checked(&[0], &[]).is_err());
    }

    #[test]
    fn acquire_intent_round_trips_and_resolves() {
        let path = temp_path("batch");
        let mut store = AcquireBatchStore::open(&path).unwrap();
        let holders = vec!["scaleset/7/1".to_owned(), "scaleset/7/2".to_owned()];
        let batch = store
            .record_intended("acq-1", 7, &[1, 2], &holders, 4)
            .unwrap();
        assert_eq!(batch.state, BatchState::Intended);
        assert!(!batch.uncertain);
        // Redelivered intent adopts the recorded row.
        let again = store
            .record_intended("acq-1", 7, &[1, 2], &holders, 4)
            .unwrap();
        assert_eq!(again, batch);
        assert_eq!(store.open_batches(7, 10).unwrap().len(), 1);
        store.resolve("acq-1", false).unwrap();
        assert_eq!(
            store.get("acq-1").unwrap().unwrap().state,
            BatchState::Resolved
        );
        assert!(store.open_batches(7, 10).unwrap().is_empty());
    }

    #[test]
    fn concurrent_processors_adopt_one_request_claim() {
        let path = temp_path("claim-race");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let left_path = path.clone();
        let left_barrier = barrier.clone();
        let left = std::thread::spawn(move || {
            let mut store = AcquireBatchStore::open(&left_path).unwrap();
            left_barrier.wait();
            store
                .claim_intended("acq-left", 7, &[42], &["scaleset/7/42".to_owned()], 4)
                .unwrap()
        });
        let right_path = path.clone();
        let right_barrier = barrier.clone();
        let right = std::thread::spawn(move || {
            let mut store = AcquireBatchStore::open(&right_path).unwrap();
            right_barrier.wait();
            store
                .claim_intended("acq-right", 7, &[42], &["scaleset/7/42".to_owned()], 4)
                .unwrap()
        });
        let outcomes = [left.join().unwrap(), right.join().unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, AcquireClaimOutcome::Claimed(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, AcquireClaimOutcome::Contended(_)))
                .count(),
            1
        );
        let mut store = AcquireBatchStore::open(&path).unwrap();
        assert_eq!(store.open_batches(7, 10).unwrap().len(), 1);
        let owner = outcomes
            .iter()
            .find_map(|outcome| match outcome {
                AcquireClaimOutcome::Claimed(batch) | AcquireClaimOutcome::Contended(batch) => {
                    Some(batch.batch_id.clone())
                }
            })
            .unwrap();
        store.resolve(&owner, false).unwrap();
        assert!(matches!(
            store
                .claim_intended("acq-after", 7, &[42], &["scaleset/7/42".to_owned()], 4,)
                .unwrap(),
            AcquireClaimOutcome::Claimed(_)
        ));
    }

    #[test]
    fn duplicate_request_membership_fails_closed() {
        let path = temp_path("claim-duplicate");
        let mut store = AcquireBatchStore::open(&path).unwrap();
        let error = store
            .claim_intended(
                "acq-duplicate",
                7,
                &[42, 42],
                &["scaleset/7/42".to_owned(), "scaleset/7/42".to_owned()],
                4,
            )
            .unwrap_err();
        assert!(error.to_string().contains("duplicate request identity"));
    }

    #[test]
    fn provision_intent_is_idempotent_on_operation() {
        let path = temp_path("provision");
        let mut store = ProvisionIntentStore::open(&path).unwrap();
        let first = store
            .record_intent(
                "prov-op-7-9-0",
                "prov-own-7-velnor-7-9",
                7,
                9,
                "velnor-7-9",
                "sha256:runner",
                "sha256:dind",
                4,
            )
            .unwrap();
        let second = store
            .record_intent(
                "prov-op-7-9-0",
                "prov-own-7-velnor-7-9",
                7,
                9,
                "velnor-7-9",
                "sha256:runner",
                "sha256:dind",
                4,
            )
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(
            store.get_by_request(7, 9).unwrap().unwrap().operation_id,
            "prov-op-7-9-0"
        );
    }

    #[test]
    fn response_evidence_is_durable_and_attempt_ordered() {
        let path = temp_path("response-evidence");
        let mut store = AcquireBatchStore::open(&path).unwrap();
        store
            .record_intended(
                "acq-response",
                7,
                &[31, 32],
                &["scaleset/7/31".to_owned(), "scaleset/7/32".to_owned()],
                4,
            )
            .unwrap();
        let first = reconcile_returned_ids_checked(&[31, 32], &[31, 999]).unwrap();
        let recorded = store.record_response("acq-response", 0, &first).unwrap();
        assert_eq!(recorded.attempt, 0);
        assert_eq!(recorded.reconciliation.unexpected, vec![999]);
        assert_eq!(store.next_response_attempt("acq-response").unwrap(), 1);

        // The exact same response/attempt is idempotent.
        assert_eq!(
            store.record_response("acq-response", 0, &first).unwrap(),
            recorded
        );
        let second = reconcile_returned_ids_checked(&[31, 32], &[32]).unwrap();
        store.record_response("acq-response", 1, &second).unwrap();
        let history = store.responses("acq-response").unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].reconciliation.returned, vec![31, 999]);
        assert_eq!(history[1].reconciliation.acquired, vec![32]);
        assert_eq!(history[1].reconciliation.missing, vec![31]);

        // A replay with changed evidence cannot overwrite the first attempt.
        let conflict = reconcile_returned_ids_checked(&[31, 32], &[32]).unwrap();
        assert!(store.record_response("acq-response", 0, &conflict).is_err());
    }

    #[test]
    fn terminal_observation_is_attempt_qualified_and_idempotent() {
        let path = temp_path("terminal-observation");
        let mut store = AcquireBatchStore::open(&path).unwrap();
        let observation =
            TerminalObservation::new(7, 77, "job-guid-77", "Canceled", 11, "velnor-7-77", 4)
                .unwrap();
        assert!(store.record_terminal_observation(&observation).unwrap());
        assert!(!store.record_terminal_observation(&observation).unwrap());
        let recorded = store
            .terminal_observation(7, observation.runner_request_id, &observation.job_id)
            .unwrap()
            .unwrap();
        assert_eq!(recorded.observation, observation);

        let conflict =
            TerminalObservation::new(7, 77, "job-guid-77", "Succeeded", 11, "velnor-7-77", 5)
                .unwrap();
        assert!(store.record_terminal_observation(&conflict).is_err());
        assert!(TerminalObservation::new(7, 0, "job-guid-77", "Canceled", 11, "", 4).is_err());
        assert!(TerminalObservation::new(7, 78, "", "Canceled", 11, "", 4).is_err());
    }
}

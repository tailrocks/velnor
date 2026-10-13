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

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

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

/// Parse `(scale_set_id, request_id)` from a runner name formatted like `velnor-{scale_set_id}-{request_id}`.
#[must_use]
pub fn parse_runner_name(name: &str) -> Option<(i32, i64)> {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() == 3 && parts[0] == "velnor" {
        let set_id = parts[1].parse::<i32>().ok()?;
        let req_id = parts[2].parse::<i64>().ok()?;
        Some((set_id, req_id))
    } else {
        None
    }
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

/// Set-reconcile of one `acquirejobs` round: `acquired = returned ∩
/// requested`, `missing = requested − returned`. Never assume the server
/// granted everything; extras outside the request set are ignored (they
/// address work this adapter never reserved for).
#[must_use]
pub fn reconcile_returned_ids(requested: &[i64], returned: &[i64]) -> (Vec<i64>, Vec<i64>) {
    let mut acquired = Vec::new();
    let mut missing = Vec::new();
    for request in requested {
        if returned.contains(request) {
            acquired.push(*request);
        } else {
            missing.push(*request);
        }
    }
    (acquired, missing)
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
        Ok(Self { conn })
    }

    fn now_rfc3339() -> String {
        velnor_model::Timestamp::now()
            .to_rfc3339()
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
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

    /// Persist the acquire intent BEFORE the `acquirejobs` call. Idempotent
    /// on `batch_id`: a redelivered intent returns the recorded row.
    pub fn record_intended(
        &mut self,
        batch_id: &str,
        scale_set_id: i32,
        request_ids: &[i64],
        holders: &[String],
        generation: u64,
    ) -> Result<AcquireBatch> {
        let now = Self::now_rfc3339();
        let request_json =
            serde_json::to_string(request_ids).context("encode acquire request ids")?;
        let holders_json = serde_json::to_string(holders).context("encode acquire holders")?;
        self.conn
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
        self.get(batch_id)?
            .with_context(|| format!("acquire intent {batch_id:?} vanished after insert"))
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
        let now = Self::now_rfc3339();
        let state = if uncertain {
            BatchState::Uncertain
        } else {
            BatchState::Resolved
        };
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_acquire_batches
                 SET state = ?1, uncertain = ?2, updated_at = ?3 WHERE batch_id = ?4",
                params![state.as_str(), i32::from(uncertain), now, batch_id],
            )
            .context("resolve acquire batch")?;
        if updated == 0 {
            anyhow::bail!("acquire batch {batch_id:?} does not exist");
        }
        Ok(())
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
}

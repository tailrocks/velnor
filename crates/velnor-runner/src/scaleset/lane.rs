//! Production [`WorkerLane`][crate::scaleset::converge::WorkerLane]: JIT
//! fetch + provision + supervision + owned cleanup.
//!
//! This is the daemon's half of the §5.2 lifecycle. The loop ([`Processor`]
//! step 5, step 1 observations) decides *what*; this lane does the Docker
//! work and keeps the durable worker registry truthful:
//!
//! * `provision`: fetch the JIT config over the admin client, record its
//!   fingerprint (never the blob), then [`provision_worker`] (adopt by
//!   ownership labels on retry — never a second pair).
//! * `note_assigned` / `note_started`: adopt the tracked worker when this
//!   is a restarted process, tick supervision, advance the record.
//! * `note_terminal`: drive the worker `terminal → diagnostic_export →
//!   owned_cleanup`, then release the permit — or hold it `uncertain`
//!   when cleanup fails. A failed cleanup vetoes the ACK so the message
//!   redelivers until cleanup confirms. Replay-safe: a second call
//!   converges instead of double-freeing.
//! * [`DaemonWorkerLane::adopt_live_workers`] (startup) and
//!   [`DaemonWorkerLane::shutdown_pass`] (graceful stop): adopt-or-fail
//!   every recorded worker. Live work survives the restart (containers
//!   keep running, rows keep their states); dead work is explicitly
//!   failed with diagnostics + cleanup. Never a lost job (every intent
//!   resolves) and never a false success (only a GitHub `JobCompleted`
//!   observation marks demand terminal — the lane never writes demand).
//!
//! The lane is synchronous (the [`WorkerLane`][wl] seam is sync) while the
//! JIT fetch is async: the fetch runs on a scoped thread driving the
//! daemon runtime handle, which works on any runtime flavor. Docker and
//! SQLite calls are blocking already.
//!
//! [`Processor`]: crate::scaleset::scale::Processor
//! [wl]: crate::scaleset::converge::WorkerLane

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use velnor_model::{
    RunnerScaleSetJitRunnerSetting, ScaleSetJobAssigned, ScaleSetJobCompleted, ScaleSetJobStarted,
    ScaleSetWorkerState,
};

use crate::scaleset::converge::WorkerLane;
use crate::scaleset::demand::{DemandState, DemandStore, StagedAttemptRelease};
use crate::scaleset::errors::ScaleSetFault;
use crate::scaleset::intents::{
    jit_fingerprint, permit_holder, ProvisionIntent, ProvisionIntentStore,
};
use crate::scaleset::reconcile::PERMIT_STATES;
use crate::scaleset::shared_ledger::SharedLedger;
use crate::scaleset::worker::runner::RUNNER_WORK_DIR;
use crate::scaleset::worker::{
    provision_worker, EdgeSink, HomogeneousProfile, OwnershipId, ProvisionPlan, RunnerConnection,
    ScaleSetWorker, Supervision, SupervisionOutcome, ToolContentHook, VecEdgeSink, WorkerEdge,
    WorkerIdentity, WorkerRunner,
};
use crate::scaleset::{CapacityLedger, LedgerPermitState, ScaleSetClient};

/// One `scaleset_workers` row: the durable side of [`ScaleSetWorker`].
#[derive(Debug, Clone)]
pub struct WorkerRow {
    pub ownership_id: String,
    pub operation_id: String,
    pub request_id: Option<i64>,
    pub runner_name: String,
    pub network_name: Option<String>,
    pub workspace_path: Option<String>,
    pub dind_data_path: Option<String>,
    pub runner_digest: String,
    pub dind_digest: String,
    pub worker_state: ScaleSetWorkerState,
    pub generation: u64,
    pub permit_attempt_token: Option<String>,
    pub runner_start_deadline_epoch: Option<u64>,
    pub dind_restarts_used: u32,
    pub diagnostics_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StagedAttemptRotation {
    holder: String,
    scale_set_id: i32,
    request_id: i64,
    ownership_id: String,
    old_attempt_token: Option<String>,
    new_attempt_token: String,
    previous_pid: Option<u32>,
}

/// Parse a stored lifecycle value. Unknown values fail closed: a worker
/// whose state cannot be read is failed explicitly, never guessed.
fn parse_worker_state(raw: &str) -> Result<ScaleSetWorkerState> {
    use ScaleSetWorkerState as S;
    match raw {
        "observed" => Ok(S::Observed),
        "eligible" => Ok(S::Eligible),
        "reserved" => Ok(S::Reserved),
        "acquire_intent" => Ok(S::AcquireIntent),
        "acquired" => Ok(S::Acquired),
        "uncertain" => Ok(S::Uncertain),
        "provision_intent" => Ok(S::ProvisionIntent),
        "dind_ready" => Ok(S::DindReady),
        "runner_connected" => Ok(S::RunnerConnected),
        "running" => Ok(S::Running),
        "terminal" => Ok(S::Terminal),
        "diagnostic_export" => Ok(S::DiagnosticExport),
        "owned_cleanup" => Ok(S::OwnedCleanup),
        "permit_released" => Ok(S::PermitReleased),
        other => anyhow::bail!("unknown scale-set worker state {other:?}"),
    }
}

/// Durable worker registry over `scaleset_workers`.
///
/// Opens its own connection to the state database; [`Store::open`][store]
/// runs first so the v21 schema is guaranteed. The registry key is the
/// canonical ownership string (`<set>/<runner-name>` from [`OwnershipId`]);
/// the provision intent's `prov-own-*` key is the d1c idempotency
/// namespace and is translated on the way in, never stored here.
///
/// [store]: velnor_control::store::Store::open
#[derive(Debug)]
pub struct WorkerRegistry {
    conn: Connection,
    generation: u64,
}

impl WorkerRegistry {
    /// Open the registry at the state database path.
    pub fn open(path: &Path) -> Result<Self> {
        velnor_control::store::Store::open(path).context("migrate worker registry schema")?;
        let conn = Connection::open(path).context("open worker registry database")?;
        conn.busy_timeout(Duration::from_secs(5))
            .context("set worker registry busy timeout")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS scaleset_worker_runtime (
                 ownership_id TEXT PRIMARY KEY,
                 runner_start_deadline_epoch INTEGER,
                 dind_restarts_used INTEGER NOT NULL DEFAULT 0 CHECK (dind_restarts_used >= 0),
                 diagnostics_complete INTEGER NOT NULL DEFAULT 0
                     CHECK (diagnostics_complete IN (0, 1))
             );
             INSERT OR IGNORE INTO scaleset_worker_runtime
                 (ownership_id, runner_start_deadline_epoch, dind_restarts_used,
                  diagnostics_complete)
             SELECT ownership_id, NULL, 0, 0
             FROM scaleset_workers;",
        )
        .context("create durable scale-set runtime table")?;
        Ok(Self {
            conn,
            generation: 0,
        })
    }

    /// Generation stamped on subsequent edge writes. The lane refreshes
    /// this from its ledger handle on every entry.
    pub fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    fn now_rfc3339() -> String {
        velnor_model::Timestamp::now()
            .to_rfc3339()
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
    }

    fn row_to_worker(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkerRow> {
        let state_raw: String = row.get(11)?;
        let worker_state = parse_worker_state(&state_raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(11, rusqlite::types::Type::Text, error.into())
        })?;
        let generation_raw: i64 = row.get(12)?;
        Ok(WorkerRow {
            ownership_id: row.get(0)?,
            operation_id: row.get(1)?,
            request_id: row.get(2)?,
            runner_name: row.get(3)?,
            network_name: row.get(6)?,
            workspace_path: row.get(7)?,
            dind_data_path: row.get(8)?,
            runner_digest: row.get(9)?,
            dind_digest: row.get(10)?,
            worker_state,
            generation: generation_raw.max(0) as u64,
            runner_start_deadline_epoch: row
                .get::<_, Option<i64>>(15)?
                .map(|seconds| seconds.max(0) as u64),
            dind_restarts_used: row.get::<_, i64>(16)?.clamp(0, u32::MAX as i64) as u32,
            diagnostics_complete: row.get::<_, i64>(17)? != 0,
            permit_attempt_token: row.get(18)?,
        })
    }

    /// Record a worker BEFORE its first Docker call. Replays keep the
    /// recorded lifecycle state (never reset backwards) but refresh the
    /// operation id, so a retry under a new attempt adopts the row.
    #[allow(clippy::too_many_arguments, reason = "registry row, few call sites")]
    pub fn upsert(
        &mut self,
        ownership_id: &str,
        operation_id: &str,
        request_id: i64,
        runner_name: &str,
        network_name: &str,
        workspace_path: &str,
        dind_data_path: &str,
        runner_digest: &str,
        dind_digest: &str,
        attempt_token: &str,
    ) -> Result<WorkerRow> {
        if attempt_token.is_empty() {
            anyhow::bail!("worker permit attempt token cannot be empty");
        }
        if let Some(existing) = self.get(ownership_id)?
            && existing.permit_attempt_token.as_deref() != Some(attempt_token)
        {
            anyhow::bail!(
                "worker {ownership_id:?} belongs to a different or tokenless permit attempt"
            );
        }
        let now = Self::now_rfc3339();
        let generation = i64::try_from(self.generation).unwrap_or(i64::MAX);
        self.conn
            .execute(
                "INSERT INTO scaleset_workers
                 (ownership_id, operation_id, request_id, runner_name, network_name,
                  workspace_path, dind_data_path, runner_digest, dind_digest,
                  worker_state, generation, created_at, updated_at, permit_attempt_token)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'observed', ?10, ?11, ?11, ?12)
                 ON CONFLICT(ownership_id) DO UPDATE SET
                   operation_id = excluded.operation_id,
                   request_id = excluded.request_id,
                   generation = excluded.generation,
                   updated_at = excluded.updated_at",
                params![
                    ownership_id,
                    operation_id,
                    request_id,
                    runner_name,
                    network_name,
                    workspace_path,
                    dind_data_path,
                    runner_digest,
                    dind_digest,
                    generation,
                    now,
                    attempt_token,
                ],
            )
            .context("upsert worker registry row")?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO scaleset_worker_runtime
                 (ownership_id, runner_start_deadline_epoch, dind_restarts_used,
                  diagnostics_complete) VALUES (?1, NULL, 0, 0)",
                params![ownership_id],
            )
            .context("ensure worker runtime row")?;
        let row = self
            .get(ownership_id)?
            .with_context(|| format!("worker row {ownership_id:?} vanished after upsert"))?;
        Ok(row)
    }

    /// Fetch one worker by canonical ownership id.
    pub fn get(&self, ownership_id: &str) -> Result<Option<WorkerRow>> {
        self.conn
            .query_row(
                "SELECT ownership_id, operation_id, request_id, runner_name,
                        runner_container_id, dind_container_id, network_name, workspace_path,
                        dind_data_path, runner_digest, dind_digest, worker_state,
                        generation, created_at, updated_at,
                        r.runner_start_deadline_epoch, COALESCE(r.dind_restarts_used, 0),
                        COALESCE(r.diagnostics_complete, 0), w.permit_attempt_token
                 FROM scaleset_workers w
                 LEFT JOIN scaleset_worker_runtime r USING (ownership_id)
                 WHERE w.ownership_id = ?1",
                params![ownership_id],
                Self::row_to_worker,
            )
            .optional()
            .context("fetch worker row")
    }

    /// Fetch the worker bound to one acquired request, if any.
    pub fn get_by_request(&self, request_id: i64) -> Result<Option<WorkerRow>> {
        self.conn
            .query_row(
                "SELECT ownership_id, operation_id, request_id, runner_name,
                        runner_container_id, dind_container_id, network_name, workspace_path,
                        dind_data_path, runner_digest, dind_digest, worker_state,
                        generation, created_at, updated_at,
                        r.runner_start_deadline_epoch, COALESCE(r.dind_restarts_used, 0),
                        COALESCE(r.diagnostics_complete, 0), w.permit_attempt_token
                 FROM scaleset_workers w
                 LEFT JOIN scaleset_worker_runtime r USING (ownership_id)
                 WHERE w.request_id = ?1 LIMIT 1",
                params![request_id],
                Self::row_to_worker,
            )
            .optional()
            .context("fetch worker row by request")
    }

    /// Every worker that has not released its permit, oldest first.
    /// Adoption and shutdown iterate this list; nothing live hides from it.
    pub fn list_live(&self) -> Result<Vec<WorkerRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ownership_id, operation_id, request_id, runner_name,
                        runner_container_id, dind_container_id, network_name, workspace_path,
                        dind_data_path, runner_digest, dind_digest, worker_state,
                        generation, created_at, updated_at,
                        r.runner_start_deadline_epoch, COALESCE(r.dind_restarts_used, 0),
                        COALESCE(r.diagnostics_complete, 0), w.permit_attempt_token
                 FROM scaleset_workers w
                 LEFT JOIN scaleset_worker_runtime r USING (ownership_id)
                 WHERE w.worker_state != 'permit_released' ORDER BY w.created_at ASC",
            )
            .context("list live workers")?;
        stmt.query_map([], Self::row_to_worker)
            .context("list live workers")?
            .collect::<Result<Vec<_>, _>>()
            .context("list live workers")
    }

    /// Persist one lifecycle state (the [`EdgeSink`] write path).
    pub fn set_state(
        &mut self,
        ownership_id: &str,
        state: ScaleSetWorkerState,
        attempt_token: &str,
    ) -> Result<()> {
        let now = Self::now_rfc3339();
        let generation = i64::try_from(self.generation).unwrap_or(i64::MAX);
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_workers
                 SET worker_state = ?1, generation = ?2, updated_at = ?3
                 WHERE ownership_id = ?4 AND permit_attempt_token = ?5",
                params![state.as_str(), generation, now, ownership_id, attempt_token],
            )
            .context("record worker edge")?;
        if updated == 0 {
            anyhow::bail!(
                "worker registry holds no row for {ownership_id:?} and this attempt token"
            );
        }
        Ok(())
    }

    /// Persist a target token before changing the separate host permit
    /// ledger. A repeated stage for the same holder is idempotent; any
    /// changed identity or previous token fails closed.
    fn stage_attempt_rotation(
        &mut self,
        holder: &str,
        ownership_id: &str,
        scale_set_id: i32,
        request_id: i64,
        previous_pid: u32,
        old_token: Option<&str>,
    ) -> Result<StagedAttemptRotation> {
        if previous_pid == 0 || old_token.is_some_and(str::is_empty) {
            anyhow::bail!("staged attempt requires an exact prior pid and valid prior token");
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin staged scale-set attempt rotation")?;
        let existing: Option<StagedAttemptRotation> = tx
            .query_row(
                "SELECT rotation.holder, rotation.scale_set_id, rotation.request_id,
                        rotation.ownership_id, rotation.old_attempt_token,
                        rotation.new_attempt_token, prior.previous_pid
                 FROM scaleset_attempt_rotations AS rotation
                 LEFT JOIN scaleset_attempt_rotation_priors AS prior USING (holder)
                 WHERE rotation.holder = ?1",
                params![holder],
                |row| {
                    let previous_pid: Option<i64> = row.get(6)?;
                    Ok(StagedAttemptRotation {
                        holder: row.get(0)?,
                        scale_set_id: row.get(1)?,
                        request_id: row.get(2)?,
                        ownership_id: row.get(3)?,
                        old_attempt_token: row.get(4)?,
                        new_attempt_token: row.get(5)?,
                        previous_pid: previous_pid.and_then(|pid| u32::try_from(pid).ok()),
                    })
                },
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing.scale_set_id != scale_set_id
                || existing.request_id != request_id
                || existing.ownership_id != ownership_id
                || existing.old_attempt_token.as_deref() != old_token
            {
                anyhow::bail!("pending attempt rotation for {holder:?} has another identity");
            }
            if existing.previous_pid.is_none() {
                anyhow::bail!(
                    "pending attempt rotation for {holder:?} has no persisted prior pid; refusing ambiguous recovery"
                );
            }
            tx.commit().context("confirm staged attempt rotation")?;
            return Ok(existing);
        }
        let rotation = StagedAttemptRotation {
            holder: holder.to_owned(),
            scale_set_id,
            request_id,
            ownership_id: ownership_id.to_owned(),
            old_attempt_token: old_token.map(str::to_owned),
            new_attempt_token: uuid::Uuid::new_v4().to_string(),
            previous_pid: Some(previous_pid),
        };
        tx.execute(
            "INSERT INTO scaleset_attempt_rotations
             (holder, scale_set_id, request_id, ownership_id, old_attempt_token,
              new_attempt_token, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                rotation.holder,
                rotation.scale_set_id,
                rotation.request_id,
                rotation.ownership_id,
                rotation.old_attempt_token,
                rotation.new_attempt_token,
                Self::now_rfc3339(),
            ],
        )
        .context("persist staged attempt rotation")?;
        tx.execute(
            "INSERT INTO scaleset_attempt_rotation_priors
             (holder, previous_pid, created_at) VALUES (?1, ?2, ?3)",
            params![
                rotation.holder,
                i64::from(previous_pid),
                Self::now_rfc3339(),
            ],
        )
        .context("persist staged attempt prior pid")?;
        tx.commit().context("commit staged attempt rotation")?;
        Ok(rotation)
    }

    fn pending_attempt_rotations(&self) -> Result<Vec<StagedAttemptRotation>> {
        let mut stmt = self.conn.prepare(
            "SELECT rotation.holder, rotation.scale_set_id, rotation.request_id,
                    rotation.ownership_id, rotation.old_attempt_token,
                    rotation.new_attempt_token, prior.previous_pid
             FROM scaleset_attempt_rotations AS rotation
             LEFT JOIN scaleset_attempt_rotation_priors AS prior USING (holder)
             ORDER BY rotation.scale_set_id, rotation.request_id",
        )?;
        stmt.query_map([], |row| {
            let previous_pid: Option<i64> = row.get(6)?;
            Ok(StagedAttemptRotation {
                holder: row.get(0)?,
                scale_set_id: row.get(1)?,
                request_id: row.get(2)?,
                ownership_id: row.get(3)?,
                old_attempt_token: row.get(4)?,
                new_attempt_token: row.get(5)?,
                previous_pid: previous_pid.and_then(|pid| u32::try_from(pid).ok()),
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("list staged attempt rotations")
    }

    /// Finish the state-database half of staged rotations atomically. Open
    /// batch tokens are rebuilt only from persisted demand tokens. Tokenless
    /// members are accepted only after the lane proves they are terminal and
    /// have no permit row.
    fn finish_staged_attempt_rotations(
        &mut self,
        mut validate_batch_member: impl FnMut(i32, i64, Option<&str>, bool) -> Result<()>,
    ) -> Result<()> {
        let stages = self.pending_attempt_rotations()?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin staged attempt completion")?;
        let now = Self::now_rfc3339();
        for stage in &stages {
            let current: Option<String> = tx.query_row(
                "SELECT permit_attempt_token FROM scaleset_demand
                 WHERE request_id = ?1 AND scale_set_id = ?2",
                params![stage.request_id, stage.scale_set_id],
                |row| row.get(0),
            )?;
            if current.as_deref() != Some(stage.new_attempt_token.as_str()) {
                if current.as_deref() != stage.old_attempt_token.as_deref() {
                    anyhow::bail!(
                        "demand token for {} changed during staged recovery",
                        stage.holder
                    );
                }
                if tx.execute(
                    "UPDATE scaleset_demand SET permit_attempt_token = ?1, updated_at = ?2
                     WHERE request_id = ?3 AND scale_set_id = ?4
                       AND permit_attempt_token IS ?5",
                    params![
                        stage.new_attempt_token,
                        now,
                        stage.request_id,
                        stage.scale_set_id,
                        stage.old_attempt_token,
                    ],
                )? != 1
                {
                    anyhow::bail!(
                        "demand token for {} moved during staged recovery",
                        stage.holder
                    );
                }
            }

            if !stage.ownership_id.is_empty() {
                let worker: Option<Option<String>> = tx
                    .query_row(
                        "SELECT permit_attempt_token FROM scaleset_workers
                         WHERE ownership_id = ?1 AND request_id = ?2",
                        params![stage.ownership_id, stage.request_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(token) = worker
                    && token.as_deref() != Some(stage.new_attempt_token.as_str())
                {
                    if token.as_deref() != stage.old_attempt_token.as_deref() {
                        anyhow::bail!(
                            "worker token for {} changed during staged recovery",
                            stage.holder
                        );
                    }
                    if tx.execute(
                        "UPDATE scaleset_workers SET permit_attempt_token = ?1
                         WHERE ownership_id = ?2 AND request_id = ?3
                           AND permit_attempt_token IS ?4",
                        params![
                            stage.new_attempt_token,
                            stage.ownership_id,
                            stage.request_id,
                            stage.old_attempt_token,
                        ],
                    )? != 1
                    {
                        anyhow::bail!(
                            "worker token for {} moved during staged recovery",
                            stage.holder
                        );
                    }
                }
            }
            let latest_intent: Option<(String, Option<String>, String)> = tx
                .query_row(
                    "SELECT operation_id, permit_attempt_token, runner_name
                     FROM scaleset_provision_intents
                     WHERE scale_set_id = ?1 AND request_id = ?2
                     ORDER BY created_at DESC, operation_id DESC LIMIT 1",
                    params![stage.scale_set_id, stage.request_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if let Some((operation_id, token, runner_name)) = latest_intent {
                let expected_ownership = OwnershipId::bind(stage.scale_set_id, &runner_name)
                    .as_str()
                    .as_str()
                    .to_owned();
                if expected_ownership != stage.ownership_id {
                    anyhow::bail!(
                        "provision identity for {} changed during staged recovery",
                        stage.holder
                    );
                }
                if token.as_deref() != Some(stage.new_attempt_token.as_str()) {
                    if token.as_deref() != stage.old_attempt_token.as_deref() {
                        anyhow::bail!(
                            "provision token for {} changed during staged recovery",
                            stage.holder
                        );
                    }
                    if tx.execute(
                        "UPDATE scaleset_provision_intents SET permit_attempt_token = ?1,
                             updated_at = ?2
                         WHERE operation_id = ?3 AND permit_attempt_token IS ?4",
                        params![
                            stage.new_attempt_token,
                            now,
                            operation_id,
                            stage.old_attempt_token,
                        ],
                    )? != 1
                    {
                        anyhow::bail!(
                            "provision token for {} moved during staged recovery",
                            stage.holder
                        );
                    }
                }
            }
        }

        let mut stmt = tx.prepare(
            "SELECT batch_id, scale_set_id, request_ids_json, attempt_tokens_json
             FROM scaleset_acquire_batches WHERE state IN ('intended', 'uncertain')",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let batches = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for (batch_id, scale_set_id, request_ids_raw, old_tokens_raw) in batches {
            let request_ids: Vec<i64> = serde_json::from_str(&request_ids_raw)
                .context("decode staged batch request ids")?;
            let mut tokens = Vec::with_capacity(request_ids.len());
            for request_id in &request_ids {
                let (token, state_raw): (Option<String>, String) = tx
                    .query_row(
                        "SELECT permit_attempt_token, state FROM scaleset_demand
                         WHERE request_id = ?1 AND scale_set_id = ?2",
                        params![request_id, scale_set_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?
                    .with_context(|| {
                        format!("active batch {batch_id:?} demand {request_id} is missing")
                    })?;
                let demand_state = DemandState::parse(&state_raw).with_context(|| {
                    format!("active batch {batch_id:?} has invalid demand state")
                })?;
                if token.as_deref().is_some_and(str::is_empty) {
                    anyhow::bail!("active batch {batch_id:?} has an empty demand token");
                }
                validate_batch_member(
                    scale_set_id,
                    *request_id,
                    token.as_deref(),
                    demand_state.holds_permit(),
                )?;
                tokens.push(token);
            }
            let previous: Option<Vec<Option<String>>> = old_tokens_raw
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .context("decode previous batch attempt tokens")?;
            if let Some(previous) = previous.as_ref()
                && previous.len() != request_ids.len()
            {
                anyhow::bail!("active batch {batch_id:?} has misaligned tokens");
            }
            if let Some(previous) = previous.as_ref() {
                for (index, (request_id, current_token)) in
                    request_ids.iter().zip(&tokens).enumerate()
                {
                    if previous[index] == *current_token {
                        continue;
                    }
                    let authorized = match (previous[index].as_deref(), current_token.as_deref()) {
                        (old, Some(current)) => stages.iter().any(|stage| {
                            stage.scale_set_id == scale_set_id
                                && stage.request_id == *request_id
                                && stage.old_attempt_token.as_deref() == old
                                && stage.new_attempt_token == current
                        }),
                        // A batch member cannot lose a recorded owner token.
                        (Some(_), None) => false,
                        (None, None) => true,
                    };
                    if !authorized {
                        anyhow::bail!("active batch {batch_id:?} token changed without a stage");
                    }
                }
            }
            let next = serde_json::to_string(&tokens)?;
            if old_tokens_raw.as_deref() != Some(next.as_str())
                && tx.execute(
                    "UPDATE scaleset_acquire_batches SET attempt_tokens_json = ?1, updated_at = ?2
                     WHERE batch_id = ?3 AND attempt_tokens_json IS ?4",
                    params![next, now, batch_id, old_tokens_raw],
                )? != 1
            {
                anyhow::bail!("active batch {batch_id:?} changed during staged recovery");
            }
        }

        for stage in &stages {
            if tx.execute(
                "DELETE FROM scaleset_attempt_rotations WHERE holder = ?1
                 AND new_attempt_token = ?2",
                params![stage.holder, stage.new_attempt_token],
            )? != 1
            {
                anyhow::bail!(
                    "staged rotation for {} moved during completion",
                    stage.holder
                );
            }
            let previous_pid = stage
                .previous_pid
                .context("completed staged rotation has no prior pid")?;
            if tx.execute(
                "DELETE FROM scaleset_attempt_rotation_priors WHERE holder = ?1
                 AND previous_pid = ?2",
                params![stage.holder, i64::from(previous_pid)],
            )? != 1
            {
                anyhow::bail!(
                    "staged rotation prior pid for {} moved during completion",
                    stage.holder
                );
            }
        }
        tx.commit().context("commit staged attempt completion")?;
        Ok(())
    }

    fn set_runner_start_deadline_if_none(
        &mut self,
        ownership_id: &str,
        attempt_token: &str,
        deadline_epoch: u64,
    ) -> Result<u64> {
        let deadline = i64::try_from(deadline_epoch).unwrap_or(i64::MAX);
        self.conn
            .execute(
                "UPDATE scaleset_worker_runtime
             SET runner_start_deadline_epoch = COALESCE(runner_start_deadline_epoch, ?1)
             WHERE ownership_id = ?2 AND EXISTS (
                 SELECT 1 FROM scaleset_workers w
                 WHERE w.ownership_id = ?2 AND w.permit_attempt_token = ?3
             )",
                params![deadline, ownership_id, attempt_token],
            )
            .context("persist runner startup deadline")?;
        let stored: Option<i64> = self
            .conn
            .query_row(
                "SELECT runner_start_deadline_epoch FROM scaleset_worker_runtime
             WHERE ownership_id = ?1 AND EXISTS (
                 SELECT 1 FROM scaleset_workers w
                 WHERE w.ownership_id = ?1 AND w.permit_attempt_token = ?2
             )",
                params![ownership_id, attempt_token],
                |row| row.get(0),
            )
            .context("read runner startup deadline")?;
        stored
            .map(|seconds| seconds.max(0) as u64)
            .with_context(|| format!("worker runtime row {ownership_id:?} is missing"))
    }

    fn clear_runner_start_deadline(
        &mut self,
        ownership_id: &str,
        attempt_token: &str,
    ) -> Result<()> {
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_worker_runtime SET runner_start_deadline_epoch = NULL
             WHERE ownership_id = ?1 AND EXISTS (
                 SELECT 1 FROM scaleset_workers w
                 WHERE w.ownership_id = ?1 AND w.permit_attempt_token = ?2
             )",
                params![ownership_id, attempt_token],
            )
            .context("clear runner startup deadline")?;
        if updated == 0 {
            anyhow::bail!("worker runtime token changed for {ownership_id:?}");
        }
        Ok(())
    }

    fn set_dind_restarts_used(
        &mut self,
        ownership_id: &str,
        used: u32,
        attempt_token: &str,
    ) -> Result<()> {
        let used = i64::from(used);
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_worker_runtime
             SET dind_restarts_used = MAX(dind_restarts_used, ?1)
             WHERE ownership_id = ?2 AND EXISTS (
                 SELECT 1 FROM scaleset_workers w
                 WHERE w.ownership_id = ?2 AND w.permit_attempt_token = ?3
             )",
                params![used, ownership_id, attempt_token],
            )
            .context("persist DinD restart budget")?;
        if updated == 0 {
            anyhow::bail!("worker runtime holds no row for {ownership_id:?}");
        }
        Ok(())
    }

    fn set_diagnostics_complete(&mut self, ownership_id: &str, attempt_token: &str) -> Result<()> {
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_worker_runtime SET diagnostics_complete = 1
             WHERE ownership_id = ?1 AND EXISTS (
                 SELECT 1 FROM scaleset_workers w
                 WHERE w.ownership_id = ?1 AND w.permit_attempt_token = ?2
             )",
                params![ownership_id, attempt_token],
            )
            .context("persist diagnostic export completion")?;
        if updated == 0 {
            anyhow::bail!("worker runtime holds no row for {ownership_id:?}");
        }
        Ok(())
    }
}

/// Persist worker edges with the token captured by the owning runtime
/// attempt. The sink cannot consult mutable registry state for a newer
/// token, so delayed work from an old worker remains fenced.
struct AttemptBoundEdgeSink<'a> {
    registry: &'a mut WorkerRegistry,
    attempt_token: &'a str,
}

impl EdgeSink for AttemptBoundEdgeSink<'_> {
    fn record_edge(&mut self, edge: &WorkerEdge) -> anyhow::Result<()> {
        self.registry
            .set_state(&edge.ownership, edge.to, self.attempt_token)
    }
}

/// Lane failures. Every variant vetoes the ACK (via the loop's error
/// path): the message is redelivered and the lane call replays
/// idempotently. The JIT blob never appears in any rendering.
#[derive(Debug)]
pub struct LaneError {
    message: String,
}

impl LaneError {
    fn new(context: &str, error: anyhow::Error) -> Self {
        Self {
            message: format!("{context}: {error:#}"),
        }
    }
}

impl std::fmt::Display for LaneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for LaneError {}

/// Static lane configuration.
#[derive(Debug, Clone)]
pub struct LaneConfig {
    pub scale_set_id: i32,
    pub profile: HomogeneousProfile,
    /// Host state root; each worker gets `<root>/<ownership-slug>`.
    pub state_root: PathBuf,
    /// DinD readiness probes before giving up (times the worker
    /// readiness poll interval).
    pub ready_attempts: u32,
    /// Minimum gap between opportunistic supervision sweeps.
    pub sweep_interval: Duration,
}

/// One tracked worker: lifecycle record + supervision budget.
struct LiveWorker {
    worker: ScaleSetWorker,
    supervision: Supervision,
    attempt_token: String,
}

/// The daemon's [`WorkerLane`]: JIT fetch, provision, supervision, cleanup.
///
/// See the module docs for the lifecycle contract.
pub struct DaemonWorkerLane {
    client: ScaleSetClient,
    config: LaneConfig,
    runner: Box<dyn WorkerRunner + Send>,
    hook: Box<dyn ToolContentHook + Send>,
    intents: ProvisionIntentStore,
    demand: DemandStore,
    registry: WorkerRegistry,
    ledger: SharedLedger,
    recovery_claim_token: Option<String>,
    #[cfg(test)]
    _test_recovery_claim: Option<velnor_control::permit_ledger::ScaleSetRecoveryClaim>,
    workers: HashMap<String, LiveWorker>,
    last_sweep: Option<Instant>,
}

impl DaemonWorkerLane {
    fn finish_staged_attempt_rotations(&mut self) -> Result<()> {
        let ledger = &self.ledger;
        self.registry.finish_staged_attempt_rotations(
            |scale_set_id, request_id, token, holds_permit| {
                let holder = permit_holder(scale_set_id, request_id);
                let state = ledger.holder_state(&holder)?;
                match (token, state) {
                    (Some(token), Some(_)) if ledger.is_current_attempt(&holder, token)? => Ok(()),
                    (Some(_), Some(_)) => {
                        anyhow::bail!("batch member {holder:?} belongs to another attempt")
                    }
                    (Some(_), None) | (None, Some(_)) if holds_permit => {
                        anyhow::bail!("active batch member {holder:?} has no matching permit")
                    }
                    (None, None) if !holds_permit => Ok(()),
                    (Some(_), None) => Ok(()),
                    (None, Some(_)) => {
                        anyhow::bail!("tokenless batch member {holder:?} still has a permit")
                    }
                    (None, None) => {
                        anyhow::bail!("active batch member {holder:?} has no permit token")
                    }
                }
            },
        )
    }

    /// Open the lane: its own store connections (SQLite, shared files),
    /// the admin client for JIT fetch, and the process runner + content
    /// hook for Docker work. Production passes
    /// [`ProcessCommandRunner`][pcr] and [`DockerToolContentHook`][hook];
    /// tests pass scripted doubles.
    ///
    /// [pcr]: crate::executor::ProcessCommandRunner
    /// [hook]: crate::scaleset::worker::DockerToolContentHook
    #[allow(
        clippy::too_many_arguments,
        reason = "lane construction, one call site"
    )]
    pub fn open(
        client: ScaleSetClient,
        config: LaneConfig,
        state_db: &Path,
        ledger_path: &Path,
        runner: Box<dyn WorkerRunner + Send>,
        hook: Box<dyn ToolContentHook + Send>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&config.state_root).with_context(|| {
            format!(
                "create scale-set worker state root {}",
                config.state_root.display()
            )
        })?;
        Ok(Self {
            client,
            config,
            runner,
            hook,
            intents: ProvisionIntentStore::open(state_db)?,
            demand: DemandStore::open(state_db)?,
            registry: WorkerRegistry::open(state_db)?,
            ledger: SharedLedger::open(ledger_path)?,
            recovery_claim_token: None,
            #[cfg(test)]
            _test_recovery_claim: None,
            workers: HashMap::new(),
            last_sweep: None,
        })
    }

    /// Bind the lane to the daemon's serialized recovery claim. Recovery
    /// mutations fail closed when no claim token has been installed.
    pub(crate) fn set_recovery_claim_token(&mut self, claim_token: &str) -> Result<()> {
        if claim_token.is_empty() {
            anyhow::bail!("scale-set recovery claim token cannot be empty");
        }
        if self
            .recovery_claim_token
            .as_deref()
            .is_some_and(|current| current != claim_token)
        {
            anyhow::bail!("scale-set recovery claim token cannot be replaced");
        }
        self.recovery_claim_token = Some(claim_token.to_owned());
        Ok(())
    }

    /// Canonical ownership key for one intent (the registry + live-map key).
    fn ownership_key(intent: &ProvisionIntent) -> String {
        OwnershipId::bind(intent.scale_set_id, &intent.runner_name).as_str()
    }

    fn terminal_key(&self, request_id: i64) -> Result<String, LaneError> {
        Ok(self
            .intents
            .get_by_request(self.config.scale_set_id, request_id)
            .map_err(|error| LaneError::new("find provision intent", error))?
            .map(|intent| Self::ownership_key(&intent))
            .unwrap_or_else(|| {
                OwnershipId::bind(
                    self.config.scale_set_id,
                    &crate::scaleset::runner_name(self.config.scale_set_id, request_id),
                )
                .as_str()
            }))
    }

    fn note_terminal_request(
        &mut self,
        request_id: i64,
        attempt_token: &str,
    ) -> Result<(), LaneError> {
        self.refresh_generation()?;
        self.opportunistic_sweep();
        let key = self.terminal_key(request_id)?;
        self.drive_terminal(&key, attempt_token)
            .map_err(|error| LaneError::new("drive worker terminal", error))
    }

    fn worker_state_dir(&self, ownership: &OwnershipId) -> PathBuf {
        self.config.state_root.join(ownership.slug())
    }

    /// Recover the exact state directory recorded when this worker was
    /// provisioned. Config changes across restart must not redirect cleanup.
    fn recorded_state_dir(&self, row: &WorkerRow) -> Result<PathBuf> {
        let ownership = OwnershipId::bind(self.config.scale_set_id, &row.runner_name);
        if row.ownership_id != ownership.as_str() {
            anyhow::bail!("worker ownership id does not match its runner name");
        }
        let workspace = PathBuf::from(
            row.workspace_path
                .as_deref()
                .context("worker has no recorded workspace path")?,
        );
        if workspace.file_name().is_none_or(|name| name != "workspace")
            || workspace
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            anyhow::bail!("worker has an invalid recorded workspace path");
        }
        let state_dir = workspace
            .parent()
            .context("recorded workspace path has no state directory")?;
        let state_dir_slug = ownership.slug();
        if state_dir
            .file_name()
            .is_none_or(|name| name.to_string_lossy() != state_dir_slug)
        {
            anyhow::bail!("recorded worker state path does not match its ownership id");
        }
        let dind_data = PathBuf::from(
            row.dind_data_path
                .as_deref()
                .context("worker has no recorded DinD data path")?,
        );
        if dind_data != state_dir.join("dind-data") {
            anyhow::bail!("recorded worker DinD data path does not match its state directory");
        }
        Ok(state_dir.to_path_buf())
    }

    /// Refresh the registry generation from the ledger. Edges stamped with
    /// a stale generation would lie about which epoch recorded them.
    fn refresh_generation(&mut self) -> Result<(), LaneError> {
        let generation = self
            .ledger
            .generation()
            .map_err(|error| LaneError::new("read ledger generation", error.into()))?;
        self.registry.set_generation(generation);
        Ok(())
    }

    /// Read-only census of the deterministic runner/DinD pair. Every
    /// present container must carry this worker's exact ownership label;
    /// an absent container is safe to recover, while daemon/inspect errors
    /// fail closed.
    fn verify_recovery_containers(&mut self, identity: &WorkerIdentity) -> Result<(bool, bool)> {
        let ownership = identity.ownership().as_str();
        let mut present = [false; 2];
        for (index, name) in [identity.runner_container(), identity.dind_container()]
            .into_iter()
            .enumerate()
        {
            let inspect = self.runner.run(
                "docker",
                &[
                    "inspect".to_owned(),
                    "--format".to_owned(),
                    "{{.Id}}".to_owned(),
                    "--".to_owned(),
                    name.clone(),
                ],
            )?;
            if inspect.code == 0 && !inspect.stdout.trim().is_empty() {
                crate::scaleset::worker::dind::verify_container_ownership(
                    &mut *self.runner,
                    &name,
                    &ownership,
                )?;
                present[index] = true;
            } else if inspect.code != 0
                && !crate::docker::client::daemon_reports_missing(&inspect.stderr)
            {
                anyhow::bail!(
                    "cannot prove recovery state of container {name:?}: docker exited {}: {}",
                    inspect.code,
                    inspect.stderr.trim()
                );
            } else if inspect.code == 0 {
                anyhow::bail!("docker inspect returned no identity for {name:?}");
            }
        }
        Ok((present[0], present[1]))
    }

    fn require_dead_prior_owner(&mut self, holder: &str) -> Result<(u64, LedgerPermitState, u32)> {
        let current_generation = self.ledger.generation()?;
        let recorded = self
            .ledger
            .holders()?
            .into_iter()
            .find(|record| record.holder == holder)
            .with_context(|| format!("no recorded permit holder for {holder:?}"))?;
        if recorded.lane != crate::scaleset::capacity::LedgerLane::ScaleSet {
            anyhow::bail!("permit {holder:?} belongs to another lane");
        }
        let old_pid = recorded
            .pid
            .context("Scale Set permit has no recorded owner pid; recovery is not authorized")?;
        if old_pid == std::process::id() || crate::permit_guard::pid_alive(old_pid) {
            anyhow::bail!(
                "Scale Set permit {holder:?} still has a live or same-process owner pid {old_pid}"
            );
        }
        Ok((current_generation, recorded.state, old_pid))
    }

    /// Recognize the ledger half of an already committed staged rotation
    /// before checking the old worker PID. The original recovery pass proved
    /// that prior process dead before its atomic CAS. A same-claim retry can
    /// therefore continue from that exact target after the CAS changed the
    /// row PID to this recovery process. This path only probes an exact
    /// staged target and accepts only `AlreadyRotated`; it never starts a
    /// rotation or relaxes the dead-owner check for an old-token row.
    fn resume_staged_rotation_if_proven(
        &mut self,
        stage: &StagedAttemptRotation,
    ) -> Result<Option<(u64, LedgerPermitState, u32)>> {
        let previous_pid = stage
            .previous_pid
            .context("staged recovery has no persisted prior pid")?;
        if !self
            .ledger
            .is_current_attempt(&stage.holder, &stage.new_attempt_token)?
        {
            return Ok(None);
        }
        let current_generation = self.ledger.generation()?;
        let Some(recorded) = self
            .ledger
            .holders()?
            .into_iter()
            .find(|record| record.holder == stage.holder)
        else {
            return Ok(None);
        };
        if recorded.lane != crate::scaleset::capacity::LedgerLane::ScaleSet
            || recorded.generation == 0
            || recorded.generation > current_generation
        {
            return Ok(None);
        }
        // Use the exact generation atomically written by the original
        // rotation, even if this same process began a later daemon epoch
        // before retrying the projection.
        let rotation_generation = recorded.generation;
        let claim_token = self
            .recovery_claim_token
            .as_deref()
            .context("scale-set recovery resume requires a claim token")?;
        let outcome = self.ledger.rotate_scaleset_attempt_for_recovery(
            &stage.holder,
            recorded.state,
            rotation_generation,
            previous_pid,
            stage.old_attempt_token.as_deref(),
            &stage.new_attempt_token,
            claim_token,
        )?;
        if outcome == velnor_control::permit_ledger::AttemptRotationOutcome::AlreadyRotated {
            return Ok(Some((rotation_generation, recorded.state, previous_pid)));
        }
        Ok(None)
    }

    fn recorded_attempt_token(
        demand: &crate::scaleset::demand::Demand,
        worker: Option<&WorkerRow>,
        intent: Option<&ProvisionIntent>,
    ) -> Result<Option<String>> {
        let mut tokens = vec![demand.permit_attempt_token.as_deref()];
        if let Some(worker) = worker {
            tokens.push(worker.permit_attempt_token.as_deref());
        }
        if let Some(intent) = intent {
            tokens.push(intent.permit_attempt_token.as_deref());
        }
        if tokens.iter().flatten().any(|token| token.is_empty()) {
            anyhow::bail!("durable permit attempt token is empty");
        }
        let first = tokens.first().copied().flatten();
        if tokens.iter().any(|token| *token != first) {
            anyhow::bail!(
                "demand, worker, and provision records disagree on permit attempt ownership"
            );
        }
        Ok(first.map(str::to_owned))
    }

    /// Prove the prior worker's exact Docker footprint before a staged
    /// rotation. Worker and provision intent rows are written before Docker;
    /// their joint absence proves provisioning never began.
    fn verify_recovery_evidence(
        &mut self,
        scale_set_id: i32,
        request_id: i64,
        worker: Option<&WorkerRow>,
        intent: Option<&ProvisionIntent>,
    ) -> Result<String> {
        match (worker, intent) {
            (Some(worker), Some(intent)) => {
                if worker.request_id != Some(request_id)
                    || worker.operation_id != intent.operation_id
                    || worker.runner_name != intent.runner_name
                    || intent.scale_set_id != scale_set_id
                    || intent.request_id != request_id
                {
                    anyhow::bail!("worker and provision records do not identify the same attempt");
                }
                let identity =
                    WorkerIdentity::new(OwnershipId::bind(scale_set_id, &worker.runner_name));
                let resources = self.verify_recovery_containers(&identity)?;
                if worker.worker_state == ScaleSetWorkerState::PermitReleased
                    && resources != (false, false)
                {
                    anyhow::bail!("released worker still has an owned container");
                }
                Ok(worker.ownership_id.clone())
            }
            (None, Some(intent)) => {
                if intent.scale_set_id != scale_set_id || intent.request_id != request_id {
                    anyhow::bail!("provision record identifies another attempt");
                }
                let identity =
                    WorkerIdentity::new(OwnershipId::bind(scale_set_id, &intent.runner_name));
                if self.verify_recovery_containers(&identity)? != (false, false) {
                    anyhow::bail!(
                        "worker containers exist without a durable worker row for request {request_id}"
                    );
                }
                Ok(identity.ownership().as_str().as_str().to_owned())
            }
            (None, None) => {
                if self.registry.get_by_request(request_id)?.is_some() {
                    anyhow::bail!("worker row exists without its provision intent");
                }
                // Both rows precede every Docker call. Check the deterministic
                // identity as well, so an orphaned labeled pair blocks token
                // recovery even if state rows were damaged.
                let runner_name = crate::scaleset::runner_name(scale_set_id, request_id);
                let identity = WorkerIdentity::new(OwnershipId::bind(scale_set_id, &runner_name));
                if self.verify_recovery_containers(&identity)? != (false, false) {
                    anyhow::bail!("worker containers exist without durable rows for {request_id}");
                }
                Ok(String::new())
            }
            (Some(_), None) => anyhow::bail!(
                "worker row exists without its durable provision intent; ownership is ambiguous"
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rotate_staged_attempt(
        &mut self,
        holder: &str,
        scale_set_id: i32,
        request_id: i64,
        ownership_id: &str,
        old_token: Option<&str>,
        generation: u64,
        previous_pid: u32,
        state: LedgerPermitState,
    ) -> Result<String> {
        let claim_token = self
            .recovery_claim_token
            .as_deref()
            .context("scale-set recovery rotation requires a claim token")?
            .to_owned();
        let staged = self.registry.stage_attempt_rotation(
            holder,
            ownership_id,
            scale_set_id,
            request_id,
            previous_pid,
            old_token,
        )?;
        let expected_previous_pid = staged
            .previous_pid
            .context("staged recovery has no persisted prior pid")?;
        match self.ledger.rotate_scaleset_attempt_for_recovery(
            holder,
            state,
            generation,
            expected_previous_pid,
            old_token,
            &staged.new_attempt_token,
            &claim_token,
        )? {
            velnor_control::permit_ledger::AttemptRotationOutcome::Rotated
            | velnor_control::permit_ledger::AttemptRotationOutcome::AlreadyRotated => {
                Ok(staged.new_attempt_token)
            }
            outcome => anyhow::bail!("cannot rotate staged attempt for {holder:?}: {outcome:?}"),
        }
    }

    /// Rebind every prior Scale Set attempt only at daemon startup. Every
    /// stage is persisted before ledger mutation, and every resource proof
    /// is repeated when startup resumes a stage after a crash.
    fn recover_durable_attempts(&mut self) -> Result<()> {
        self.recover_staged_attempt_releases()?;
        self.recover_staged_attempt_acquisitions()?;
        let pending = self.registry.pending_attempt_rotations()?;
        let pending_holders: std::collections::HashSet<String> =
            pending.iter().map(|stage| stage.holder.clone()).collect();
        for stage in pending {
            let demand = self
                .demand
                .get(stage.request_id)?
                .context("staged attempt has no durable demand row")?;
            if demand.scale_set_id != stage.scale_set_id {
                anyhow::bail!("staged attempt demand belongs to another scale set");
            }
            let worker = self.registry.get_by_request(stage.request_id)?;
            let intent = self
                .intents
                .get_by_request(stage.scale_set_id, stage.request_id)?;
            if worker
                .as_ref()
                .is_some_and(|row| row.ownership_id != stage.ownership_id)
                || (stage.ownership_id.is_empty() && worker.is_some())
            {
                anyhow::bail!("staged attempt worker identity changed");
            }
            let previous = Self::recorded_attempt_token(&demand, worker.as_ref(), intent.as_ref())?;
            if previous.as_deref() != stage.old_attempt_token.as_deref() {
                anyhow::bail!("staged attempt token projections changed before resume");
            }
            let ownership_id = self.verify_recovery_evidence(
                stage.scale_set_id,
                stage.request_id,
                worker.as_ref(),
                intent.as_ref(),
            )?;
            if ownership_id != stage.ownership_id {
                anyhow::bail!("staged attempt's exact worker identity changed");
            }
            let (generation, state, previous_pid) =
                match self.resume_staged_rotation_if_proven(&stage)? {
                    Some(proof) => proof,
                    None => self.require_dead_prior_owner(&stage.holder)?,
                };
            let _target_token = self.rotate_staged_attempt(
                &stage.holder,
                stage.scale_set_id,
                stage.request_id,
                &stage.ownership_id,
                stage.old_attempt_token.as_deref(),
                generation,
                previous_pid,
                state,
            )?;
        }

        let active = self
            .demand
            .list_in_states(self.config.scale_set_id, &PERMIT_STATES)?;
        for (request_id, _state) in active {
            let Some(demand) = self.demand.get(request_id)? else {
                anyhow::bail!("active demand {request_id} vanished during attempt recovery");
            };
            let holder = permit_holder(self.config.scale_set_id, request_id);
            let permit = self
                .ledger
                .holders()?
                .into_iter()
                .find(|record| record.holder == holder);
            let Some(permit) = permit else {
                if demand.state == DemandState::Granted {
                    continue;
                }
                anyhow::bail!("active demand {holder:?} has no permit row");
            };
            if permit.lane != crate::scaleset::capacity::LedgerLane::ScaleSet {
                anyhow::bail!("active Scale Set demand {holder:?} has a foreign permit lane");
            }
            if pending_holders.contains(&holder) {
                continue;
            }
            let worker = self.registry.get_by_request(request_id)?;
            let intent = self
                .intents
                .get_by_request(self.config.scale_set_id, request_id)?;
            let old_token =
                Self::recorded_attempt_token(&demand, worker.as_ref(), intent.as_ref())?;
            if permit.pid == Some(std::process::id()) {
                let Some(attempt_token) = old_token.as_deref() else {
                    anyhow::bail!(
                        "current process holds {holder:?} but durable rows have no attempt token"
                    );
                };
                if !self.ledger.is_current_attempt(&holder, attempt_token)? {
                    anyhow::bail!("current process token for {holder:?} is stale");
                }
                continue;
            }
            let (generation, state, previous_pid) = self.require_dead_prior_owner(&holder)?;
            let ownership_id = self.verify_recovery_evidence(
                self.config.scale_set_id,
                request_id,
                worker.as_ref(),
                intent.as_ref(),
            )?;
            if let Some(token) = old_token.as_deref()
                && !self.ledger.is_current_attempt(&holder, token)?
            {
                anyhow::bail!("persisted attempt token for {holder:?} is stale");
            }
            let _target_token = self.rotate_staged_attempt(
                &holder,
                self.config.scale_set_id,
                request_id,
                &ownership_id,
                old_token.as_deref(),
                generation,
                previous_pid,
                state,
            )?;
        }
        self.finish_staged_attempt_rotations()
    }

    /// Finish caller-token acquisitions left between the permit database and
    /// the Scale Set batch record. A committed permit with no batch is
    /// released back to Eligible; an exact batch proves the network replay
    /// boundary and keeps the permit. An absent permit with no batch proves
    /// the ledger insert did not commit and simply clears the stage.
    fn recover_staged_attempt_acquisitions(&mut self) -> Result<()> {
        let stages = self.demand.pending_attempt_acquires()?;
        for stage in stages {
            if stage.scale_set_id != self.config.scale_set_id
                || stage.holder != permit_holder(stage.scale_set_id, stage.request_id)
                || stage.target_attempt_token.is_empty()
            {
                anyhow::bail!("staged acquire identity is invalid for {:?}", stage.holder);
            }
            let demand = self
                .demand
                .get(stage.request_id)?
                .context("staged acquire has no durable demand row")?;
            if demand.scale_set_id != stage.scale_set_id
                || demand.state != DemandState::Granted
                || (demand.permit_attempt_token.as_deref()
                    != stage.previous_attempt_token.as_deref()
                    && demand.permit_attempt_token.as_deref()
                        != Some(stage.target_attempt_token.as_str()))
            {
                anyhow::bail!("staged acquire projection changed for {:?}", stage.holder);
            }
            let worker = self.registry.get_by_request(stage.request_id)?;
            let intent = self
                .intents
                .get_by_request(stage.scale_set_id, stage.request_id)?;
            if worker.is_some() || intent.is_some() {
                anyhow::bail!(
                    "staged fresh acquire {:?} already has worker state",
                    stage.holder
                );
            }
            // A staged fresh acquire precedes JIT intent and every Docker
            // call. Recheck the deterministic resource names before either
            // releasing or handing the holder back to batch recovery.
            self.verify_recovery_evidence(stage.scale_set_id, stage.request_id, None, None)?;
            let has_batch = self.demand.has_acquire_batch_attempt(
                stage.scale_set_id,
                stage.request_id,
                &stage.target_attempt_token,
            )?;
            let permit = self
                .ledger
                .holders()?
                .into_iter()
                .find(|record| record.holder == stage.holder);
            match permit {
                None => {
                    if has_batch {
                        anyhow::bail!(
                            "staged acquire {:?} has a durable batch but no permit",
                            stage.holder
                        );
                    }
                    if demand.permit_attempt_token.as_deref()
                        != stage.previous_attempt_token.as_deref()
                    {
                        anyhow::bail!(
                            "absent staged acquire {:?} already projected its target token",
                            stage.holder
                        );
                    }
                    self.demand.finish_attempt_acquire(&stage, false)?;
                }
                Some(record) => {
                    if record.lane != crate::scaleset::capacity::LedgerLane::ScaleSet
                        || record.state != LedgerPermitState::Reserved
                        || !self
                            .ledger
                            .is_current_attempt(&stage.holder, &stage.target_attempt_token)?
                    {
                        anyhow::bail!(
                            "staged acquire {:?} is not the exact reserved attempt",
                            stage.holder
                        );
                    }
                    self.require_dead_prior_owner(&stage.holder)?;
                    if has_batch {
                        self.demand.finish_attempt_acquire(&stage, true)?;
                        self.demand.finish_attempt_acquire_batch(&stage)?;
                    } else {
                        let release = self.demand.finish_staged_acquire_as_release(&stage)?;
                        self.complete_staged_attempt_release(&release)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn release_attempt_with_projection(
        &mut self,
        holder: &str,
        request_id: i64,
        attempt_token: &str,
        ledger_demand_state: velnor_control::permit_ledger::DemandState,
        next_demand_state: Option<DemandState>,
        worker_ownership_id: Option<&str>,
    ) -> Result<bool> {
        // An adopted or legacy worker may have no demand projection at all;
        // there is then no demand state to move, so release without one. The
        // worker record and the exact ledger attempt token still fence the
        // release below, and a present-but-mismatched demand still fails in
        // staging. (Request IDs are GitHub-global, so the unscoped lookup is
        // exact.)
        let next_demand_state = match next_demand_state {
            Some(_state) if self.demand.get(request_id)?.is_none() => {
                eprintln!(
                    "forensics.lifecycle: scaleset release for {holder:?} has no demand projection; releasing worker and permit only"
                );
                None
            }
            next => next,
        };
        let stage = self.demand.stage_attempt_release(
            holder,
            self.config.scale_set_id,
            request_id,
            attempt_token,
            ledger_demand_state,
            next_demand_state,
            worker_ownership_id,
        )?;
        self.complete_staged_attempt_release(&stage)
    }

    fn release_recovered_without_worker(
        &mut self,
        holder: &str,
        request_id: i64,
        attempt_token: &str,
        demand_state: DemandState,
    ) -> Result<()> {
        let (ledger_state, next_demand_state) = match demand_state {
            DemandState::Terminal => (
                velnor_control::permit_ledger::DemandState::Terminal,
                Some(DemandState::Terminal),
            ),
            DemandState::CanceledDone => (
                velnor_control::permit_ledger::DemandState::Cancelled,
                Some(DemandState::CanceledDone),
            ),
            DemandState::Declined => (velnor_control::permit_ledger::DemandState::Terminal, None),
            _ => anyhow::bail!("cannot stage release for nonterminal demand {holder:?}"),
        };
        self.release_attempt_with_projection(
            holder,
            request_id,
            attempt_token,
            ledger_state,
            next_demand_state,
            None,
        )?;
        Ok(())
    }

    fn complete_staged_attempt_release(&mut self, stage: &StagedAttemptRelease) -> Result<bool> {
        let released = match self.ledger.release_staged(
            &stage.holder,
            &stage.attempt_token,
            stage.ledger_demand_state,
        )? {
            velnor_control::permit_ledger::OwnedReleaseOutcome::Released => true,
            velnor_control::permit_ledger::OwnedReleaseOutcome::AlreadyAbsent => false,
            velnor_control::permit_ledger::OwnedReleaseOutcome::StaleAttempt => {
                anyhow::bail!("permit {:?} belongs to a different attempt", stage.holder)
            }
        };
        if !matches!(
            stage.ledger_demand_state,
            velnor_control::permit_ledger::DemandState::Eligible
                | velnor_control::permit_ledger::DemandState::Cancelled
                | velnor_control::permit_ledger::DemandState::Terminal
        ) {
            anyhow::bail!("invalid release target for {}", stage.holder);
        }
        let generation = self.ledger.generation()?;
        self.demand
            .finish_attempt_release(&stage.holder, generation)?;
        Ok(released)
    }

    fn recover_staged_attempt_releases(&mut self) -> Result<()> {
        let stages = self.demand.pending_attempt_releases()?;
        for stage in stages {
            if stage.scale_set_id != self.config.scale_set_id
                || stage.holder != permit_holder(stage.scale_set_id, stage.request_id)
            {
                anyhow::bail!("staged release identity is invalid for {:?}", stage.holder);
            }
            if let Some(ownership_id) = stage.worker_ownership_id.as_deref() {
                let worker = self
                    .registry
                    .get(ownership_id)?
                    .context("staged worker release has no worker projection")?;
                if worker.ownership_id != ownership_id
                    || worker.permit_attempt_token.as_deref() != Some(stage.attempt_token.as_str())
                    || !matches!(
                        worker.worker_state,
                        ScaleSetWorkerState::OwnedCleanup | ScaleSetWorkerState::PermitReleased
                    )
                {
                    anyhow::bail!("staged release worker changed for {:?}", stage.holder);
                }
                let identity =
                    WorkerIdentity::new(OwnershipId::bind(stage.scale_set_id, &worker.runner_name));
                if self.verify_recovery_containers(&identity)? != (false, false) {
                    anyhow::bail!(
                        "staged release worker {:?} still has owned containers",
                        stage.holder
                    );
                }
            }
            if self.ledger.holder_state(&stage.holder)?.is_some() {
                self.require_dead_prior_owner(&stage.holder)?;
            }
            self.complete_staged_attempt_release(&stage)?;
        }
        Ok(())
    }

    fn require_current_attempt(
        &self,
        holder: &str,
        attempt_token: &str,
        worker_state: ScaleSetWorkerState,
    ) -> Result<()> {
        if attempt_token.is_empty() {
            anyhow::bail!("worker attempt token cannot be empty");
        }
        // PermitReleased rows are durable tombstones; their permit is
        // expected to be absent, so they must not re-enter runtime work.
        if worker_state == ScaleSetWorkerState::PermitReleased {
            return Ok(());
        }
        if !self.ledger.is_current_attempt(holder, attempt_token)? {
            anyhow::bail!("permit {holder:?} is not owned by this worker attempt");
        }
        Ok(())
    }

    fn recover_worker_attempt(&mut self, row: &WorkerRow, intent: &ProvisionIntent) -> Result<()> {
        let request_id = row
            .request_id
            .context("live worker row has no request id")?;
        let demand = self
            .demand
            .get(request_id)?
            .context("live worker has no durable demand row")?;
        let old_token = Self::recorded_attempt_token(&demand, Some(row), Some(intent))?;
        let holder = permit_holder(self.config.scale_set_id, request_id);
        let permit = self
            .ledger
            .holders()?
            .into_iter()
            .find(|record| record.holder == holder);
        let Some(permit) = permit else {
            if row.worker_state == ScaleSetWorkerState::PermitReleased {
                return Ok(());
            }
            anyhow::bail!("live worker {holder:?} has no permit holder");
        };
        let ownership_id = self.verify_recovery_evidence(
            self.config.scale_set_id,
            request_id,
            Some(row),
            Some(intent),
        )?;
        if ownership_id != row.ownership_id {
            anyhow::bail!("worker recovery identity changed");
        }
        if row.worker_state == ScaleSetWorkerState::PermitReleased
            && permit.pid == Some(std::process::id())
        {
            let token = old_token
                .as_deref()
                .context("released worker has no current attempt token")?;
            if !self.ledger.is_current_attempt(&holder, token)? {
                anyhow::bail!("released worker {holder:?} has a stale attempt token");
            }
            self.release_attempt_with_projection(
                &holder,
                request_id,
                token,
                velnor_control::permit_ledger::DemandState::Terminal,
                Some(DemandState::Terminal),
                Some(&row.ownership_id),
            )?;
            return Ok(());
        }
        if permit.pid == Some(std::process::id()) {
            let token = old_token
                .as_deref()
                .context("current worker process has no durable attempt token")?;
            if !self.ledger.is_current_attempt(&holder, token)? {
                anyhow::bail!("current worker token for {holder:?} is stale");
            }
            return Ok(());
        }
        let (generation, permit_state, previous_pid) = self.require_dead_prior_owner(&holder)?;
        if let Some(token) = old_token.as_deref()
            && !self.ledger.is_current_attempt(&holder, token)?
        {
            anyhow::bail!("persisted worker attempt for {holder:?} is not current");
        }
        let target_token = self.rotate_staged_attempt(
            &holder,
            self.config.scale_set_id,
            request_id,
            &row.ownership_id,
            old_token.as_deref(),
            generation,
            previous_pid,
            permit_state,
        )?;
        self.finish_staged_attempt_rotations()?;
        if row.worker_state == ScaleSetWorkerState::PermitReleased {
            self.release_attempt_with_projection(
                &holder,
                request_id,
                &target_token,
                velnor_control::permit_ledger::DemandState::Terminal,
                Some(DemandState::Terminal),
                Some(&row.ownership_id),
            )?;
        }
        Ok(())
    }

    fn recover_attempt_without_worker(
        &mut self,
        intent: &ProvisionIntent,
        demand_state: DemandState,
    ) -> Result<()> {
        let request_id = intent.request_id;
        let holder = permit_holder(self.config.scale_set_id, request_id);
        let demand = self
            .demand
            .get(request_id)?
            .context("provision intent has no durable demand row")?;
        let old_token = Self::recorded_attempt_token(&demand, None, Some(intent))?;
        let identity = WorkerIdentity::new(OwnershipId::bind(
            self.config.scale_set_id,
            &intent.runner_name,
        ));
        if self.verify_recovery_containers(&identity)? != (false, false) {
            anyhow::bail!("worker containers exist without a durable worker row for {holder:?}");
        }
        let recorded = self
            .ledger
            .holders()?
            .into_iter()
            .find(|record| record.holder == holder);
        let Some(recorded) = recorded else {
            if demand_state.holds_permit() {
                anyhow::bail!("active demand {holder:?} has no permit holder");
            }
            return Ok(());
        };
        if recorded.pid == Some(std::process::id()) {
            let token = old_token
                .as_deref()
                .context("current process holds tokenless provision attempt")?;
            if !self.ledger.is_current_attempt(&holder, token)? {
                anyhow::bail!("provision attempt token for {holder:?} is stale");
            }
            if matches!(
                demand_state,
                DemandState::Terminal | DemandState::CanceledDone | DemandState::Declined
            ) {
                self.release_recovered_without_worker(&holder, request_id, token, demand_state)?;
            }
            return Ok(());
        }
        let (generation, permit_state, previous_pid) = self.require_dead_prior_owner(&holder)?;
        if let Some(token) = old_token.as_deref()
            && !self.ledger.is_current_attempt(&holder, token)?
        {
            anyhow::bail!("persisted provision attempt for {holder:?} is stale");
        }
        let target_token = self.rotate_staged_attempt(
            &holder,
            self.config.scale_set_id,
            request_id,
            identity.ownership().as_str().as_str(),
            old_token.as_deref(),
            generation,
            previous_pid,
            permit_state,
        )?;
        self.finish_staged_attempt_rotations()?;
        if demand_state == DemandState::Terminal
            || demand_state == DemandState::CanceledDone
            || demand_state == DemandState::Declined
        {
            self.release_recovered_without_worker(
                &holder,
                request_id,
                &target_token,
                demand_state,
            )?;
            return Ok(());
        }
        Ok(())
    }

    /// Fetch one runner's JIT config over the admin client. Awaited
    /// inline: a scoped `block_on` here self-deadlocks the runtime's
    /// I/O driver whenever no other thread keeps driving it. Borrows
    /// only the client: the lane itself is `!Sync` (SQLite handles),
    /// so holding `&self` across the await would un-`Send` the run
    /// task.
    async fn fetch_jit(
        client: &ScaleSetClient,
        scale_set_id: i32,
        runner_name: &str,
    ) -> Result<String> {
        let setting = RunnerScaleSetJitRunnerSetting {
            name: runner_name.to_owned(),
            work_folder: RUNNER_WORK_DIR.to_owned(),
        };
        let config = match client
            .generate_jit_runner_config(&setting, scale_set_id)
            .await
        {
            Ok(config) => config,
            Err(ref error)
                if error.fault() == Some(ScaleSetFault::RunnerExists)
                    || error.fault() == Some(ScaleSetFault::Conflict) =>
            {
                tracing::warn!(
                    runner_name,
                    "JIT configuration returned 409 Conflict (runner already exists); attempting cleanup"
                );
                if let Ok(Some(existing)) = client.get_runner_by_name(runner_name).await {
                    tracing::info!(
                        runner_name,
                        runner_id = existing.id,
                        "removing existing runner to unblock JIT configuration"
                    );
                    let _ = client.remove_runner(i64::from(existing.id)).await;
                }
                client
                    .generate_jit_runner_config(&setting, scale_set_id)
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "retry generate JIT config for runner {runner_name:?}: {error}"
                        )
                    })?
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "generate JIT config for runner {runner_name:?}: {error}"
                ));
            }
        };
        if config.encoded_jit_config.is_empty() {
            anyhow::bail!("empty JIT config for runner {runner_name:?}");
        }
        Ok(config.encoded_jit_config)
    }

    /// Move one held permit to `state`, re-reading the generation once on
    /// a fencing failure. A missing row is fine (a concurrent release won);
    /// anything else propagates and vetoes the ACK.
    fn fenced_transition(
        &mut self,
        holder: &str,
        state: LedgerPermitState,
        attempt_token: &str,
    ) -> Result<()> {
        for _ in 0..2 {
            let generation = self.ledger.generation()?;
            match self
                .ledger
                .transition(holder, state, generation, attempt_token)
            {
                Ok(()) => return Ok(()),
                Err(error) if SharedLedger::is_stale_generation(&error) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("ledger epoch moved twice under one lane transition for {holder:?}")
    }

    /// Retain worker occupancy and close its demand atomically after a
    /// cleanup failure. Retry one generation race; never turn an unknown
    /// holder into false free capacity.
    fn retain_uncertain(&mut self, holder: &str, attempt_token: &str) -> Result<()> {
        for _ in 0..2 {
            let generation = self.ledger.generation()?;
            match self
                .ledger
                .retain_uncertain(holder, generation, attempt_token)
            {
                Ok(()) => return Ok(()),
                Err(error) if SharedLedger::is_stale_generation(&error) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("ledger epoch moved twice while retaining uncertain holder {holder:?}")
    }

    /// Recorded state of one tracked worker.
    fn worker_state(&self, key: &str) -> Result<ScaleSetWorkerState> {
        self.workers
            .get(key)
            .map(|live| live.worker.state())
            .with_context(|| format!("live worker {key:?} is not tracked"))
    }

    /// Advance one tracked worker along a legal edge (registry-backed).
    fn transition_worker(&mut self, key: &str, to: ScaleSetWorkerState) -> Result<()> {
        let live = self
            .workers
            .get_mut(key)
            .with_context(|| format!("live worker {key:?} is not tracked"))?;
        let attempt_token = live.attempt_token.clone();
        let mut sink = AttemptBoundEdgeSink {
            registry: &mut self.registry,
            attempt_token: &attempt_token,
        };
        live.worker.transition(&mut sink, to)
    }

    /// Ensure a live entry for `key`, rebuilding it from the registry row
    /// (restart adoption) or failing when nothing durable names it.
    fn ensure_live(&mut self, key: &str) -> Result<()> {
        let row = self
            .registry
            .get(key)?
            .with_context(|| format!("no worker recorded for {key:?}"))?;
        let attempt_token = row
            .permit_attempt_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .context("worker row has no permit attempt token")?
            .to_owned();
        if let Some(live) = self.workers.get(key)
            && live.attempt_token != attempt_token
        {
            anyhow::bail!("worker {key:?} live entry has a stale permit attempt token");
        }
        let request_id = row
            .request_id
            .context("worker row has no request id for permit ownership check")?;
        let holder = permit_holder(self.config.scale_set_id, request_id);
        self.require_current_attempt(&holder, &attempt_token, row.worker_state)?;
        if self.workers.contains_key(key) {
            return Ok(());
        }
        let ownership = OwnershipId::bind(self.config.scale_set_id, &row.runner_name);
        let identity = WorkerIdentity::new(ownership);
        let state_dir = self.recorded_state_dir(&row)?;
        let deadline = match row.runner_start_deadline_epoch {
            Some(deadline) => Some(deadline),
            None if matches!(
                row.worker_state,
                ScaleSetWorkerState::ProvisionIntent | ScaleSetWorkerState::DindReady
            ) =>
            {
                Some(self.registry.set_runner_start_deadline_if_none(
                    key,
                    &attempt_token,
                    crate::scaleset::worker::supervise::epoch_seconds().saturating_add(
                        crate::scaleset::worker::supervise::RUNNER_START_TIMEOUT.as_secs(),
                    ),
                )?)
            }
            None => None,
        };
        let mut worker = ScaleSetWorker::new(identity.clone(), &row.operation_id);
        if let Some(request_id) = row.request_id {
            worker.bind_request(request_id);
            worker.bind_permit(&permit_holder(self.config.scale_set_id, request_id));
        }
        // Replay the recorded state through the transition table so the
        // in-memory record cannot disagree with the row (illegal recorded
        // states fail the adoption, surfacing corruption loudly).
        replay_recorded_state(&mut worker, row.worker_state)?;
        self.workers.insert(
            key.to_owned(),
            LiveWorker {
                worker,
                attempt_token,
                supervision: Supervision::from_runtime(
                    identity,
                    &state_dir,
                    row.dind_restarts_used,
                    deadline,
                ),
            },
        );
        Ok(())
    }

    fn ensure_live_for_token(&mut self, key: &str, attempt_token: &str) -> Result<()> {
        if attempt_token.is_empty() {
            anyhow::bail!("worker attempt token cannot be empty");
        }
        self.ensure_live(key)?;
        let row = self
            .registry
            .get(key)?
            .with_context(|| format!("no worker recorded for {key:?}"))?;
        let live = self
            .workers
            .get(key)
            .with_context(|| format!("live worker {key:?} is not tracked"))?;
        if live.attempt_token != attempt_token
            || row.permit_attempt_token.as_deref() != Some(attempt_token)
        {
            anyhow::bail!("worker {key:?} belongs to a different permit attempt");
        }
        let request_id = row
            .request_id
            .context("worker row has no request id for permit ownership check")?;
        self.require_current_attempt(
            &permit_holder(self.config.scale_set_id, request_id),
            attempt_token,
            row.worker_state,
        )?;
        Ok(())
    }

    /// Tick one worker's supervision. `WorkerFailed` drives the terminal
    /// path immediately (explicit fail with diagnostics + cleanup).
    fn tick_worker(&mut self, key: &str) -> Result<SupervisionOutcome, LaneError> {
        if !self.workers.contains_key(key) {
            let row = self
                .registry
                .get(key)
                .map_err(|error| LaneError::new("read worker row", error))?;
            if row.is_some_and(|row| provision_pending(row.worker_state)) {
                return Ok(SupervisionOutcome::Healthy);
            }
        }
        self.ensure_live(key)
            .map_err(|error| LaneError::new("adopt worker", error))?;
        let recorded = self
            .worker_state(key)
            .map_err(|error| LaneError::new("adopt worker", error))?;
        if recorded == ScaleSetWorkerState::PermitReleased {
            return Ok(SupervisionOutcome::Healthy);
        }
        let attempt_token = self
            .workers
            .get(key)
            .context("live worker vanished before supervision")
            .map_err(|error| LaneError::new("adopt worker", error))?
            .attempt_token
            .clone();
        let outcome = {
            let runner = &mut self.runner;
            let registry = &mut self.registry;
            let live = self
                .workers
                .get_mut(key)
                .with_context(|| format!("live worker {key:?} vanished"))
                .map_err(|error| LaneError::new("adopt worker", error))?;
            live.supervision
                .tick_with_runtime(
                    &mut **runner,
                    recorded,
                    crate::scaleset::worker::supervise::epoch_seconds(),
                    &mut |used| registry.set_dind_restarts_used(key, used, &attempt_token),
                )
                .map_err(|error| LaneError::new("supervise worker", error))?
        };
        if outcome == SupervisionOutcome::RunnerConnected {
            self.registry
                .clear_runner_start_deadline(key, &attempt_token)
                .map_err(|error| LaneError::new("clear runner startup deadline", error))?;
            if let Some(live) = self.workers.get_mut(key) {
                live.supervision.clear_runner_start_deadline();
            }
            match recorded {
                ScaleSetWorkerState::ProvisionIntent => {
                    self.transition_worker(key, ScaleSetWorkerState::DindReady)
                        .map_err(|error| LaneError::new("record DinD ready", error))?;
                    self.transition_worker(key, ScaleSetWorkerState::RunnerConnected)
                        .map_err(|error| LaneError::new("record runner connected", error))?;
                }
                ScaleSetWorkerState::DindReady => self
                    .transition_worker(key, ScaleSetWorkerState::RunnerConnected)
                    .map_err(|error| LaneError::new("record runner connected", error))?,
                _ => {}
            }
        }
        if matches!(outcome, SupervisionOutcome::WorkerFailed { .. }) {
            self.drive_terminal(key, &attempt_token)
                .map_err(|error| LaneError::new("fail worker", error))?;
        }
        Ok(outcome)
    }

    /// Opportunistic sweep over every live worker, throttled to
    /// [`LaneConfig::sweep_interval`]. Best-effort: failures are traced and
    /// the message path retries them; only the message path vetoes ACKs.
    fn opportunistic_sweep(&mut self) {
        let due = self
            .last_sweep
            .is_none_or(|at| at.elapsed() >= self.config.sweep_interval);
        if !due {
            return;
        }
        self.last_sweep = Some(Instant::now());
        match self.registry.list_live() {
            Ok(rows) => {
                for row in rows {
                    let Some(attempt_token) = row.permit_attempt_token.as_deref() else {
                        tracing::warn!(
                            worker = row.ownership_id.as_str(),
                            "terminal worker has no permit attempt token; retaining occupancy"
                        );
                        continue;
                    };
                    if terminal_side(row.worker_state)
                        && let Err(error) = self.drive_terminal(&row.ownership_id, attempt_token)
                    {
                        tracing::warn!(
                            worker = row.ownership_id.as_str(),
                            error = format!("{error:#}"),
                            "scale-set terminal cleanup retry failed"
                        );
                    }
                }
            }
            Err(error) => tracing::warn!(
                error = format!("{error:#}"),
                "scale-set terminal cleanup scan failed"
            ),
        }
        let keys: Vec<String> = self.workers.keys().cloned().collect();
        for key in keys {
            if self
                .registry
                .get(&key)
                .ok()
                .flatten()
                .is_some_and(|row| terminal_side(row.worker_state))
            {
                continue;
            }
            if let Err(error) = self.tick_worker(&key) {
                tracing::warn!(
                    worker = key.as_str(),
                    error = error.to_string(),
                    "scale-set background supervision failed; the message path will retry"
                );
            }
        }
    }

    /// Drive one worker `→ terminal → diagnostic_export → owned_cleanup`,
    /// then release its permit — or hold the permit `uncertain` when
    /// cleanup fails. Idempotent: replays converge (released stays
    /// released). A failed cleanup vetoes the ACK (the caller maps this
    /// error into the lane error): the message redelivers and the terminal
    /// path retries until cleanup confirms — durable cleanup before ACK.
    fn drive_terminal(&mut self, key: &str, attempt_token: &str) -> Result<()> {
        let outcome = self.drive_terminal_inner(key, attempt_token)?;
        match outcome {
            TerminalOutcome::Released | TerminalOutcome::AlreadyReleased => {
                self.workers.remove(key);
                Ok(())
            }
            TerminalOutcome::CleanupFailed { failures } => {
                // The durable row stays at its current cleanup phase; the
                // next message or idle sweep resumes that exact phase.
                anyhow::bail!("owned cleanup failed for {key}: {}", failures.join("; "))
            }
        }
    }

    fn drive_terminal_inner(&mut self, key: &str, attempt_token: &str) -> Result<TerminalOutcome> {
        // Unknown worker: nothing provisioned, only the permit (if held)
        // needs releasing. The Processor moved it to `cleaning` before
        // calling; the release below finishes it.
        let row = self.registry.get(key)?;
        let Some(mut row) = row else {
            if let Some(holder) = holder_for_key(self.config.scale_set_id, key) {
                let request_id = holder
                    .rsplit('/')
                    .next()
                    .and_then(|raw| raw.parse::<i64>().ok())
                    .context("terminal holder has no request id")?;
                self.release_attempt_with_projection(
                    &holder,
                    request_id,
                    attempt_token,
                    velnor_control::permit_ledger::DemandState::Terminal,
                    Some(DemandState::Terminal),
                    None,
                )?;
            }
            return Ok(TerminalOutcome::AlreadyReleased);
        };
        if row.permit_attempt_token.as_deref() != Some(attempt_token) {
            anyhow::bail!("terminal worker row {key:?} belongs to a different permit attempt");
        }
        if row.worker_state == ScaleSetWorkerState::PermitReleased {
            // The row says released, but a restart between the
            // adoption-time release and the completion observation lets
            // startup reconcile re-attest the permit from the
            // still-active demand row. Converge to the recorded truth.
            // The exported diagnostics stay on disk for post-mortem,
            // exactly like a fresh release: deletion ends owned Docker
            // objects, never the exported logs.
            if let Some(holder) = holder_for_key(self.config.scale_set_id, key) {
                let request_id = row
                    .request_id
                    .or_else(|| {
                        holder
                            .rsplit('/')
                            .next()
                            .and_then(|raw| raw.parse::<i64>().ok())
                    })
                    .context("released worker holder has no request id")?;
                self.release_attempt_with_projection(
                    &holder,
                    request_id,
                    attempt_token,
                    velnor_control::permit_ledger::DemandState::Terminal,
                    Some(DemandState::Terminal),
                    Some(&row.ownership_id),
                )?;
            }
            return Ok(TerminalOutcome::AlreadyReleased);
        }
        self.ensure_live_for_token(key, attempt_token)?;
        let mut recorded = self.worker_state(key)?;
        if !terminal_side(recorded) {
            self.transition_worker(key, ScaleSetWorkerState::Terminal)?;
            recorded = ScaleSetWorkerState::Terminal;
        }
        if recorded == ScaleSetWorkerState::Terminal {
            self.transition_worker(key, ScaleSetWorkerState::DiagnosticExport)?;
            recorded = ScaleSetWorkerState::DiagnosticExport;
        }
        if recorded == ScaleSetWorkerState::DiagnosticExport {
            let export = (|| {
                let live = self
                    .workers
                    .get_mut(key)
                    .with_context(|| format!("live worker {key:?} vanished"))?;
                if !row.diagnostics_complete {
                    live.supervision.clear_diagnostic_completion_marker()?;
                }
                live.supervision.prepare_cleanup(&mut *self.runner)
            })();
            let export = match export {
                Ok(export) => export,
                Err(error) => {
                    return self.record_cleanup_failure(
                        &row,
                        key,
                        vec![format!("prepare diagnostics: {error:#}")],
                    );
                }
            };
            if !export.failures.is_empty() {
                return self.record_cleanup_failure(&row, key, export.failures);
            }
            self.registry.set_diagnostics_complete(key, attempt_token)?;
            row.diagnostics_complete = true;
            self.transition_worker(key, ScaleSetWorkerState::OwnedCleanup)?;
            recorded = ScaleSetWorkerState::OwnedCleanup;
        }
        if recorded == ScaleSetWorkerState::OwnedCleanup {
            let recovery_export = (|| {
                let live = self
                    .workers
                    .get_mut(key)
                    .with_context(|| format!("live worker {key:?} vanished"))?;
                if !live.supervision.state_dir_exists()? {
                    return Ok(None);
                }
                if !row.diagnostics_complete {
                    live.supervision.clear_diagnostic_completion_marker()?;
                }
                if row.diagnostics_complete && live.supervision.diagnostics_complete()? {
                    return Ok(None);
                }
                live.supervision
                    .prepare_cleanup(&mut *self.runner)
                    .map(Some)
            })();
            let recovery_export = match recovery_export {
                Ok(export) => export,
                Err(error) => {
                    return self.record_cleanup_failure(
                        &row,
                        key,
                        vec![format!("recover diagnostics: {error:#}")],
                    );
                }
            };
            if let Some(export) = recovery_export {
                if !export.failures.is_empty() {
                    return self.record_cleanup_failure(&row, key, export.failures);
                }
                self.registry.set_diagnostics_complete(key, attempt_token)?;
            }
            let failures = {
                let live = self
                    .workers
                    .get_mut(key)
                    .with_context(|| format!("live worker {key:?} vanished"))?;
                live.supervision.teardown_owned_resources(&mut *self.runner)
            };
            if !failures.is_empty() {
                return self.record_cleanup_failure(&row, key, failures);
            }
            // The state dir is deliberately NOT deleted here: the exported
            // diagnostics survive the worker for post-mortem ("diagnostics
            // exported before deletion" — deletion ends owned Docker
            // objects, never the exported logs).
        }
        if let Some(request_id) = row.request_id {
            let holder = permit_holder(self.config.scale_set_id, request_id);
            self.release_attempt_with_projection(
                &holder,
                request_id,
                attempt_token,
                velnor_control::permit_ledger::DemandState::Terminal,
                Some(DemandState::Terminal),
                Some(&row.ownership_id),
            )?;
        } else if let Some(holder) = holder_for_key(self.config.scale_set_id, key) {
            let request_id = holder
                .rsplit('/')
                .next()
                .and_then(|raw| raw.parse::<i64>().ok())
                .context("terminal worker holder has no request id")?;
            self.release_attempt_with_projection(
                &holder,
                request_id,
                attempt_token,
                velnor_control::permit_ledger::DemandState::Terminal,
                None,
                Some(&row.ownership_id),
            )?;
        }
        Ok(TerminalOutcome::Released)
    }

    /// Whether the terminal path already claimed `key`. Only
    /// `drive_terminal_inner` writes terminal-side states, so a
    /// terminal-side row after a failed tick proves the worker failed
    /// explicitly (cleanup unconfirmed, permit retained uncertain) as
    /// opposed to a tick that never got that far. Unreadable rows answer
    /// conservatively: keep the worker tracked.
    fn terminal_cleanup_started(&self, key: &str) -> bool {
        self.registry
            .get(key)
            .map(|row| row.is_some_and(|row| terminal_side(row.worker_state)))
            .unwrap_or(false)
    }

    fn record_cleanup_failure(
        &mut self,
        row: &WorkerRow,
        key: &str,
        failures: Vec<String>,
    ) -> Result<TerminalOutcome> {
        if let Some(request_id) = row.request_id {
            let holder = permit_holder(self.config.scale_set_id, request_id);
            let attempt_token = row
                .permit_attempt_token
                .as_deref()
                .context("cleanup failure row has no permit attempt token")?;
            self.retain_uncertain(&holder, attempt_token)?;
        }
        tracing::warn!(
            worker = key,
            failures = failures.join("; ").as_str(),
            "scale-set terminal cleanup failed; permit retained uncertain"
        );
        Ok(TerminalOutcome::CleanupFailed { failures })
    }

    /// Adopt-or-fail every recorded worker (startup + crash recovery).
    ///
    /// For each provision intent: a live container pair is adopted into the
    /// live map (permits + demand rows keep their states, so the restarted
    /// loop resumes supervision without re-provisioning); a dead or
    /// missing pair is failed explicitly through the terminal path. Fully
    /// released workers are skipped. Never deletes the scale set, never
    /// writes demand, never fabricates a completion.
    pub fn adopt_live_workers(&mut self) -> Result<AdoptReport> {
        self.recovery_claim_token
            .as_deref()
            .filter(|claim_token| !claim_token.is_empty())
            .context("scale-set worker adoption requires a recovery claim")?;
        self.refresh_generation()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        self.recover_durable_attempts()
            .context("recover staged Scale Set permit attempts before supervision")?;
        let mut report = AdoptReport::default();
        let mut latest_by_request = HashMap::new();
        for intent in self.intents.list_for_set(self.config.scale_set_id)? {
            latest_by_request.insert(intent.request_id, intent);
        }
        for intent in latest_by_request.values() {
            let key = Self::ownership_key(intent);
            let row = self.registry.get(&key)?;
            let demand = self
                .demand
                .get(intent.request_id)?
                .context("provision intent has no demand row")?;
            let holder = permit_holder(self.config.scale_set_id, intent.request_id);
            let Some(row) = row else {
                // WorkerRegistry is written before the first Docker call,
                // so its absence is an exact no-resource proof. Rebind the
                // persisted attempt only after proving the old process dead.
                if self.ledger.holder_state(&holder)?.is_some() {
                    self.recover_attempt_without_worker(intent, demand.state)?;
                } else if demand.state.holds_permit() {
                    anyhow::bail!("active demand {holder:?} has no permit holder");
                }
                if matches!(
                    demand.state,
                    DemandState::Acquired | DemandState::ProvisionIntent
                ) {
                    report.awaiting_provision += 1;
                }
                continue;
            };
            if row.worker_state == ScaleSetWorkerState::PermitReleased {
                if self.ledger.holder_state(&holder)?.is_some() {
                    self.recover_worker_attempt(&row, intent)?;
                } else {
                    let identity = WorkerIdentity::new(OwnershipId::bind(
                        self.config.scale_set_id,
                        &row.runner_name,
                    ));
                    if self.verify_recovery_containers(&identity)? != (false, false) {
                        anyhow::bail!("released worker {holder:?} still has an owned container");
                    }
                }
                report.skipped_released += 1;
                continue;
            }
            self.recover_worker_attempt(&row, intent)?;
            let current_row = self
                .registry
                .get(&key)?
                .context("worker row vanished after attempt adoption")?;
            let attempt_token = current_row
                .permit_attempt_token
                .as_deref()
                .context("adopted worker row lost its attempt token")?
                .to_owned();
            self.ensure_live_for_token(&key, &attempt_token)?;
            if provision_pending(current_row.worker_state) {
                report.awaiting_provision += 1;
                continue;
            }
            if terminal_side(row.worker_state) {
                // Crash during cleanup: resume the terminal path now. A
                // still-failing cleanup must not fail adoption (that would
                // wedge daemon startup): the worker stays tracked at
                // `owned_cleanup` and the message path retries it.
                if let Err(error) = self.drive_terminal(&key, &attempt_token) {
                    tracing::warn!(
                        worker = key.as_str(),
                        error = format!("{error:#}"),
                        "scale-set adoption cleanup failed; the message path will retry"
                    );
                }
                report.resumed_cleanup += 1;
                continue;
            }
            match self.tick_worker(&key) {
                Ok(SupervisionOutcome::Healthy | SupervisionOutcome::DindRestarted { .. }) => {
                    report.adopted += 1;
                }
                Ok(SupervisionOutcome::RunnerConnected) => {
                    report.adopted += 1;
                }
                Ok(SupervisionOutcome::WorkerFailed { .. }) => {
                    // `tick_worker` already drove the terminal path.
                    report.failed += 1;
                }
                Err(error) => {
                    // A tick that reached the terminal path failed the
                    // worker explicitly (terminal-side row, permit
                    // retained uncertain): that is a failure, never an
                    // adoption. Anything earlier (Docker unreachable,
                    // restart impossible) keeps the worker tracked; the
                    // message path retries the tick. The permit stays
                    // held either way — never freed blind.
                    if self.terminal_cleanup_started(&key) {
                        tracing::warn!(
                            worker = key.as_str(),
                            error = error.to_string(),
                            "scale-set adoption terminal cleanup failed; the message path will retry"
                        );
                        report.failed += 1;
                    } else {
                        tracing::warn!(
                            worker = key.as_str(),
                            error = error.to_string(),
                            "scale-set adoption tick failed; worker stays tracked"
                        );
                        report.adopted += 1;
                    }
                }
            }
        }
        Ok(report)
    }

    /// Graceful-shutdown pass: tick every live worker once. Healthy
    /// workers are left running for the restarted daemon to adopt
    /// (containers, demand rows, and permits untouched); dead workers go
    /// through the explicit terminal path. Never marks demand complete.
    pub fn shutdown_pass(&mut self) -> Result<ShutdownReport> {
        let mut report = ShutdownReport::default();
        let keys: Vec<String> = self.workers.keys().cloned().collect();
        for key in keys {
            match self.tick_worker(&key) {
                Ok(_) => {
                    if self.workers.contains_key(&key) {
                        report.adopted_across_restart += 1;
                    } else {
                        report.failed += 1;
                    }
                }
                Err(error) => {
                    // Same taxonomy as adoption: a tick that reached the
                    // terminal path failed the worker explicitly, even
                    // though cleanup stays unconfirmed for the restart.
                    if self.terminal_cleanup_started(&key) {
                        tracing::warn!(
                            worker = key.as_str(),
                            error = error.to_string(),
                            "scale-set shutdown terminal cleanup failed; restart adoption resumes it"
                        );
                        report.failed += 1;
                    } else {
                        tracing::warn!(
                            worker = key.as_str(),
                            error = error.to_string(),
                            "scale-set shutdown tick failed; worker left for restart adoption"
                        );
                        report.adopted_across_restart += 1;
                    }
                }
            }
        }
        // Registry rows the live map never learned (provisioned, then the
        // process died before any observation): left recorded; the
        // restarted daemon's adoption pass triages them.
        report.recorded_total = self.registry.list_live()?.len() as u64;
        Ok(report)
    }

    /// Live workers currently tracked (observability for the daemon).
    #[must_use]
    pub fn live_workers(&self) -> usize {
        self.workers.len()
    }
}

/// Replay `target` from `Observed` through the transition table so a
/// rebuilt record cannot disagree with its durable row.
fn replay_recorded_state(worker: &mut ScaleSetWorker, target: ScaleSetWorkerState) -> Result<()> {
    use ScaleSetWorkerState as S;
    // Forward chain; the table's retry/back edges never need replay.
    const CHAIN: [S; 13] = [
        S::Observed,
        S::Eligible,
        S::Reserved,
        S::AcquireIntent,
        S::Acquired,
        S::ProvisionIntent,
        S::DindReady,
        S::RunnerConnected,
        S::Running,
        S::Terminal,
        S::DiagnosticExport,
        S::OwnedCleanup,
        S::PermitReleased,
    ];
    // `Uncertain` sits off the happy path: it replays as `Acquired` (the
    // state it resolves forward from) — adoption re-ticks from there.
    let goal = if target == S::Uncertain {
        S::Acquired
    } else {
        target
    };
    let goal_index = CHAIN.iter().position(|s| *s == goal).unwrap_or(0);
    let mut sink = VecEdgeSink::default();
    for (index, state) in CHAIN.iter().enumerate() {
        if worker.state() == goal || index > goal_index {
            break;
        }
        if *state == S::Observed {
            continue;
        }
        worker.transition(&mut sink, *state)?;
    }
    if worker.state() != goal {
        anyhow::bail!(
            "cannot replay worker state {} from observed",
            target.as_str()
        );
    }
    Ok(())
}

fn terminal_side(state: ScaleSetWorkerState) -> bool {
    matches!(
        state,
        ScaleSetWorkerState::Terminal
            | ScaleSetWorkerState::DiagnosticExport
            | ScaleSetWorkerState::OwnedCleanup
    )
}

fn provision_pending(state: ScaleSetWorkerState) -> bool {
    matches!(
        state,
        ScaleSetWorkerState::Observed
            | ScaleSetWorkerState::Eligible
            | ScaleSetWorkerState::Reserved
            | ScaleSetWorkerState::AcquireIntent
            | ScaleSetWorkerState::Acquired
            | ScaleSetWorkerState::Uncertain
            | ScaleSetWorkerState::ProvisionIntent
    )
}

/// Whether the worker already has (or had) containers: replays at these
/// states only need a health tick, not a fresh provision.
fn post_provision(state: ScaleSetWorkerState) -> bool {
    matches!(
        state,
        ScaleSetWorkerState::DindReady
            | ScaleSetWorkerState::RunnerConnected
            | ScaleSetWorkerState::Running
            | ScaleSetWorkerState::Terminal
            | ScaleSetWorkerState::DiagnosticExport
            | ScaleSetWorkerState::OwnedCleanup
            | ScaleSetWorkerState::PermitReleased
    )
}

/// Recover the ledger holder for an ownership key of the canonical form
/// `<set>/<runner-name>` where the runner name embeds the request id
/// (`velnor-<set>-<request>`). Returns `None` for foreign shapes — the
/// caller then releases nothing instead of guessing.
fn holder_for_key(scale_set_id: i32, key: &str) -> Option<String> {
    let (key_set, name) = key.split_once('/')?;
    let key_set = key_set.parse::<i32>().ok()?;
    let (name_set, request) = crate::scaleset::intents::parse_runner_name(name)?;
    (key_set == scale_set_id && name_set == scale_set_id)
        .then_some(permit_holder(scale_set_id, request))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TerminalOutcome {
    Released,
    AlreadyReleased,
    CleanupFailed { failures: Vec<String> },
}

/// What [`DaemonWorkerLane::adopt_live_workers`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdoptReport {
    pub adopted: u64,
    pub failed: u64,
    pub resumed_cleanup: u64,
    pub awaiting_provision: u64,
    pub skipped_released: u64,
}

/// What [`DaemonWorkerLane::shutdown_pass`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Healthy workers left running for restart adoption.
    pub adopted_across_restart: u64,
    /// Dead workers failed explicitly with cleanup.
    pub failed: u64,
    /// Durable live rows after the pass (the restart's adoption input).
    pub recorded_total: u64,
}

impl WorkerLane for DaemonWorkerLane {
    type Error = LaneError;

    async fn provision(&mut self, intent: &ProvisionIntent) -> Result<(), Self::Error> {
        self.refresh_generation()?;
        let key = Self::ownership_key(intent);
        let attempt_token = intent
            .permit_attempt_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                LaneError::new(
                    "provision worker",
                    anyhow::anyhow!("provision intent has no permit attempt token"),
                )
            })?
            .to_owned();
        if intent.scale_set_id != self.config.scale_set_id {
            return Err(LaneError::new(
                "provision worker",
                anyhow::anyhow!("provision intent belongs to another scale set"),
            ));
        }
        let holder = permit_holder(intent.scale_set_id, intent.request_id);
        self.require_current_attempt(&holder, &attempt_token, ScaleSetWorkerState::Acquired)
            .map_err(|error| LaneError::new("verify permit attempt", error))?;
        self.opportunistic_sweep();

        // Idempotent replay: a live post-provision worker for the same
        // operation only needs a health tick.
        if self.workers.contains_key(&key) {
            if self
                .workers
                .get(&key)
                .is_some_and(|live| live.attempt_token != attempt_token)
            {
                return Err(LaneError::new(
                    "stale provision",
                    anyhow::anyhow!("worker {key} belongs to a different permit attempt"),
                ));
            }
            let same_operation = self
                .workers
                .get(&key)
                .is_some_and(|live| live.worker.operation_id() == intent.operation_id);
            let recorded = self
                .worker_state(&key)
                .map_err(|error| LaneError::new("read worker state", error))?;
            if same_operation && post_provision(recorded) {
                let outcome = self.tick_worker(&key)?;
                if !matches!(outcome, SupervisionOutcome::WorkerFailed { .. }) {
                    return Ok(());
                }
                // Failed: fall through and re-provision under the same
                // ownership (adopt-or-create converges on the same names).
            }
        }

        // Terminal-side rows never re-provision: the loop only provisions
        // acquired-without-intent rows, so a terminal row here is a stale
        // redelivery, not work.
        if let Some(row) = self
            .registry
            .get(&key)
            .map_err(|error| LaneError::new("read worker row", error))?
            && (terminal_side(row.worker_state)
                || row.worker_state == ScaleSetWorkerState::PermitReleased)
        {
            return Err(LaneError::new(
                "stale provision",
                anyhow::anyhow!("worker {key} is already terminal; refusing to re-provision"),
            ));
        }

        let jit_config =
            Self::fetch_jit(&self.client, self.config.scale_set_id, &intent.runner_name)
                .await
                .map_err(|error| LaneError::new("fetch JIT config", error))?;
        self.intents
            .record_jit_fingerprint(&intent.operation_id, &jit_fingerprint(&jit_config))
            .map_err(|error| LaneError::new("record JIT fingerprint", error))?;

        let ownership = OwnershipId::bind(intent.scale_set_id, &intent.runner_name);
        let identity = WorkerIdentity::new(ownership.clone());
        let proposed_state_dir = self.worker_state_dir(&ownership);
        // The registry row precedes the first Docker call: a crash between
        // Docker calls still converges on these exact names.
        let row = self
            .registry
            .upsert(
                &key,
                &intent.operation_id,
                intent.request_id,
                &intent.runner_name,
                &identity.network(),
                proposed_state_dir
                    .join("workspace")
                    .to_string_lossy()
                    .as_ref(),
                proposed_state_dir
                    .join("dind-data")
                    .to_string_lossy()
                    .as_ref(),
                &intent.runner_digest,
                &intent.dind_digest,
                &attempt_token,
            )
            .map_err(|error| LaneError::new("record worker", error))?;
        let state_dir = self
            .recorded_state_dir(&row)
            .map_err(|error| LaneError::new("read worker state path", error))?;

        let mut worker = ScaleSetWorker::new(identity.clone(), &intent.operation_id);
        worker.bind_request(intent.request_id);
        worker.bind_permit(&holder);
        // Replay to the recorded state (a retry adopts the row's progress),
        // then walk forward to `provision_intent`. Forward-only: a crash
        // between prefix edges replays mid-chain, and backward edges are
        // illegal in the transition table.
        replay_recorded_state(&mut worker, row.worker_state)
            .map_err(|error| LaneError::new("replay worker state", error))?;
        let path: &[ScaleSetWorkerState] = match worker.state() {
            ScaleSetWorkerState::Observed => &[
                ScaleSetWorkerState::Eligible,
                ScaleSetWorkerState::Reserved,
                ScaleSetWorkerState::AcquireIntent,
                ScaleSetWorkerState::Acquired,
                ScaleSetWorkerState::ProvisionIntent,
            ],
            ScaleSetWorkerState::Eligible => &[
                ScaleSetWorkerState::Reserved,
                ScaleSetWorkerState::AcquireIntent,
                ScaleSetWorkerState::Acquired,
                ScaleSetWorkerState::ProvisionIntent,
            ],
            ScaleSetWorkerState::Reserved => &[
                ScaleSetWorkerState::AcquireIntent,
                ScaleSetWorkerState::Acquired,
                ScaleSetWorkerState::ProvisionIntent,
            ],
            ScaleSetWorkerState::AcquireIntent => &[
                ScaleSetWorkerState::Acquired,
                ScaleSetWorkerState::ProvisionIntent,
            ],
            ScaleSetWorkerState::Acquired => &[ScaleSetWorkerState::ProvisionIntent],
            // `provision_intent` and later: already there (terminal-side
            // rows are refused above, never re-provisioned).
            _ => &[],
        };
        for edge in path {
            let mut sink = AttemptBoundEdgeSink {
                registry: &mut self.registry,
                attempt_token: &attempt_token,
            };
            worker
                .transition(&mut sink, *edge)
                .map_err(|error| LaneError::new("advance worker", error))?;
        }

        let plan = ProvisionPlan {
            identity: identity.clone(),
            profile: self.config.profile.clone(),
            state_dir: state_dir.clone(),
            jit_config,
            ready_attempts: self.config.ready_attempts,
        };
        let mut runner_start_deadline_epoch = row.runner_start_deadline_epoch;
        let outcome = {
            let registry = &mut self.registry;
            let ownership_key = key.clone();
            let mut before_runner_start = || {
                let deadline = registry.set_runner_start_deadline_if_none(
                    &ownership_key,
                    &attempt_token,
                    crate::scaleset::worker::supervise::epoch_seconds().saturating_add(
                        crate::scaleset::worker::supervise::RUNNER_START_TIMEOUT.as_secs(),
                    ),
                )?;
                runner_start_deadline_epoch = Some(deadline);
                Ok(())
            };
            let runner = &mut self.runner;
            let hook = &self.hook;
            provision_worker(
                &mut **runner,
                &**hook,
                &plan,
                &std::thread::sleep,
                &mut before_runner_start,
            )
        }
        .map_err(|error| LaneError::new("provision worker pair", error))?;
        if outcome.connection == RunnerConnection::Connected {
            self.registry
                .clear_runner_start_deadline(&key, &attempt_token)
                .map_err(|error| LaneError::new("clear runner startup deadline", error))?;
            runner_start_deadline_epoch = None;
        }
        worker.record_versions(
            &outcome.runner_attestation.content_version,
            &outcome.dind_attestation.content_version,
        );
        if worker.state() == ScaleSetWorkerState::ProvisionIntent {
            let mut sink = AttemptBoundEdgeSink {
                registry: &mut self.registry,
                attempt_token: &attempt_token,
            };
            worker
                .transition(&mut sink, ScaleSetWorkerState::DindReady)
                .map_err(|error| LaneError::new("record DinD ready", error))?;
        }
        if outcome.connection == RunnerConnection::Connected
            && worker.state() == ScaleSetWorkerState::DindReady
        {
            let mut sink = AttemptBoundEdgeSink {
                registry: &mut self.registry,
                attempt_token: &attempt_token,
            };
            worker
                .transition(&mut sink, ScaleSetWorkerState::RunnerConnected)
                .map_err(|error| LaneError::new("record runner connected", error))?;
        }
        self.fenced_transition(&holder, LedgerPermitState::Provisioning, &attempt_token)
            .map_err(|error| LaneError::new("mark permit provisioning", error))?;
        self.workers.insert(
            key,
            LiveWorker {
                worker,
                attempt_token,
                supervision: Supervision::from_runtime(
                    identity,
                    &state_dir,
                    row.dind_restarts_used,
                    runner_start_deadline_epoch,
                ),
            },
        );
        Ok(())
    }

    fn note_assigned(
        &mut self,
        assigned: &ScaleSetJobAssigned,
        attempt_token: &str,
    ) -> Result<(), Self::Error> {
        self.refresh_generation()?;
        self.opportunistic_sweep();
        let request_id = crate::scaleset::demand::resolve_job_request_id(&assigned.base);
        let Some(intent) = self
            .intents
            .get_by_request(self.config.scale_set_id, request_id)
            .map_err(|error| LaneError::new("find provision intent", error))?
        else {
            // Assigned before we provisioned (redelivery race): the loop's
            // step 5 provisions from the demand row; nothing to tick yet.
            return Ok(());
        };
        let key = Self::ownership_key(&intent);
        if intent.permit_attempt_token.as_deref() != Some(attempt_token) {
            return Err(LaneError::new(
                "observe job assigned",
                anyhow::anyhow!("provision intent belongs to a different permit attempt"),
            ));
        }
        let known = self
            .registry
            .get(&key)
            .map_err(|error| LaneError::new("read worker row", error))?
            .is_some();
        if known {
            self.ensure_live_for_token(&key, attempt_token)
                .map_err(|error| LaneError::new("verify worker attempt", error))?;
            self.tick_worker(&key)?;
        }
        Ok(())
    }

    fn note_started(
        &mut self,
        started: &ScaleSetJobStarted,
        attempt_token: &str,
    ) -> Result<(), Self::Error> {
        self.refresh_generation()?;
        self.opportunistic_sweep();
        let key = if !started.runner_name.is_empty() {
            let Some((scale_set_id, _)) =
                crate::scaleset::intents::parse_runner_name(&started.runner_name)
            else {
                return Ok(());
            };
            if scale_set_id != self.config.scale_set_id {
                return Ok(());
            }
            OwnershipId::bind(self.config.scale_set_id, &started.runner_name)
                .as_str()
                .to_string()
        } else {
            let request_id = crate::scaleset::demand::resolve_job_request_id(&started.base);
            let Some(intent) = self
                .intents
                .get_by_request(self.config.scale_set_id, request_id)
                .map_err(|error| LaneError::new("find provision intent", error))?
            else {
                return Ok(());
            };
            Self::ownership_key(&intent)
        };
        let known = self
            .registry
            .get(&key)
            .map_err(|error| LaneError::new("read worker row", error))?
            .is_some();
        if !known {
            return Ok(());
        }
        self.ensure_live_for_token(&key, attempt_token)
            .map_err(|error| LaneError::new("verify worker attempt", error))?;
        let outcome = self.tick_worker(&key)?;
        if matches!(outcome, SupervisionOutcome::WorkerFailed { .. }) {
            // The tick already failed the worker explicitly; GitHub owns
            // the job outcome from here (the completion observation
            // converges the demand row).
            return Ok(());
        }
        // Advance the record toward `running` along the happy path only;
        // terminal-side and retry states are owned by their own paths.
        let recorded = self
            .worker_state(&key)
            .map_err(|error| LaneError::new("read worker state", error))?;
        let path: &[ScaleSetWorkerState] = match recorded {
            ScaleSetWorkerState::ProvisionIntent => &[
                ScaleSetWorkerState::DindReady,
                ScaleSetWorkerState::RunnerConnected,
                ScaleSetWorkerState::Running,
            ],
            ScaleSetWorkerState::DindReady => &[
                ScaleSetWorkerState::RunnerConnected,
                ScaleSetWorkerState::Running,
            ],
            ScaleSetWorkerState::RunnerConnected => &[ScaleSetWorkerState::Running],
            _ => &[],
        };
        for edge in path {
            self.transition_worker(&key, *edge)
                .map_err(|error| LaneError::new("record job started", error))?;
        }
        let request_id = crate::scaleset::demand::resolve_job_request_id(&started.base);
        let holder = holder_for_key(self.config.scale_set_id, &key)
            .unwrap_or_else(|| permit_holder(self.config.scale_set_id, request_id));
        self.fenced_transition(&holder, LedgerPermitState::Running, attempt_token)
            .map_err(|error| LaneError::new("mark permit running", error))?;
        Ok(())
    }

    fn note_terminal(
        &mut self,
        completed: &ScaleSetJobCompleted,
        attempt_token: &str,
    ) -> Result<(), Self::Error> {
        self.refresh_generation()?;
        self.opportunistic_sweep();
        let key = if !completed.runner_name.is_empty() {
            let Some((scale_set_id, _)) =
                crate::scaleset::intents::parse_runner_name(&completed.runner_name)
            else {
                return Ok(());
            };
            if scale_set_id != self.config.scale_set_id {
                return Ok(());
            }
            OwnershipId::bind(self.config.scale_set_id, &completed.runner_name)
                .as_str()
                .to_string()
        } else {
            let request_id = crate::scaleset::demand::resolve_job_request_id(&completed.base);
            self.terminal_key(request_id)?
        };
        self.drive_terminal(&key, attempt_token)
            .map_err(|error| LaneError::new("drive worker terminal", error))
    }

    fn note_canceled(&mut self, request_id: i64, attempt_token: &str) -> Result<(), Self::Error> {
        self.note_terminal_request(request_id, attempt_token)
    }

    fn idle_tick(&mut self) -> Result<(), Self::Error> {
        self.refresh_generation()?;
        self.opportunistic_sweep();
        Ok(())
    }

    fn owns_terminal_cleanup(&self, request_id: i64) -> Result<bool, Self::Error> {
        let key = self.terminal_key(request_id)?;
        let owned = self
            .registry
            .get(&key)
            .map_err(|error| LaneError::new("read worker row", error))?
            .is_some();
        Ok(owned)
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
    use crate::scaleset::intents::AcquireBatchStore;
    use crate::scaleset::worker::ownership::OWNERSHIP_LABEL;

    struct CleanupRunner {
        fail_runner_remove: bool,
        runner_name: String,
        ownership: Option<String>,
        seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    }

    impl CleanupRunner {
        fn missing(
            runner_name: String,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            Self {
                fail_runner_remove: false,
                runner_name,
                ownership: None,
                seen,
            }
        }

        fn fail_runner_remove(
            runner_name: String,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            Self {
                fail_runner_remove: true,
                runner_name,
                ownership: None,
                seen,
            }
        }

        fn with_ownership(mut self, ownership: &str) -> Self {
            self.ownership = Some(ownership.to_owned());
            self
        }
    }

    impl WorkerRunner for CleanupRunner {
        fn run(
            &mut self,
            program: &str,
            args: &[String],
        ) -> anyhow::Result<crate::scaleset::worker::WorkerOutput> {
            assert_eq!(program, "docker");
            self.seen.lock().unwrap().push(args.to_vec());
            if args.first().is_some_and(|arg| arg == "logs") {
                return Ok(crate::scaleset::worker::WorkerOutput {
                    code: 0,
                    stdout: "diagnostic log\n".to_owned(),
                    stderr: String::new(),
                });
            }
            if args.first().is_some_and(|arg| arg == "inspect") {
                if args.iter().any(|arg| arg == "{{.Id}}") {
                    return Ok(crate::scaleset::worker::WorkerOutput {
                        code: 0,
                        stdout: "container-id\n".to_owned(),
                        stderr: String::new(),
                    });
                }
                if args.iter().any(|arg| {
                    arg == r#"{{range $k, $v := .Config.Labels}}{{$k}}={{$v}}{{"\n"}}{{end}}"#
                }) {
                    let ownership = self.ownership.as_deref().unwrap_or("test-ownership");
                    return Ok(crate::scaleset::worker::WorkerOutput {
                        code: 0,
                        stdout: format!("{}={ownership}\n", OWNERSHIP_LABEL),
                        stderr: String::new(),
                    });
                }
                return Ok(crate::scaleset::worker::WorkerOutput {
                    code: 0,
                    stdout: r#"[{"Id":"id","Name":"/worker","State":{"Status":"exited"},"NetworkSettings":{},"Config":{"Env":["JIT_SECRET=sentinel"],"Labels":{"owner":"test"}}}]"#.to_owned(),
                    stderr: String::new(),
                });
            }
            if self.fail_runner_remove
                && args.iter().any(|arg| arg == &self.runner_name)
                && args.iter().any(|arg| arg == "rm")
            {
                self.fail_runner_remove = false;
                return Ok(crate::scaleset::worker::WorkerOutput {
                    code: 1,
                    stdout: String::new(),
                    stderr: "container is busy".to_owned(),
                });
            }
            Ok(crate::scaleset::worker::WorkerOutput {
                code: 1,
                stdout: String::new(),
                stderr: "Error: No such object".to_owned(),
            })
        }
    }

    struct RecoveryRunner {
        containers: HashMap<String, String>,
    }

    impl WorkerRunner for RecoveryRunner {
        fn run(
            &mut self,
            program: &str,
            args: &[String],
        ) -> anyhow::Result<crate::scaleset::worker::WorkerOutput> {
            assert_eq!(program, "docker");
            let missing = || crate::scaleset::worker::WorkerOutput {
                code: 1,
                stdout: String::new(),
                stderr: "Error: No such object".to_owned(),
            };
            let present = || crate::scaleset::worker::WorkerOutput {
                code: 0,
                stdout: "container-id\n".to_owned(),
                stderr: String::new(),
            };
            let Some(name) = args.last() else {
                anyhow::bail!("docker inspect omitted container name");
            };
            if args.first().is_some_and(|arg| arg == "inspect")
                && args.iter().any(|arg| arg == "{{.Id}}")
            {
                return Ok(if self.containers.contains_key(name) {
                    present()
                } else {
                    missing()
                });
            }
            if args.first().is_some_and(|arg| arg == "inspect")
                && args.iter().any(|arg| {
                    arg == r#"{{range $k, $v := .Config.Labels}}{{$k}}={{$v}}{{"\n"}}{{end}}"#
                })
            {
                let Some(ownership) = self.containers.get(name) else {
                    return Ok(missing());
                };
                return Ok(crate::scaleset::worker::WorkerOutput {
                    code: 0,
                    stdout: format!("{}={ownership}\n", OWNERSHIP_LABEL),
                    stderr: String::new(),
                });
            }
            Ok(missing())
        }
    }

    struct FailOnceRecoveryRunner {
        inner: RecoveryRunner,
        fail_on_container: String,
        fail_once: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl WorkerRunner for FailOnceRecoveryRunner {
        fn run(
            &mut self,
            program: &str,
            args: &[String],
        ) -> anyhow::Result<crate::scaleset::worker::WorkerOutput> {
            if args
                .last()
                .is_some_and(|name| name == &self.fail_on_container)
                && self
                    .fail_once
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                anyhow::bail!("injected resource inspection failure after prior rotation");
            }
            self.inner.run(program, args)
        }
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "velnor-lane-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn test_offer(request_id: i64) -> velnor_model::ScaleSetJobAvailable {
        velnor_model::ScaleSetJobAvailable {
            acquire_job_url: String::new(),
            base: velnor_model::ScaleSetJobMessage {
                message_type: velnor_model::ScaleSetJobMessageType::JobAvailable,
                runner_request_id: request_id,
                repository_name: "velnor".to_owned(),
                owner_name: "tailrocks".to_owned(),
                job_id: format!("job-{request_id}"),
                job_workflow_ref: String::new(),
                job_display_name: String::new(),
                workflow_run_id: 0,
                event_name: "push".to_owned(),
                request_labels: Vec::new(),
                queue_time: String::new(),
                scale_set_assign_time: String::new(),
                runner_assign_time: String::new(),
                finish_time: String::new(),
            },
        }
    }

    #[test]
    fn worker_edge_uses_the_captured_token_after_registry_rotation() {
        let dir = unique_test_dir("captured-edge-token");
        let db = dir.join("state.db");
        let ownership = OwnershipId::bind(7, "velnor-7-901");
        let key = ownership.as_str();
        let identity = WorkerIdentity::new(ownership);
        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry.set_generation(1);
        registry
            .upsert(
                &key,
                "op-901",
                901,
                "velnor-7-901",
                &identity.network(),
                "/tmp/worker/workspace",
                "/tmp/worker/dind-data",
                "sha256:runner",
                "sha256:dind",
                "attempt-old",
            )
            .unwrap();
        let mut worker = ScaleSetWorker::new(identity, "op-901");
        worker.bind_request(901);
        worker.bind_permit("scaleset/7/901");
        let mut old_sink = AttemptBoundEdgeSink {
            registry: &mut registry,
            attempt_token: "attempt-old",
        };
        worker
            .transition(&mut old_sink, ScaleSetWorkerState::Eligible)
            .unwrap();

        registry
            .conn
            .execute(
                "UPDATE scaleset_workers SET permit_attempt_token = 'attempt-new'
                 WHERE ownership_id = ?1",
                params![key],
            )
            .unwrap();
        let mut stale_sink = AttemptBoundEdgeSink {
            registry: &mut registry,
            attempt_token: "attempt-old",
        };
        assert!(worker
            .transition(&mut stale_sink, ScaleSetWorkerState::Reserved)
            .is_err());
        assert_eq!(worker.state(), ScaleSetWorkerState::Eligible);
        assert_eq!(
            registry.get(&key).unwrap().unwrap().worker_state,
            ScaleSetWorkerState::Eligible
        );

        let mut current_sink = AttemptBoundEdgeSink {
            registry: &mut registry,
            attempt_token: "attempt-new",
        };
        worker
            .transition(&mut current_sink, ScaleSetWorkerState::Reserved)
            .unwrap();
        assert_eq!(
            registry.get(&key).unwrap().unwrap().worker_state,
            ScaleSetWorkerState::Reserved
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn test_lane(
        db: &Path,
        ledger: &Path,
        state_root: &Path,
        runner: Box<dyn WorkerRunner + Send>,
    ) -> DaemonWorkerLane {
        let mut control_ledger = velnor_control::permit_ledger::PermitLedger::open(ledger).unwrap();
        let recovery_claim = control_ledger.claim_scaleset_recovery(&|_| false).unwrap();
        let recovery_claim_token = recovery_claim.token().to_owned();
        let client = ScaleSetClient::new_with_pat(
            "http://127.0.0.1/octo-org",
            "test-token",
            crate::scaleset::client::SystemInfo::default(),
            crate::scaleset::backoff::RetryPolicy::default(),
        )
        .unwrap();
        DaemonWorkerLane {
            client,
            config: LaneConfig {
                scale_set_id: 7,
                profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
                state_root: state_root.to_path_buf(),
                ready_attempts: 1,
                sweep_interval: Duration::ZERO,
            },
            runner,
            hook: Box::new(crate::scaleset::worker::DockerToolContentHook),
            intents: ProvisionIntentStore::open(db).unwrap(),
            demand: DemandStore::open(db).unwrap(),
            registry: WorkerRegistry::open(db).unwrap(),
            ledger: SharedLedger::open(ledger).unwrap(),
            recovery_claim_token: Some(recovery_claim_token),
            _test_recovery_claim: Some(recovery_claim),
            workers: HashMap::new(),
            last_sweep: None,
        }
    }

    #[test]
    fn pre_v29_pending_rotation_without_prior_pid_fails_closed() {
        let root = unique_test_dir("pre-v29-rotation-without-pid");
        let db = root.join("state.db");
        let holder = "scaleset/7/901";
        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry
            .conn
            .execute(
                "INSERT INTO scaleset_attempt_rotations
                 (holder, scale_set_id, request_id, ownership_id, old_attempt_token,
                  new_attempt_token, created_at)
                 VALUES (?1, 7, 901, 'owner-901', 'old-token', 'new-token', '2026-10-04T00:00:00Z')",
                [holder],
            )
            .unwrap();

        let error = registry
            .stage_attempt_rotation(holder, "owner-901", 7, 901, u32::MAX, Some("old-token"))
            .unwrap_err();
        assert!(error.to_string().contains("no persisted prior pid"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tokenless_legacy_attempts_resume_staged_rotation_across_both_crash_cuts() {
        let root = unique_test_dir("staged-legacy-recovery");
        let db = root.join("state.db");
        let ledger_path = root.join("permit-ledger.db");
        let state_root = root.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        let dead_pid = 2_000_000_000_u32;
        assert!(!crate::permit_guard::pid_alive(dead_pid));

        let mut permit_ledger =
            velnor_control::permit_ledger::PermitLedger::open(&ledger_path).unwrap();
        permit_ledger.set_max_jobs(4).unwrap();
        let generation = permit_ledger.begin_epoch().unwrap();
        let mut old_tokens = HashMap::new();
        for request_id in [301_i64, 302_i64] {
            let holder = permit_holder(7, request_id);
            let token = match permit_ledger
                .acquire_attempt(
                    &holder,
                    velnor_control::permit_ledger::PermitLane::ScaleSet,
                    velnor_control::permit_ledger::PermitState::Acquiring,
                    generation,
                    Some(dead_pid),
                )
                .unwrap()
            {
                velnor_control::permit_ledger::AcquireAttemptOutcome::Acquired {
                    attempt_token,
                } => attempt_token,
                outcome => panic!("unexpected legacy permit outcome: {outcome:?}"),
            };
            old_tokens.insert(request_id, token);
        }
        let migration_provenance = Connection::open(&ledger_path).unwrap();
        for request_id in [301_i64, 302_i64] {
            migration_provenance
                .execute(
                    "INSERT INTO permit_token_migrations
                     (holder, attempt_token, acquired_unix) VALUES (?1, ?2, 0)",
                    params![permit_holder(7, request_id), old_tokens[&request_id]],
                )
                .unwrap();
        }
        drop(migration_provenance);

        let mut demand = DemandStore::open(&db).unwrap();
        for request_id in [301_i64, 302_i64] {
            demand
                .submit_offer(7, &test_offer(request_id), generation)
                .unwrap();
            let token = &old_tokens[&request_id];
            demand
                .set_permit_attempt_token(request_id, None, token)
                .unwrap();
            demand
                .set_state_owned(
                    request_id,
                    if request_id == 302 {
                        DemandState::ProvisionIntent
                    } else {
                        DemandState::Acquired
                    },
                    None,
                    generation,
                    token,
                )
                .unwrap();
        }

        let holders = [permit_holder(7, 301), permit_holder(7, 302)];
        let tokens = vec![old_tokens[&301].clone(), old_tokens[&302].clone()];
        let mut batches = AcquireBatchStore::open(&db).unwrap();
        batches
            .record_intended(
                "legacy-batch",
                7,
                &[301, 302],
                &holders,
                &tokens,
                generation,
            )
            .unwrap();
        batches.resolve("legacy-batch", true).unwrap();

        let runner_name = "velnor-7-302";
        let identity = WorkerIdentity::new(OwnershipId::bind(7, runner_name));
        let worker_owner = identity.ownership().as_str().as_str().to_owned();
        let worker_state_root = state_root.join(identity.ownership().slug());
        std::fs::create_dir_all(&worker_state_root).unwrap();
        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry.set_generation(generation);
        registry
            .upsert(
                &worker_owner,
                "op-302",
                302,
                runner_name,
                &identity.network(),
                worker_state_root
                    .join("workspace")
                    .to_string_lossy()
                    .as_ref(),
                worker_state_root
                    .join("dind-data")
                    .to_string_lossy()
                    .as_ref(),
                "sha256:runner",
                "sha256:dind",
                &old_tokens[&302],
            )
            .unwrap();
        registry
            .set_state(
                &worker_owner,
                ScaleSetWorkerState::Running,
                &old_tokens[&302],
            )
            .unwrap();
        let mut intents = ProvisionIntentStore::open(&db).unwrap();
        intents
            .record_intent(
                "op-302",
                &crate::scaleset::intents::provision_ownership_id(7, runner_name),
                7,
                302,
                runner_name,
                "sha256:runner",
                "sha256:dind",
                &old_tokens[&302],
                generation,
            )
            .unwrap();
        drop(intents);
        drop(registry);

        // Model the populated-v23 migration: all new owner-token columns
        // were added nullable and remain NULL until proof-bearing recovery.
        let state_conn = Connection::open(&db).unwrap();
        state_conn
            .execute(
                "UPDATE scaleset_demand SET permit_attempt_token = NULL
                 WHERE request_id IN (301, 302)",
                [],
            )
            .unwrap();
        state_conn
            .execute(
                "UPDATE scaleset_workers SET permit_attempt_token = NULL WHERE request_id = 302",
                [],
            )
            .unwrap();
        state_conn
            .execute(
                "UPDATE scaleset_provision_intents SET permit_attempt_token = NULL
                 WHERE request_id = 302",
                [],
            )
            .unwrap();
        state_conn
            .execute(
                "UPDATE scaleset_acquire_batches SET attempt_tokens_json = NULL
                 WHERE batch_id = 'legacy-batch'",
                [],
            )
            .unwrap();
        drop(state_conn);

        let containers = HashMap::from([
            (identity.runner_container(), worker_owner.clone()),
            (identity.dind_container(), worker_owner.clone()),
        ]);
        let fail_once = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mut lane = test_lane(
            &db,
            &ledger_path,
            &state_root,
            Box::new(FailOnceRecoveryRunner {
                inner: RecoveryRunner { containers },
                fail_on_container: identity.runner_container(),
                fail_once,
            }),
        );

        // Request 301 crashes after staging but before ledger rotation.
        let first_stage = lane
            .registry
            .stage_attempt_rotation(&holders[0], "", 7, 301, dead_pid, None)
            .unwrap();
        // Request 302 has a matching durable worker and exact labeled Docker
        // pair, but its injected inspection error fires after request 301's
        // permit rotation commits.
        let second_stage = lane
            .registry
            .stage_attempt_rotation(&holders[1], &worker_owner, 7, 302, dead_pid, None)
            .unwrap();
        let error = lane.recover_durable_attempts().unwrap_err();
        assert!(error
            .to_string()
            .contains("injected resource inspection failure"));
        assert!(lane
            .ledger
            .is_current_attempt(&holders[0], &first_stage.new_attempt_token)
            .unwrap());
        assert_eq!(
            lane.ledger
                .holders()
                .unwrap()
                .into_iter()
                .find(|record| record.holder == holders[0])
                .unwrap()
                .pid,
            Some(std::process::id()),
            "the first CAS committed under the current process before later-stage failure"
        );

        // A new daemon pass advances the shared epoch before retrying. The
        // completed ledger rotation remains exact staged evidence even though
        // its committed row generation is now older than the current epoch.
        let mut retry_ledger =
            velnor_control::permit_ledger::PermitLedger::open(&ledger_path).unwrap();
        retry_ledger.begin_epoch().unwrap();
        let retry_reconcile = retry_ledger
            .reconcile_attempts_with_staged(
                &[],
                &[
                    (
                        &holders[0],
                        velnor_control::permit_ledger::PermitLane::ScaleSet,
                        None,
                        &first_stage.new_attempt_token,
                    ),
                    (
                        &holders[1],
                        velnor_control::permit_ledger::PermitLane::ScaleSet,
                        None,
                        &second_stage.new_attempt_token,
                    ),
                ],
            )
            .unwrap();
        assert_eq!(retry_reconcile.confirmed.len(), 2);
        drop(retry_ledger);

        // Retry with this same lane and claim. The first row already has the
        // staged target and durable proof; it must resume before testing the
        // old worker PID, now overwritten by the first CAS with this PID.
        lane.recover_durable_attempts().unwrap();

        let demand = DemandStore::open(&db).unwrap();
        let first = demand.get(301).unwrap().unwrap();
        let second = demand.get(302).unwrap().unwrap();
        assert_eq!(
            first.permit_attempt_token.as_deref(),
            Some(first_stage.new_attempt_token.as_str())
        );
        assert_eq!(
            second.permit_attempt_token.as_deref(),
            Some(second_stage.new_attempt_token.as_str())
        );
        let batch = AcquireBatchStore::open(&db)
            .unwrap()
            .get("legacy-batch")
            .unwrap()
            .unwrap();
        assert_eq!(
            batch.attempt_tokens,
            Some(vec![
                Some(first_stage.new_attempt_token.clone()),
                Some(second_stage.new_attempt_token.clone()),
            ])
        );
        let worker = lane.registry.get(&worker_owner).unwrap().unwrap();
        assert_eq!(
            worker.permit_attempt_token.as_deref(),
            Some(second_stage.new_attempt_token.as_str())
        );
        let intent = lane.intents.get_by_request(7, 302).unwrap().unwrap();
        assert_eq!(
            intent.permit_attempt_token.as_deref(),
            Some(second_stage.new_attempt_token.as_str())
        );
        assert!(lane
            .registry
            .pending_attempt_rotations()
            .unwrap()
            .is_empty());
        assert!(lane
            .ledger
            .is_current_attempt(&holders[0], &first_stage.new_attempt_token)
            .unwrap());
        assert!(lane
            .ledger
            .is_current_attempt(&holders[1], &second_stage.new_attempt_token)
            .unwrap());
        drop(lane);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn populated_v23_mixed_batch_finishes_after_ledger_rotation_crash() {
        let root = unique_test_dir("staged-v23-mixed-batch");
        let db = root.join("state.db");
        let ledger_path = root.join("permit-ledger.db");
        let state_root = root.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        let dead_pid = 2_000_000_000_u32;
        assert!(!crate::permit_guard::pid_alive(dead_pid));

        let mut permits = velnor_control::permit_ledger::PermitLedger::open(&ledger_path).unwrap();
        permits.set_max_jobs(2).unwrap();
        let generation = permits.begin_epoch().unwrap();
        let mut tokens = HashMap::new();
        for request_id in [401_i64, 402_i64] {
            let holder = permit_holder(7, request_id);
            let token = match permits
                .acquire_attempt(
                    &holder,
                    velnor_control::permit_ledger::PermitLane::ScaleSet,
                    velnor_control::permit_ledger::PermitState::Acquiring,
                    generation,
                    Some(dead_pid),
                )
                .unwrap()
            {
                velnor_control::permit_ledger::AcquireAttemptOutcome::Acquired {
                    attempt_token,
                } => attempt_token,
                outcome => panic!("unexpected mixed-batch permit outcome: {outcome:?}"),
            };
            tokens.insert(request_id, token);
        }
        Connection::open(&ledger_path)
            .unwrap()
            .execute(
                "INSERT INTO permit_token_migrations
                 (holder, attempt_token, acquired_unix) VALUES (?1, ?2, 0)",
                params![permit_holder(7, 402), tokens[&402]],
            )
            .unwrap();

        let mut demand = DemandStore::open(&db).unwrap();
        for request_id in [401_i64, 402_i64] {
            demand
                .submit_offer(7, &test_offer(request_id), generation)
                .unwrap();
            demand
                .set_permit_attempt_token(request_id, None, &tokens[&request_id])
                .unwrap();
            demand
                .set_state_owned(
                    request_id,
                    if request_id == 401 {
                        DemandState::Terminal
                    } else {
                        DemandState::Uncertain
                    },
                    None,
                    generation,
                    &tokens[&request_id],
                )
                .unwrap();
        }
        permits
            .release_owned(&permit_holder(7, 401), &tokens[&401])
            .unwrap();
        drop(demand);

        let holders = vec![permit_holder(7, 401), permit_holder(7, 402)];
        let old_tokens = vec![tokens[&401].clone(), tokens[&402].clone()];
        let mut batches = AcquireBatchStore::open(&db).unwrap();
        batches
            .record_intended(
                "mixed-v23-batch",
                7,
                &[401, 402],
                &holders,
                &old_tokens,
                generation,
            )
            .unwrap();
        batches.resolve("mixed-v23-batch", true).unwrap();
        drop(batches);
        drop(permits);

        // v23 has no token projections. One batch member is already
        // terminal and released; the uncertain member remains held.
        let state = Connection::open(&db).unwrap();
        state
            .execute(
                "UPDATE scaleset_demand SET permit_attempt_token = NULL
                 WHERE request_id IN (401, 402)",
                [],
            )
            .unwrap();
        state
            .execute(
                "UPDATE scaleset_acquire_batches SET attempt_tokens_json = NULL
                 WHERE batch_id = 'mixed-v23-batch'",
                [],
            )
            .unwrap();
        drop(state);

        let mut lane = test_lane(
            &db,
            &ledger_path,
            &state_root,
            Box::new(RecoveryRunner {
                containers: HashMap::new(),
            }),
        );
        let holder = permit_holder(7, 402);
        let stage = lane
            .registry
            .stage_attempt_rotation(&holder, "", 7, 402, dead_pid, None)
            .unwrap();
        let current_state = lane.ledger.holder_state(&holder).unwrap().unwrap();
        assert_eq!(
            lane.ledger
                .rotate_scaleset_attempt_for_recovery(
                    &holder,
                    current_state,
                    generation,
                    dead_pid,
                    None,
                    &stage.new_attempt_token,
                    lane.recovery_claim_token.as_deref().unwrap(),
                )
                .unwrap(),
            velnor_control::permit_ledger::AttemptRotationOutcome::Rotated
        );
        drop(lane); // crash after the permit ledger commit, before state projection commit

        let mut resumed = test_lane(
            &db,
            &ledger_path,
            &state_root,
            Box::new(RecoveryRunner {
                containers: HashMap::new(),
            }),
        );
        resumed.recover_durable_attempts().unwrap();

        let batch = AcquireBatchStore::open(&db)
            .unwrap()
            .get("mixed-v23-batch")
            .unwrap()
            .unwrap();
        assert_eq!(
            batch.attempt_tokens,
            Some(vec![None, Some(stage.new_attempt_token.clone())])
        );
        let demand = DemandStore::open(&db).unwrap();
        assert_eq!(demand.get(401).unwrap().unwrap().permit_attempt_token, None);
        assert_eq!(
            demand
                .get(402)
                .unwrap()
                .unwrap()
                .permit_attempt_token
                .as_deref(),
            Some(stage.new_attempt_token.as_str())
        );
        assert!(resumed
            .registry
            .pending_attempt_rotations()
            .unwrap()
            .is_empty());
        assert_eq!(
            resumed.ledger.holder_state(&permit_holder(7, 401)).unwrap(),
            None
        );
        assert!(resumed
            .ledger
            .is_current_attempt(&holder, &stage.new_attempt_token)
            .unwrap());
        drop(resumed);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_state_round_trips_and_rejects_unknown() {
        use ScaleSetWorkerState as S;
        for state in [
            S::Observed,
            S::Eligible,
            S::Reserved,
            S::AcquireIntent,
            S::Acquired,
            S::Uncertain,
            S::ProvisionIntent,
            S::DindReady,
            S::RunnerConnected,
            S::Running,
            S::Terminal,
            S::DiagnosticExport,
            S::OwnedCleanup,
            S::PermitReleased,
        ] {
            assert_eq!(parse_worker_state(state.as_str()).unwrap(), state);
        }
        assert!(parse_worker_state("evaporated").is_err());
        assert!(parse_worker_state("").is_err());
    }

    #[test]
    fn holder_for_key_recovers_requests_and_rejects_foreign_shapes() {
        assert_eq!(
            holder_for_key(7, "7/velnor-7-4244").as_deref(),
            Some("scaleset/7/4244")
        );
        assert_eq!(holder_for_key(7, "7/velnor-8-4244"), None);
        assert_eq!(holder_for_key(7, "8/velnor-8-4244"), None);
        assert_eq!(holder_for_key(7, "no-slash-here"), None);
        assert_eq!(holder_for_key(7, "7/velnor-7-notanumber"), None);
        assert_eq!(holder_for_key(7, ""), None);
    }

    #[test]
    fn replay_walks_the_forward_chain_and_maps_uncertain() {
        use ScaleSetWorkerState as S;
        let identity = WorkerIdentity::new(OwnershipId::bind(7, "velnor-7-4244"));
        for target in [
            S::Observed,
            S::Eligible,
            S::Reserved,
            S::AcquireIntent,
            S::Acquired,
            S::ProvisionIntent,
            S::DindReady,
            S::RunnerConnected,
            S::Running,
            S::Terminal,
            S::DiagnosticExport,
            S::OwnedCleanup,
            S::PermitReleased,
        ] {
            let mut worker = ScaleSetWorker::new(identity.clone(), "op-1");
            replay_recorded_state(&mut worker, target).unwrap();
            assert_eq!(worker.state(), target);
        }
        let mut worker = ScaleSetWorker::new(identity, "op-1");
        replay_recorded_state(&mut worker, S::Uncertain).unwrap();
        assert_eq!(worker.state(), S::Acquired);
    }

    #[test]
    fn registry_upsert_keeps_state_but_refreshes_operation() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-lane-registry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.db");
        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry.set_generation(3);
        let attempt_token = "registry-attempt";
        let row = registry
            .upsert(
                "7/velnor-7-4244",
                "op-1",
                4244,
                "velnor-7-4244",
                "net",
                "/work",
                "/dind",
                "sha256:runner",
                "sha256:dind",
                attempt_token,
            )
            .unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::Observed);
        assert_eq!(row.generation, 3);
        registry
            .set_state(
                "7/velnor-7-4244",
                ScaleSetWorkerState::DindReady,
                attempt_token,
            )
            .unwrap();
        // A retry under a new operation adopts the row's progress: the
        // state never resets backwards.
        let row = registry
            .upsert(
                "7/velnor-7-4244",
                "op-2",
                4244,
                "velnor-7-4244",
                "net",
                "/work",
                "/dind",
                "sha256:runner",
                "sha256:dind",
                attempt_token,
            )
            .unwrap();
        assert_eq!(row.operation_id, "op-2");
        assert_eq!(row.worker_state, ScaleSetWorkerState::DindReady);
        assert_eq!(registry.list_live().unwrap().len(), 1);
        assert!(registry.get_by_request(4244).unwrap().is_some());
        registry
            .set_state(
                "7/velnor-7-4244",
                ScaleSetWorkerState::PermitReleased,
                attempt_token,
            )
            .unwrap();
        assert!(registry.list_live().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registry_runtime_budget_and_start_deadline_survive_reopen() {
        let dir = unique_test_dir("runtime-persist");
        let db = dir.join("state.db");
        let key = "7/velnor-7-4244";
        let attempt_token = "runtime-attempt";
        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry
            .upsert(
                key,
                "op-1",
                4244,
                "velnor-7-4244",
                "net",
                "/tmp/workers/worker/workspace",
                "/tmp/workers/worker/dind-data",
                "sha256:runner",
                "sha256:dind",
                attempt_token,
            )
            .unwrap();
        assert_eq!(
            registry
                .set_runner_start_deadline_if_none(key, attempt_token, 1_000)
                .unwrap(),
            1_000
        );
        assert_eq!(
            registry
                .set_runner_start_deadline_if_none(key, attempt_token, 2_000)
                .unwrap(),
            1_000
        );
        registry
            .set_dind_restarts_used(key, 2, attempt_token)
            .unwrap();
        drop(registry);

        let registry = WorkerRegistry::open(&db).unwrap();
        let row = registry.get(key).unwrap().unwrap();
        assert_eq!(row.runner_start_deadline_epoch, Some(1_000));
        assert_eq!(row.dind_restarts_used, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn owned_cleanup_replay_preserves_diagnostics_across_permit_release() {
        let dir = unique_test_dir("cleanup-replay");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let ownership = OwnershipId::bind(7, "velnor-7-4244");
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str();
        let state_dir = state_root.join(ownership.slug());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("raw-job.log"), "secret diagnostic bytes").unwrap();
        std::fs::create_dir_all(state_dir.join("diagnostics")).unwrap();
        std::fs::write(
            state_dir.join("diagnostics/capture.complete"),
            b"velnor-diagnostics-v1\n",
        )
        .unwrap();

        let holder = permit_holder(7, 4244);
        let attempt_token = {
            let mut global = velnor_control::permit_ledger::PermitLedger::open(&ledger).unwrap();
            global.set_max_jobs(1).unwrap();
            let generation = global.begin_epoch().unwrap();
            let now = velnor_model::Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp()
                .max(0) as u64;
            global
                .observe_demand(
                    &holder,
                    velnor_control::permit_ledger::PermitLane::ScaleSet,
                    "scaleset/7",
                    now,
                    now,
                )
                .unwrap();
            match global
                .acquire_attempt(
                    &holder,
                    velnor_control::permit_ledger::PermitLane::ScaleSet,
                    velnor_control::permit_ledger::PermitState::Provisioning,
                    generation,
                    Some(std::process::id()),
                )
                .unwrap()
            {
                velnor_control::permit_ledger::AcquireAttemptOutcome::Acquired {
                    attempt_token,
                } => attempt_token,
                outcome => panic!("unexpected permit acquisition: {outcome:?}"),
            }
        };

        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry
            .upsert(
                &key,
                "op-1",
                4244,
                "velnor-7-4244",
                &identity.network(),
                state_dir.join("workspace").to_string_lossy().as_ref(),
                state_dir.join("dind-data").to_string_lossy().as_ref(),
                "sha256:runner",
                "sha256:dind",
                &attempt_token,
            )
            .unwrap();
        registry
            .set_state(&key, ScaleSetWorkerState::OwnedCleanup, &attempt_token)
            .unwrap();
        registry
            .set_diagnostics_complete(&key, &attempt_token)
            .unwrap();
        drop(registry);

        let first_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let first_runner =
            CleanupRunner::fail_runner_remove(identity.runner_container(), first_seen.clone());
        let mut first_lane = test_lane(&db, &ledger, &state_root, Box::new(first_runner));
        assert!(first_lane.drive_terminal(&key, &attempt_token).is_err());
        let row = first_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::OwnedCleanup);
        assert!(state_dir.join("raw-job.log").exists());
        assert_eq!(first_lane.ledger.occupied().unwrap(), 1);
        assert_eq!(
            first_lane.ledger.holder_state(&holder).unwrap(),
            Some(LedgerPermitState::Uncertain)
        );
        let global = velnor_control::permit_ledger::PermitLedger::open(&ledger).unwrap();
        assert_eq!(
            global.demand(&holder).unwrap().unwrap().state,
            velnor_control::permit_ledger::DemandState::Terminal
        );
        drop(global);
        assert!(first_seen
            .lock()
            .unwrap()
            .iter()
            .all(|args| !args.iter().any(|arg| arg == "logs" || arg == "inspect")));
        drop(first_lane);

        // New process resumes from OwnedCleanup. Missing Docker objects are
        // accepted; the exported diagnostics survive the released
        // checkpoint for post-mortem while the permit frees.
        let replay_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let replay_runner =
            CleanupRunner::missing(identity.runner_container(), replay_seen.clone());
        let mut replay_lane = test_lane(&db, &ledger, &state_root, Box::new(replay_runner));
        replay_lane.drive_terminal(&key, &attempt_token).unwrap();
        assert!(state_dir.join("raw-job.log").is_file());
        assert!(
            state_dir.join("diagnostics/capture.complete").is_file(),
            "exported diagnostics survive the permit release"
        );
        let row = replay_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::PermitReleased);
        assert_eq!(replay_lane.ledger.occupied().unwrap(), 0);
        let calls_after_release = replay_seen.lock().unwrap().len();
        assert_eq!(calls_after_release, 6);

        // A duplicate terminal observation is idempotent and performs no
        // Docker work after the durable release checkpoint.
        replay_lane.drive_terminal(&key, &attempt_token).unwrap();
        assert_eq!(replay_seen.lock().unwrap().len(), calls_after_release);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn terminal_diagnostics_export_is_persisted_once_before_teardown() {
        let dir = unique_test_dir("terminal-diagnostics-once");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let identity = WorkerIdentity::new(OwnershipId::bind(7, "velnor-7-4245"));
        let key = identity.ownership().as_str();
        let state_dir = state_root.join(identity.ownership().slug());
        std::fs::create_dir_all(&state_dir).unwrap();
        let holder = permit_holder(7, 4245);
        let (generation, attempt_token) = {
            let mut global = velnor_control::permit_ledger::PermitLedger::open(&ledger).unwrap();
            global.set_max_jobs(1).unwrap();
            let generation = global.begin_epoch().unwrap();
            let now = velnor_model::Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp()
                .max(0) as u64;
            global
                .observe_demand(
                    &holder,
                    velnor_control::permit_ledger::PermitLane::ScaleSet,
                    "scaleset/7",
                    now,
                    now,
                )
                .unwrap();
            let attempt_token = match global
                .acquire_attempt(
                    &holder,
                    velnor_control::permit_ledger::PermitLane::ScaleSet,
                    velnor_control::permit_ledger::PermitState::Provisioning,
                    generation,
                    Some(std::process::id()),
                )
                .unwrap()
            {
                velnor_control::permit_ledger::AcquireAttemptOutcome::Acquired {
                    attempt_token,
                } => attempt_token,
                outcome => panic!("unexpected permit acquisition: {outcome:?}"),
            };
            (generation, attempt_token)
        };

        let mut demand = DemandStore::open(&db).unwrap();
        demand
            .submit_offer(7, &test_offer(4245), generation)
            .unwrap();
        demand
            .set_state(4245, DemandState::ProvisionIntent, None, generation)
            .unwrap();
        demand
            .set_permit_attempt_token(4245, None, &attempt_token)
            .unwrap();
        drop(demand);

        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry
            .upsert(
                &key,
                "op-2",
                4245,
                "velnor-7-4245",
                &identity.network(),
                state_dir.join("workspace").to_string_lossy().as_ref(),
                state_dir.join("dind-data").to_string_lossy().as_ref(),
                "sha256:runner",
                "sha256:dind",
                &attempt_token,
            )
            .unwrap();
        registry
            .set_state(&key, ScaleSetWorkerState::RunnerConnected, &attempt_token)
            .unwrap();
        drop(registry);

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::missing(identity.runner_container(), seen.clone());
        let mut lane = test_lane(&db, &ledger, &state_root, Box::new(runner));
        lane.drive_terminal(&key, &attempt_token).unwrap();

        let calls = seen.lock().unwrap();
        let diagnostic_calls = calls
            .iter()
            .filter(|args| {
                args.first()
                    .is_some_and(|arg| arg == "logs" || arg == "inspect")
            })
            .count();
        assert_eq!(diagnostic_calls, 4);
        assert!(calls
            .iter()
            .any(|args| args.first().is_some_and(|arg| arg == "rm")));
        // The export persists for forensics; teardown removed the objects.
        assert!(state_dir.join("diagnostics/runner.log").is_file());
        assert!(state_dir.join("diagnostics/capture.complete").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adoption_counts_terminal_cleanup_failure_as_failed() {
        let dir = unique_test_dir("adopt-terminal-failure");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let ownership = OwnershipId::bind(7, "velnor-7-4246");
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str();
        let state_dir = state_root.join(ownership.slug());
        std::fs::create_dir_all(&state_dir).unwrap();

        let holder = permit_holder(7, 4246);
        let attempt_token = {
            let mut global = velnor_control::permit_ledger::PermitLedger::open(&ledger).unwrap();
            global.set_max_jobs(1).unwrap();
            let generation = global.begin_epoch().unwrap();
            let now = velnor_model::Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp()
                .max(0) as u64;
            global
                .observe_demand(
                    &holder,
                    velnor_control::permit_ledger::PermitLane::ScaleSet,
                    "scaleset/7",
                    now,
                    now,
                )
                .unwrap();
            match global
                .acquire_attempt(
                    &holder,
                    velnor_control::permit_ledger::PermitLane::ScaleSet,
                    velnor_control::permit_ledger::PermitState::Provisioning,
                    generation,
                    Some(u32::MAX),
                )
                .unwrap()
            {
                velnor_control::permit_ledger::AcquireAttemptOutcome::Acquired {
                    attempt_token,
                } => attempt_token,
                outcome => panic!("unexpected permit acquisition: {outcome:?}"),
            }
        };

        let mut demand = DemandStore::open(&db).unwrap();
        demand.submit_offer(7, &test_offer(4246), 1).unwrap();
        demand
            .set_state(4246, DemandState::ProvisionIntent, None, 1)
            .unwrap();
        demand
            .set_permit_attempt_token(4246, None, &attempt_token)
            .unwrap();
        drop(demand);

        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry
            .upsert(
                &key,
                "op-3",
                4246,
                "velnor-7-4246",
                &identity.network(),
                state_dir.join("workspace").to_string_lossy().as_ref(),
                state_dir.join("dind-data").to_string_lossy().as_ref(),
                "sha256:runner",
                "sha256:dind",
                &attempt_token,
            )
            .unwrap();
        registry
            .set_state(&key, ScaleSetWorkerState::Running, &attempt_token)
            .unwrap();
        drop(registry);

        let mut intents = ProvisionIntentStore::open(&db).unwrap();
        intents
            .record_intent(
                "op-3",
                &crate::scaleset::intents::provision_ownership_id(7, "velnor-7-4246"),
                7,
                4246,
                "velnor-7-4246",
                "sha256:runner",
                "sha256:dind",
                &attempt_token,
                1,
            )
            .unwrap();
        drop(intents);

        // Dead pair (nothing answers) + a stuck runner removal: the tick
        // fails the worker, the terminal path retains the permit
        // uncertain, and adoption must count the failure — not an adoption.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::fail_runner_remove(identity.runner_container(), seen.clone())
            .with_ownership(identity.ownership().as_str().as_str());
        let mut lane = test_lane(&db, &ledger, &state_root, Box::new(runner));
        let report = lane.adopt_live_workers().unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(report.adopted, 0);
        assert_eq!(report.resumed_cleanup, 0);
        assert_eq!(report.awaiting_provision, 0);
        assert_eq!(report.skipped_released, 0);
        let row = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::OwnedCleanup);
        assert_eq!(
            lane.ledger.holder_state(&holder).unwrap(),
            Some(LedgerPermitState::Uncertain)
        );
        assert_eq!(lane.ledger.occupied().unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tokenless_released_worker_row_fails_closed() {
        // Historical worker rows have no owner token. Matching demand and
        // intent records cannot repair that missing worker ownership proof.
        let dir = unique_test_dir("released-backfill-diagnostics");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let ownership = OwnershipId::bind(7, "velnor-7-4247");
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str();
        let state_dir = state_root.join(ownership.slug());
        std::fs::create_dir_all(state_dir.join("diagnostics")).unwrap();
        std::fs::write(state_dir.join("raw-job.log"), b"job output\n").unwrap();
        std::fs::write(
            state_dir.join("diagnostics/capture.complete"),
            b"velnor-diagnostics-v1\n",
        )
        .unwrap();

        let holder = permit_holder(7, 4247);
        let attempt_token = {
            let mut global = velnor_control::permit_ledger::PermitLedger::open(&ledger).unwrap();
            global.set_max_jobs(1).unwrap();
            let generation = global.begin_epoch().unwrap();
            let now = velnor_model::Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp()
                .max(0) as u64;
            global
                .observe_demand(
                    &holder,
                    velnor_control::permit_ledger::PermitLane::ScaleSet,
                    "scaleset/7",
                    now,
                    now,
                )
                .unwrap();
            match global
                .acquire_attempt(
                    &holder,
                    velnor_control::permit_ledger::PermitLane::ScaleSet,
                    velnor_control::permit_ledger::PermitState::Provisioning,
                    generation,
                    Some(u32::MAX),
                )
                .unwrap()
            {
                velnor_control::permit_ledger::AcquireAttemptOutcome::Acquired {
                    attempt_token,
                } => attempt_token,
                outcome => panic!("unexpected permit acquisition: {outcome:?}"),
            }
        };

        let mut demand = DemandStore::open(&db).unwrap();
        demand.submit_offer(7, &test_offer(4247), 1).unwrap();
        demand
            .set_state(4247, DemandState::Terminal, None, 1)
            .unwrap();
        demand
            .set_permit_attempt_token(4247, None, &attempt_token)
            .unwrap();
        drop(demand);

        // Legacy row, as the pre-runtime-table implementation left it:
        // straight into `scaleset_workers`, no runtime row.
        velnor_control::store::Store::open(&db).unwrap();
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "INSERT INTO scaleset_workers
                 (ownership_id, operation_id, request_id, runner_name, network_name,
                  workspace_path, dind_data_path, runner_digest, dind_digest,
                  worker_state, generation, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'permit_released', 0,
                         '2026-09-18T00:00:00Z', '2026-09-18T00:00:00Z')",
                params![
                    key,
                    "op-legacy",
                    4247,
                    "velnor-7-4247",
                    identity.network(),
                    state_dir.join("workspace").to_string_lossy(),
                    state_dir.join("dind-data").to_string_lossy(),
                    "sha256:runner",
                    "sha256:dind",
                ],
            )
            .unwrap();
        }
        let mut intents = ProvisionIntentStore::open(&db).unwrap();
        intents
            .record_intent(
                "op-legacy",
                &crate::scaleset::intents::provision_ownership_id(7, "velnor-7-4247"),
                7,
                4247,
                "velnor-7-4247",
                "sha256:runner",
                "sha256:dind",
                &attempt_token,
                1,
            )
            .unwrap();
        drop(intents);

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::missing(identity.runner_container(), seen.clone());
        let mut lane = test_lane(&db, &ledger, &state_root, Box::new(runner));

        // The historical row remains tokenless after schema migration.
        let row = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::PermitReleased);
        assert!(!row.diagnostics_complete);

        // Recovery cannot release the held permit from this legacy row.
        let error = lane.adopt_live_workers().unwrap_err();
        assert!(error.to_string().contains(
            "demand, worker, and provision records disagree on permit attempt ownership"
        ));
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(
            lane.ledger.holder_state(&holder).unwrap(),
            Some(LedgerPermitState::Provisioning)
        );
        assert_eq!(lane.ledger.occupied().unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

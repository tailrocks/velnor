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
use crate::scaleset::intents::{
    jit_fingerprint, permit_holder, ProvisionIntent, ProvisionIntentStore,
};
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
    pub runner_start_deadline_epoch: Option<u64>,
    pub dind_restarts_used: u32,
    pub diagnostics_complete: bool,
    pub state_dir_cleanup_pending: bool,
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
                     CHECK (diagnostics_complete IN (0, 1)),
                 state_dir_cleanup_pending INTEGER NOT NULL DEFAULT 0
                     CHECK (state_dir_cleanup_pending IN (0, 1))
             );
             INSERT OR IGNORE INTO scaleset_worker_runtime
                 (ownership_id, runner_start_deadline_epoch, dind_restarts_used,
                  diagnostics_complete, state_dir_cleanup_pending)
             SELECT ownership_id, NULL, 0, 0,
                    CASE WHEN worker_state IN ('owned_cleanup', 'permit_released') THEN 1 ELSE 0 END
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
            state_dir_cleanup_pending: row.get::<_, i64>(18)? != 0,
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
    ) -> Result<WorkerRow> {
        let now = Self::now_rfc3339();
        let generation = i64::try_from(self.generation).unwrap_or(i64::MAX);
        self.conn
            .execute(
                "INSERT INTO scaleset_workers
                 (ownership_id, operation_id, request_id, runner_name, network_name,
                  workspace_path, dind_data_path, runner_digest, dind_digest,
                  worker_state, generation, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'observed', ?10, ?11, ?11)
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
                ],
            )
            .context("upsert worker registry row")?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO scaleset_worker_runtime
                 (ownership_id, runner_start_deadline_epoch, dind_restarts_used,
                  diagnostics_complete, state_dir_cleanup_pending) VALUES (?1, NULL, 0, 0, 0)",
                params![ownership_id],
            )
            .context("ensure worker runtime row")?;
        self.get(ownership_id)?
            .with_context(|| format!("worker row {ownership_id:?} vanished after upsert"))
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
                        COALESCE(r.diagnostics_complete, 0), COALESCE(r.state_dir_cleanup_pending, 0)
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
                        COALESCE(r.diagnostics_complete, 0), COALESCE(r.state_dir_cleanup_pending, 0)
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
                        COALESCE(r.diagnostics_complete, 0), COALESCE(r.state_dir_cleanup_pending, 0)
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

    /// Workers whose host state directories still need deletion.
    pub fn list_state_cleanup_pending(&self) -> Result<Vec<WorkerRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ownership_id, operation_id, request_id, runner_name,
                        runner_container_id, dind_container_id, network_name, workspace_path,
                        dind_data_path, runner_digest, dind_digest, worker_state,
                        generation, created_at, updated_at,
                        r.runner_start_deadline_epoch, COALESCE(r.dind_restarts_used, 0),
                        COALESCE(r.diagnostics_complete, 0), COALESCE(r.state_dir_cleanup_pending, 0)
                 FROM scaleset_workers w
                 LEFT JOIN scaleset_worker_runtime r USING (ownership_id)
                 WHERE COALESCE(r.state_dir_cleanup_pending, 0) = 1
                 ORDER BY w.updated_at ASC",
            )
            .context("prepare worker state cleanup list")?;
        stmt.query_map([], Self::row_to_worker)
            .context("list worker state cleanup pending")?
            .collect::<Result<Vec<_>, _>>()
            .context("list worker state cleanup pending")
    }

    /// Persist one lifecycle state (the [`EdgeSink`] write path).
    pub fn set_state(&mut self, ownership_id: &str, state: ScaleSetWorkerState) -> Result<()> {
        let now = Self::now_rfc3339();
        let generation = i64::try_from(self.generation).unwrap_or(i64::MAX);
        let tx = self
            .conn
            .transaction()
            .context("begin worker state transaction")?;
        let updated = tx
            .execute(
                "UPDATE scaleset_workers
                 SET worker_state = ?1, generation = ?2, updated_at = ?3
                 WHERE ownership_id = ?4",
                params![state.as_str(), generation, now, ownership_id],
            )
            .context("record worker edge")?;
        if updated == 0 {
            anyhow::bail!("worker registry holds no row for {ownership_id:?}");
        }
        if matches!(
            state,
            ScaleSetWorkerState::OwnedCleanup | ScaleSetWorkerState::PermitReleased
        ) {
            let pending = i64::from(state == ScaleSetWorkerState::OwnedCleanup);
            tx.execute(
                "INSERT INTO scaleset_worker_runtime
                 (ownership_id, runner_start_deadline_epoch, dind_restarts_used,
                  diagnostics_complete, state_dir_cleanup_pending)
                 VALUES (?1, NULL, 0, 0, ?2)
                 ON CONFLICT(ownership_id) DO UPDATE SET state_dir_cleanup_pending = excluded.state_dir_cleanup_pending",
                params![ownership_id, pending],
            )
            .context("record worker state cleanup phase")?;
        }
        tx.commit().context("commit worker state transition")?;
        Ok(())
    }

    fn set_runner_start_deadline_if_none(
        &mut self,
        ownership_id: &str,
        deadline_epoch: u64,
    ) -> Result<u64> {
        let deadline = i64::try_from(deadline_epoch).unwrap_or(i64::MAX);
        self.conn
            .execute(
                "UPDATE scaleset_worker_runtime
             SET runner_start_deadline_epoch = COALESCE(runner_start_deadline_epoch, ?1)
             WHERE ownership_id = ?2",
                params![deadline, ownership_id],
            )
            .context("persist runner startup deadline")?;
        let stored: Option<i64> = self
            .conn
            .query_row(
                "SELECT runner_start_deadline_epoch FROM scaleset_worker_runtime
             WHERE ownership_id = ?1",
                params![ownership_id],
                |row| row.get(0),
            )
            .context("read runner startup deadline")?;
        stored
            .map(|seconds| seconds.max(0) as u64)
            .with_context(|| format!("worker runtime row {ownership_id:?} is missing"))
    }

    fn clear_runner_start_deadline(&mut self, ownership_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE scaleset_worker_runtime SET runner_start_deadline_epoch = NULL
             WHERE ownership_id = ?1",
                params![ownership_id],
            )
            .context("clear runner startup deadline")?;
        Ok(())
    }

    fn set_dind_restarts_used(&mut self, ownership_id: &str, used: u32) -> Result<()> {
        let used = i64::from(used);
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_worker_runtime
             SET dind_restarts_used = MAX(dind_restarts_used, ?1)
             WHERE ownership_id = ?2",
                params![used, ownership_id],
            )
            .context("persist DinD restart budget")?;
        if updated == 0 {
            anyhow::bail!("worker runtime holds no row for {ownership_id:?}");
        }
        Ok(())
    }

    fn clear_state_dir_cleanup_pending(&mut self, ownership_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE scaleset_worker_runtime SET state_dir_cleanup_pending = 0
             WHERE ownership_id = ?1",
                params![ownership_id],
            )
            .context("clear released state cleanup marker")?;
        Ok(())
    }

    fn set_diagnostics_complete(&mut self, ownership_id: &str) -> Result<()> {
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_worker_runtime SET diagnostics_complete = 1
             WHERE ownership_id = ?1",
                params![ownership_id],
            )
            .context("persist diagnostic export completion")?;
        if updated == 0 {
            anyhow::bail!("worker runtime holds no row for {ownership_id:?}");
        }
        Ok(())
    }
}

impl EdgeSink for WorkerRegistry {
    fn record_edge(&mut self, edge: &WorkerEdge) -> anyhow::Result<()> {
        self.set_state(&edge.ownership, edge.to)
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
    registry: WorkerRegistry,
    ledger: SharedLedger,
    workers: HashMap<String, LiveWorker>,
    last_sweep: Option<Instant>,
}

impl DaemonWorkerLane {
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
            registry: WorkerRegistry::open(state_db)?,
            ledger: SharedLedger::open(ledger_path)?,
            workers: HashMap::new(),
            last_sweep: None,
        })
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

    fn note_terminal_request(&mut self, request_id: i64) -> Result<(), LaneError> {
        self.refresh_generation()?;
        self.opportunistic_sweep();
        let key = self.terminal_key(request_id)?;
        self.drive_terminal(&key)
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
        let config = client
            .generate_jit_runner_config(&setting, scale_set_id)
            .await
            .map_err(|error| {
                anyhow::anyhow!("generate JIT config for runner {runner_name:?}: {error}")
            })?;
        if config.encoded_jit_config.is_empty() {
            anyhow::bail!("empty JIT config for runner {runner_name:?}");
        }
        Ok(config.encoded_jit_config)
    }

    /// Move one held permit to `state`, re-reading the generation once on
    /// a fencing failure. A missing row is fine (a concurrent release won);
    /// anything else propagates and vetoes the ACK.
    fn fenced_transition(&mut self, holder: &str, state: LedgerPermitState) -> Result<()> {
        for _ in 0..2 {
            let generation = self.ledger.generation()?;
            match self.ledger.transition(holder, state, generation) {
                Ok(()) => return Ok(()),
                Err(error) if SharedLedger::is_stale_generation(&error) => continue,
                Err(velnor_control::permit_ledger::LedgerError::UnknownHolder(_)) => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("ledger epoch moved twice under one lane transition for {holder:?}")
    }

    /// Retain worker occupancy and close its demand atomically after a
    /// cleanup failure. Retry one generation race; never turn an unknown
    /// holder into false free capacity.
    fn retain_uncertain(&mut self, holder: &str) -> Result<()> {
        for _ in 0..2 {
            let generation = self.ledger.generation()?;
            match self.ledger.retain_uncertain(holder, generation) {
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
        live.worker.transition(&mut self.registry, to)
    }

    /// Ensure a live entry for `key`, rebuilding it from the registry row
    /// (restart adoption) or failing when nothing durable names it.
    fn ensure_live(&mut self, key: &str) -> Result<()> {
        if self.workers.contains_key(key) {
            return Ok(());
        }
        let row = self
            .registry
            .get(key)?
            .with_context(|| format!("no worker recorded for {key:?}"))?;
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
                    &mut |used| registry.set_dind_restarts_used(key, used),
                )
                .map_err(|error| LaneError::new("supervise worker", error))?
        };
        if outcome == SupervisionOutcome::RunnerConnected {
            self.registry
                .clear_runner_start_deadline(key)
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
            self.drive_terminal(key)
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
                    if terminal_side(row.worker_state)
                        && let Err(error) = self.drive_terminal(&row.ownership_id)
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
        match self.registry.list_state_cleanup_pending() {
            Ok(rows) => {
                for row in rows {
                    if row.worker_state == ScaleSetWorkerState::PermitReleased
                        && let Err(error) = self.cleanup_released_state_dir(&row)
                    {
                        tracing::warn!(
                            worker = row.ownership_id.as_str(),
                            error = format!("{error:#}"),
                            "released scale-set state cleanup retry failed"
                        );
                    }
                }
            }
            Err(error) => tracing::warn!(
                error = format!("{error:#}"),
                "released scale-set state cleanup scan failed"
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

    fn cleanup_released_state_dir(&mut self, row: &WorkerRow) -> Result<()> {
        if row.worker_state != ScaleSetWorkerState::PermitReleased || !row.state_dir_cleanup_pending
        {
            anyhow::bail!(
                "worker {} is not awaiting released state cleanup",
                row.ownership_id
            );
        }
        let state_dir = self.recorded_state_dir(row)?;
        let identity = WorkerIdentity::new(OwnershipId::bind(
            self.config.scale_set_id,
            &row.runner_name,
        ));
        Supervision::from_runtime(
            identity,
            &state_dir,
            row.dind_restarts_used,
            row.runner_start_deadline_epoch,
        )
        .release_owned_state()?;
        self.registry
            .clear_state_dir_cleanup_pending(&row.ownership_id)?;
        self.workers.remove(&row.ownership_id);
        Ok(())
    }

    /// Drive one worker `→ terminal → diagnostic_export → owned_cleanup`,
    /// then release its permit — or hold the permit `uncertain` when
    /// cleanup fails. Idempotent: replays converge (released stays
    /// released). A failed cleanup vetoes the ACK (the caller maps this
    /// error into the lane error): the message redelivers and the terminal
    /// path retries until cleanup confirms — durable cleanup before ACK.
    fn drive_terminal(&mut self, key: &str) -> Result<()> {
        let outcome = self.drive_terminal_inner(key)?;
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

    fn drive_terminal_inner(&mut self, key: &str) -> Result<TerminalOutcome> {
        // Unknown worker: nothing provisioned, only the permit (if held)
        // needs releasing. The Processor moved it to `cleaning` before
        // calling; the release below finishes it.
        let row = self.registry.get(key)?;
        let Some(mut row) = row else {
            if let Some(holder) = holder_for_key(self.config.scale_set_id, key) {
                self.ledger.release(&holder)?;
            }
            return Ok(TerminalOutcome::AlreadyReleased);
        };
        if row.worker_state == ScaleSetWorkerState::PermitReleased {
            // Legacy rows from the pre-replay implementation may have
            // released the permit before deleting host state. Finish that
            // durable cleanup before treating the replay as complete.
            if let Some(holder) = holder_for_key(self.config.scale_set_id, key) {
                self.ledger.release(&holder)?;
            }
            if row.state_dir_cleanup_pending {
                self.cleanup_released_state_dir(&row)?;
            }
            return Ok(TerminalOutcome::AlreadyReleased);
        }
        self.ensure_live(key)?;
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
            self.registry.set_diagnostics_complete(key)?;
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
                self.registry.set_diagnostics_complete(key)?;
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
            let state_cleanup = {
                let live = self
                    .workers
                    .get(key)
                    .with_context(|| format!("live worker {key:?} vanished"))?;
                live.supervision.release_owned_state()
            };
            if let Err(error) = state_cleanup {
                return self.record_cleanup_failure(
                    &row,
                    key,
                    vec![format!("delete worker state: {error:#}")],
                );
            }
            self.registry.clear_state_dir_cleanup_pending(key)?;
        }
        if let Some(request_id) = row.request_id {
            let holder = permit_holder(self.config.scale_set_id, request_id);
            self.ledger.release(&holder)?;
        }
        self.transition_worker(key, ScaleSetWorkerState::PermitReleased)?;
        Ok(TerminalOutcome::Released)
    }

    fn record_cleanup_failure(
        &mut self,
        row: &WorkerRow,
        key: &str,
        failures: Vec<String>,
    ) -> Result<TerminalOutcome> {
        if let Some(request_id) = row.request_id {
            let holder = permit_holder(self.config.scale_set_id, request_id);
            self.retain_uncertain(&holder)?;
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
        self.refresh_generation()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let mut report = AdoptReport::default();
        for row in self.registry.list_state_cleanup_pending()? {
            if row.worker_state == ScaleSetWorkerState::PermitReleased {
                if let Err(error) = self.cleanup_released_state_dir(&row) {
                    tracing::warn!(
                        worker = row.ownership_id.as_str(),
                        error = format!("{error:#}"),
                        "released scale-set state cleanup will retry on idle"
                    );
                }
                report.resumed_cleanup += 1;
            }
        }
        let intents = self.intents.list_for_set(self.config.scale_set_id)?;
        for intent in &intents {
            let key = Self::ownership_key(intent);
            let row = self.registry.get(&key)?;
            let Some(row) = row else {
                // Intent without a worker row: the crash landed between
                // intent and provision. The loop's step 5 re-drives
                // provisioning from the intent; adoption skips it here.
                // (Demand still `acquired` keeps the permit attested.)
                report.awaiting_provision += 1;
                continue;
            };
            if row.worker_state == ScaleSetWorkerState::PermitReleased {
                report.skipped_released += 1;
                continue;
            }
            if provision_pending(row.worker_state) {
                report.awaiting_provision += 1;
                continue;
            }
            if terminal_side(row.worker_state) {
                // Crash during cleanup: resume the terminal path now. A
                // still-failing cleanup must not fail adoption (that would
                // wedge daemon startup): the worker stays tracked at
                // `owned_cleanup` and the message path retries it.
                if let Err(error) = self.drive_terminal(&key) {
                    tracing::warn!(
                        worker = key.as_str(),
                        error = format!("{error:#}"),
                        "scale-set adoption cleanup failed; the message path will retry"
                    );
                }
                report.resumed_cleanup += 1;
                continue;
            }
            self.ensure_live(&key)?;
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
                    // Docker unreachable (daemon restart race): keep the
                    // worker tracked; the message path retries the tick.
                    // The permit stays held either way — never freed blind.
                    tracing::warn!(
                        worker = key.as_str(),
                        error = error.to_string(),
                        "scale-set adoption tick failed; worker stays tracked"
                    );
                    report.adopted += 1;
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
                    tracing::warn!(
                        worker = key.as_str(),
                        error = error.to_string(),
                        "scale-set shutdown tick failed; worker left for restart adoption"
                    );
                    report.adopted_across_restart += 1;
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
    let name = key.split('/').next_back()?;
    let request = name.split('-').next_back()?.parse::<i64>().ok()?;
    Some(permit_holder(scale_set_id, request))
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
        self.opportunistic_sweep();
        let key = Self::ownership_key(intent);

        // Idempotent replay: a live post-provision worker for the same
        // operation only needs a health tick.
        if self.workers.contains_key(&key) {
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
            )
            .map_err(|error| LaneError::new("record worker", error))?;
        let state_dir = self
            .recorded_state_dir(&row)
            .map_err(|error| LaneError::new("read worker state path", error))?;

        let mut worker = ScaleSetWorker::new(identity.clone(), &intent.operation_id);
        worker.bind_request(intent.request_id);
        let holder = permit_holder(intent.scale_set_id, intent.request_id);
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
            worker
                .transition(&mut self.registry, *edge)
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
                .clear_runner_start_deadline(&key)
                .map_err(|error| LaneError::new("clear runner startup deadline", error))?;
            runner_start_deadline_epoch = None;
        }
        worker.record_versions(
            &outcome.runner_attestation.content_version,
            &outcome.dind_attestation.content_version,
        );
        if worker.state() == ScaleSetWorkerState::ProvisionIntent {
            worker
                .transition(&mut self.registry, ScaleSetWorkerState::DindReady)
                .map_err(|error| LaneError::new("record DinD ready", error))?;
        }
        if outcome.connection == RunnerConnection::Connected
            && worker.state() == ScaleSetWorkerState::DindReady
        {
            worker
                .transition(&mut self.registry, ScaleSetWorkerState::RunnerConnected)
                .map_err(|error| LaneError::new("record runner connected", error))?;
        }
        self.fenced_transition(&holder, LedgerPermitState::Provisioning)
            .map_err(|error| LaneError::new("mark permit provisioning", error))?;
        self.workers.insert(
            key,
            LiveWorker {
                worker,
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

    fn note_assigned(&mut self, assigned: &ScaleSetJobAssigned) -> Result<(), Self::Error> {
        self.refresh_generation()?;
        self.opportunistic_sweep();
        let request_id = assigned.base.runner_request_id;
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
        let known = self
            .registry
            .get(&key)
            .map_err(|error| LaneError::new("read worker row", error))?
            .is_some();
        if known {
            self.tick_worker(&key)?;
        }
        Ok(())
    }

    fn note_started(&mut self, started: &ScaleSetJobStarted) -> Result<(), Self::Error> {
        self.refresh_generation()?;
        self.opportunistic_sweep();
        let request_id = started.base.runner_request_id;
        let Some(intent) = self
            .intents
            .get_by_request(self.config.scale_set_id, request_id)
            .map_err(|error| LaneError::new("find provision intent", error))?
        else {
            return Ok(());
        };
        let key = Self::ownership_key(&intent);
        let known = self
            .registry
            .get(&key)
            .map_err(|error| LaneError::new("read worker row", error))?
            .is_some();
        if !known {
            return Ok(());
        }
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
        let holder = permit_holder(self.config.scale_set_id, request_id);
        self.fenced_transition(&holder, LedgerPermitState::Running)
            .map_err(|error| LaneError::new("mark permit running", error))?;
        Ok(())
    }

    fn note_terminal(&mut self, completed: &ScaleSetJobCompleted) -> Result<(), Self::Error> {
        self.note_terminal_request(completed.base.runner_request_id)
    }

    fn note_canceled(&mut self, request_id: i64) -> Result<(), Self::Error> {
        self.note_terminal_request(request_id)
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

    struct CleanupRunner {
        fail_runner_remove: bool,
        runner_name: String,
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
                seen,
            }
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

    fn test_lane(
        db: &Path,
        ledger: &Path,
        state_root: &Path,
        runner: Box<dyn WorkerRunner + Send>,
    ) -> DaemonWorkerLane {
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
            registry: WorkerRegistry::open(db).unwrap(),
            ledger: SharedLedger::open(ledger).unwrap(),
            workers: HashMap::new(),
            last_sweep: None,
        }
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
            )
            .unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::Observed);
        assert_eq!(row.generation, 3);
        registry
            .set_state("7/velnor-7-4244", ScaleSetWorkerState::DindReady)
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
            )
            .unwrap();
        assert_eq!(row.operation_id, "op-2");
        assert_eq!(row.worker_state, ScaleSetWorkerState::DindReady);
        assert_eq!(registry.list_live().unwrap().len(), 1);
        assert!(registry.get_by_request(4244).unwrap().is_some());
        registry
            .set_state("7/velnor-7-4244", ScaleSetWorkerState::PermitReleased)
            .unwrap();
        assert!(registry.list_live().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registry_runtime_budget_and_start_deadline_survive_reopen() {
        let dir = unique_test_dir("runtime-persist");
        let db = dir.join("state.db");
        let key = "7/velnor-7-4244";
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
            )
            .unwrap();
        assert_eq!(
            registry
                .set_runner_start_deadline_if_none(key, 1_000)
                .unwrap(),
            1_000
        );
        assert_eq!(
            registry
                .set_runner_start_deadline_if_none(key, 2_000)
                .unwrap(),
            1_000
        );
        registry.set_dind_restarts_used(key, 2).unwrap();
        drop(registry);

        let registry = WorkerRegistry::open(&db).unwrap();
        let row = registry.get(key).unwrap().unwrap();
        assert_eq!(row.runner_start_deadline_epoch, Some(1_000));
        assert_eq!(row.dind_restarts_used, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn owned_cleanup_replay_removes_state_before_permit_release() {
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
        {
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
            assert_eq!(
                global
                    .acquire(
                        &holder,
                        velnor_control::permit_ledger::PermitLane::ScaleSet,
                        velnor_control::permit_ledger::PermitState::Provisioning,
                        generation,
                        None,
                    )
                    .unwrap(),
                velnor_control::permit_ledger::AcquireOutcome::Acquired
            );
        }

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
            )
            .unwrap();
        registry
            .set_state(&key, ScaleSetWorkerState::OwnedCleanup)
            .unwrap();
        registry.set_diagnostics_complete(&key).unwrap();
        drop(registry);

        let first_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let first_runner =
            CleanupRunner::fail_runner_remove(identity.runner_container(), first_seen.clone());
        let mut first_lane = test_lane(&db, &ledger, &state_root, Box::new(first_runner));
        assert!(first_lane.drive_terminal(&key).is_err());
        let row = first_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::OwnedCleanup);
        assert!(row.state_dir_cleanup_pending);
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
        // accepted; the state dir is removed before the released checkpoint.
        let replay_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let replay_runner =
            CleanupRunner::missing(identity.runner_container(), replay_seen.clone());
        let mut replay_lane = test_lane(&db, &ledger, &state_root, Box::new(replay_runner));
        replay_lane.drive_terminal(&key).unwrap();
        assert!(!state_dir.exists());
        let row = replay_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::PermitReleased);
        assert!(!row.state_dir_cleanup_pending);
        assert_eq!(replay_lane.ledger.occupied().unwrap(), 0);
        let calls_after_release = replay_seen.lock().unwrap().len();
        assert_eq!(calls_after_release, 6);

        // A duplicate terminal observation is idempotent and performs no
        // Docker work after the durable release checkpoint.
        replay_lane.drive_terminal(&key).unwrap();
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
            )
            .unwrap();
        registry
            .set_state(&key, ScaleSetWorkerState::RunnerConnected)
            .unwrap();
        drop(registry);

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::missing(identity.runner_container(), seen.clone());
        let mut lane = test_lane(&db, &ledger, &state_root, Box::new(runner));
        lane.drive_terminal(&key).unwrap();

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
        assert!(!state_dir.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

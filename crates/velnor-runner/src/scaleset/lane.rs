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

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use velnor_model::{
    RunnerScaleSetJitRunnerSetting, ScaleSetJobAssigned, ScaleSetJobCompleted, ScaleSetJobStarted,
    ScaleSetWorkerState,
};

use crate::scaleset::converge::WorkerLane;
use crate::scaleset::errors::ScaleSetFault;
use crate::scaleset::intents::{
    jit_fingerprint, permit_holder, ProvisionIntent, ProvisionIntentStore,
};
use crate::scaleset::shared_ledger::SharedLedger;
use crate::scaleset::worker::runner::RUNNER_WORK_DIR;
use crate::scaleset::worker::{
    provision_worker, EdgeSink, HomogeneousProfile, OwnershipId, ProvisionPlan,
    RestartWorkerPairAttestation, RunnerConnection, ScaleSetWorker, Supervision,
    SupervisionOutcome, ToolContentHook, VecEdgeSink, WorkerEdge, WorkerIdentity, WorkerRunner,
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

    /// Generation stamped on subsequent edge writes. A registry belongs to
    /// one daemon epoch; callers must create a new registry for a new epoch.
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
                   updated_at = excluded.updated_at
                 WHERE scaleset_workers.generation = excluded.generation",
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
                  diagnostics_complete) VALUES (?1, NULL, 0, 0)",
                params![ownership_id],
            )
            .context("ensure worker runtime row")?;
        self.get(ownership_id)?
            .with_context(|| format!("worker row {ownership_id:?} vanished after upsert"))
            .and_then(|row| {
                if row.generation != self.generation {
                    anyhow::bail!(
                        "worker {ownership_id:?} belongs to generation {}, current lane is {}",
                        row.generation,
                        self.generation
                    );
                }
                Ok(row)
            })
    }

    /// Transfer a durable worker row to this epoch during explicit restart
    /// adoption. The compare-and-set prevents an older lane from claiming a
    /// row that a newer lane already owns.
    fn claim_generation(&mut self, ownership_id: &str, previous: u64) -> Result<()> {
        let now = Self::now_rfc3339();
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_workers SET generation = ?1, updated_at = ?2
                 WHERE ownership_id = ?3 AND generation = ?4",
                params![
                    i64::try_from(self.generation).unwrap_or(i64::MAX),
                    now,
                    ownership_id,
                    i64::try_from(previous).unwrap_or(i64::MAX),
                ],
            )
            .context("claim worker generation")?;
        if updated == 1 {
            return Ok(());
        }
        let row = self
            .get(ownership_id)?
            .with_context(|| format!("worker row {ownership_id:?} vanished during adoption"))?;
        if row.generation == self.generation {
            return Ok(());
        }
        anyhow::bail!(
            "worker {ownership_id:?} changed generation during adoption (saw {}, now {})",
            previous,
            row.generation
        )
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
                        COALESCE(r.diagnostics_complete, 0)
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
                        COALESCE(r.diagnostics_complete, 0)
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
                        COALESCE(r.diagnostics_complete, 0)
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
    pub fn set_state(&mut self, ownership_id: &str, state: ScaleSetWorkerState) -> Result<()> {
        let now = Self::now_rfc3339();
        let generation = i64::try_from(self.generation).unwrap_or(i64::MAX);
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_workers
                 SET worker_state = ?1, generation = ?2, updated_at = ?3
                 WHERE ownership_id = ?4 AND generation = ?2",
                params![state.as_str(), generation, now, ownership_id],
            )
            .context("record worker edge")?;
        if updated == 0 {
            let row = self
                .get(ownership_id)?
                .with_context(|| format!("worker registry holds no row for {ownership_id:?}"))?;
            anyhow::bail!(
                "worker {ownership_id:?} belongs to generation {}, current registry is {}",
                row.generation,
                self.generation
            );
        }
        Ok(())
    }

    fn set_runner_start_deadline_if_none(
        &mut self,
        ownership_id: &str,
        deadline_epoch: u64,
    ) -> Result<u64> {
        let deadline = i64::try_from(deadline_epoch).unwrap_or(i64::MAX);
        let generation = i64::try_from(self.generation).unwrap_or(i64::MAX);
        self.conn
            .execute(
                "UPDATE scaleset_worker_runtime
             SET runner_start_deadline_epoch = COALESCE(runner_start_deadline_epoch, ?1)
             WHERE ownership_id = ?2 AND EXISTS (
                 SELECT 1 FROM scaleset_workers
                 WHERE ownership_id = ?2 AND generation = ?3
             )",
                params![deadline, ownership_id, generation],
            )
            .context("persist runner startup deadline")?;
        let (row_generation, stored): (i64, Option<i64>) = self
            .conn
            .query_row(
                "SELECT w.generation, r.runner_start_deadline_epoch
                 FROM scaleset_workers w
                 JOIN scaleset_worker_runtime r USING (ownership_id)
                 WHERE w.ownership_id = ?1",
                params![ownership_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("read runner startup deadline")?;
        if row_generation.max(0) as u64 != self.generation {
            anyhow::bail!(
                "worker {ownership_id:?} belongs to generation {}, current registry is {}",
                row_generation,
                self.generation
            );
        }
        stored
            .map(|seconds| seconds.max(0) as u64)
            .with_context(|| format!("worker runtime row {ownership_id:?} has no deadline"))
    }

    fn clear_runner_start_deadline(&mut self, ownership_id: &str) -> Result<()> {
        let generation = i64::try_from(self.generation).unwrap_or(i64::MAX);
        self.conn
            .execute(
                "UPDATE scaleset_worker_runtime SET runner_start_deadline_epoch = NULL
             WHERE ownership_id = ?1 AND EXISTS (
                 SELECT 1 FROM scaleset_workers
                 WHERE ownership_id = ?1 AND generation = ?2
             )",
                params![ownership_id, generation],
            )
            .context("clear runner startup deadline")?;
        Ok(())
    }

    fn set_dind_restarts_used(&mut self, ownership_id: &str, used: u32) -> Result<()> {
        let used = i64::from(used);
        let generation = i64::try_from(self.generation).unwrap_or(i64::MAX);
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_worker_runtime
             SET dind_restarts_used = MAX(dind_restarts_used, ?1)
             WHERE ownership_id = ?2 AND EXISTS (
                 SELECT 1 FROM scaleset_workers
                 WHERE ownership_id = ?2 AND generation = ?3
             )",
                params![used, ownership_id, generation],
            )
            .context("persist DinD restart budget")?;
        if updated == 0 {
            anyhow::bail!("worker runtime holds no row for {ownership_id:?}");
        }
        Ok(())
    }

    fn set_diagnostics_complete(&mut self, ownership_id: &str) -> Result<()> {
        let generation = i64::try_from(self.generation).unwrap_or(i64::MAX);
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_worker_runtime SET diagnostics_complete = 1
             WHERE ownership_id = ?1 AND EXISTS (
                 SELECT 1 FROM scaleset_workers
                 WHERE ownership_id = ?1 AND generation = ?2
             )",
                params![ownership_id, generation],
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
    generation: u64,
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
        let ledger = SharedLedger::open(ledger_path)?;
        let generation = ledger.generation()?;
        let mut registry = WorkerRegistry::open(state_db)?;
        registry.set_generation(generation);
        Ok(Self {
            client,
            config,
            generation,
            runner,
            hook,
            intents: ProvisionIntentStore::open(state_db)?,
            registry,
            ledger,
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

    /// Verify that this lane still owns its startup epoch. A lane never
    /// retargets itself to a newer generation; the daemon must redeliver the
    /// work to the new lane instead.
    fn refresh_generation(&mut self) -> Result<(), LaneError> {
        let current = self
            .ledger
            .generation()
            .map_err(|error| LaneError::new("read ledger generation", error.into()))?;
        if current != self.generation {
            return Err(LaneError::new(
                "fence worker lifecycle",
                anyhow::anyhow!(
                    "lane epoch {} is stale; ledger is at generation {}",
                    self.generation,
                    current
                ),
            ));
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

    /// Move one held permit to `state` under this lane's immutable epoch.
    /// Stale generations are terminal for this lane: retrying with the new
    /// epoch could mutate ownership adopted by another daemon.
    fn fenced_transition(&mut self, holder: &str, state: LedgerPermitState) -> Result<()> {
        self.ledger
            .transition(holder, state, self.generation)
            .map_err(Into::into)
    }

    /// Retain worker occupancy and close its demand atomically after a
    /// cleanup failure. Retry one generation race; never turn an unknown
    /// holder into false free capacity.
    fn retain_uncertain(&mut self, holder: &str) -> Result<()> {
        self.ledger
            .retain_uncertain(holder, self.generation)
            .map_err(Into::into)
    }

    /// Release through the control ledger's atomic holder+epoch transaction.
    /// `SharedLedger` intentionally keeps the generic capacity surface small;
    /// opening the same SQLite file here selects the fenced worker primitive
    /// without widening that public trait.
    fn fenced_release(&mut self, holder: &str) -> Result<bool> {
        let mut ledger = velnor_control::permit_ledger::PermitLedger::open(self.ledger.path())
            .context("open permit ledger for fenced release")?;
        ledger
            .release_fenced(holder, self.generation)
            .map_err(anyhow::Error::new)
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
        if row.generation > self.generation {
            anyhow::bail!(
                "worker {key:?} belongs to newer generation {}, current lane is {}",
                row.generation,
                self.generation
            );
        }
        if row.generation < self.generation {
            self.registry.claim_generation(key, row.generation)?;
        }
        let row = self
            .registry
            .get(key)?
            .with_context(|| format!("worker {key:?} vanished after generation claim"))?;
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
                self.fenced_release(&holder)?;
            }
            return Ok(TerminalOutcome::AlreadyReleased);
        };
        if row.worker_state == ScaleSetWorkerState::PermitReleased {
            // The row says released, but a restart between the
            // adoption-time release and the completion observation lets
            // startup reconcile re-attest the permit from the
            // still-active demand row. Converge to the recorded truth.
            // The exported diagnostics stay on disk for post-mortem,
            // exactly like a fresh release: deletion ends owned Docker
            // objects, never the exported logs.
            if let Some(holder) = holder_for_key(self.config.scale_set_id, key) {
                self.fenced_release(&holder)?;
            }
            return Ok(TerminalOutcome::AlreadyReleased);
        }
        if row.generation > self.generation {
            anyhow::bail!(
                "worker {key:?} belongs to newer generation {}, current lane is {}",
                row.generation,
                self.generation
            );
        }
        if row.generation < self.generation {
            self.registry.claim_generation(key, row.generation)?;
            row = self
                .registry
                .get(key)?
                .with_context(|| format!("worker {key:?} vanished after generation claim"))?;
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
            // The state dir is deliberately NOT deleted here: the exported
            // diagnostics survive the worker for post-mortem ("diagnostics
            // exported before deletion" — deletion ends owned Docker
            // objects, never the exported logs).
        }
        if let Some(request_id) = row.request_id {
            let holder = permit_holder(self.config.scale_set_id, request_id);
            self.fenced_release(&holder)?;
        }
        self.transition_worker(key, ScaleSetWorkerState::PermitReleased)?;
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
    /// The worker registry is the adoption source of truth: a live container
    /// pair is adopted into the live map (permits + demand rows keep their
    /// states, so the restarted loop resumes supervision without
    /// re-provisioning); a dead or missing pair is failed explicitly through
    /// the terminal path. A pre-provision row with no matching intent is an
    /// orphan and is terminalized instead of waiting for a scheduler replay.
    /// Intent rows without worker rows remain pending for the normal
    /// provision pass. Fully released workers are skipped. Never deletes the
    /// scale set, never writes demand, never fabricates a completion.
    pub fn adopt_live_workers(&mut self) -> Result<AdoptReport> {
        self.refresh_generation()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let mut report = AdoptReport::default();
        let intents = self.intents.list_for_set(self.config.scale_set_id)?;
        let intent_operations: HashMap<String, String> = intents
            .iter()
            .map(|intent| (Self::ownership_key(intent), intent.operation_id.clone()))
            .collect();
        let mut seen_workers = HashSet::new();

        for row in self.registry.list_live()? {
            let expected_key = OwnershipId::bind(self.config.scale_set_id, &row.runner_name);
            if row.ownership_id != expected_key.as_str() {
                // The state database is shared by scale-set daemons. A lane
                // must never adopt another set's worker row.
                continue;
            }
            let key = row.ownership_id.clone();
            seen_workers.insert(key.clone());
            if provision_pending(row.worker_state) {
                let has_matching_intent = intent_operations
                    .get(&key)
                    .is_some_and(|operation_id| operation_id == &row.operation_id);
                if !has_matching_intent {
                    // The row proves that provisioning had started, but no
                    // durable intent can replay it. Cleanup is the only
                    // safe recovery: do not invoke the scheduler or invent
                    // a second provision operation.
                    report.failed += 1;
                    if let Err(error) = self.drive_terminal(&key) {
                        tracing::warn!(
                            worker = key.as_str(),
                            error = format!("{error:#}"),
                            "scale-set orphan worker terminal recovery will retry"
                        );
                    }
                    continue;
                }
                if row.generation > self.generation {
                    anyhow::bail!(
                        "worker {key:?} belongs to newer generation {}, current lane is {}",
                        row.generation,
                        self.generation
                    );
                }
                if row.generation < self.generation {
                    // The processor will replay this intent after startup.
                    // Claim the row first so its idempotent upsert lands in
                    // this epoch instead of being rejected by the registry
                    // generation fence. The CAS also prevents an older lane
                    // from stealing a row already claimed by a newer lane.
                    self.registry.claim_generation(&key, row.generation)?;
                }
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
            let identity = self
                .workers
                .get(&key)
                .with_context(|| format!("live worker {key:?} vanished after adoption"))?
                .worker
                .identity()
                .clone();
            let attestation = crate::scaleset::worker::attest_restart_worker_pair(
                &mut *self.runner,
                &identity,
                &self.config.profile,
            )
            .with_context(|| format!("attest restart worker pair {key:?}"))?;
            if attestation == RestartWorkerPairAttestation::Absent {
                report.failed += 1;
                if let Err(error) = self.drive_terminal(&key) {
                    tracing::warn!(
                        worker = key.as_str(),
                        error = format!("{error:#}"),
                        "scale-set restart adoption found an absent Docker object; terminal recovery will retry"
                    );
                }
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

        for intent in &intents {
            let key = Self::ownership_key(intent);
            if seen_workers.contains(&key) {
                continue;
            }
            match self.registry.get(&key)? {
                Some(row) if row.worker_state == ScaleSetWorkerState::PermitReleased => {
                    report.skipped_released += 1;
                }
                Some(_) => {
                    // A concurrent lifecycle writer owns the row observed
                    // outside list_live; leave it to that epoch's retry.
                }
                None => {
                    // Intent without a worker row: the crash landed between
                    // intent and provision. The loop's step 5 re-drives
                    // provisioning from the intent; adoption skips it here.
                    // (Demand still `acquired` keeps the permit attested.)
                    report.awaiting_provision += 1;
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
        let Some(request_id) = crate::scaleset::demand::resolve_job_request_id(&assigned.base)
        else {
            // A scale-set event without runnerRequestId and jobId has no
            // durable ownership identity. It cannot safely affect a permit.
            return Ok(());
        };
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
        let (key, holder) = if !started.runner_name.is_empty() {
            let Some((scale_set_id, _)) =
                crate::scaleset::intents::parse_runner_name(&started.runner_name)
            else {
                return Ok(());
            };
            if scale_set_id != self.config.scale_set_id {
                return Ok(());
            }
            let key = OwnershipId::bind(self.config.scale_set_id, &started.runner_name)
                .as_str()
                .to_string();
            let Some(holder) = holder_for_key(self.config.scale_set_id, &key) else {
                return Ok(());
            };
            (key, holder)
        } else {
            let Some(request_id) = crate::scaleset::demand::resolve_job_request_id(&started.base)
            else {
                // A scale-set event without runnerRequestId and jobId has no
                // durable ownership identity. It cannot safely affect a permit.
                return Ok(());
            };
            let Some(intent) = self
                .intents
                .get_by_request(self.config.scale_set_id, request_id)
                .map_err(|error| LaneError::new("find provision intent", error))?
            else {
                return Ok(());
            };
            (
                Self::ownership_key(&intent),
                permit_holder(self.config.scale_set_id, request_id),
            )
        };
        let known = self
            .registry
            .get(&key)
            .map_err(|error| LaneError::new("read worker row", error))?
            .is_some();
        if !known {
            return Ok(());
        }
        // Check before supervision: a worker without a counted permit must
        // fail/requeue, and must not advance its durable state toward
        // Running. The fenced transition below repeats the check at the
        // mutation boundary for concurrent cleanup.
        if self
            .ledger
            .holder_state(&holder)
            .map_err(|error| LaneError::new("check worker permit", error.into()))?
            .is_none()
        {
            return Err(LaneError::new(
                "start worker",
                anyhow::anyhow!("worker {key:?} has no permit; refusing Running/ACK"),
            ));
        }
        let outcome = self.tick_worker(&key)?;
        if matches!(outcome, SupervisionOutcome::WorkerFailed { .. }) {
            // The tick already failed the worker explicitly; GitHub owns
            // the job outcome from here (the completion observation
            // converges the demand row).
            return Ok(());
        }
        // Fence the permit before advancing the worker record. If cleanup
        // raced this start and removed the row, the message errors and is
        // redelivered; no Running edge or ACK is emitted.
        self.fenced_transition(&holder, LedgerPermitState::Running)
            .map_err(|error| LaneError::new("mark permit running", error))?;

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
        Ok(())
    }

    fn note_terminal(&mut self, completed: &ScaleSetJobCompleted) -> Result<(), Self::Error> {
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
            let Some(request_id) = crate::scaleset::demand::resolve_job_request_id(&completed.base)
            else {
                // A terminal event without identity cannot release or mutate
                // a permit. Leave it for the next broker delivery/diagnostics.
                return Ok(());
            };
            self.terminal_key(request_id)?
        };
        self.drive_terminal(&key)
            .map_err(|error| LaneError::new("drive worker terminal", error))?;
        Ok(())
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
                if args.iter().any(|arg| arg == "--format={{.State.Status}}") {
                    return Ok(crate::scaleset::worker::WorkerOutput {
                        code: 0,
                        stdout: "exited\n".to_owned(),
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
        let mut registry = WorkerRegistry::open(db).unwrap();
        let ledger = SharedLedger::open(ledger).unwrap();
        let generation = ledger.generation().unwrap();
        registry.set_generation(generation);
        DaemonWorkerLane {
            client,
            config: LaneConfig {
                scale_set_id: 7,
                profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
                state_root: state_root.to_path_buf(),
                ready_attempts: 1,
                sweep_interval: Duration::ZERO,
            },
            generation,
            runner,
            hook: Box::new(crate::scaleset::worker::DockerToolContentHook),
            intents: ProvisionIntentStore::open(db).unwrap(),
            registry,
            ledger,
            workers: HashMap::new(),
            last_sweep: None,
        }
    }

    #[test]
    fn lifecycle_without_identity_does_not_touch_workers_or_permits() {
        let dir = unique_test_dir("missing-identity");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::missing("unused".to_owned(), seen.clone());
        let mut lane = test_lane(&db, &ledger, &state_root, Box::new(runner));

        assert!(lane.note_assigned(&ScaleSetJobAssigned::default()).is_ok());
        assert!(lane.note_started(&ScaleSetJobStarted::default()).is_ok());
        assert!(lane.note_terminal(&ScaleSetJobCompleted::default()).is_ok());

        assert_eq!(lane.live_workers(), 0);
        assert!(lane.registry.list_live().unwrap().is_empty());
        assert_eq!(lane.ledger.occupied().unwrap(), 0);
        assert!(seen.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_permit_at_start_requeues_without_running_edge() {
        let dir = unique_test_dir("missing-start-permit");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let ownership = OwnershipId::bind(7, "velnor-7-4244");
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str();

        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry
            .upsert(
                &key,
                "op-start",
                4244,
                "velnor-7-4244",
                &identity.network(),
                state_root
                    .join(ownership.slug())
                    .join("workspace")
                    .to_string_lossy()
                    .as_ref(),
                state_root
                    .join(ownership.slug())
                    .join("dind-data")
                    .to_string_lossy()
                    .as_ref(),
                "sha256:runner",
                "sha256:dind",
            )
            .unwrap();
        registry
            .set_state(&key, ScaleSetWorkerState::RunnerConnected)
            .unwrap();
        let mut intents = ProvisionIntentStore::open(&db).unwrap();
        intents
            .record_intent(
                "op-start",
                &key,
                7,
                4244,
                "velnor-7-4244",
                "sha256:runner",
                "sha256:dind",
                0,
            )
            .unwrap();
        drop(intents);

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut lane = test_lane(
            &db,
            &ledger,
            &state_root,
            Box::new(CleanupRunner::missing(
                identity.runner_container(),
                seen.clone(),
            )),
        );
        let mut started = ScaleSetJobStarted::default();
        started.base.runner_request_id = 4244;
        let error = lane.note_started(&started).unwrap_err();
        assert!(error.to_string().contains("refusing Running/ACK"));
        assert_eq!(
            lane.registry.get(&key).unwrap().unwrap().worker_state,
            ScaleSetWorkerState::RunnerConnected
        );
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(lane.ledger.occupied().unwrap(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adoption_terminalizes_worker_row_without_provision_intent() {
        let dir = unique_test_dir("adopt-worker-without-intent");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let ownership = OwnershipId::bind(7, "velnor-7-4251");
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str();
        let state_dir = state_root.join(ownership.slug());
        std::fs::create_dir_all(&state_dir).unwrap();
        let holder = permit_holder(7, 4251);

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

            let mut registry = WorkerRegistry::open(&db).unwrap();
            registry.set_generation(generation);
            registry
                .upsert(
                    &key,
                    "op-orphan",
                    4251,
                    "velnor-7-4251",
                    &identity.network(),
                    state_dir.join("workspace").to_string_lossy().as_ref(),
                    state_dir.join("dind-data").to_string_lossy().as_ref(),
                    "sha256:runner",
                    "sha256:dind",
                )
                .unwrap();
            registry
                .set_state(&key, ScaleSetWorkerState::ProvisionIntent)
                .unwrap();
        }

        // No provision intent exists. Startup must recover the durable
        // worker row directly; it must not wait for or mint a scheduler job.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut lane = test_lane(
            &db,
            &ledger,
            &state_root,
            Box::new(CleanupRunner::missing(
                identity.runner_container(),
                seen.clone(),
            )),
        );
        let report = lane.adopt_live_workers().unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(report.adopted, 0);
        assert_eq!(report.awaiting_provision, 0);
        assert_eq!(
            lane.registry.get(&key).unwrap().unwrap().worker_state,
            ScaleSetWorkerState::PermitReleased
        );
        assert_eq!(lane.ledger.holder_state(&holder).unwrap(), None);
        assert_eq!(lane.ledger.occupied().unwrap(), 0);
        assert_eq!(lane.live_workers(), 0);
        assert!(seen.lock().unwrap().iter().all(|args| {
            args.first()
                .is_none_or(|command| command != "create" && command != "start")
        }));
        let _ = std::fs::remove_dir_all(&dir);
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
        replay_lane.drive_terminal(&key).unwrap();
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
        replay_lane.drive_terminal(&key).unwrap();
        assert_eq!(replay_seen.lock().unwrap().len(), calls_after_release);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adoption_claims_stale_provision_pending_row_before_replay() {
        let dir = unique_test_dir("adopt-stale-provision");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let ownership = OwnershipId::bind(7, "velnor-7-4248");
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str();
        let state_dir = state_root.join(ownership.slug());
        let previous_generation;
        let current_generation;
        {
            let mut global = velnor_control::permit_ledger::PermitLedger::open(&ledger).unwrap();
            global.set_max_jobs(1).unwrap();
            previous_generation = (0..4).fold(0, |_, _| global.begin_epoch().unwrap());
            current_generation = global.begin_epoch().unwrap();
        }

        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry.set_generation(previous_generation);
        registry
            .upsert(
                &key,
                "op-stale",
                4248,
                "velnor-7-4248",
                &identity.network(),
                state_dir.join("workspace").to_string_lossy().as_ref(),
                state_dir.join("dind-data").to_string_lossy().as_ref(),
                "sha256:runner",
                "sha256:dind",
            )
            .unwrap();
        registry
            .set_state(&key, ScaleSetWorkerState::ProvisionIntent)
            .unwrap();
        drop(registry);

        let mut intents = ProvisionIntentStore::open(&db).unwrap();
        intents
            .record_intent(
                "op-stale",
                &crate::scaleset::intents::provision_ownership_id(7, "velnor-7-4248"),
                7,
                4248,
                "velnor-7-4248",
                "sha256:runner",
                "sha256:dind",
                previous_generation,
            )
            .unwrap();
        drop(intents);

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut lane = test_lane(
            &db,
            &ledger,
            &state_root,
            Box::new(CleanupRunner::missing(
                identity.runner_container(),
                seen.clone(),
            )),
        );
        assert_eq!(lane.generation, current_generation);

        let report = lane.adopt_live_workers().unwrap();
        assert_eq!(report.awaiting_provision, 1);
        assert_eq!(report.adopted, 0);
        assert_eq!(report.failed, 0);
        assert_eq!(
            lane.registry.get(&key).unwrap().unwrap().generation,
            current_generation
        );

        // The processor's retry/upsert now lands in the claimed epoch instead
        // of being rejected as an old-generation row.
        let row = lane
            .registry
            .upsert(
                &key,
                "op-stale",
                4248,
                "velnor-7-4248",
                &identity.network(),
                state_dir.join("workspace").to_string_lossy().as_ref(),
                state_dir.join("dind-data").to_string_lossy().as_ref(),
                "sha256:runner",
                "sha256:dind",
            )
            .unwrap();
        assert_eq!(row.generation, current_generation);
        assert!(seen.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adoption_rejects_newer_provision_pending_row_without_downgrade() {
        let dir = unique_test_dir("adopt-newer-provision");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let ownership = OwnershipId::bind(7, "velnor-7-4249");
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str();
        let current_generation;
        let newer_generation;
        {
            let mut global = velnor_control::permit_ledger::PermitLedger::open(&ledger).unwrap();
            global.set_max_jobs(1).unwrap();
            current_generation = global.begin_epoch().unwrap();
            newer_generation = current_generation + 1;
        }

        let state_dir = state_root.join(ownership.slug());
        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry.set_generation(newer_generation);
        registry
            .upsert(
                &key,
                "op-newer",
                4249,
                "velnor-7-4249",
                &identity.network(),
                state_dir.join("workspace").to_string_lossy().as_ref(),
                state_dir.join("dind-data").to_string_lossy().as_ref(),
                "sha256:runner",
                "sha256:dind",
            )
            .unwrap();
        registry
            .set_state(&key, ScaleSetWorkerState::ProvisionIntent)
            .unwrap();
        drop(registry);

        let mut intents = ProvisionIntentStore::open(&db).unwrap();
        intents
            .record_intent(
                "op-newer",
                &crate::scaleset::intents::provision_ownership_id(7, "velnor-7-4249"),
                7,
                4249,
                "velnor-7-4249",
                "sha256:runner",
                "sha256:dind",
                newer_generation,
            )
            .unwrap();
        drop(intents);

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut lane = test_lane(
            &db,
            &ledger,
            &state_root,
            Box::new(CleanupRunner::missing(
                identity.runner_container(),
                seen.clone(),
            )),
        );
        assert_eq!(lane.generation, current_generation);

        let error = lane.adopt_live_workers().unwrap_err();
        assert!(error.to_string().contains("newer generation"));
        let row = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.generation, newer_generation);
        assert_eq!(row.worker_state, ScaleSetWorkerState::ProvisionIntent);
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(lane.live_workers(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adoption_claims_stale_uncertain_without_release_or_provision() {
        let dir = unique_test_dir("adopt-stale-uncertain");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let ownership = OwnershipId::bind(7, "velnor-7-4250");
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str();
        let holder = permit_holder(7, 4250);
        let previous_generation;
        let current_generation;
        {
            let mut global = velnor_control::permit_ledger::PermitLedger::open(&ledger).unwrap();
            global.set_max_jobs(1).unwrap();
            previous_generation = global.begin_epoch().unwrap();
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
                        velnor_control::permit_ledger::PermitState::Acquiring,
                        previous_generation,
                        None,
                    )
                    .unwrap(),
                velnor_control::permit_ledger::AcquireOutcome::Acquired
            );
            global
                .transition(
                    &holder,
                    velnor_control::permit_ledger::PermitState::Uncertain,
                    previous_generation,
                )
                .unwrap();
            current_generation = global.begin_epoch().unwrap();
        }

        let state_dir = state_root.join(ownership.slug());
        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry.set_generation(previous_generation);
        registry
            .upsert(
                &key,
                "op-uncertain",
                4250,
                "velnor-7-4250",
                &identity.network(),
                state_dir.join("workspace").to_string_lossy().as_ref(),
                state_dir.join("dind-data").to_string_lossy().as_ref(),
                "sha256:runner",
                "sha256:dind",
            )
            .unwrap();
        registry
            .set_state(&key, ScaleSetWorkerState::AcquireIntent)
            .unwrap();
        registry
            .set_state(&key, ScaleSetWorkerState::Uncertain)
            .unwrap();
        drop(registry);

        let mut intents = ProvisionIntentStore::open(&db).unwrap();
        intents
            .record_intent(
                "op-uncertain",
                &crate::scaleset::intents::provision_ownership_id(7, "velnor-7-4250"),
                7,
                4250,
                "velnor-7-4250",
                "sha256:runner",
                "sha256:dind",
                previous_generation,
            )
            .unwrap();
        drop(intents);

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut lane = test_lane(
            &db,
            &ledger,
            &state_root,
            Box::new(CleanupRunner::missing(
                identity.runner_container(),
                seen.clone(),
            )),
        );
        assert_eq!(lane.generation, current_generation);

        let report = lane.adopt_live_workers().unwrap();
        assert_eq!(report.awaiting_provision, 1);
        assert_eq!(report.adopted, 0);
        assert_eq!(report.failed, 0);
        let row = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.generation, current_generation);
        assert_eq!(row.worker_state, ScaleSetWorkerState::Uncertain);
        assert_eq!(
            lane.ledger.holder_state(&holder).unwrap(),
            Some(LedgerPermitState::Uncertain)
        );
        assert_eq!(lane.ledger.occupied().unwrap(), 1);
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(lane.live_workers(), 0);
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
                "op-3",
                4246,
                "velnor-7-4246",
                &identity.network(),
                state_dir.join("workspace").to_string_lossy().as_ref(),
                state_dir.join("dind-data").to_string_lossy().as_ref(),
                "sha256:runner",
                "sha256:dind",
            )
            .unwrap();
        registry
            .set_state(&key, ScaleSetWorkerState::Running)
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
                1,
            )
            .unwrap();
        drop(intents);

        // Dead pair (nothing answers) + a stuck runner removal: the tick
        // fails the worker, the terminal path retains the permit
        // uncertain, and adoption must count the failure — not an adoption.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::fail_runner_remove(identity.runner_container(), seen.clone());
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
    fn released_row_backfill_preserves_diagnostics_without_stranding() {
        // A worker row recorded as `permit_released` before the runtime
        // table existed: the backfill adopts it, and both adoption and a
        // terminal replay treat it exactly like a fresh release — the
        // permit converges to released, the exported diagnostics stay on
        // disk for post-mortem, no Docker work runs, nothing strands.
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

        // Seed the holder as held: a restart re-attested the permit from
        // the still-active demand row, so the replay must converge it.
        let holder = permit_holder(7, 4247);
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
                1,
            )
            .unwrap();
        drop(intents);

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::missing(identity.runner_container(), seen.clone());
        let mut lane = test_lane(&db, &ledger, &state_root, Box::new(runner));

        // The backfill adopted the legacy row with defaults.
        let row = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::PermitReleased);
        assert!(!row.diagnostics_complete);

        // Adoption skips the released row without Docker work.
        let report = lane.adopt_live_workers().unwrap();
        assert_eq!(report.skipped_released, 1);
        assert_eq!(report.resumed_cleanup, 0);
        assert_eq!(report.failed, 0);
        assert!(seen.lock().unwrap().is_empty());

        // A terminal replay converges the re-attested permit and keeps
        // the exported diagnostics; nothing strands.
        lane.drive_terminal(&key).unwrap();
        assert!(seen.lock().unwrap().is_empty());
        assert!(state_dir.join("raw-job.log").is_file());
        assert!(
            state_dir.join("diagnostics/capture.complete").is_file(),
            "backfilled release preserves diagnostics like a fresh release"
        );
        let row = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::PermitReleased);
        assert_eq!(lane.ledger.holder_state(&holder).unwrap(), None);
        assert_eq!(lane.ledger.occupied().unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

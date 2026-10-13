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
use std::path::{Path, PathBuf};
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
                        generation, created_at, updated_at
                 FROM scaleset_workers WHERE ownership_id = ?1",
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
                        generation, created_at, updated_at
                 FROM scaleset_workers WHERE request_id = ?1 LIMIT 1",
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
                        generation, created_at, updated_at
                 FROM scaleset_workers
                 WHERE worker_state != 'permit_released' ORDER BY created_at ASC",
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
                 WHERE ownership_id = ?4",
                params![state.as_str(), generation, now, ownership_id],
            )
            .context("record worker edge")?;
        if updated == 0 {
            anyhow::bail!("worker registry holds no row for {ownership_id:?}");
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

    fn worker_state_dir(&self, ownership: &OwnershipId) -> PathBuf {
        self.config.state_root.join(ownership.slug())
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
        let state_dir = self.worker_state_dir(identity.ownership());
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
                supervision: Supervision::new(identity, &state_dir),
            },
        );
        Ok(())
    }

    /// Tick one worker's supervision. `WorkerFailed` drives the terminal
    /// path immediately (explicit fail with diagnostics + cleanup).
    fn tick_worker(&mut self, key: &str) -> Result<SupervisionOutcome, LaneError> {
        self.ensure_live(key)
            .map_err(|error| LaneError::new("adopt worker", error))?;
        let recorded = self
            .worker_state(key)
            .map_err(|error| LaneError::new("adopt worker", error))?;
        if recorded == ScaleSetWorkerState::PermitReleased {
            return Ok(SupervisionOutcome::Healthy);
        }
        let outcome = {
            let live = self
                .workers
                .get_mut(key)
                .with_context(|| format!("live worker {key:?} vanished"))
                .map_err(|error| LaneError::new("adopt worker", error))?;
            live.supervision
                .tick(&mut *self.runner, recorded)
                .map_err(|error| LaneError::new("supervise worker", error))?
        };
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
        let keys: Vec<String> = self.workers.keys().cloned().collect();
        for key in keys {
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
                // Stays live at `owned_cleanup`: the redelivered retry
                // resumes the terminal path from the durable row.
                anyhow::bail!("owned cleanup failed for {key}: {}", failures.join("; "))
            }
        }
    }

    fn drive_terminal_inner(&mut self, key: &str) -> Result<TerminalOutcome> {
        // Unknown worker: nothing provisioned, only the permit (if held)
        // needs releasing. The Processor moved it to `cleaning` before
        // calling; the release below finishes it.
        let row = self.registry.get(key)?;
        let Some(row) = row else {
            if let Some(holder) = holder_for_key(self.config.scale_set_id, key) {
                self.ledger.release(&holder)?;
            }
            return Ok(TerminalOutcome::AlreadyReleased);
        };
        if row.worker_state == ScaleSetWorkerState::PermitReleased {
            // The row says released, but a restart between the
            // adoption-time release and the completion observation lets
            // startup reconcile re-attest the permit from the
            // still-active demand row. Converge to the recorded truth.
            if let Some(holder) = holder_for_key(self.config.scale_set_id, key) {
                self.ledger.release(&holder)?;
            }
            self.workers.remove(key);
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
            self.transition_worker(key, ScaleSetWorkerState::OwnedCleanup)?;
        }
        let report = {
            let live = self
                .workers
                .get_mut(key)
                .with_context(|| format!("live worker {key:?} vanished"))?;
            live.supervision.cleanup(&mut *self.runner)?
        };
        if !report.confirmed() {
            // Cleanup failed: the permit stays as a visible `uncertain`
            // reservation (§4.1) and the worker stays at `owned_cleanup`
            // for the redelivered retry. Never released, never lost.
            if let Some(request_id) = row.request_id {
                let holder = permit_holder(self.config.scale_set_id, request_id);
                self.fenced_transition(&holder, LedgerPermitState::Uncertain)?;
            }
            tracing::warn!(
                worker = key,
                failures = report.failures.join("; ").as_str(),
                "scale-set owned cleanup failed; permit retained uncertain"
            );
            return Ok(TerminalOutcome::CleanupFailed {
                failures: report.failures,
            });
        }
        if let Some(request_id) = row.request_id {
            let holder = permit_holder(self.config.scale_set_id, request_id);
            self.ledger.release(&holder)?;
        }
        self.transition_worker(key, ScaleSetWorkerState::PermitReleased)?;
        Ok(TerminalOutcome::Released)
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
        let state_dir = self.worker_state_dir(&ownership);
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
                state_dir.join("workspace").to_string_lossy().as_ref(),
                state_dir.join("dind-data").to_string_lossy().as_ref(),
                &intent.runner_digest,
                &intent.dind_digest,
            )
            .map_err(|error| LaneError::new("record worker", error))?;

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
        let outcome = provision_worker(&mut *self.runner, &*self.hook, &plan, &std::thread::sleep)
            .map_err(|error| LaneError::new("provision worker pair", error))?;
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
                supervision: Supervision::new(identity, &state_dir),
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
        self.refresh_generation()?;
        self.opportunistic_sweep();
        let request_id = completed.base.runner_request_id;
        // Resolve the ownership key from the intent when one exists (the
        // common path), else from the request id directly.
        let key = self
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
            });
        self.drive_terminal(&key)
            .map_err(|error| LaneError::new("drive worker terminal", error))?;
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
}

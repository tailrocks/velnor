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
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use velnor_model::{
    RunnerScaleSetJitRunnerSetting, ScaleSetJobAssigned, ScaleSetJobCompleted, ScaleSetJobStarted,
    ScaleSetWorkerState,
};

use crate::scaleset::converge::WorkerLane;
use crate::scaleset::demand::{DemandState as LocalDemandState, DemandStore};
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

const RELEASED_STATE_DIR_CLEANUP_MIGRATION: &str = "released_state_dir_cleanup_v2";
const TERMINAL_STATE_DIR_CLEANUP_MIGRATION: &str = "terminal_state_dir_cleanup_v3";
const DOCKER_CREATE_TARGETS_MIGRATION: &str = "docker_create_targets_v2";
const RUNNER_START_ATTEMPT_MIGRATION: &str = "runner_start_attempt_v1";

/// Opaque proof that the worker registry durably authorized deleting this
/// worker's recorded host state during terminal cleanup. Only the registry
/// can create it, and the token binds the exact recorded state path.
#[derive(Debug)]
pub(crate) struct ReleasedStateCleanupAuthorization {
    ownership_id: String,
    state_dir: PathBuf,
}

impl ReleasedStateCleanupAuthorization {
    pub(crate) fn authorizes(&self, ownership_id: &str, state_dir: &Path) -> bool {
        self.ownership_id == ownership_id && self.state_dir.as_path() == state_dir
    }
}

#[cfg(test)]
pub(crate) fn test_release_authorization(
    ownership_id: &str,
    state_dir: &Path,
) -> ReleasedStateCleanupAuthorization {
    ReleasedStateCleanupAuthorization {
        ownership_id: ownership_id.to_owned(),
        state_dir: state_dir.to_path_buf(),
    }
}

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
    /// A start request may have reached Docker while the worker is still
    /// durably in ProvisionIntent. Such rows must be health-ticked, never
    /// provisioned again.
    pub runner_start_attempted: bool,
    pub dind_restarts_used: u32,
    pub diagnostics_complete: bool,
    pub state_dir_cleanup_pending: bool,
    /// Docker may still materialize this resource from an accepted create
    /// request. This remains set across ambiguous CLI failures and process
    /// restarts until create success/rejection or an inspect proves the
    /// exact owned object exists.
    pub pending_docker_create: Option<crate::scaleset::worker::runner::DockerCreateTarget>,
    /// A pre-session startup failure is durable evidence that this request
    /// must wait for GitHub's completion/cancellation before any new start.
    pub awaiting_upstream_completion: bool,
    /// A DinD restart was accepted but its private socket is not ready yet.
    pub dind_restart_ready_deadline_epoch: Option<u64>,
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
        let mut conn = Connection::open(path).context("open worker registry database")?;
        conn.busy_timeout(Duration::from_secs(5))
            .context("set worker registry busy timeout")?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin worker registry runtime migration")?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS scaleset_worker_runtime (
                 ownership_id TEXT PRIMARY KEY,
                 runner_start_deadline_epoch INTEGER,
                 runner_start_attempted INTEGER NOT NULL DEFAULT 0
                     CHECK (runner_start_attempted IN (0, 1)),
                 dind_restarts_used INTEGER NOT NULL DEFAULT 0 CHECK (dind_restarts_used >= 0),
                 diagnostics_complete INTEGER NOT NULL DEFAULT 0
                     CHECK (diagnostics_complete IN (0, 1)),
                 state_dir_cleanup_pending INTEGER NOT NULL DEFAULT 0
                     CHECK (state_dir_cleanup_pending IN (0, 1)),
                 pending_docker_create TEXT CHECK (
                     pending_docker_create IS NULL OR
                     pending_docker_create IN (
                         'network', 'dind', 'dind-data-volume', 'workspace-volume', 'runner'
                     )
                 ),
                 awaiting_upstream_completion INTEGER NOT NULL DEFAULT 0
                     CHECK (awaiting_upstream_completion IN (0, 1)),
                 dind_restart_ready_deadline_epoch INTEGER
             );
             CREATE TABLE IF NOT EXISTS scaleset_worker_migrations (
                 migration_id TEXT PRIMARY KEY
             );
             INSERT OR IGNORE INTO scaleset_worker_runtime
                 (ownership_id, runner_start_deadline_epoch, dind_restarts_used,
                  diagnostics_complete, state_dir_cleanup_pending)
             SELECT ownership_id, NULL, 0, 0, 0 FROM scaleset_workers;",
        )
        .context("create durable scale-set runtime table")?;
        // `CREATE TABLE IF NOT EXISTS` does not expand existing databases.
        // Ambiguous create-stage rows cannot be made safe by guessing a
        // target, so refuse to open while any old marker remains set. A
        // cleared marker carries no unresolved state and can be removed.
        let columns = {
            let mut statement = tx
                .prepare("PRAGMA table_info(scaleset_worker_runtime)")
                .context("inspect worker runtime schema")?;
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))
                .context("read worker runtime columns")?
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("collect worker runtime columns")?;
            columns
        };
        let has_pending_docker_create = columns.iter().any(|name| name == "pending_docker_create");
        let has_runner_start_attempted =
            columns.iter().any(|name| name == "runner_start_attempted");
        let has_legacy_docker_create_stage_unknown = columns
            .iter()
            .any(|name| name == "legacy_docker_create_stage_unknown");
        let has_awaiting_upstream_completion = columns
            .iter()
            .any(|name| name == "awaiting_upstream_completion");
        let has_dind_restart_ready_deadline_epoch = columns
            .iter()
            .any(|name| name == "dind_restart_ready_deadline_epoch");
        let has_runner_create_pending = columns.iter().any(|name| name == "runner_create_pending");
        if !has_pending_docker_create {
            let ambiguous: Option<String> = tx
                .query_row(
                    "SELECT ownership_id FROM scaleset_workers
                     WHERE worker_state = 'provision_intent' LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .context("inspect workers without durable Docker create intent")?;
            if let Some(ownership_id) = ambiguous {
                anyhow::bail!(
                    "worker {ownership_id:?} predates durable Docker create intent; reconcile its owned Docker resources and repair or reset the state database before reopening"
                );
            }
            tx.execute_batch(
                "ALTER TABLE scaleset_worker_runtime
                 ADD COLUMN pending_docker_create TEXT CHECK (
                     pending_docker_create IS NULL OR
                     pending_docker_create IN (
                         'network', 'dind', 'dind-data-volume', 'workspace-volume', 'runner'
                     )
                 );",
            )
            .context("add durable Docker create intent")?;
        }
        if has_legacy_docker_create_stage_unknown {
            let ambiguous: Option<String> = tx
                .query_row(
                    "SELECT ownership_id FROM scaleset_worker_runtime
                     WHERE legacy_docker_create_stage_unknown IS NOT 0 LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .context("inspect ambiguous Docker create state")?;
            if let Some(ownership_id) = ambiguous {
                anyhow::bail!(
                    "worker {ownership_id:?} has an ambiguous Docker create stage; reconcile its owned Docker resources and repair or reset the state database before reopening"
                );
            }
            tx.execute_batch(
                "ALTER TABLE scaleset_worker_runtime
                 DROP COLUMN legacy_docker_create_stage_unknown;",
            )
            .context("remove obsolete Docker create stage state")?;
        }
        if !has_runner_start_attempted {
            tx.execute_batch(
                "ALTER TABLE scaleset_worker_runtime
                 ADD COLUMN runner_start_attempted INTEGER NOT NULL DEFAULT 0
                     CHECK (runner_start_attempted IN (0, 1));",
            )
            .context("add durable runner start attempt")?;
        }
        if !has_awaiting_upstream_completion {
            tx.execute_batch(
                "ALTER TABLE scaleset_worker_runtime
                 ADD COLUMN awaiting_upstream_completion INTEGER NOT NULL DEFAULT 0
                     CHECK (awaiting_upstream_completion IN (0, 1));",
            )
            .context("add upstream completion wait marker")?;
        }
        if !has_dind_restart_ready_deadline_epoch {
            tx.execute_batch(
                "ALTER TABLE scaleset_worker_runtime
                 ADD COLUMN dind_restart_ready_deadline_epoch INTEGER;",
            )
            .context("add DinD restart readiness deadline")?;
        }
        // A runner-only create marker cannot prove whether earlier network,
        // DinD, or volume requests were accepted. Keep such databases
        // closed instead of translating incomplete history into an exact
        // target.
        if has_runner_create_pending {
            let ambiguous: Option<String> = tx
                .query_row(
                    "SELECT ownership_id FROM scaleset_worker_runtime
                     WHERE runner_create_pending IS NOT 0 LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .context("inspect runner-only Docker create state")?;
            if let Some(ownership_id) = ambiguous {
                anyhow::bail!(
                    "worker {ownership_id:?} has a runner-only Docker create marker; reconcile its owned Docker resources and repair or reset the state database before reopening"
                );
            }
            tx.execute_batch(
                "ALTER TABLE scaleset_worker_runtime DROP COLUMN runner_create_pending;",
            )
            .context("remove obsolete runner-only create state")?;
        }
        let runner_start_attempt_migrated = tx
            .execute(
                "INSERT OR IGNORE INTO scaleset_worker_migrations (migration_id) VALUES (?1)",
                params![RUNNER_START_ATTEMPT_MIGRATION],
            )
            .context("record runner start attempt migration")?
            != 0;
        if runner_start_attempt_migrated {
            tx.execute(
                "UPDATE scaleset_worker_runtime
                 SET runner_start_attempted = 1
                 WHERE ownership_id IN (
                     SELECT ownership_id FROM scaleset_workers
                     WHERE worker_state = 'provision_intent'
                 )",
                [],
            )
            .context("fence legacy runner start attempts")?;
        }
        let docker_create_targets_migrated = tx
            .execute(
                "INSERT OR IGNORE INTO scaleset_worker_migrations (migration_id) VALUES (?1)",
                params![DOCKER_CREATE_TARGETS_MIGRATION],
            )
            .context("record Docker create target migration")?
            != 0;
        if docker_create_targets_migrated {
            tx.execute_batch(
                "CREATE TABLE scaleset_worker_runtime_new (
                     ownership_id TEXT PRIMARY KEY,
                     runner_start_deadline_epoch INTEGER,
                     runner_start_attempted INTEGER NOT NULL DEFAULT 0
                         CHECK (runner_start_attempted IN (0, 1)),
                     dind_restarts_used INTEGER NOT NULL DEFAULT 0 CHECK (dind_restarts_used >= 0),
                     diagnostics_complete INTEGER NOT NULL DEFAULT 0
                         CHECK (diagnostics_complete IN (0, 1)),
                     state_dir_cleanup_pending INTEGER NOT NULL DEFAULT 0
                         CHECK (state_dir_cleanup_pending IN (0, 1)),
                     pending_docker_create TEXT CHECK (
                         pending_docker_create IS NULL OR
                         pending_docker_create IN (
                             'network', 'dind', 'dind-data-volume', 'workspace-volume', 'runner'
                         )
                     ),
                     awaiting_upstream_completion INTEGER NOT NULL DEFAULT 0
                         CHECK (awaiting_upstream_completion IN (0, 1)),
                     dind_restart_ready_deadline_epoch INTEGER
                 );
                 INSERT INTO scaleset_worker_runtime_new (
                     ownership_id, runner_start_deadline_epoch, runner_start_attempted,
                     dind_restarts_used,
                     diagnostics_complete, state_dir_cleanup_pending, pending_docker_create,
                     awaiting_upstream_completion, dind_restart_ready_deadline_epoch
                 )
                 SELECT ownership_id, runner_start_deadline_epoch, runner_start_attempted,
                        dind_restarts_used,
                        diagnostics_complete, state_dir_cleanup_pending, pending_docker_create,
                        awaiting_upstream_completion, dind_restart_ready_deadline_epoch
                 FROM scaleset_worker_runtime;
                 DROP TABLE scaleset_worker_runtime;
                 ALTER TABLE scaleset_worker_runtime_new RENAME TO scaleset_worker_runtime;",
            )
            .context("widen durable Docker create target constraint")?;
        }
        tx.execute(
            "DELETE FROM scaleset_worker_migrations
             WHERE migration_id IN (
                 'docker_create_intent_v1',
                 'legacy_provision_stage_unknown_v1'
             )",
            [],
        )
        .context("remove obsolete Docker create migrations")?;
        // Recover state dirs left behind by the old release-before-delete
        // ordering once. This newer migration also repairs databases where
        // v1 was already recorded while a PermitReleased row still had no
        // pending bit. A separate one-time migration repairs legacy
        // terminal rows left with a cleared/missing bit; current rows set
        // that bit atomically on their lifecycle edge.
        let released_cleanup_migrated = tx
            .execute(
                "INSERT OR IGNORE INTO scaleset_worker_migrations (migration_id) VALUES (?1)",
                params![RELEASED_STATE_DIR_CLEANUP_MIGRATION],
            )
            .context("record released state cleanup migration")?
            != 0;
        if released_cleanup_migrated {
            tx.execute(
                "UPDATE scaleset_worker_runtime
                 SET state_dir_cleanup_pending = 1
                 WHERE ownership_id IN (
                     SELECT ownership_id FROM scaleset_workers
                     WHERE worker_state = 'permit_released'
                 )",
                [],
            )
            .context("migrate released worker state cleanup")?;
        }
        let terminal_cleanup_migrated = tx
            .execute(
                "INSERT OR IGNORE INTO scaleset_worker_migrations (migration_id) VALUES (?1)",
                params![TERMINAL_STATE_DIR_CLEANUP_MIGRATION],
            )
            .context("record terminal worker state cleanup migration")?
            != 0;
        if terminal_cleanup_migrated {
            tx.execute(
                "UPDATE scaleset_worker_runtime
                 SET state_dir_cleanup_pending = 1
                 WHERE ownership_id IN (
                     SELECT ownership_id FROM scaleset_workers
                     WHERE worker_state IN ('owned_cleanup', 'permit_released')
                 )",
                [],
            )
            .context("migrate terminal worker state cleanup")?;
        }
        tx.commit()
            .context("commit worker registry runtime migration")?;
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
            runner_start_attempted: row.get::<_, i64>(20)? != 0,
            dind_restarts_used: row.get::<_, i64>(16)?.clamp(0, u32::MAX as i64) as u32,
            diagnostics_complete: row.get::<_, i64>(17)? != 0,
            state_dir_cleanup_pending: row.get::<_, i64>(18)? != 0,
            pending_docker_create: match row.get::<_, Option<String>>(19)? {
                Some(raw) => Some(
                    crate::scaleset::worker::runner::DockerCreateTarget::parse(&raw).ok_or_else(
                        || {
                            rusqlite::Error::FromSqlConversionFailure(
                                19,
                                rusqlite::types::Type::Text,
                                anyhow::anyhow!("unknown pending Docker create target {raw:?}")
                                    .into(),
                            )
                        },
                    )?,
                ),
                None => None,
            },
            awaiting_upstream_completion: row.get::<_, i64>(21)? != 0,
            dind_restart_ready_deadline_epoch: row
                .get::<_, Option<i64>>(22)?
                .map(|seconds| seconds.max(0) as u64),
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
                  diagnostics_complete, state_dir_cleanup_pending, pending_docker_create)
                 VALUES (?1, NULL, 0, 0, 0, NULL)",
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
                        COALESCE(r.diagnostics_complete, 0), COALESCE(r.state_dir_cleanup_pending, 0),
                        r.pending_docker_create, r.runner_start_attempted,
                        COALESCE(r.awaiting_upstream_completion, 0),
                        r.dind_restart_ready_deadline_epoch
                 FROM scaleset_workers w
                 LEFT JOIN scaleset_worker_runtime r USING (ownership_id)
                 WHERE w.ownership_id = ?1",
                params![ownership_id],
                Self::row_to_worker,
            )
            .optional()
            .context("fetch worker row")
    }

    fn authorize_state_cleanup(
        &self,
        ownership_id: &str,
    ) -> Result<ReleasedStateCleanupAuthorization> {
        let row = self
            .get(ownership_id)?
            .with_context(|| format!("worker registry has no row for {ownership_id:?}"))?;
        if row.worker_state != ScaleSetWorkerState::PermitReleased || !row.state_dir_cleanup_pending
        {
            anyhow::bail!(
                "worker {ownership_id:?} has no durable released-state cleanup authorization"
            );
        }
        let (raw_set_id, _) = ownership_id
            .split_once('/')
            .context("worker ownership id has no scale-set prefix")?;
        let scale_set_id = raw_set_id
            .parse::<i32>()
            .context("worker ownership id has an invalid scale-set prefix")?;
        let expected_ownership = OwnershipId::bind(scale_set_id, &row.runner_name);
        if expected_ownership.as_str() != ownership_id {
            anyhow::bail!("worker ownership id does not match its recorded runner name");
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
            .context("recorded workspace path has no state directory")?
            .to_path_buf();
        if state_dir
            .file_name()
            .is_none_or(|name| name.to_string_lossy() != expected_ownership.slug())
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
        Ok(ReleasedStateCleanupAuthorization {
            ownership_id: ownership_id.to_owned(),
            state_dir,
        })
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
                        COALESCE(r.diagnostics_complete, 0), COALESCE(r.state_dir_cleanup_pending, 0),
                        r.pending_docker_create, r.runner_start_attempted,
                        COALESCE(r.awaiting_upstream_completion, 0),
                        r.dind_restart_ready_deadline_epoch
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
                        COALESCE(r.diagnostics_complete, 0), COALESCE(r.state_dir_cleanup_pending, 0),
                        r.pending_docker_create, r.runner_start_attempted,
                        COALESCE(r.awaiting_upstream_completion, 0),
                        r.dind_restart_ready_deadline_epoch
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
                        COALESCE(r.diagnostics_complete, 0), COALESCE(r.state_dir_cleanup_pending, 0),
                        r.pending_docker_create, r.runner_start_attempted,
                        COALESCE(r.awaiting_upstream_completion, 0),
                        r.dind_restart_ready_deadline_epoch
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
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin worker state transaction")?;
        let previous_state: Option<String> = tx
            .query_row(
                "SELECT worker_state FROM scaleset_workers WHERE ownership_id = ?1",
                params![ownership_id],
                |row| row.get(0),
            )
            .optional()
            .context("read prior worker state")?;
        let previous_state = previous_state
            .with_context(|| format!("worker registry holds no row for {ownership_id:?}"))?;
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
        if state != ScaleSetWorkerState::ProvisionIntent {
            tx.execute(
                "UPDATE scaleset_worker_runtime SET runner_start_attempted = 0
                 WHERE ownership_id = ?1",
                params![ownership_id],
            )
            .context("clear runner start attempt after lifecycle advance")?;
        }
        if matches!(
            state,
            ScaleSetWorkerState::OwnedCleanup | ScaleSetWorkerState::PermitReleased
        ) {
            let pending = i64::from(
                state == ScaleSetWorkerState::OwnedCleanup
                    || previous_state != ScaleSetWorkerState::PermitReleased.as_str(),
            );
            tx.execute(
                "INSERT INTO scaleset_worker_runtime
                 (ownership_id, runner_start_deadline_epoch, dind_restarts_used,
                  diagnostics_complete, state_dir_cleanup_pending)
                 VALUES (?1, NULL, 0, 0, ?2)
                 ON CONFLICT(ownership_id) DO UPDATE SET
                   state_dir_cleanup_pending = MAX(
                     state_dir_cleanup_pending,
                     excluded.state_dir_cleanup_pending
                   )",
                params![ownership_id, pending],
            )
            .context("record worker state cleanup phase")?;

            // Reaching OwnedCleanup or entering PermitReleased must leave a
            // durable cleanup fence in the same transaction as the lifecycle
            // edge. A row that was already PermitReleased may legitimately
            // have cleared the bit after a prior successful deletion.
            let cleanup_pending_required = state == ScaleSetWorkerState::OwnedCleanup
                || (state == ScaleSetWorkerState::PermitReleased
                    && previous_state != ScaleSetWorkerState::PermitReleased.as_str());
            if cleanup_pending_required {
                let pending: bool = tx
                    .query_row(
                        "SELECT state_dir_cleanup_pending = 1
                         FROM scaleset_worker_runtime WHERE ownership_id = ?1",
                        params![ownership_id],
                        |row| row.get(0),
                    )
                    .context("verify worker state cleanup fence")?;
                if !pending {
                    anyhow::bail!(
                        "worker {ownership_id:?} entered {} without a cleanup-pending fence",
                        state.as_str()
                    );
                }
            }
        }
        tx.commit().context("commit worker state transition")?;
        Ok(())
    }

    /// Persist one Docker create boundary before the Engine can receive the
    /// request. Runner startup deadline is recorded in the same transaction
    /// only for runner creates.
    fn begin_docker_create(
        &mut self,
        ownership_id: &str,
        target: crate::scaleset::worker::runner::DockerCreateTarget,
        deadline_epoch: u64,
    ) -> Result<Option<u64>> {
        let deadline = i64::try_from(deadline_epoch).unwrap_or(i64::MAX);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin Docker create intent")?;
        let updated = if target == crate::scaleset::worker::runner::DockerCreateTarget::Runner {
            tx.execute(
                "UPDATE scaleset_worker_runtime
                 SET runner_start_deadline_epoch = COALESCE(runner_start_deadline_epoch, ?1),
                     pending_docker_create = ?2
                 WHERE ownership_id = ?3
                   AND pending_docker_create IS NULL
                   AND EXISTS (
                       SELECT 1 FROM scaleset_workers
                       WHERE scaleset_workers.ownership_id = ?3
                         AND scaleset_workers.worker_state = ?4
                   )",
                params![
                    deadline,
                    target.as_str(),
                    ownership_id,
                    ScaleSetWorkerState::ProvisionIntent.as_str()
                ],
            )
        } else {
            tx.execute(
                "UPDATE scaleset_worker_runtime
                 SET pending_docker_create = ?1
                 WHERE ownership_id = ?2
                   AND pending_docker_create IS NULL
                   AND EXISTS (
                       SELECT 1 FROM scaleset_workers
                       WHERE scaleset_workers.ownership_id = ?2
                         AND scaleset_workers.worker_state = ?3
                   )",
                params![
                    target.as_str(),
                    ownership_id,
                    ScaleSetWorkerState::ProvisionIntent.as_str()
                ],
            )
        }
        .context("persist Docker create intent")?;
        if updated == 0 {
            let runtime: Option<Option<String>> = tx
                .query_row(
                    "SELECT pending_docker_create
                     FROM scaleset_worker_runtime
                     WHERE ownership_id = ?1",
                    params![ownership_id],
                    |row| row.get(0),
                )
                .optional()
                .context("inspect existing Docker create intent")?;
            let existing = runtime.flatten();
            if let Some(existing) = existing {
                anyhow::bail!(
                    "worker {ownership_id:?} has unresolved {existing} create; refusing {target:?} create"
                );
            }
            anyhow::bail!(
                "worker {ownership_id:?} is not in ProvisionIntent; refusing Docker create"
            );
        }
        let stored: Option<i64> = tx
            .query_row(
                "SELECT runner_start_deadline_epoch FROM scaleset_worker_runtime
                 WHERE ownership_id = ?1",
                params![ownership_id],
                |row| row.get(0),
            )
            .context("read runner startup deadline after create intent")?;
        tx.commit().context("commit Docker create intent")?;
        Ok(stored.map(|seconds| seconds.max(0) as u64))
    }

    /// Clear an accepted or explicitly rejected create attempt after its
    /// Docker result is definitive, or after a locked inspect proves the
    /// exact owned object exists. A different resource's pending create is
    /// never cleared by this event.
    fn resolve_docker_create(
        &mut self,
        ownership_id: &str,
        target: crate::scaleset::worker::runner::DockerCreateTarget,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin Docker create resolution")?;
        let pending: Option<String> = tx
            .query_row(
                "SELECT pending_docker_create FROM scaleset_worker_runtime
                 WHERE ownership_id = ?1",
                params![ownership_id],
                |row| row.get(0),
            )
            .optional()
            .context("read pending Docker create")?
            .flatten();
        if pending.as_deref() == Some(target.as_str()) {
            tx.execute(
                "UPDATE scaleset_worker_runtime SET pending_docker_create = NULL
                 WHERE ownership_id = ?1 AND pending_docker_create = ?2",
                params![ownership_id, target.as_str()],
            )
            .context("resolve Docker create intent")?;
        } else if pending.is_none() {
            // A successful response may be replayed after resolution already
            // committed. This is an exact idempotent receipt.
        } else if pending.is_some() {
            // An earlier resource may still be pending. Provision is barred
            // from a second create in that state; only normal adopts reach
            // this branch, so retain the other resource's fence.
        }
        tx.commit().context("commit Docker create resolution")?;
        Ok(())
    }

    fn resolve_pending_docker_create_after_inspect(
        &mut self,
        ownership_id: &str,
        identity: &WorkerIdentity,
        runner: &mut dyn WorkerRunner,
    ) -> Result<()> {
        let Some(row) = self.get(ownership_id)? else {
            anyhow::bail!("worker runtime holds no row for {ownership_id:?}");
        };
        if let Some(target) = row.pending_docker_create {
            // The lifecycle lock serializes Velnor callers, but Docker may
            // still materialize an accepted request after its CLI exits.
            // Only exact owned presence settles this intent; absence does
            // not prove rejection and must keep the create fence in place.
            let exists =
                crate::scaleset::worker::dind::create_target_exists(runner, identity, target)
                    .with_context(|| {
                        format!("inspect pending {target:?} create for {ownership_id}")
                    })?;
            if !exists {
                anyhow::bail!(
                    "pending {target:?} Docker create for {ownership_id:?} is absent but unresolved; refusing replay"
                );
            }
            self.resolve_docker_create(ownership_id, target)?;
        }
        Ok(())
    }

    fn set_runner_start_deadline_if_none(
        &mut self,
        ownership_id: &str,
        deadline_epoch: u64,
    ) -> Result<u64> {
        let deadline = i64::try_from(deadline_epoch).unwrap_or(i64::MAX);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin runner start attempt")?;
        let updated = tx
            .execute(
                "UPDATE scaleset_worker_runtime
             SET runner_start_deadline_epoch = COALESCE(runner_start_deadline_epoch, ?1),
                 runner_start_attempted = 1
             WHERE ownership_id = ?2",
                params![deadline, ownership_id],
            )
            .context("persist runner start attempt")?;
        if updated == 0 {
            anyhow::bail!("worker runtime holds no row for {ownership_id:?}");
        }
        let stored: Option<i64> = tx
            .query_row(
                "SELECT runner_start_deadline_epoch FROM scaleset_worker_runtime
             WHERE ownership_id = ?1",
                params![ownership_id],
                |row| row.get(0),
            )
            .context("read runner startup deadline")?;
        let stored = stored
            .map(|seconds| seconds.max(0) as u64)
            .with_context(|| format!("worker runtime row {ownership_id:?} is missing"))?;
        tx.commit().context("commit runner start attempt")?;
        Ok(stored)
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

    fn set_dind_restart_runtime(
        &mut self,
        ownership_id: &str,
        used: u32,
        ready_deadline_epoch: Option<u64>,
    ) -> Result<()> {
        let used = i64::from(used);
        let ready_deadline =
            ready_deadline_epoch.map(|value| i64::try_from(value).unwrap_or(i64::MAX));
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin DinD restart runtime update")?;
        let updated = tx
            .execute(
                "UPDATE scaleset_worker_runtime
                 SET dind_restarts_used = MAX(dind_restarts_used, ?1),
                     dind_restart_ready_deadline_epoch = ?2
                 WHERE ownership_id = ?3",
                params![used, ready_deadline, ownership_id],
            )
            .context("persist DinD restart runtime")?;
        if updated == 0 {
            anyhow::bail!("worker runtime holds no row for {ownership_id:?}");
        }
        tx.commit().context("commit DinD restart runtime")?;
        Ok(())
    }

    fn set_awaiting_upstream_completion(&mut self, ownership_id: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin upstream completion wait marker")?;
        let state: Option<String> = tx
            .query_row(
                "SELECT worker_state FROM scaleset_workers WHERE ownership_id = ?1",
                params![ownership_id],
                |row| row.get(0),
            )
            .optional()
            .context("read worker state for upstream completion wait")?;
        let state =
            state.with_context(|| format!("worker registry holds no row for {ownership_id:?}"))?;
        if !matches!(
            parse_worker_state(&state)?,
            ScaleSetWorkerState::ProvisionIntent | ScaleSetWorkerState::DindReady
        ) {
            anyhow::bail!(
                "worker {ownership_id:?} cannot await pre-session completion from state {state:?}"
            );
        }
        let updated = tx
            .execute(
                "UPDATE scaleset_worker_runtime
                 SET awaiting_upstream_completion = 1
                 WHERE ownership_id = ?1",
                params![ownership_id],
            )
            .context("persist upstream completion wait marker")?;
        if updated == 0 {
            anyhow::bail!("worker runtime holds no row for {ownership_id:?}");
        }
        tx.commit()
            .context("commit upstream completion wait marker")?;
        Ok(())
    }

    fn clear_state_dir_cleanup_pending(&mut self, ownership_id: &str) -> Result<()> {
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_worker_runtime SET state_dir_cleanup_pending = 0
             WHERE ownership_id = ?1 AND state_dir_cleanup_pending = 1
               AND EXISTS (
                   SELECT 1 FROM scaleset_workers
                   WHERE scaleset_workers.ownership_id = ?1
                     AND scaleset_workers.worker_state = 'permit_released'
               )",
                params![ownership_id],
            )
            .context("clear released state cleanup marker")?;
        if updated == 0 {
            anyhow::bail!("worker {ownership_id:?} has no pending released-state cleanup to clear");
        }
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

    fn clear_diagnostics_complete(&mut self, ownership_id: &str) -> Result<()> {
        let updated = self
            .conn
            .execute(
                "UPDATE scaleset_worker_runtime SET diagnostics_complete = 0
                 WHERE ownership_id = ?1
                   AND EXISTS (
                       SELECT 1 FROM scaleset_workers
                       WHERE scaleset_workers.ownership_id = ?1
                         AND scaleset_workers.worker_state = ?2
                   )",
                params![ownership_id, ScaleSetWorkerState::ProvisionIntent.as_str()],
            )
            .context("invalidate pre-runner diagnostics checkpoint")?;
        if updated == 0 {
            anyhow::bail!(
                "worker {ownership_id:?} is no longer in provision intent; cannot invalidate diagnostics"
            );
        }
        Ok(())
    }
}

/// Capability for worker lifecycle operations that require the held lock for
/// one exact ownership key. Worker modules cannot construct one themselves;
/// lane call sites mint it only after acquiring the Docker lifecycle lock.
pub(crate) struct WorkerLifecycleCapability<'a> {
    lock: &'a crate::scaleset::reconcile::WorkerDockerLifecycleLock,
    registry: &'a mut WorkerRegistry,
    ownership_id: String,
    runner_start_deadline_epoch: Option<&'a mut Option<u64>>,
}

impl<'a> WorkerLifecycleCapability<'a> {
    fn for_provision(
        lock: &'a crate::scaleset::reconcile::WorkerDockerLifecycleLock,
        registry: &'a mut WorkerRegistry,
        ownership_id: &str,
        runner_start_deadline_epoch: &'a mut Option<u64>,
    ) -> Result<Self> {
        let capability = Self {
            lock,
            registry,
            ownership_id: ownership_id.to_owned(),
            runner_start_deadline_epoch: Some(runner_start_deadline_epoch),
        };
        let row = capability.row_for(ownership_id)?;
        anyhow::ensure!(
            row.worker_state == ScaleSetWorkerState::ProvisionIntent,
            "worker {ownership_id:?} is not in ProvisionIntent for provisioning"
        );
        anyhow::ensure!(
            row.pending_docker_create.is_none(),
            "worker {ownership_id:?} has an unresolved Docker create before provisioning"
        );
        Ok(capability)
    }

    fn for_tick(
        lock: &'a crate::scaleset::reconcile::WorkerDockerLifecycleLock,
        registry: &'a mut WorkerRegistry,
        ownership_id: &str,
    ) -> Result<Self> {
        let capability = Self {
            lock,
            registry,
            ownership_id: ownership_id.to_owned(),
            runner_start_deadline_epoch: None,
        };
        capability.validate_tick_owner(ownership_id)?;
        Ok(capability)
    }

    fn for_cleanup(
        lock: &'a crate::scaleset::reconcile::WorkerDockerLifecycleLock,
        registry: &'a mut WorkerRegistry,
        ownership_id: &str,
    ) -> Result<Self> {
        let capability = Self {
            lock,
            registry,
            ownership_id: ownership_id.to_owned(),
            runner_start_deadline_epoch: None,
        };
        capability.validate_cleanup_owner(ownership_id)?;
        Ok(capability)
    }

    fn row_for(&self, ownership_id: &str) -> Result<WorkerRow> {
        anyhow::ensure!(
            ownership_id == self.ownership_id,
            "worker lifecycle capability for {:?} cannot authorize {ownership_id:?}",
            self.ownership_id
        );
        anyhow::ensure!(
            self.lock.authorizes(ownership_id),
            "worker lifecycle lock does not authorize {ownership_id:?}"
        );
        let row = self
            .registry
            .get(ownership_id)?
            .with_context(|| format!("worker registry has no row for {ownership_id:?}"))?;
        let (raw_scale_set_id, _) = ownership_id
            .split_once('/')
            .context("worker ownership id has no scale-set prefix")?;
        let scale_set_id = raw_scale_set_id
            .parse::<i32>()
            .context("worker ownership id has an invalid scale-set prefix")?;
        anyhow::ensure!(
            OwnershipId::bind(scale_set_id, &row.runner_name).as_str() == ownership_id,
            "worker registry row does not match ownership id {ownership_id:?}"
        );
        Ok(row)
    }

    pub(crate) fn validate_provision_owner(&self, ownership_id: &str) -> Result<()> {
        let row = self.row_for(ownership_id)?;
        anyhow::ensure!(
            row.worker_state == ScaleSetWorkerState::ProvisionIntent,
            "worker {ownership_id:?} is not in ProvisionIntent for provisioning"
        );
        Ok(())
    }

    pub(crate) fn validate_tick_owner(&self, ownership_id: &str) -> Result<()> {
        let row = self.row_for(ownership_id)?;
        anyhow::ensure!(
            !terminal_side(row.worker_state)
                && row.worker_state != ScaleSetWorkerState::PermitReleased,
            "worker {ownership_id:?} is terminal and cannot be supervised"
        );
        anyhow::ensure!(
            row.pending_docker_create.is_none(),
            "worker {ownership_id:?} has an unresolved Docker create and cannot be supervised"
        );
        Ok(())
    }

    pub(crate) fn validate_cleanup_owner(&self, ownership_id: &str) -> Result<()> {
        let row = self.row_for(ownership_id)?;
        anyhow::ensure!(
            matches!(
                row.worker_state,
                ScaleSetWorkerState::ProvisionIntent
                    | ScaleSetWorkerState::Terminal
                    | ScaleSetWorkerState::DiagnosticExport
                    | ScaleSetWorkerState::OwnedCleanup
            ),
            "worker {ownership_id:?} is outside terminal cleanup"
        );
        anyhow::ensure!(
            row.pending_docker_create.is_none(),
            "worker {ownership_id:?} has an unresolved Docker create during cleanup"
        );
        Ok(())
    }

    pub(crate) fn validate_owned_cleanup(&self, ownership_id: &str) -> Result<()> {
        let row = self.row_for(ownership_id)?;
        anyhow::ensure!(
            row.worker_state == ScaleSetWorkerState::OwnedCleanup,
            "worker {ownership_id:?} is not in OwnedCleanup"
        );
        anyhow::ensure!(
            row.pending_docker_create.is_none(),
            "worker {ownership_id:?} has an unresolved Docker create during teardown"
        );
        Ok(())
    }

    pub(crate) fn record_docker_lifecycle_event(
        &mut self,
        ownership_id: &str,
        event: crate::scaleset::worker::runner::DockerLifecycleEvent,
    ) -> Result<()> {
        self.validate_provision_owner(ownership_id)?;
        use crate::scaleset::worker::runner::DockerLifecycleEvent as Event;
        match event {
            Event::BeforeCreateRequest(target) => {
                let row = self.row_for(ownership_id)?;
                anyhow::ensure!(
                    row.pending_docker_create.is_none(),
                    "worker {ownership_id:?} already has an unresolved Docker create"
                );
                let deadline = crate::scaleset::worker::supervise::epoch_seconds().saturating_add(
                    crate::scaleset::worker::supervise::RUNNER_START_TIMEOUT.as_secs(),
                );
                let stored_deadline =
                    self.registry
                        .begin_docker_create(ownership_id, target, deadline)?;
                if let Some(stored_deadline) = stored_deadline {
                    let deadline_slot = self
                        .runner_start_deadline_epoch
                        .as_mut()
                        .context("provision capability has no runner deadline slot")?;
                    **deadline_slot = Some(stored_deadline);
                }
            }
            Event::CreateResolved(target) => {
                let row = self.row_for(ownership_id)?;
                anyhow::ensure!(
                    row.pending_docker_create.is_none_or(|pending| pending == target),
                    "worker {ownership_id:?} resolved {target:?} while a different create is pending"
                );
                self.registry.resolve_docker_create(ownership_id, target)?;
            }
            Event::BeforeStart => {
                let row = self.row_for(ownership_id)?;
                anyhow::ensure!(
                    row.pending_docker_create.is_none(),
                    "worker {ownership_id:?} cannot start with an unresolved Docker create"
                );
                let deadline = self.registry.set_runner_start_deadline_if_none(
                    ownership_id,
                    crate::scaleset::worker::supervise::epoch_seconds().saturating_add(
                        crate::scaleset::worker::supervise::RUNNER_START_TIMEOUT.as_secs(),
                    ),
                )?;
                let deadline_slot = self
                    .runner_start_deadline_epoch
                    .as_mut()
                    .context("provision capability has no runner deadline slot")?;
                **deadline_slot = Some(deadline);
            }
        }
        Ok(())
    }

    pub(crate) fn persist_dind_runtime(
        &mut self,
        ownership_id: &str,
        used: u32,
        ready_deadline_epoch: Option<u64>,
    ) -> Result<()> {
        self.validate_tick_owner(ownership_id)?;
        self.registry
            .set_dind_restart_runtime(ownership_id, used, ready_deadline_epoch)
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
    state_db: PathBuf,
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
        mut config: LaneConfig,
        state_db: &Path,
        ledger_path: &Path,
        runner: Box<dyn WorkerRunner + Send>,
        hook: Box<dyn ToolContentHook + Send>,
    ) -> Result<Self> {
        config.state_root = crate::scaleset::worker::prepare_state_root(&config.state_root)
            .with_context(|| {
                format!(
                    "prepare secure scale-set worker state root {}",
                    config.state_root.display()
                )
            })?;
        // Lane startup completes before native slot supervision. Restore
        // persisted eligible offers now so the shared queue can order them
        // ahead of later native acquisitions.
        let demand = DemandStore::open(state_db).context("open scale-set demand for backfill")?;
        let canonical_state_db = demand.path().to_path_buf();
        let mut ledger = SharedLedger::open(ledger_path).context("open shared permit ledger")?;
        crate::scaleset::reconcile::backfill_eligible_demands(&demand, &mut ledger)
            .context("backfill eligible scale-set demand")?;
        Ok(Self {
            client,
            config,
            state_db: canonical_state_db,
            runner,
            hook,
            intents: ProvisionIntentStore::open(state_db)?,
            registry: WorkerRegistry::open(state_db)?,
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

    fn note_canceled_request(&mut self, request_id: i64) -> Result<(), LaneError> {
        self.refresh_generation()?;
        let key = self.terminal_key(request_id)?;
        self.drive_canceled(&key)
            .map_err(|error| LaneError::new("drive canceled worker cleanup", error))
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

    /// Resolve the immutable lease for one durable Scale Set request. Old
    /// rows are backfilled only from a currently held ledger lease; a saved
    /// identity remains available for exact idempotent release after the
    /// ledger row disappears.
    fn exact_permit_lease_generation(&mut self, request_id: i64) -> Result<u64> {
        let mut demand = DemandStore::open(&self.state_db)?;
        let row = demand
            .get(request_id)?
            .with_context(|| format!("missing Scale Set demand {request_id}"))?;
        let holder = permit_holder(row.scale_set_id, request_id);
        let current = self.ledger.permit_lease_generation(&holder)?;
        match (row.permit_lease_generation, current) {
            (Some(saved), Some(current)) if saved == current => Ok(saved),
            (Some(saved), Some(current)) => anyhow::bail!(
                "Scale Set request {request_id} records lease {saved}, ledger holds replacement lease {current}"
            ),
            (Some(saved), None) => Ok(saved),
            (None, Some(current)) => {
                demand.record_permit_lease_generation(request_id, current)?;
                Ok(current)
            }
            (None, None) => anyhow::bail!(
                "Scale Set request {request_id} has no recorded or held permit lease"
            ),
        }
    }

    /// Require that a live provision/tick still owns the exact held lease
    /// recorded on its durable demand row. The generic terminal helpers
    /// allow an absent lease as an idempotent release receipt; live work
    /// must fail closed when that lease is gone or replaced.
    fn require_held_permit_lease_generation(&mut self, request_id: i64) -> Result<u64> {
        let lease_generation = self.exact_permit_lease_generation(request_id)?;
        let demand = DemandStore::open(&self.state_db)?
            .get(request_id)?
            .with_context(|| format!("missing Scale Set demand {request_id}"))?;
        let holder = permit_holder(demand.scale_set_id, request_id);
        let current = self.ledger.permit_lease_generation(&holder)?;
        if current != Some(lease_generation) {
            anyhow::bail!(
                "live Scale Set request {request_id} does not hold exact permit lease {lease_generation}"
            );
        }
        Ok(lease_generation)
    }

    /// Move one held permit to `state` using its immutable acquisition
    /// identity. Mutable daemon epochs never authorize retagging a lease.
    fn fenced_transition(
        &mut self,
        request_id: i64,
        holder: &str,
        state: LedgerPermitState,
    ) -> Result<()> {
        let lease_generation = self.exact_permit_lease_generation(request_id)?;
        if self
            .ledger
            .transition_if_lease_generation(holder, state, lease_generation)?
        {
            Ok(())
        } else {
            anyhow::bail!(
                "Scale Set request {request_id} no longer owns exact permit lease {lease_generation}"
            )
        }
    }

    /// Require an exact durable completion/cancellation oracle before any
    /// terminal cleanup edge can delete resources or finalize occupancy.
    fn require_terminal_demand_proof(
        &self,
        key: &str,
        row: Option<&WorkerRow>,
        allow_canceled_demand: bool,
    ) -> Result<i64> {
        let request_id = request_id_for_key(self.config.scale_set_id, key).with_context(|| {
            format!("worker key {key:?} does not encode an exact Scale Set request")
        })?;
        if let Some(row) = row {
            if row.ownership_id != key
                || row.request_id != Some(request_id)
                || row.runner_name
                    != crate::scaleset::runner_name(self.config.scale_set_id, request_id)
            {
                anyhow::bail!("worker {key:?} does not match exact Scale Set request {request_id}");
            }
        }
        let demand = DemandStore::open(&self.state_db)?
            .get(request_id)?
            .with_context(|| format!("missing Scale Set demand proof for request {request_id}"))?;
        if demand.request_id != request_id || demand.scale_set_id != self.config.scale_set_id {
            anyhow::bail!(
                "demand row does not match Scale Set {} request {request_id}",
                self.config.scale_set_id
            );
        }
        let terminal = demand.state == LocalDemandState::Terminal;
        let canceled = allow_canceled_demand && demand.state == LocalDemandState::CanceledAcquired;
        let finalized_cancellation = allow_canceled_demand
            && demand.state == LocalDemandState::CanceledDone
            && row.is_none_or(|row| {
                row.worker_state == ScaleSetWorkerState::PermitReleased
                    && !row.state_dir_cleanup_pending
                    && row.pending_docker_create.is_none()
            });
        if !terminal && !canceled && !finalized_cancellation {
            anyhow::bail!(
                "terminal cleanup for {key:?} requires exact terminal demand proof; found {}",
                demand.state.as_str()
            );
        }
        Ok(request_id)
    }

    /// Release terminal occupancy only after cleanup. If the lease is
    /// already absent, close demand without touching any same-holder
    /// replacement.
    fn release_terminal_permit(&mut self, request_id: i64) -> Result<()> {
        let mut demand = DemandStore::open(&self.state_db)?;
        let row = demand
            .get(request_id)?
            .with_context(|| format!("missing Scale Set demand {request_id}"))?;
        if row.request_id != request_id
            || row.scale_set_id != self.config.scale_set_id
            || row.state != LocalDemandState::Terminal
        {
            anyhow::bail!(
                "cannot release Scale Set request {request_id} without its exact terminal demand proof"
            );
        }
        let holder = permit_holder(self.config.scale_set_id, request_id);
        let current = self.ledger.permit_lease_generation(&holder)?;
        if let Some(current) = current {
            let lease_generation = match row.permit_lease_generation {
                Some(saved) if saved == current => saved,
                Some(saved) => anyhow::bail!(
                    "Scale Set request {request_id} records lease {saved}, ledger holds replacement lease {current}"
                ),
                None => {
                    demand.record_permit_lease_generation(request_id, current)?;
                    current
                }
            };
            if !self
                .ledger
                .release_if_generation(&holder, lease_generation)?
            {
                anyhow::bail!(
                    "could not release exact Scale Set lease {lease_generation} for request {request_id}"
                );
            }
        } else if !self.ledger.close_demand_if_unheld(
            &holder,
            crate::scaleset::capacity::LedgerDemandTerminalState::Terminal,
        )? {
            anyhow::bail!("unheld Scale Set demand {request_id} was not durably closed");
        }
        Ok(())
    }

    /// Retain worker occupancy after cleanup failure, fenced by the exact
    /// lease that provisioned this request.
    fn retain_uncertain(&mut self, request_id: i64, holder: &str) -> Result<()> {
        let lease_generation = self.exact_permit_lease_generation(request_id)?;
        if self
            .ledger
            .retain_uncertain_if_generation(holder, lease_generation)?
        {
            Ok(())
        } else {
            anyhow::bail!(
                "could not retain exact Scale Set lease {lease_generation} for request {request_id}"
            )
        }
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

    fn lock_worker_lifecycle(
        &self,
        key: &str,
    ) -> Result<crate::scaleset::reconcile::WorkerDockerLifecycleLock> {
        crate::scaleset::reconcile::lock_worker_docker_lifecycle(self.ledger.path(), key)
    }

    /// Ensure a live entry for `key`, rebuilding it from the registry row
    /// (restart adoption) or failing when nothing durable names it. Refresh
    /// lifecycle and runtime state because another lane process may commit
    /// either while this process is idle.
    fn ensure_live(&mut self, key: &str) -> Result<()> {
        let row = self
            .registry
            .get(key)?
            .with_context(|| format!("no worker recorded for {key:?}"))?;
        let ownership = OwnershipId::bind(self.config.scale_set_id, &row.runner_name);
        let identity = WorkerIdentity::new(ownership);
        let state_dir = self.recorded_state_dir(&row)?;
        let deadline = match row.runner_start_deadline_epoch {
            Some(deadline) => Some(deadline),
            None if row.worker_state == ScaleSetWorkerState::DindReady
                && !row.awaiting_upstream_completion =>
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
        let supervision = Supervision::from_runtime(
            identity.clone(),
            &state_dir,
            self.config.profile.clone(),
            row.dind_restarts_used,
            deadline,
            row.dind_restart_ready_deadline_epoch,
        );
        let mut worker = ScaleSetWorker::new(identity.clone(), &row.operation_id);
        if let Some(request_id) = row.request_id {
            worker.bind_request(request_id);
            worker.bind_permit(&permit_holder(self.config.scale_set_id, request_id));
        }
        // Replay the recorded state through the transition table so the
        // in-memory record cannot disagree with the row (illegal recorded
        // states fail the adoption, surfacing corruption loudly).
        replay_recorded_state(&mut worker, row.worker_state)?;
        if let Some(live) = self.workers.get_mut(key) {
            live.worker = worker;
            live.supervision = supervision;
            return Ok(());
        }
        self.workers.insert(
            key.to_owned(),
            LiveWorker {
                worker,
                supervision,
            },
        );
        Ok(())
    }

    /// Tick one worker's supervision. `WorkerFailed` is only a health
    /// observation; retain the permit and worker state until GitHub's
    /// completion message confirms the job outcome.
    fn tick_worker(&mut self, key: &str) -> Result<SupervisionOutcome, LaneError> {
        let _docker_lock = self
            .lock_worker_lifecycle(key)
            .map_err(|error| LaneError::new("lock worker lifecycle", error))?;
        self.tick_worker_locked(key, &_docker_lock)
    }

    /// Tick while the caller already holds the worker lifecycle lock.
    fn tick_worker_locked(
        &mut self,
        key: &str,
        docker_lock: &crate::scaleset::reconcile::WorkerDockerLifecycleLock,
    ) -> Result<SupervisionOutcome, LaneError> {
        if !self.workers.contains_key(key) {
            let row = self
                .registry
                .get(key)
                .map_err(|error| LaneError::new("read worker row", error))?;
            if row.is_some_and(|row| {
                provision_pending(row.worker_state)
                    && !row.runner_start_attempted
                    && !row.awaiting_upstream_completion
                    && row.dind_restart_ready_deadline_epoch.is_none()
            }) {
                return Ok(SupervisionOutcome::Healthy);
            }
        }
        self.ensure_live(key)
            .map_err(|error| LaneError::new("adopt worker", error))?;
        let durable_row = self
            .registry
            .get(key)
            .map_err(|error| LaneError::new("read worker runtime", error))?
            .with_context(|| format!("worker {key:?} disappeared before supervision"))
            .map_err(|error| LaneError::new("read worker runtime", error))?;
        if let Some(request_id) = durable_row.request_id {
            let demand = DemandStore::open(&self.state_db)
                .and_then(|demand| demand.get(request_id))
                .map_err(|error| LaneError::new("read demand before supervision", error))?
                .with_context(|| format!("demand {request_id} is missing before supervision"))
                .map_err(|error| LaneError::new("read demand before supervision", error))?;
            if demand_blocks_provision(demand.state) {
                // Completion and cancellation own the worker outcome. Do
                // not restart DinD while terminal cleanup is unresolved.
                return Ok(SupervisionOutcome::Healthy);
            }
            if demand.state.holds_permit() {
                self.require_held_permit_lease_generation(request_id)
                    .map_err(|error| {
                        LaneError::new("attest live permit before supervision", error)
                    })?;
            }
        }
        if durable_row.awaiting_upstream_completion {
            // A pre-session startup failure cannot have a job completion
            // from this runner. Keep the exact demand and lease until the
            // upstream cancellation/completion observation arrives.
            return Ok(SupervisionOutcome::Healthy);
        }
        let recorded = self
            .worker_state(key)
            .map_err(|error| LaneError::new("adopt worker", error))?;
        if terminal_side(recorded) || recorded == ScaleSetWorkerState::PermitReleased {
            return Ok(SupervisionOutcome::Healthy);
        }
        let outcome = {
            let runner = &mut self.runner;
            let registry = &mut self.registry;
            let mut lifecycle = WorkerLifecycleCapability::for_tick(docker_lock, registry, key)
                .map_err(|error| LaneError::new("authorize worker supervision", error))?;
            let live = self
                .workers
                .get_mut(key)
                .with_context(|| format!("live worker {key:?} vanished"))
                .map_err(|error| LaneError::new("adopt worker", error))?;
            live.supervision
                .tick_with_runtime(
                    &mut lifecycle,
                    &mut **runner,
                    recorded,
                    crate::scaleset::worker::supervise::epoch_seconds(),
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
            tracing::warn!(
                worker = key,
                "worker health failed; retaining permit until GitHub reports job completion"
            );
        }
        if let SupervisionOutcome::ProvisioningFailedBeforeSession { ref reason } = outcome {
            self.registry
                .set_awaiting_upstream_completion(key)
                .map_err(|error| LaneError::new("fence pre-session worker", error))?;
            tracing::warn!(
                worker = key,
                reason,
                "runner startup failed before session; retaining demand and permit for upstream completion"
            );
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
        let demand = match DemandStore::open(&self.state_db) {
            Ok(demand) => Some(demand),
            Err(error) => {
                tracing::warn!(
                    error = format!("{error:#}"),
                    "scale-set pre-runner terminal recovery could not open demand store"
                );
                None
            }
        };
        let mut pre_runner_terminal = HashSet::new();
        match self.registry.list_live() {
            Ok(rows) => {
                for row in rows {
                    let demand_state = match (demand.as_ref(), row.request_id) {
                        (Some(demand), Some(request_id)) => match demand.get(request_id) {
                            Ok(row) => row.map(|row| row.state),
                            Err(error) => {
                                tracing::warn!(
                                    worker = row.ownership_id.as_str(),
                                    error = format!("{error:#}"),
                                    "scale-set cleanup scan could not read demand"
                                );
                                continue;
                            }
                        },
                        (None, Some(_)) => continue,
                        _ => None,
                    };
                    let canceled_acquired =
                        demand_state == Some(LocalDemandState::CanceledAcquired);
                    if canceled_acquired {
                        pre_runner_terminal.insert(row.ownership_id.clone());
                        if let Err(error) = self.drive_canceled(&row.ownership_id) {
                            tracing::warn!(
                                worker = row.ownership_id.as_str(),
                                error = format!("{error:#}"),
                                "scale-set canceled worker cleanup retry failed"
                            );
                        }
                        continue;
                    }
                    if demand_state == Some(LocalDemandState::Terminal) {
                        pre_runner_terminal.insert(row.ownership_id.clone());
                        if let Err(error) = self.drive_terminal(&row.ownership_id) {
                            tracing::warn!(
                                worker = row.ownership_id.as_str(),
                                error = format!("{error:#}"),
                                "scale-set terminal worker cleanup retry failed"
                            );
                        }
                        continue;
                    }
                    if terminal_side(row.worker_state) {
                        tracing::warn!(
                            worker = row.ownership_id.as_str(),
                            "terminal registry row lacks durable terminal demand; retaining permit and state"
                        );
                        continue;
                    }
                    if row.worker_state != ScaleSetWorkerState::ProvisionIntent {
                        continue;
                    }
                    if demand_state != Some(LocalDemandState::Terminal) {
                        continue;
                    }
                    pre_runner_terminal.insert(row.ownership_id.clone());
                    if let Err(error) = self.drive_terminal(&row.ownership_id) {
                        tracing::warn!(
                            worker = row.ownership_id.as_str(),
                            error = format!("{error:#}"),
                            "pre-runner terminal cleanup retry failed"
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
                    if row.worker_state == ScaleSetWorkerState::PermitReleased {
                        let demand_state = match (demand.as_ref(), row.request_id) {
                            (Some(demand), Some(request_id)) => match demand.get(request_id) {
                                Ok(row) => row.map(|row| row.state),
                                Err(error) => {
                                    tracing::warn!(
                                        worker = row.ownership_id.as_str(),
                                        error = format!("{error:#}"),
                                        "released scale-set cleanup could not read demand"
                                    );
                                    continue;
                                }
                            },
                            (None, Some(_)) => continue,
                            _ => None,
                        };
                        let cleanup = if demand_state == Some(LocalDemandState::CanceledAcquired) {
                            self.drive_canceled(&row.ownership_id)
                        } else if demand_state == Some(LocalDemandState::Terminal)
                            || row.request_id.is_none()
                        {
                            self.drive_terminal(&row.ownership_id)
                        } else {
                            // Without terminal or cancellation proof, leave the
                            // permit and state fenced for a later retry.
                            continue;
                        };
                        if let Err(error) = cleanup {
                            tracing::warn!(
                                worker = row.ownership_id.as_str(),
                                error = format!("{error:#}"),
                                "released scale-set permit/state cleanup retry failed"
                            );
                        }
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
            if pre_runner_terminal.contains(&key) {
                continue;
            }
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
        let supervision = Supervision::from_runtime(
            identity,
            &state_dir,
            self.config.profile.clone(),
            row.dind_restarts_used,
            row.runner_start_deadline_epoch,
            row.dind_restart_ready_deadline_epoch,
        );
        let authorization = self.registry.authorize_state_cleanup(&row.ownership_id)?;
        let marker_complete = if row.diagnostics_complete {
            match supervision.diagnostics_complete() {
                Ok(complete) => complete,
                Err(error) => {
                    tracing::warn!(
                        worker = row.ownership_id.as_str(),
                        error = format!("{error:#}"),
                        "diagnostic marker is unreadable; using registry-authorized state cleanup"
                    );
                    false
                }
            }
        } else {
            false
        };
        if marker_complete {
            supervision.release_state(&authorization)?;
        } else {
            // Legacy rows may carry the old in-state capture bit without the
            // new host-only marker, or a prior partial delete may have
            // removed or corrupted that marker. The durable
            // PermitReleased+pending row authorizes idempotent cleanup in
            // either case.
            supervision.release_owned_state(&authorization)?;
        }
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
        self.drive_terminal_with_policy(key, true, false)
    }

    /// Complete canceled worker cleanup while leaving ledger and demand
    /// finalization to the processor. The durable CanceledAcquired demand
    /// row keeps this path replayable until cancellation is committed.
    fn drive_canceled(&mut self, key: &str) -> Result<()> {
        self.drive_terminal_with_policy(key, false, true)
    }

    fn drive_terminal_with_policy(
        &mut self,
        key: &str,
        release_permit: bool,
        allow_canceled_demand: bool,
    ) -> Result<()> {
        let outcome = self.drive_terminal_inner(key, release_permit, allow_canceled_demand)?;
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

    fn drive_terminal_inner(
        &mut self,
        key: &str,
        release_permit: bool,
        allow_canceled_demand: bool,
    ) -> Result<TerminalOutcome> {
        // Serialize terminal cleanup against every provision/start/tick for
        // this identity. Docker children inherit the lock, and a durable
        // create-pending bit covers the case where the CLI itself dies while
        // Engine settlement is still ambiguous.
        let _docker_lock = self.lock_worker_lifecycle(key)?;
        // A registry lifecycle phase never substitutes for the durable
        // request oracle. Recheck it on every replay, including rows already
        // at PermitReleased and requests whose worker row never existed.
        let row = self.registry.get(key)?;
        let request_id =
            self.require_terminal_demand_proof(key, row.as_ref(), allow_canceled_demand)?;
        let Some(mut row) = row else {
            if release_permit {
                let _source_lock =
                    crate::scaleset::reconcile::lock_demand_source_lifecycle(&self.state_db)?;
                self.require_terminal_demand_proof(key, None, allow_canceled_demand)?;
                self.release_terminal_permit(request_id)?;
            }
            return Ok(TerminalOutcome::AlreadyReleased);
        };
        if row.worker_state != ScaleSetWorkerState::ProvisionIntent
            && let Some(target) = row.pending_docker_create
        {
            anyhow::bail!(
                "worker {key:?} entered {} with unresolved {target:?} Docker create; refusing terminal release",
                row.worker_state.as_str()
            );
        }
        if row.worker_state == ScaleSetWorkerState::PermitReleased {
            // Keep capacity held until state deletion succeeds. The pending
            // bit also closes shared admission if a process dies between the
            // filesystem and ledger commits.
            let _source_lock =
                crate::scaleset::reconcile::lock_demand_source_lifecycle(&self.state_db)?;
            row = self
                .registry
                .get(key)?
                .with_context(|| format!("released worker row {key:?} vanished during cleanup"))?;
            self.require_terminal_demand_proof(key, Some(&row), allow_canceled_demand)?;
            if row.state_dir_cleanup_pending {
                self.cleanup_released_state_dir(&row)?;
            }
            if release_permit && let Some(request_id) = row.request_id {
                self.release_terminal_permit(request_id)?;
            }
            if row.state_dir_cleanup_pending {
                self.registry.clear_state_dir_cleanup_pending(key)?;
            }
            return Ok(TerminalOutcome::AlreadyReleased);
        }
        self.ensure_live(key)?;
        let marker_read = self
            .workers
            .get(key)
            .with_context(|| format!("live worker {key:?} vanished"))?
            .supervision
            .diagnostic_completion_kind();
        let diagnostics_completion_kind = match marker_read {
            Ok(kind) => kind,
            Err(error) => {
                return self.record_cleanup_failure(
                    &row,
                    key,
                    vec![format!("read diagnostic completion marker: {error:#}")],
                );
            }
        };
        let is_provision_intent = row.worker_state == ScaleSetWorkerState::ProvisionIntent;
        if is_provision_intent {
            // A Docker create may have completed after a prior no-runner
            // observation. Every replay revalidates terminal demand and the
            // exact pending resource before promoting diagnostic evidence.
            if row.diagnostics_complete {
                self.registry.clear_diagnostics_complete(key)?;
                row.diagnostics_complete = false;
            }
            // A no-runner receipt is only a historical observation. A
            // create that was still in flight may have completed after that
            // receipt, so every ProvisionIntent replay re-inspects the exact
            // runner before choosing the no-runner or normal capture path.
            let identity = WorkerIdentity::new(OwnershipId::bind(
                self.config.scale_set_id,
                &row.runner_name,
            ));
            let runner_probe = crate::scaleset::worker::dind::create_target_exists(
                &mut *self.runner,
                &identity,
                crate::scaleset::worker::runner::DockerCreateTarget::Runner,
            );
            let runner_exists = match runner_probe {
                Ok(exists) => exists,
                Err(error) => {
                    return self.record_cleanup_failure(
                        &row,
                        key,
                        vec![format!(
                            "inspect runner before no-runner cleanup: {error:#}"
                        )],
                    );
                }
            };
            if let Some(target) = row.pending_docker_create {
                let target_exists = match crate::scaleset::worker::dind::create_target_exists(
                    &mut *self.runner,
                    &identity,
                    target,
                ) {
                    Ok(exists) => exists,
                    Err(error) => {
                        return self.record_cleanup_failure(
                            &row,
                            key,
                            vec![format!("resolve pending {target:?} create: {error:#}")],
                        );
                    }
                };
                if !target_exists {
                    return self.record_cleanup_failure(
                        &row,
                        key,
                        vec![format!(
                            "pending {target:?} Docker create is still unresolved; retaining terminal fence"
                        )],
                    );
                }
                // Exact owned presence proves this request materialized.
                // Absence remains ambiguous because Docker may still be
                // completing an accepted create.
                self.registry.resolve_docker_create(key, target)?;
                row.pending_docker_create = None;
            }
            if runner_exists {
                // A previous RunnerAbsent marker may have been written while
                // Docker create was still in flight. Invalidate its durable
                // registry bit before normal stop/capture. The capture path
                // removes and fsyncs that marker before replacing artifacts;
                // a crash therefore cannot leave the old receipt authorized.
                self.registry.clear_diagnostics_complete(key)?;
                // Persist the terminal phase before any full-capture call.
                // A failed capture followed by runner disappearance must
                // remain on the full-capture path, never downgrade to the
                // no-runner waiver.
                self.transition_worker(key, ScaleSetWorkerState::Terminal)?;
                row.worker_state = ScaleSetWorkerState::Terminal;
                row.diagnostics_complete = false;
            } else if diagnostics_completion_kind
                == Some(crate::scaleset::worker::supervise::DiagnosticCompletionKind::FullCapture)
            {
                // A full capture is already valid evidence if the container
                // has since disappeared. Persist the checkpoint so terminal
                // replay need not attempt to recapture an absent container.
                self.registry.set_diagnostics_complete(key)?;
                row.diagnostics_complete = true;
            } else {
                // For no marker or a prior RunnerAbsent marker, this helper
                // rechecks exact absence and validates/replays DinD evidence.
                let export = (|| {
                    let lifecycle = WorkerLifecycleCapability::for_cleanup(
                        &_docker_lock,
                        &mut self.registry,
                        key,
                    )?;
                    self.workers
                        .get_mut(key)
                        .with_context(|| format!("live worker {key:?} vanished"))?
                        .supervision
                        .complete_diagnostics_without_runner(&lifecycle, &mut *self.runner)
                })();
                let export = match export {
                    Ok(export) => export,
                    Err(error) => {
                        return self.record_cleanup_failure(
                            &row,
                            key,
                            vec![format!("capture no-runner diagnostics: {error:#}")],
                        );
                    }
                };
                if !export.failures.is_empty() {
                    return self.record_cleanup_failure(&row, key, export.failures);
                }
                self.registry.set_diagnostics_complete(key)?;
                row.diagnostics_complete = true;
            }
        } else {
            // A RunnerAbsent receipt is valid only when the registry's
            // durable diagnostics bit was committed in ProvisionIntent.
            // Without that bit, keep the full-capture path active; its
            // prepare_cleanup call invalidates the wrong-kind marker before
            // replacing artifacts.
            if diagnostics_completion_kind
                == Some(crate::scaleset::worker::supervise::DiagnosticCompletionKind::FullCapture)
                && !row.diagnostics_complete
            {
                self.registry.set_diagnostics_complete(key)?;
                row.diagnostics_complete = true;
            }
        }
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
            if !row.diagnostics_complete {
                let export = (|| {
                    let lifecycle = WorkerLifecycleCapability::for_cleanup(
                        &_docker_lock,
                        &mut self.registry,
                        key,
                    )?;
                    let live = self
                        .workers
                        .get_mut(key)
                        .with_context(|| format!("live worker {key:?} vanished"))?;
                    live.supervision
                        .prepare_cleanup(&lifecycle, &mut *self.runner)
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
            }
            self.transition_worker(key, ScaleSetWorkerState::OwnedCleanup)?;
            recorded = ScaleSetWorkerState::OwnedCleanup;
        }
        if recorded == ScaleSetWorkerState::OwnedCleanup {
            let recovery_export = (|| {
                let lifecycle =
                    WorkerLifecycleCapability::for_cleanup(&_docker_lock, &mut self.registry, key)?;
                let live = self
                    .workers
                    .get_mut(key)
                    .with_context(|| format!("live worker {key:?} vanished"))?;
                if row.diagnostics_complete {
                    return Ok(None);
                }
                if !live.supervision.state_dir_exists()? {
                    anyhow::bail!(
                        "worker {key:?} lost its state directory before diagnostics were recorded"
                    );
                }
                live.supervision
                    .prepare_cleanup(&lifecycle, &mut *self.runner)
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
                let lifecycle =
                    WorkerLifecycleCapability::for_cleanup(&_docker_lock, &mut self.registry, key)?;
                let live = self
                    .workers
                    .get_mut(key)
                    .with_context(|| format!("live worker {key:?} vanished"))?;
                live.supervision
                    .teardown_owned_resources(&lifecycle, &mut *self.runner)
            };
            if !failures.is_empty() {
                return self.record_cleanup_failure(&row, key, failures);
            }
        }
        // Serialize the cross-database cleanup boundary with startup
        // attestation. Keep both the permit and cleanup-pending fence until
        // every state path is durably removed. If ledger release then fails,
        // replay sees PermitReleased+pending and retries without admitting a
        // new owner into the old worker's state directory.
        let _source_lock =
            crate::scaleset::reconcile::lock_demand_source_lifecycle(&self.state_db)?;
        self.require_terminal_demand_proof(key, Some(&row), allow_canceled_demand)?;
        self.transition_worker(key, ScaleSetWorkerState::PermitReleased)?;
        let released = self
            .registry
            .get(key)?
            .with_context(|| format!("released worker row {key:?} vanished"))?;
        if released.state_dir_cleanup_pending {
            self.cleanup_released_state_dir(&released)?;
        }
        if release_permit && let Some(request_id) = row.request_id {
            self.release_terminal_permit(request_id)?;
        }
        if released.state_dir_cleanup_pending {
            self.registry.clear_state_dir_cleanup_pending(key)?;
        }
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
            self.retain_uncertain(request_id, &holder)?;
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
        let demand = DemandStore::open(&self.state_db)?;
        for row in self.registry.list_state_cleanup_pending()? {
            if row.worker_state == ScaleSetWorkerState::PermitReleased {
                let demand_state = match row.request_id {
                    Some(request_id) => demand.get(request_id)?.map(|demand| demand.state),
                    None => None,
                };
                let canceled_acquired = demand_state == Some(LocalDemandState::CanceledAcquired);
                if let Err(error) = self.require_terminal_demand_proof(
                    &row.ownership_id,
                    Some(&row),
                    canceled_acquired,
                ) {
                    tracing::warn!(
                        worker = row.ownership_id.as_str(),
                        error = format!("{error:#}"),
                        "released scale-set state lacks fresh terminal or cancellation proof; retaining cleanup fence"
                    );
                    continue;
                }
                let cleanup = if canceled_acquired {
                    self.drive_canceled(&row.ownership_id)
                } else {
                    self.drive_terminal(&row.ownership_id)
                };
                cleanup.with_context(|| {
                    format!(
                        "finish released scale-set state cleanup for {} before session admission",
                        row.ownership_id
                    )
                })?;
                report.resumed_cleanup += 1;
            }
        }
        // A completion or cancellation can be durable before terminal
        // cleanup reaches its first lifecycle edge. Reconcile that oracle
        // before any active row can be adopted or supervised.
        let mut resolved_demand_workers = HashSet::new();
        for row in self.registry.list_live()? {
            if terminal_side(row.worker_state) {
                continue;
            }
            let Some(request_id) = row.request_id else {
                continue;
            };
            let demand_state = demand.get(request_id)?.map(|demand| demand.state);
            let canceled_acquired = demand_state == Some(LocalDemandState::CanceledAcquired);
            if !canceled_acquired && demand_state != Some(LocalDemandState::Terminal) {
                continue;
            }
            resolved_demand_workers.insert(row.ownership_id.clone());
            let cleanup = if canceled_acquired {
                self.drive_canceled(&row.ownership_id)
            } else {
                self.drive_terminal(&row.ownership_id)
            };
            if let Err(error) = cleanup {
                tracing::warn!(
                    worker = row.ownership_id.as_str(),
                    error = format!("{error:#}"),
                    "pre-runner cleanup will retry after startup"
                );
            }
            report.resumed_cleanup += 1;
        }
        // Completion may have removed the provision intent after the worker
        // entered terminal cleanup. Recover every recorded terminal row
        // directly from the registry; its durable local Terminal demand is
        // the completion proof, and no missing intent may hide its state dir.
        for row in self.registry.list_live()? {
            if !terminal_side(row.worker_state)
                || resolved_demand_workers.contains(&row.ownership_id)
            {
                continue;
            }
            let demand_state = match row.request_id {
                Some(request_id) => demand.get(request_id)?.map(|demand| demand.state),
                None => None,
            };
            let canceled_acquired = demand_state == Some(LocalDemandState::CanceledAcquired);
            if !canceled_acquired && demand_state != Some(LocalDemandState::Terminal) {
                tracing::warn!(
                    worker = row.ownership_id.as_str(),
                    "terminal registry row lacks durable terminal or cancellation demand; retaining permit and state"
                );
                continue;
            }
            if let Err(error) =
                self.require_terminal_demand_proof(&row.ownership_id, Some(&row), canceled_acquired)
            {
                tracing::warn!(
                    worker = row.ownership_id.as_str(),
                    error = format!("{error:#}"),
                    "terminal registry row lacks fresh exact demand proof; retaining permit and state"
                );
                continue;
            }
            resolved_demand_workers.insert(row.ownership_id.clone());
            let cleanup = if canceled_acquired {
                self.drive_canceled(&row.ownership_id)
            } else {
                self.drive_terminal(&row.ownership_id)
            };
            if let Err(error) = cleanup {
                tracing::warn!(
                    worker = row.ownership_id.as_str(),
                    error = format!("{error:#}"),
                    "terminal registry cleanup will retry after startup"
                );
            }
            report.resumed_cleanup += 1;
        }
        let intents = self.intents.list_for_set(self.config.scale_set_id)?;
        for intent in &intents {
            let key = Self::ownership_key(intent);
            if resolved_demand_workers.contains(&key) {
                continue;
            }
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
            if row.awaiting_upstream_completion {
                // The worker cannot create its own completion after a
                // pre-session startup failure. Keep its stable request row
                // and lease tracked until GitHub reports completion.
                self.ensure_live(&key)?;
                report.adopted += 1;
                continue;
            }
            if provision_pending(row.worker_state)
                && (row.runner_start_attempted || row.dind_restart_ready_deadline_epoch.is_some())
            {
                // Docker may have accepted runner start before the process
                // persisted DindReady. Reconstruct and supervise this row;
                // never leave it invisible to both adoption and Acquired
                // replay.
                self.ensure_live(&key)?;
                match self.tick_worker(&key) {
                    Ok(
                        SupervisionOutcome::Healthy
                        | SupervisionOutcome::DindRestarted { .. }
                        | SupervisionOutcome::DindRestartPending { .. }
                        | SupervisionOutcome::RunnerConnected
                        | SupervisionOutcome::ProvisioningFailedBeforeSession { .. }
                        | SupervisionOutcome::WorkerFailed { .. },
                    ) => report.adopted += 1,
                    Err(error) => {
                        tracing::warn!(
                            worker = key.as_str(),
                            error = error.to_string(),
                            "ambiguous runner start adoption tick failed; worker stays tracked"
                        );
                        report.adopted += 1;
                    }
                }
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
                Ok(
                    SupervisionOutcome::Healthy
                    | SupervisionOutcome::DindRestarted { .. }
                    | SupervisionOutcome::DindRestartPending { .. }
                    | SupervisionOutcome::ProvisioningFailedBeforeSession { .. },
                ) => {
                    report.adopted += 1;
                }
                Ok(SupervisionOutcome::RunnerConnected) => {
                    report.adopted += 1;
                }
                Ok(SupervisionOutcome::WorkerFailed { .. }) => {
                    // A dead runner is not a completion oracle. Keep the
                    // durable worker row and permit for JobCompleted replay.
                    report.adopted += 1;
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

/// Active provisioned states: replays only need a health tick, not a fresh
/// provision. Terminal states must reach the stale-provision rejection below.
fn post_provision(state: ScaleSetWorkerState) -> bool {
    matches!(
        state,
        ScaleSetWorkerState::DindReady
            | ScaleSetWorkerState::RunnerConnected
            | ScaleSetWorkerState::Running
    )
}

fn is_health_only_provision_retry(row: &WorkerRow, intent: &ProvisionIntent) -> bool {
    row.request_id == Some(intent.request_id)
        && (row.awaiting_upstream_completion
            || post_provision(row.worker_state)
            || (row.worker_state == ScaleSetWorkerState::ProvisionIntent
                && row.runner_start_attempted))
}

fn demand_blocks_provision(state: LocalDemandState) -> bool {
    matches!(
        state,
        LocalDemandState::Terminal
            | LocalDemandState::Declined
            | LocalDemandState::CanceledPending
            | LocalDemandState::CanceledAcquired
            | LocalDemandState::CanceledDone
    )
}

/// Recover the ledger holder for an ownership key of the canonical form
/// `<set>/<runner-name>` where the runner name embeds the request id
/// (`velnor-<set>-<request>`). Returns `None` for foreign shapes — the
/// caller then releases nothing instead of guessing.
fn holder_for_key(scale_set_id: i32, key: &str) -> Option<String> {
    request_id_for_key(scale_set_id, key).map(|request_id| permit_holder(scale_set_id, request_id))
}

fn request_id_for_key(scale_set_id: i32, key: &str) -> Option<i64> {
    let (prefix, name) = key.split_once('/')?;
    if prefix.parse::<i32>().ok()? != scale_set_id {
        return None;
    }
    let request = name.split('-').next_back()?.parse::<i64>().ok()?;
    if name != crate::scaleset::runner_name(scale_set_id, request) {
        return None;
    }
    Some(request)
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

        // Fence terminal/canceled demand before the health-only retry fast
        // path. The same lifecycle lock serializes this read with cleanup,
        // and the durable worker row—not a transient operation ID—proves
        // same-request ownership across retries.
        {
            let _docker_lock = self
                .lock_worker_lifecycle(&key)
                .map_err(|error| LaneError::new("lock worker lifecycle", error))?;
            let current_demand = DemandStore::open(&self.state_db)
                .and_then(|demand| demand.get(intent.request_id))
                .map_err(|error| LaneError::new("read demand before retry", error))?
                .with_context(|| {
                    format!("demand {} vanished before provisioning", intent.request_id)
                })
                .map_err(|error| LaneError::new("read demand before retry", error))?;
            if demand_blocks_provision(current_demand.state) {
                return Err(LaneError::new(
                    "stale provision",
                    anyhow::anyhow!(
                        "demand {} is {} before provision retry",
                        intent.request_id,
                        current_demand.state.as_str()
                    ),
                ));
            }
            if !matches!(
                current_demand.state,
                LocalDemandState::Acquired | LocalDemandState::ProvisionIntent
            ) {
                return Err(LaneError::new(
                    "stale provision",
                    anyhow::anyhow!(
                        "demand {} is {} before provision retry",
                        intent.request_id,
                        current_demand.state.as_str()
                    ),
                ));
            }
            self.require_held_permit_lease_generation(intent.request_id)
                .map_err(|error| LaneError::new("attest permit before provision retry", error))?;
            let recorded = self
                .registry
                .get(&key)
                .map_err(|error| LaneError::new("read worker row", error))?;
            if let Some(row) = recorded.as_ref() {
                if terminal_side(row.worker_state)
                    || row.worker_state == ScaleSetWorkerState::PermitReleased
                {
                    return Err(LaneError::new(
                        "stale provision",
                        anyhow::anyhow!(
                            "worker {key} is already terminal; refusing to re-provision"
                        ),
                    ));
                }
                if is_health_only_provision_retry(row, intent) {
                    // Health failure is not completion evidence. Keep this
                    // worker and lease tracked; full provisioning could
                    // restart an owned runner and re-execute its job.
                    self.tick_worker_locked(&key, &_docker_lock)?;
                    return Ok(());
                }
                if row
                    .request_id
                    .is_some_and(|request_id| request_id != intent.request_id)
                {
                    return Err(LaneError::new(
                        "stale provision",
                        anyhow::anyhow!(
                            "worker {key} belongs to request {:?}, refusing request {}",
                            row.request_id,
                            intent.request_id
                        ),
                    ));
                }
            }
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
        // The lifecycle lock spans registry intent, Docker commands, and
        // worker-state commits. Terminal cleanup takes the same lock, so it
        // cannot publish no-runner completion while this process is between
        // its final inspect and runner create.
        let docker_lock = self
            .lock_worker_lifecycle(&key)
            .map_err(|error| LaneError::new("lock worker lifecycle", error))?;
        let current_demand = DemandStore::open(&self.state_db)
            .and_then(|demand| demand.get(intent.request_id))
            .map_err(|error| LaneError::new("re-read demand under lifecycle lock", error))?
            .with_context(|| format!("demand {} vanished before provisioning", intent.request_id))
            .map_err(|error| LaneError::new("re-read demand under lifecycle lock", error))?;
        if demand_blocks_provision(current_demand.state) {
            return Err(LaneError::new(
                "stale provision",
                anyhow::anyhow!(
                    "demand {} became {} before Docker provisioning",
                    intent.request_id,
                    current_demand.state.as_str()
                ),
            ));
        }
        if !matches!(
            current_demand.state,
            LocalDemandState::Acquired | LocalDemandState::ProvisionIntent
        ) {
            return Err(LaneError::new(
                "stale provision",
                anyhow::anyhow!(
                    "demand {} is {} before Docker provisioning",
                    intent.request_id,
                    current_demand.state.as_str()
                ),
            ));
        }
        self.require_held_permit_lease_generation(intent.request_id)
            .map_err(|error| LaneError::new("attest permit before Docker provisioning", error))?;
        let existing_row = self
            .registry
            .get(&key)
            .map_err(|error| LaneError::new("re-read worker row under lifecycle lock", error))?;
        if existing_row.as_ref().is_some_and(|row| {
            terminal_side(row.worker_state)
                || row.worker_state == ScaleSetWorkerState::PermitReleased
        }) {
            return Err(LaneError::new(
                "stale provision",
                anyhow::anyhow!("worker {key} became terminal before provisioning"),
            ));
        }
        if let Some(row) = existing_row.as_ref() {
            if is_health_only_provision_retry(row, intent) {
                self.tick_worker_locked(&key, &docker_lock)?;
                return Ok(());
            }
            if row
                .request_id
                .is_some_and(|request_id| request_id != intent.request_id)
            {
                return Err(LaneError::new(
                    "stale provision",
                    anyhow::anyhow!(
                        "worker {key} belongs to request {:?}, refusing request {}",
                        row.request_id,
                        intent.request_id
                    ),
                ));
            }
        }
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

        self.registry
            .resolve_pending_docker_create_after_inspect(&key, &identity, &mut *self.runner)
            .map_err(|error| LaneError::new("recover pending Docker create", error))?;

        let plan = ProvisionPlan {
            identity: identity.clone(),
            profile: self.config.profile.clone(),
            state_dir: state_dir.clone(),
            jit_config,
            ready_attempts: self.config.ready_attempts,
        };
        let mut runner_start_deadline_epoch = row.runner_start_deadline_epoch;
        let outcome = {
            let lifecycle = WorkerLifecycleCapability::for_provision(
                &docker_lock,
                &mut self.registry,
                &key,
                &mut runner_start_deadline_epoch,
            )
            .map_err(|error| LaneError::new("authorize worker provisioning", error))?;
            let runner = &mut self.runner;
            let hook = &self.hook;
            let mut lifecycle = lifecycle;
            provision_worker(
                &mut lifecycle,
                &mut **runner,
                &**hook,
                &plan,
                &std::thread::sleep,
            )
        }
        .map_err(|error| LaneError::new("provision worker pair", error))?;
        if let Some(target) = self
            .registry
            .get(&key)
            .map_err(|error| LaneError::new("verify Docker create resolution", error))?
            .and_then(|row| row.pending_docker_create)
        {
            return Err(LaneError::new(
                "verify Docker create resolution",
                anyhow::anyhow!(
                    "worker {key:?} completed provisioning with unresolved {target:?} create"
                ),
            ));
        }
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
        self.fenced_transition(intent.request_id, &holder, LedgerPermitState::Provisioning)
            .map_err(|error| LaneError::new("mark permit provisioning", error))?;
        self.workers.insert(
            key,
            LiveWorker {
                worker,
                supervision: Supervision::from_runtime(
                    identity,
                    &state_dir,
                    self.config.profile.clone(),
                    row.dind_restarts_used,
                    runner_start_deadline_epoch,
                    row.dind_restart_ready_deadline_epoch,
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
        let _docker_lock = self
            .lock_worker_lifecycle(&key)
            .map_err(|error| LaneError::new("lock worker lifecycle", error))?;
        let Some(row) = self
            .registry
            .get(&key)
            .map_err(|error| LaneError::new("read worker row", error))?
        else {
            return Ok(());
        };
        // A previous provision call may have durably recorded its intent and
        // then failed before inserting the live worker. Do not acknowledge a
        // start by trying to advance that missing in-memory worker; the
        // processor's next provision pass replays the acquired demand and
        // resumes idempotent provisioning. Late starts after terminal cleanup
        // are also no-ops and cannot resurrect the worker or its permit.
        if terminal_side(row.worker_state)
            || row.worker_state == ScaleSetWorkerState::PermitReleased
            || row.awaiting_upstream_completion
            || provision_pending(row.worker_state)
        {
            return Ok(());
        }
        let outcome = self.tick_worker_locked(&key, &_docker_lock)?;
        if matches!(
            outcome,
            SupervisionOutcome::WorkerFailed { .. }
                | SupervisionOutcome::DindRestartPending { .. }
                | SupervisionOutcome::ProvisioningFailedBeforeSession { .. }
        ) {
            // A stale JobStarted observation cannot manufacture a connected
            // session or advance lifecycle while DinD is still unready.
            return Ok(());
        }
        // Advance the record toward `running` along the happy path only;
        // terminal-side and retry states are owned by their own paths.
        let Some(recorded_row) = self
            .registry
            .get(&key)
            .map_err(|error| LaneError::new("read worker row", error))?
        else {
            return Ok(());
        };
        let recorded = recorded_row.worker_state;
        if terminal_side(recorded)
            || recorded == ScaleSetWorkerState::PermitReleased
            || provision_pending(recorded)
            || !matches!(
                recorded,
                ScaleSetWorkerState::RunnerConnected | ScaleSetWorkerState::Running
            )
        {
            return Ok(());
        }
        let path: &[ScaleSetWorkerState] = match recorded {
            ScaleSetWorkerState::RunnerConnected => &[ScaleSetWorkerState::Running],
            ScaleSetWorkerState::Running => &[],
            _ => &[],
        };
        for edge in path {
            self.transition_worker(&key, *edge)
                .map_err(|error| LaneError::new("record job started", error))?;
        }
        let holder = permit_holder(self.config.scale_set_id, request_id);
        self.fenced_transition(request_id, &holder, LedgerPermitState::Running)
            .map_err(|error| LaneError::new("mark permit running", error))?;
        Ok(())
    }

    fn note_terminal(&mut self, completed: &ScaleSetJobCompleted) -> Result<(), Self::Error> {
        self.note_terminal_request(completed.base.runner_request_id)
    }

    fn note_canceled(&mut self, request_id: i64) -> Result<(), Self::Error> {
        self.note_canceled_request(request_id)
    }

    fn idle_tick(&mut self) -> Result<(), Self::Error> {
        self.refresh_generation()?;
        self.opportunistic_sweep();
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

    struct CleanupRunner {
        fail_runner_remove: bool,
        fail_runner_inspect: bool,
        fail_runner_logs: bool,
        fail_dind_logs: bool,
        runner_absent: bool,
        all_containers_absent: bool,
        runner_live: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        runner_name: String,
        ownership_label: Option<String>,
        identity: Option<WorkerIdentity>,
        seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        after_remove: Option<Box<dyn FnOnce() + Send>>,
    }

    impl CleanupRunner {
        fn missing(
            runner_name: String,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            Self {
                fail_runner_remove: false,
                fail_runner_inspect: false,
                fail_runner_logs: false,
                fail_dind_logs: false,
                runner_absent: false,
                all_containers_absent: false,
                runner_live: None,
                runner_name,
                ownership_label: None,
                identity: None,
                seen,
                after_remove: None,
            }
        }

        fn fail_runner_remove(
            runner_name: String,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            Self {
                fail_runner_remove: true,
                fail_runner_inspect: false,
                fail_runner_logs: false,
                fail_dind_logs: false,
                runner_absent: false,
                all_containers_absent: false,
                runner_live: None,
                runner_name,
                ownership_label: None,
                identity: None,
                seen,
                after_remove: None,
            }
        }

        fn runner_absent(
            runner_name: String,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            Self {
                fail_runner_remove: false,
                fail_runner_inspect: false,
                fail_runner_logs: false,
                fail_dind_logs: false,
                runner_absent: true,
                all_containers_absent: false,
                runner_live: None,
                runner_name,
                ownership_label: None,
                identity: None,
                seen,
                after_remove: None,
            }
        }

        fn fail_no_runner_cleanup(
            runner_name: String,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            Self {
                fail_runner_remove: false,
                fail_runner_inspect: true,
                fail_runner_logs: false,
                fail_dind_logs: true,
                runner_absent: true,
                all_containers_absent: false,
                runner_live: None,
                runner_name,
                ownership_label: None,
                identity: None,
                seen,
                after_remove: None,
            }
        }

        fn after_remove(
            runner_name: String,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
            action: impl FnOnce() + Send + 'static,
        ) -> Self {
            Self {
                fail_runner_remove: false,
                fail_runner_inspect: false,
                fail_runner_logs: false,
                fail_dind_logs: false,
                runner_absent: false,
                all_containers_absent: false,
                runner_live: None,
                runner_name,
                ownership_label: None,
                identity: None,
                seen,
                after_remove: Some(Box::new(action)),
            }
        }

        fn fail_runner_capture(
            runner_name: String,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            let mut runner = Self::missing(runner_name, seen);
            runner.fail_runner_logs = true;
            runner
        }

        fn dynamically_present(
            identity: &WorkerIdentity,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
            live: std::sync::Arc<std::sync::atomic::AtomicBool>,
        ) -> Self {
            let mut runner = Self::missing(identity.runner_container(), seen);
            runner.runner_live = Some(live);
            runner.ownership_label = Some(identity.ownership().as_str());
            runner.identity = Some(identity.clone());
            runner
        }

        fn for_identity(
            identity: &WorkerIdentity,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            let runner = Self::missing(identity.runner_container(), seen);
            runner.with_identity(identity)
        }

        fn with_identity(mut self, identity: &WorkerIdentity) -> Self {
            self.ownership_label = Some(identity.ownership().as_str());
            self.identity = Some(identity.clone());
            self
        }

        fn runner_absent_for_identity(
            identity: &WorkerIdentity,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            Self::runner_absent(identity.runner_container(), seen).with_identity(identity)
        }

        fn all_containers_absent_for_identity(
            identity: &WorkerIdentity,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            let mut runner =
                Self::runner_absent(identity.runner_container(), seen).with_identity(identity);
            runner.all_containers_absent = true;
            runner
        }

        fn fail_no_runner_cleanup_for_identity(
            identity: &WorkerIdentity,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            Self::fail_no_runner_cleanup(identity.runner_container(), seen).with_identity(identity)
        }

        fn fail_runner_remove_for_identity(
            identity: &WorkerIdentity,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) -> Self {
            Self::fail_runner_remove(identity.runner_container(), seen).with_identity(identity)
        }

        fn after_remove_for_identity(
            identity: &WorkerIdentity,
            seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
            action: impl FnOnce() + Send + 'static,
        ) -> Self {
            Self::after_remove(identity.runner_container(), seen, action).with_identity(identity)
        }

        fn refers_to_runner(&self, args: &[String]) -> bool {
            args.iter().any(|arg| arg == &self.runner_name)
                || (self.identity.is_some() && args.iter().any(|arg| arg == "runner-container-id"))
        }

        fn refers_to_dind(&self, args: &[String]) -> bool {
            if let Some(identity) = &self.identity {
                let dind_name = identity.dind_container();
                args.iter()
                    .any(|arg| arg == &dind_name || arg == "dind-container-id")
            } else {
                !self.refers_to_runner(args)
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
            if args.first().is_some_and(|arg| arg == "network")
                && args.get(1).is_some_and(|arg| arg == "inspect")
            {
                return Ok(crate::scaleset::worker::WorkerOutput {
                    code: 1,
                    stdout: String::new(),
                    stderr: "Error: No such network".to_owned(),
                });
            }
            if args.first().is_some_and(|arg| arg == "volume")
                && args.get(1).is_some_and(|arg| arg == "inspect")
            {
                return Ok(crate::scaleset::worker::WorkerOutput {
                    code: 1,
                    stdout: String::new(),
                    stderr: "Error: No such volume".to_owned(),
                });
            }
            if args.first().is_some_and(|arg| arg == "volume")
                && args.get(1).is_some_and(|arg| arg == "ls")
            {
                return Ok(crate::scaleset::worker::WorkerOutput {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
            if args.first().is_some_and(|arg| arg == "logs") {
                if self.fail_runner_logs && self.refers_to_runner(args) {
                    self.fail_runner_logs = false;
                    return Ok(crate::scaleset::worker::WorkerOutput {
                        code: 1,
                        stdout: String::new(),
                        stderr: "injected runner logs failure".to_owned(),
                    });
                }
                if self.fail_dind_logs && self.refers_to_dind(args) {
                    self.fail_dind_logs = false;
                    return Ok(crate::scaleset::worker::WorkerOutput {
                        code: 1,
                        stdout: String::new(),
                        stderr: "injected DinD logs failure".to_owned(),
                    });
                }
                return Ok(crate::scaleset::worker::WorkerOutput {
                    code: 0,
                    stdout: "diagnostic log\n".to_owned(),
                    stderr: String::new(),
                });
            }
            if args.iter().any(|arg| arg == "inspect") {
                if self.fail_runner_inspect && self.refers_to_runner(args) {
                    self.fail_runner_inspect = false;
                    anyhow::bail!("injected runner inspect failure");
                }
                let inspect_runner = self.refers_to_runner(args);
                if (self.all_containers_absent || inspect_runner)
                    && (self.all_containers_absent
                        || self.runner_absent
                        || self
                            .runner_live
                            .as_ref()
                            .is_some_and(|live| !live.load(std::sync::atomic::Ordering::SeqCst)))
                {
                    return Ok(crate::scaleset::worker::WorkerOutput {
                        code: 1,
                        stdout: String::new(),
                        stderr: "Error: No such container".to_owned(),
                    });
                }
                if args.iter().any(|arg| arg.contains(".Config.Labels")) {
                    if let Some(identity) = &self.identity {
                        let reference = args.last().unwrap();
                        let (container_id, role) = if reference == &identity.runner_container()
                            || reference == "runner-container-id"
                        {
                            ("runner-container-id", "runner")
                        } else if reference == &identity.dind_container()
                            || reference == "dind-container-id"
                        {
                            ("dind-container-id", "dind")
                        } else {
                            anyhow::bail!("unexpected container ownership inspection: {reference}");
                        };
                        let mut labels = identity.labels();
                        labels.insert("velnor.scaleset.role".to_owned(), role.to_owned());
                        return Ok(crate::scaleset::worker::WorkerOutput {
                            code: 0,
                            stdout: format!("{container_id}\n{}", serde_json::to_string(&labels)?),
                            stderr: String::new(),
                        });
                    }
                    let ownership = self
                        .ownership_label
                        .clone()
                        .unwrap_or_else(|| format!("7/{}", self.runner_name));
                    return Ok(crate::scaleset::worker::WorkerOutput {
                        code: 0,
                        stdout: format!("velnor.scaleset.ownership={ownership}\n"),
                        stderr: String::new(),
                    });
                }
                if args.iter().any(|arg| arg.contains(".Labels")) {
                    let ownership = self
                        .ownership_label
                        .clone()
                        .unwrap_or_else(|| format!("7/{}", self.runner_name));
                    return Ok(crate::scaleset::worker::WorkerOutput {
                        code: 0,
                        stdout: format!("velnor.scaleset.ownership={ownership}\n"),
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
                && self.refers_to_runner(args)
                && args.iter().any(|arg| arg == "rm")
            {
                self.fail_runner_remove = false;
                return Ok(crate::scaleset::worker::WorkerOutput {
                    code: 1,
                    stdout: String::new(),
                    stderr: "container is busy".to_owned(),
                });
            }
            if args.iter().any(|arg| arg == "rm")
                && let Some(action) = self.after_remove.take()
            {
                action();
            }
            Ok(crate::scaleset::worker::WorkerOutput {
                code: 1,
                stdout: String::new(),
                stderr: "Error: No such object".to_owned(),
            })
        }
    }

    struct RetryObservationRunner {
        identity: WorkerIdentity,
        profile: HomogeneousProfile,
        state_dir: PathBuf,
        dind_running: bool,
        runner_running: bool,
        dind_ready: bool,
        session_established: bool,
        seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    }

    impl WorkerRunner for RetryObservationRunner {
        fn run(
            &mut self,
            program: &str,
            args: &[String],
        ) -> anyhow::Result<crate::scaleset::worker::WorkerOutput> {
            assert_eq!(program, "docker");
            self.seen.lock().unwrap().push(args.to_vec());
            let output = |stdout: &str| crate::scaleset::worker::WorkerOutput {
                code: 0,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            };
            if args.first().is_some_and(|arg| arg == "network")
                && args.get(1).is_some_and(|arg| arg == "ls")
            {
                return Ok(output("network-container-id\n"));
            }
            if args.first().is_some_and(|arg| arg == "container")
                && args.get(1).is_some_and(|arg| arg == "ls")
            {
                let role = if args.iter().any(|arg| arg.ends_with("=dind")) {
                    "dind"
                } else {
                    "runner"
                };
                return Ok(output(if role == "dind" {
                    "dind-container-id\n"
                } else {
                    "runner-container-id\n"
                }));
            }
            if args.first().is_some_and(|arg| arg == "network")
                && args.get(1).is_some_and(|arg| arg == "inspect")
            {
                let format = args.get(3).map(String::as_str).unwrap_or_default();
                let mut labels = self.identity.labels();
                if format.contains(".Labels") {
                    return Ok(output(&format!(
                        "network-container-id\n{}",
                        serde_json::to_string(&labels)?
                    )));
                }
                let snapshot = serde_json::json!({
                    "Id": "network-container-id",
                    "Name": self.identity.network(),
                    "Labels": labels,
                    "Driver": "bridge",
                    "Scope": "local",
                    "Internal": false,
                    "Attachable": false,
                    "Ingress": false,
                    "ConfigOnly": false,
                    "EnableIPv6": false,
                    "Options": {},
                    "IPAM": {
                        "Driver": "default",
                        "Options": null,
                        "Config": [{
                            "Subnet": "172.30.0.0/16",
                            "IPRange": "",
                            "Gateway": "172.30.0.1"
                        }]
                    }
                });
                return Ok(output(&snapshot.to_string()));
            }
            if args.first().is_some_and(|arg| arg == "image")
                && args.get(1).is_some_and(|arg| arg == "inspect")
            {
                let reference = args.last().unwrap();
                let (role, image_id, image) = if reference == &self.profile.runner().reference() {
                    ("runner", "runner-image-id", self.profile.runner())
                } else if reference == &self.profile.dind().reference() {
                    ("dind", "dind-image-id", self.profile.dind())
                } else {
                    anyhow::bail!("unexpected pinned image inspect: {reference}");
                };
                let labels = if role == "runner" {
                    serde_json::json!({
                        crate::scaleset::worker::runner::RUNNER_SOURCE_LABEL:
                            crate::scaleset::worker::runner::RUNNER_SOURCE
                    })
                } else {
                    serde_json::json!({})
                };
                let snapshot = serde_json::json!({
                    "Id": image_id,
                    "RepoDigests": [image.reference()],
                    "Config": { "Env": [], "Labels": labels }
                });
                return Ok(output(&snapshot.to_string()));
            }
            if args.first().is_some_and(|arg| arg == "inspect")
                && args.iter().any(|arg| arg.contains(".State.Running"))
            {
                let name = args.last().unwrap();
                let running = if name == &self.identity.dind_container()
                    || name == "dind-container-id"
                {
                    self.dind_running
                } else if name == &self.identity.runner_container() || name == "runner-container-id"
                {
                    self.runner_running
                } else {
                    anyhow::bail!("unexpected container health inspection: {name}");
                };
                return Ok(output(if running { "true" } else { "false" }));
            }
            if args.first().is_some_and(|arg| arg == "inspect")
                && args.iter().any(|arg| arg.contains(".Config.Labels"))
            {
                let name = args.last().unwrap();
                let role = if name == &self.identity.dind_container() {
                    "dind"
                } else if name == &self.identity.runner_container() {
                    "runner"
                } else {
                    anyhow::bail!("unexpected container ownership inspection: {name}");
                };
                let mut labels = self.identity.labels();
                labels.insert("velnor.scaleset.role".to_owned(), role.to_owned());
                let container_id = if role == "dind" {
                    "dind-container-id"
                } else {
                    "runner-container-id"
                };
                return Ok(output(&format!(
                    "{container_id}\n{}",
                    serde_json::to_string(&labels)?
                )));
            }
            if args.first().is_some_and(|arg| arg == "inspect") {
                let reference = args.last().unwrap();
                let role = if reference == &self.identity.dind_container()
                    || reference == "dind-container-id"
                {
                    "dind"
                } else if reference == &self.identity.runner_container()
                    || reference == "runner-container-id"
                {
                    "runner"
                } else {
                    anyhow::bail!("unexpected full container inspect: {reference}");
                };
                return Ok(output(&self.container_snapshot(role).to_string()));
            }
            if args.first().is_some_and(|arg| arg == "logs") {
                return Ok(output(if self.session_established {
                    "Connected to GitHub\nListening for Jobs\n"
                } else {
                    "Connected to GitHub\n"
                }));
            }
            if args.first().is_some_and(|arg| arg == "exec") {
                return Ok(output(if self.dind_ready { "28.5.2\n" } else { "\n" }));
            }
            if args.first().is_some_and(|arg| arg == "start") {
                return Ok(output(""));
            }
            if args.first().is_some_and(|arg| arg == "restart") {
                return Ok(output(""));
            }
            anyhow::bail!("unexpected Docker command in retry observation fixture: {args:?}");
        }
    }

    impl RetryObservationRunner {
        fn container_snapshot(&self, role: &str) -> serde_json::Value {
            let (id, name, image_id, image_ref, running) = if role == "dind" {
                (
                    "dind-container-id",
                    self.identity.dind_container(),
                    "dind-image-id",
                    self.profile.dind().reference(),
                    self.dind_running,
                )
            } else {
                (
                    "runner-container-id",
                    self.identity.runner_container(),
                    "runner-image-id",
                    self.profile.runner().reference(),
                    self.runner_running,
                )
            };
            let mut labels = self.identity.labels();
            if role == "runner" {
                labels.insert(
                    crate::scaleset::worker::runner::RUNNER_SOURCE_LABEL.to_owned(),
                    crate::scaleset::worker::runner::RUNNER_SOURCE.to_owned(),
                );
            }
            labels.insert("velnor.scaleset.role".to_owned(), role.to_owned());
            let env = if role == "dind" {
                vec!["DOCKER_TLS_CERTDIR=".to_owned()]
            } else {
                vec![
                    format!(
                        "{}={}",
                        crate::scaleset::worker::runner::RUNNER_NAME_ENV,
                        self.identity.ownership().runner_name()
                    ),
                    format!(
                        "DOCKER_HOST=unix://{}",
                        crate::scaleset::worker::dind::DIND_SOCKET
                    ),
                    format!(
                        "RUNNER_WORK_FOLDER={}",
                        crate::scaleset::worker::runner::RUNNER_WORK_DIR
                    ),
                    format!(
                        "{}=fixture-jit",
                        crate::scaleset::worker::runner::JIT_CONFIG_ENV
                    ),
                ]
            };
            let state_dir = self.state_dir.canonicalize().unwrap();
            let mounts = if role == "dind" {
                vec![
                    serde_json::json!({
                        "Type": "volume",
                        "Source": format!("/var/lib/docker/volumes/{}/_data", self.identity.dind_data_volume()),
                        "Destination": crate::scaleset::worker::dind::DIND_DATA_ROOT,
                        "Name": self.identity.dind_data_volume(),
                        "RW": true
                    }),
                    serde_json::json!({
                        "Type": "bind",
                        "Source": state_dir.to_string_lossy(),
                        "Destination": crate::scaleset::worker::dind::STATE_MOUNT,
                        "RW": true
                    }),
                ]
            } else {
                let cache = self
                    .state_dir
                    .join("buildkit-cache")
                    .canonicalize()
                    .unwrap();
                vec![
                    serde_json::json!({
                        "Type": "bind",
                        "Source": state_dir.to_string_lossy(),
                        "Destination": crate::scaleset::worker::dind::STATE_MOUNT,
                        "RW": true
                    }),
                    serde_json::json!({
                        "Type": "volume",
                        "Source": format!("/var/lib/docker/volumes/{}/_data", self.identity.workspace_volume()),
                        "Destination": crate::scaleset::worker::runner::RUNNER_WORK_DIR,
                        "Name": self.identity.workspace_volume(),
                        "RW": true
                    }),
                    serde_json::json!({
                        "Type": "volume",
                        "Source": format!("/var/lib/docker/volumes/{}/_data", self.identity.workspace_volume()),
                        "Destination": crate::scaleset::worker::runner::TOOL_CACHE_DIR,
                        "Name": self.identity.workspace_volume(),
                        "RW": true
                    }),
                    serde_json::json!({
                        "Type": "bind",
                        "Source": cache.to_string_lossy(),
                        "Destination": crate::scaleset::worker::dind::BUILDKIT_CACHE_DIR,
                        "RW": true
                    }),
                ]
            };
            let networks = if role == "dind" {
                serde_json::json!({
                    self.identity.network(): { "NetworkID": "network-container-id" }
                })
            } else {
                serde_json::json!({})
            };
            let mut config = serde_json::json!({
                "Image": image_ref,
                "Labels": labels,
                "Env": env
            });
            if role == "dind" {
                config["Cmd"] = serde_json::json!([format!(
                    "-H unix://{}",
                    crate::scaleset::worker::dind::DIND_SOCKET
                )]);
            }
            serde_json::json!({
                "Id": id,
                "Name": format!("/{name}"),
                "Image": image_id,
                "Config": config,
                "HostConfig": {
                    "NetworkMode": if role == "dind" {
                        "network-container-id".to_owned()
                    } else {
                        "container:dind-container-id".to_owned()
                    },
                    "Privileged": role == "dind"
                },
                "Mounts": mounts,
                "State": { "Running": running },
                "NetworkSettings": { "Networks": networks }
            })
        }
    }

    #[derive(Default)]
    struct RejectingWorkerRunner {
        seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    }

    impl WorkerRunner for RejectingWorkerRunner {
        fn run(
            &mut self,
            program: &str,
            args: &[String],
        ) -> anyhow::Result<crate::scaleset::worker::WorkerOutput> {
            self.seen.lock().unwrap().push(args.to_vec());
            anyhow::bail!("unexpected worker command: {program} {args:?}")
        }
    }

    struct RejectingScaleSetApi {
        client: ScaleSetClient,
        request_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        stop: std::sync::mpsc::Sender<()>,
        server: Option<std::thread::JoinHandle<()>>,
    }

    impl RejectingScaleSetApi {
        fn new() -> Self {
            use std::io::{Read, Write};
            use std::sync::atomic::AtomicUsize;
            use std::sync::mpsc;

            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let request_count = std::sync::Arc::new(AtomicUsize::new(0));
            let server_request_count = request_count.clone();
            let (stop, stopped) = mpsc::channel();
            let server = std::thread::spawn(move || loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        server_request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                        let mut request = [0_u8; 2048];
                        let _ = stream.read(&mut request);
                        let _ = stream.write_all(
                            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => return,
                }
                match stopped.recv_timeout(Duration::from_millis(5)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            });
            let client = ScaleSetClient::new_with_pat(
                &format!("http://{address}/octo-org"),
                "test-token",
                crate::scaleset::client::SystemInfo::default(),
                crate::scaleset::backoff::RetryPolicy {
                    max_retries: 0,
                    wait_min: Duration::ZERO,
                    wait_max: Duration::ZERO,
                    timeout: Duration::from_secs(1),
                },
            )
            .unwrap();
            Self {
                client,
                request_count,
                stop,
                server: Some(server),
            }
        }

        fn finish(mut self) -> usize {
            let _ = self.stop.send(());
            if let Some(server) = self.server.take() {
                server.join().unwrap();
            }
            self.request_count.load(std::sync::atomic::Ordering::SeqCst)
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
        path.canonicalize().unwrap()
    }

    fn runtime_columns(path: &Path) -> Vec<String> {
        let conn = Connection::open(path).unwrap();
        let mut statement = conn
            .prepare("PRAGMA table_info(scaleset_worker_runtime)")
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
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
        test_lane_with_client(db, ledger, state_root, runner, client)
    }

    fn test_lane_with_client(
        db: &Path,
        ledger: &Path,
        state_root: &Path,
        runner: Box<dyn WorkerRunner + Send>,
        client: ScaleSetClient,
    ) -> DaemonWorkerLane {
        let intents = ProvisionIntentStore::open(db).unwrap();
        let registry = WorkerRegistry::open(db).unwrap();
        let ledger = SharedLedger::open(ledger).unwrap();
        DaemonWorkerLane {
            client,
            config: LaneConfig {
                scale_set_id: 7,
                profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
                state_root: state_root.to_path_buf(),
                ready_attempts: 1,
                sweep_interval: Duration::ZERO,
            },
            state_db: db.canonicalize().unwrap(),
            runner,
            hook: Box::new(crate::scaleset::worker::DockerToolContentHook),
            intents,
            registry,
            ledger,
            workers: HashMap::new(),
            last_sweep: None,
        }
    }

    #[cfg(unix)]
    fn write_test_diagnostics_marker(identity: &WorkerIdentity, state_dir: &Path) -> PathBuf {
        let test_dir = unique_test_dir("cleanup-capability-marker");
        let db = test_dir.join("state.db");
        let ledger_path = test_dir.join("permit-ledger.db");
        std::fs::File::create(&ledger_path).unwrap();
        let ownership_id = identity.ownership().as_str();
        let (raw_scale_set_id, runner_name) = ownership_id.split_once('/').unwrap();
        let scale_set_id = raw_scale_set_id.parse::<i32>().unwrap();
        let request_id = request_id_for_key(scale_set_id, &ownership_id).unwrap();
        let state_root = state_dir.parent().unwrap();
        let mut registry = WorkerRegistry::open(&db).unwrap();
        assert_eq!(
            seed_registry_worker(
                &mut registry,
                state_root,
                request_id,
                runner_name,
                ScaleSetWorkerState::DiagnosticExport,
            ),
            ownership_id
        );
        let lock =
            crate::scaleset::reconcile::lock_worker_docker_lifecycle(&ledger_path, &ownership_id)
                .unwrap();
        let lifecycle =
            WorkerLifecycleCapability::for_cleanup(&lock, &mut registry, &ownership_id).unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut runner =
            CleanupRunner::missing(identity.runner_container(), seen).with_identity(identity);
        let export = Supervision::new(identity.clone(), state_dir)
            .prepare_cleanup(&lifecycle, &mut runner)
            .unwrap();
        assert!(export.failures.is_empty(), "{export:?}");
        drop(lifecycle);
        drop(lock);
        drop(registry);
        let _ = std::fs::remove_dir_all(&test_dir);
        export.dir.join("capture.complete")
    }

    fn seed_registry_worker(
        registry: &mut WorkerRegistry,
        state_root: &Path,
        request_id: i64,
        runner_name: &str,
        state: ScaleSetWorkerState,
    ) -> String {
        let ownership = OwnershipId::bind(7, runner_name);
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str().to_owned();
        let state_dir = state_root.join(ownership.slug());
        registry
            .upsert(
                &key,
                &format!("op-{request_id}"),
                request_id,
                runner_name,
                &identity.network(),
                state_dir.join("workspace").to_string_lossy().as_ref(),
                state_dir.join("dind-data").to_string_lossy().as_ref(),
                "sha256:runner",
                "sha256:dind",
            )
            .unwrap();
        registry.set_state(&key, state).unwrap();
        key
    }

    fn state_dir_cleanup_pending(registry: &WorkerRegistry, key: &str) -> bool {
        registry
            .get(key)
            .unwrap()
            .unwrap()
            .state_dir_cleanup_pending
    }

    #[test]
    fn released_state_cleanup_authorization_is_bound_to_pending_worker() {
        let dir = unique_test_dir("released-state-authorization");
        let db = dir.join("state.db");
        let mut registry = WorkerRegistry::open(&db).unwrap();

        let pending_key = seed_registry_worker(
            &mut registry,
            &dir.join("workers"),
            8101,
            "velnor-7-8101",
            ScaleSetWorkerState::OwnedCleanup,
        );
        registry
            .set_state(&pending_key, ScaleSetWorkerState::PermitReleased)
            .unwrap();
        let authorization = registry.authorize_state_cleanup(&pending_key).unwrap();
        let pending_row = registry.get(&pending_key).unwrap().unwrap();
        let pending_state_dir = PathBuf::from(pending_row.workspace_path.unwrap())
            .parent()
            .unwrap()
            .to_path_buf();
        assert!(authorization.authorizes(&pending_key, &pending_state_dir));
        assert!(!authorization.authorizes("7/velnor-7-8102", &pending_state_dir));
        let same_worker_wrong_path = Supervision::new(
            WorkerIdentity::new(OwnershipId::bind(7, "velnor-7-8101")),
            &dir.join("workers/velnor-7-8102"),
        );
        assert!(same_worker_wrong_path
            .release_owned_state(&authorization)
            .is_err());

        let not_pending_key = seed_registry_worker(
            &mut registry,
            &dir.join("workers"),
            8102,
            "velnor-7-8102",
            ScaleSetWorkerState::PermitReleased,
        );
        Connection::open(&db)
            .unwrap()
            .execute(
                "UPDATE scaleset_worker_runtime SET state_dir_cleanup_pending = 0
                 WHERE ownership_id = ?1",
                params![not_pending_key],
            )
            .unwrap();
        assert!(registry.authorize_state_cleanup(&not_pending_key).is_err());
        assert!(registry
            .authorize_state_cleanup("7/missing-worker")
            .is_err());

        drop(registry);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn docker_create_target_migration_and_same_target_replay() {
        use crate::scaleset::worker::runner::DockerCreateTarget as Target;

        let dir = unique_test_dir("docker-create-target-migration");
        let db = dir.join("state.db");
        velnor_control::store::Store::open(&db).unwrap();
        let runner_name = "velnor-7-8178";
        let key = OwnershipId::bind(7, runner_name).as_str().to_owned();
        Connection::open(&db)
            .unwrap()
            .execute_batch(&format!(
                "CREATE TABLE scaleset_worker_runtime (
                     ownership_id TEXT PRIMARY KEY,
                     runner_start_deadline_epoch INTEGER,
                     dind_restarts_used INTEGER NOT NULL DEFAULT 0 CHECK (dind_restarts_used >= 0),
                     diagnostics_complete INTEGER NOT NULL DEFAULT 0
                         CHECK (diagnostics_complete IN (0, 1)),
                     state_dir_cleanup_pending INTEGER NOT NULL DEFAULT 0
                         CHECK (state_dir_cleanup_pending IN (0, 1)),
                     pending_docker_create TEXT CHECK (
                         pending_docker_create IS NULL OR
                         pending_docker_create IN ('network', 'dind', 'runner')
                     )
                 );
                 INSERT INTO scaleset_worker_runtime
                     (ownership_id, runner_start_deadline_epoch, dind_restarts_used,
                      diagnostics_complete, state_dir_cleanup_pending, pending_docker_create)
                 VALUES ('{key}', 1234, 7, 1, 0, 'runner');"
            ))
            .unwrap();

        let state_root = dir.join("workers");
        let mut registry = WorkerRegistry::open(&db).unwrap();
        let seeded = seed_registry_worker(
            &mut registry,
            &state_root,
            8178,
            runner_name,
            ScaleSetWorkerState::ProvisionIntent,
        );
        assert_eq!(seeded, key);
        let migrated = registry.get(&key).unwrap().unwrap();
        assert_eq!(migrated.runner_start_deadline_epoch, Some(1234));
        assert_eq!(migrated.dind_restarts_used, 7);
        assert!(migrated.diagnostics_complete);
        assert_eq!(migrated.pending_docker_create, Some(Target::Runner));

        let identity = WorkerIdentity::new(OwnershipId::bind(7, runner_name));
        let recovery_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut recovery_runner =
            CleanupRunner::runner_absent_for_identity(&identity, recovery_calls.clone());
        assert!(registry
            .resolve_pending_docker_create_after_inspect(&key, &identity, &mut recovery_runner)
            .unwrap_err()
            .to_string()
            .contains("absent but unresolved"));
        assert_eq!(
            registry.get(&key).unwrap().unwrap().pending_docker_create,
            Some(Target::Runner),
            "absence cannot prove that Docker rejected an accepted create"
        );
        assert!(!recovery_calls.lock().unwrap().is_empty());
        assert!(registry
            .begin_docker_create(&key, Target::Runner, 5678)
            .is_err());

        let mut present_runner = CleanupRunner::for_identity(&identity, recovery_calls.clone());
        registry
            .resolve_pending_docker_create_after_inspect(&key, &identity, &mut present_runner)
            .unwrap();
        assert_eq!(
            registry.get(&key).unwrap().unwrap().pending_docker_create,
            None
        );
        registry
            .begin_docker_create(&key, Target::Runner, 5678)
            .unwrap();

        registry
            .resolve_docker_create(&key, Target::Runner)
            .unwrap();
        registry
            .begin_docker_create(&key, Target::DindDataVolume, 5678)
            .unwrap();
        assert!(registry
            .begin_docker_create(&key, Target::DindDataVolume, 9999)
            .is_err());
        assert_eq!(
            registry.get(&key).unwrap().unwrap().pending_docker_create,
            Some(Target::DindDataVolume)
        );
        assert!(registry
            .begin_docker_create(&key, Target::WorkspaceVolume, 5678)
            .is_err());

        registry
            .resolve_docker_create(&key, Target::DindDataVolume)
            .unwrap();
        registry
            .begin_docker_create(&key, Target::WorkspaceVolume, 5678)
            .unwrap();
        assert_eq!(
            registry.get(&key).unwrap().unwrap().pending_docker_create,
            Some(Target::WorkspaceVolume)
        );
        drop(registry);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn canceled_cleanup_without_worker_defers_ledger_finalization() {
        use velnor_control::permit_ledger::{DemandState, PermitLedger};

        let dir = unique_test_dir("canceled-cleanup-no-worker");
        let db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        PermitLedger::open(&ledger_path)
            .unwrap()
            .set_max_jobs(1)
            .unwrap();

        let mut lane = test_lane(
            &db,
            &ledger_path,
            &state_root,
            Box::new(CleanupRunner::missing(
                "unused-worker".to_owned(),
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            )),
        );
        let request_id = 8179;
        let holder = permit_holder(7, request_id);
        let generation = lane.ledger.generation().unwrap();
        let mut demand = DemandStore::open(&db).unwrap();
        demand
            .submit_offer(7, &eligible_offer(request_id), generation)
            .unwrap();
        lane.ledger
            .observe_demand(
                &holder,
                crate::scaleset::capacity::LedgerLane::ScaleSet,
                "scaleset/7",
                1,
                2,
            )
            .unwrap();
        let (outcome, lease_generation) = lane
            .ledger
            .acquire_with_lease_generation(
                &holder,
                crate::scaleset::capacity::LedgerLane::ScaleSet,
                LedgerPermitState::Acquiring,
                generation,
            )
            .unwrap();
        assert_eq!(outcome, crate::scaleset::capacity::AcquireOutcome::Acquired);
        let lease_generation = lease_generation.unwrap();
        demand
            .set_state_with_permit_lease(
                request_id,
                LocalDemandState::CanceledAcquired,
                None,
                generation,
                lease_generation,
            )
            .unwrap();
        drop(demand);

        // No worker row means there is nothing for the lane to clean. It
        // must leave the permit open for Processor's Cancelled finalizer.
        lane.note_canceled_request(request_id).unwrap();
        assert_eq!(lane.ledger.occupied().unwrap(), 1);
        assert!(lane.ledger.holder_state(&holder).unwrap().is_some());
        assert_eq!(
            DemandStore::open(&db)
                .unwrap()
                .get(request_id)
                .unwrap()
                .unwrap()
                .state,
            LocalDemandState::CanceledAcquired
        );

        assert!(lane
            .ledger
            .release_cancelled_if_generation(&holder, lease_generation)
            .unwrap());
        assert_eq!(lane.ledger.occupied().unwrap(), 0);
        assert_eq!(
            PermitLedger::open(&ledger_path)
                .unwrap()
                .demand(&holder)
                .unwrap()
                .unwrap()
                .state,
            DemandState::Cancelled
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn eligible_offer(request_id: i64) -> velnor_model::ScaleSetJobAvailable {
        velnor_model::ScaleSetJobAvailable {
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
                event_name: "push".to_owned(),
                request_labels: vec!["velnor".to_owned()],
                queue_time: String::new(),
                scale_set_assign_time: String::new(),
                runner_assign_time: String::new(),
                finish_time: String::new(),
            },
        }
    }

    fn adopted_post_provision_retry_fixture(
        request_id: i64,
        dind_running: bool,
        runner_running: bool,
        dind_restarts_used: u32,
    ) -> (
        PathBuf,
        DaemonWorkerLane,
        ProvisionIntent,
        String,
        std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        RejectingScaleSetApi,
    ) {
        adopted_post_provision_retry_fixture_with_session(
            request_id,
            dind_running,
            runner_running,
            dind_restarts_used,
            true,
        )
    }

    fn adopted_post_provision_retry_fixture_with_session(
        request_id: i64,
        dind_running: bool,
        runner_running: bool,
        dind_restarts_used: u32,
        session_established: bool,
    ) -> (
        PathBuf,
        DaemonWorkerLane,
        ProvisionIntent,
        String,
        std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        RejectingScaleSetApi,
    ) {
        adopted_post_provision_retry_fixture_with_runtime(
            request_id,
            dind_running,
            runner_running,
            dind_restarts_used,
            session_established,
            true,
        )
    }

    fn adopted_post_provision_retry_fixture_with_runtime(
        request_id: i64,
        dind_running: bool,
        runner_running: bool,
        dind_restarts_used: u32,
        session_established: bool,
        dind_ready: bool,
    ) -> (
        PathBuf,
        DaemonWorkerLane,
        ProvisionIntent,
        String,
        std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        RejectingScaleSetApi,
    ) {
        let dir = unique_test_dir("post-provision-retry");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        let runner_name = crate::scaleset::runner_name(7, request_id);
        let ownership = OwnershipId::bind(7, &runner_name);
        let key = ownership.as_str().to_owned();
        let operation_id = format!("op-{request_id}");
        let state_dir = state_root.join(ownership.slug());
        std::fs::create_dir_all(state_dir.join("buildkit-cache")).unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let profile = HomogeneousProfile::for_arch("x86_64").unwrap();
        let runner = RetryObservationRunner {
            identity: WorkerIdentity::new(ownership),
            profile,
            state_dir,
            dind_running,
            runner_running,
            dind_ready,
            session_established,
            seen: seen.clone(),
        };
        let api = RejectingScaleSetApi::new();
        velnor_control::permit_ledger::PermitLedger::open(&ledger)
            .unwrap()
            .set_max_jobs(1)
            .unwrap();
        let mut lane = test_lane_with_client(
            &db,
            &ledger,
            &state_root,
            Box::new(runner),
            api.client.clone(),
        );
        let generation = lane.ledger.generation().unwrap();
        let mut demand = DemandStore::open(&db).unwrap();
        demand
            .submit_offer(7, &eligible_offer(request_id), generation)
            .unwrap();
        let holder = permit_holder(7, request_id);
        lane.ledger
            .observe_demand(
                &holder,
                crate::scaleset::capacity::LedgerLane::ScaleSet,
                "scaleset/7",
                1,
                2,
            )
            .unwrap();
        let (outcome, lease_generation) = lane
            .ledger
            .acquire_with_lease_generation(
                &holder,
                crate::scaleset::capacity::LedgerLane::ScaleSet,
                LedgerPermitState::Acquiring,
                generation,
            )
            .unwrap();
        assert_eq!(outcome, crate::scaleset::capacity::AcquireOutcome::Acquired);
        let lease_generation = lease_generation.unwrap();
        demand
            .set_state_with_permit_lease(
                request_id,
                LocalDemandState::Acquired,
                None,
                generation,
                lease_generation,
            )
            .unwrap();
        let intent = lane
            .intents
            .record_intent(
                &operation_id,
                &crate::scaleset::provision_ownership_id(7, &runner_name),
                7,
                request_id,
                &runner_name,
                "sha256:runner",
                "sha256:dind",
                generation,
            )
            .unwrap();
        assert_eq!(
            seed_registry_worker(
                &mut lane.registry,
                &state_root,
                request_id,
                &runner_name,
                ScaleSetWorkerState::Running,
            ),
            key
        );
        lane.registry
            .set_dind_restarts_used(&key, dind_restarts_used)
            .unwrap();

        // Reconstruct the post-provision worker after the demand transition
        // crashed. Adoption must keep this worker tracked for completion.
        let report = lane.adopt_live_workers().unwrap();
        assert_eq!(report.adopted, 1);
        assert_eq!(lane.live_workers(), 1);
        (dir, lane, intent, key, seen, api)
    }

    fn assert_no_docker_starts(seen: &std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>) {
        let calls = seen.lock().unwrap();
        let starts: Vec<_> = calls
            .iter()
            .filter(|args| args.first().is_some_and(|arg| arg == "start"))
            .collect();
        assert!(starts.is_empty(), "unexpected Docker starts: {starts:?}");
    }

    #[tokio::test]
    async fn same_operation_retry_keeps_failed_runner_tracked_for_github_completion() {
        let request_id = 8120;
        let (dir, mut lane, intent, key, seen, api) =
            adopted_post_provision_retry_fixture(request_id, true, false, 0);
        assert!(matches!(
            lane.tick_worker(&key).unwrap(),
            SupervisionOutcome::WorkerFailed { .. }
        ));
        // Reproduce the crash window where the durable worker row survived
        // but provision never inserted the live worker into this process.
        lane.workers.remove(&key);
        assert_eq!(lane.live_workers(), 0);

        let result = lane.provision(&intent).await;
        let api_requests = api.finish();
        assert!(result.is_ok(), "retry failed: {result:?}");
        assert_eq!(api_requests, 0, "post-provision retry fetched a new JIT");
        assert_no_docker_starts(&seen);

        let worker = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(worker.worker_state, ScaleSetWorkerState::Running);
        assert_eq!(worker.operation_id, intent.operation_id);
        assert_eq!(worker.dind_restarts_used, 0);
        assert_eq!(
            DemandStore::open(&dir.join("state.db"))
                .unwrap()
                .get(request_id)
                .unwrap()
                .unwrap()
                .state,
            LocalDemandState::Acquired
        );
        assert_eq!(lane.live_workers(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn live_retry_fails_closed_when_acquired_demand_lost_its_permit() {
        let request_id = 8125;
        let (dir, mut lane, intent, key, seen, api) =
            adopted_post_provision_retry_fixture(request_id, true, true, 0);
        let holder = permit_holder(7, request_id);
        let lease_generation = lane
            .ledger
            .permit_lease_generation(&holder)
            .unwrap()
            .expect("fixture must hold the acquired request's permit");
        let before = seen.lock().unwrap().len();
        assert!(lane
            .ledger
            .release_if_generation(&holder, lease_generation)
            .unwrap());

        assert!(lane.tick_worker(&key).is_err());
        lane.workers.remove(&key);
        let retry = lane.provision(&intent).await;
        let api_requests = api.finish();
        assert!(retry.is_err(), "unleased retry must be rejected");
        assert_eq!(
            api_requests, 0,
            "unleased retry must not fetch a JIT config"
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            before,
            "unleased retry must not inspect, start, or create Docker resources"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pre_session_failure_fence_survives_restart_without_restarting_or_releasing_lease() {
        let request_id = 8130;
        let (dir, mut lane, intent, key, _seen, api) =
            adopted_post_provision_retry_fixture_with_session(request_id, true, true, 0, false);
        let holder = permit_holder(7, request_id);
        let lease_generation = lane
            .ledger
            .permit_lease_generation(&holder)
            .unwrap()
            .expect("fixture must hold the acquired request's permit");

        lane.registry
            .set_state(&key, ScaleSetWorkerState::DindReady)
            .unwrap();
        lane.registry
            .set_runner_start_deadline_if_none(
                &key,
                crate::scaleset::worker::supervise::epoch_seconds().saturating_sub(1),
            )
            .unwrap();
        assert!(matches!(
            lane.tick_worker(&key).unwrap(),
            SupervisionOutcome::ProvisioningFailedBeforeSession { .. }
        ));
        let fenced = lane.registry.get(&key).unwrap().unwrap();
        assert!(fenced.awaiting_upstream_completion);
        assert_eq!(fenced.worker_state, ScaleSetWorkerState::DindReady);
        assert_eq!(lane.ledger.occupied().unwrap(), 1);
        assert_eq!(
            lane.ledger.permit_lease_generation(&holder).unwrap(),
            Some(lease_generation)
        );
        assert_eq!(
            DemandStore::open(&dir.join("state.db"))
                .unwrap()
                .get(request_id)
                .unwrap()
                .unwrap()
                .state,
            LocalDemandState::Acquired
        );
        drop(lane);

        let retry_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut recovered = test_lane_with_client(
            &dir.join("state.db"),
            &dir.join("permit-ledger.db"),
            &dir.join("workers"),
            Box::new(RejectingWorkerRunner {
                seen: retry_seen.clone(),
            }),
            api.client.clone(),
        );
        let report = recovered.adopt_live_workers().unwrap();
        assert_eq!(report.adopted, 1);
        assert_eq!(recovered.live_workers(), 1);
        assert!(retry_seen.lock().unwrap().is_empty());
        recovered.workers.remove(&key);

        let result = recovered.provision(&intent).await;
        let api_requests = api.finish();
        assert!(result.is_ok(), "pre-session retry failed: {result:?}");
        assert_eq!(
            api_requests, 0,
            "pre-session retry fetched a new JIT config"
        );
        assert!(retry_seen.lock().unwrap().is_empty());
        let row = recovered.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::DindReady);
        assert!(row.awaiting_upstream_completion);
        assert_eq!(recovered.ledger.occupied().unwrap(), 1);
        assert_eq!(
            recovered.ledger.permit_lease_generation(&holder).unwrap(),
            Some(lease_generation)
        );
        let demand = DemandStore::open(&dir.join("state.db"))
            .unwrap()
            .get(request_id)
            .unwrap()
            .unwrap();
        assert_eq!(demand.state, LocalDemandState::Acquired);
        assert_eq!(demand.permit_lease_generation, Some(lease_generation));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn attempted_runner_start_in_provision_intent_is_adopted_without_restarting() {
        let request_id = 8133;
        let (dir, mut lane, intent, key, _seen, api) =
            adopted_post_provision_retry_fixture_with_runtime(
                request_id, true, true, 0, false, true,
            );
        lane.registry
            .set_state(&key, ScaleSetWorkerState::ProvisionIntent)
            .unwrap();
        lane.registry
            .set_runner_start_deadline_if_none(
                &key,
                crate::scaleset::worker::supervise::epoch_seconds().saturating_add(
                    crate::scaleset::worker::supervise::RUNNER_START_TIMEOUT.as_secs(),
                ),
            )
            .unwrap();
        assert!(
            lane.registry
                .get(&key)
                .unwrap()
                .unwrap()
                .runner_start_attempted
        );
        lane.workers.remove(&key);
        drop(lane);

        let ownership = OwnershipId::bind(7, &crate::scaleset::runner_name(7, request_id));
        let retry_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = RetryObservationRunner {
            identity: WorkerIdentity::new(ownership),
            profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
            state_dir: dir
                .join("workers")
                .join(OwnershipId::bind(7, &crate::scaleset::runner_name(7, request_id)).slug()),
            dind_running: true,
            runner_running: true,
            dind_ready: true,
            session_established: false,
            seen: retry_seen.clone(),
        };
        let mut recovered = test_lane_with_client(
            &dir.join("state.db"),
            &dir.join("permit-ledger.db"),
            &dir.join("workers"),
            Box::new(runner),
            api.client.clone(),
        );
        let report = recovered.adopt_live_workers().unwrap();
        assert_eq!(report.adopted, 1);
        assert_eq!(recovered.live_workers(), 1);
        let adopted = recovered.registry.get(&key).unwrap().unwrap();
        assert_eq!(adopted.worker_state, ScaleSetWorkerState::ProvisionIntent);
        assert!(adopted.runner_start_attempted);
        assert!(!adopted.awaiting_upstream_completion);
        recovered.workers.remove(&key);

        let retry = recovered.provision(&intent).await;
        let api_requests = api.finish();
        assert!(retry.is_ok(), "start-attempt retry failed: {retry:?}");
        assert_eq!(api_requests, 0, "retry fetched a second JIT config");
        assert!(
            retry_seen.lock().unwrap().iter().all(|args| {
                !args
                    .first()
                    .is_some_and(|arg| arg == "start" || arg == "restart")
            }),
            "ambiguous start retry must health-tick the recorded worker only"
        );
        assert_eq!(recovered.live_workers(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_started_event_cannot_advance_dind_unready_worker() {
        let request_id = 8132;
        let (dir, mut lane, _intent, key, seen, api) =
            adopted_post_provision_retry_fixture_with_runtime(
                request_id, false, true, 0, true, false,
            );
        let holder = permit_holder(7, request_id);
        let lease_generation = lane
            .ledger
            .permit_lease_generation(&holder)
            .unwrap()
            .expect("fixture must hold the acquired request's permit");

        // Reopen the last durable pre-session state after startup had to
        // schedule a DinD restart. The runner fixture still emits a stale
        // `Listening for Jobs` marker while DinD's socket is unavailable.
        lane.registry
            .set_state(&key, ScaleSetWorkerState::DindReady)
            .unwrap();
        let restart_count = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|args| args.first().is_some_and(|arg| arg == "restart"))
            .count();
        lane.note_started(&ScaleSetJobStarted {
            runner_id: 1,
            runner_name: crate::scaleset::runner_name(7, request_id),
            base: velnor_model::ScaleSetJobMessage {
                runner_request_id: request_id,
                ..Default::default()
            },
        })
        .unwrap();

        let worker = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(worker.worker_state, ScaleSetWorkerState::DindReady);
        assert_eq!(worker.dind_restarts_used, 1);
        assert!(worker.dind_restart_ready_deadline_epoch.is_some());
        assert_eq!(
            seen.lock()
                .unwrap()
                .iter()
                .filter(|args| args.first().is_some_and(|arg| arg == "restart"))
                .count(),
            restart_count,
            "a pending restart must be polled, not repeated"
        );
        assert_eq!(lane.ledger.occupied().unwrap(), 1);
        assert_eq!(
            lane.ledger.permit_lease_generation(&holder).unwrap(),
            Some(lease_generation)
        );
        assert_eq!(api.finish(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn dind_restart_pending_deadline_survives_restart_without_repeating_restart() {
        let request_id = 8131;
        let (dir, mut lane, _intent, key, first_seen, api) =
            adopted_post_provision_retry_fixture_with_runtime(
                request_id, true, true, 0, true, false,
            );
        let holder = permit_holder(7, request_id);
        let lease_generation = lane
            .ledger
            .permit_lease_generation(&holder)
            .unwrap()
            .expect("fixture must hold the acquired request's permit");
        let pending = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(pending.worker_state, ScaleSetWorkerState::Running);
        assert_eq!(pending.dind_restarts_used, 1);
        let deadline = pending.dind_restart_ready_deadline_epoch.unwrap();
        assert!(deadline > crate::scaleset::worker::supervise::epoch_seconds());
        assert_eq!(
            first_seen
                .lock()
                .unwrap()
                .iter()
                .filter(|args| args.first().is_some_and(|arg| arg == "restart"))
                .count(),
            1
        );
        drop(lane);

        let ownership = OwnershipId::bind(7, &crate::scaleset::runner_name(7, request_id));
        let state_dir = dir.join("workers").join(ownership.slug());
        let retry_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = RetryObservationRunner {
            identity: WorkerIdentity::new(ownership),
            profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
            state_dir,
            dind_running: true,
            runner_running: true,
            dind_ready: false,
            session_established: true,
            seen: retry_seen.clone(),
        };
        let mut recovered = test_lane_with_client(
            &dir.join("state.db"),
            &dir.join("permit-ledger.db"),
            &dir.join("workers"),
            Box::new(runner),
            api.client.clone(),
        );
        let report = recovered.adopt_live_workers().unwrap();
        assert_eq!(report.adopted, 1);
        assert_eq!(
            retry_seen
                .lock()
                .unwrap()
                .iter()
                .filter(|args| args.first().is_some_and(|arg| arg == "restart"))
                .count(),
            0,
            "a persisted restart-pending worker must be polled, not restarted again"
        );
        let replayed = recovered.registry.get(&key).unwrap().unwrap();
        assert_eq!(replayed.dind_restarts_used, 1);
        assert_eq!(replayed.dind_restart_ready_deadline_epoch, Some(deadline));
        assert_eq!(recovered.ledger.occupied().unwrap(), 1);
        assert_eq!(
            recovered.ledger.permit_lease_generation(&holder).unwrap(),
            Some(lease_generation)
        );
        assert_eq!(
            DemandStore::open(&dir.join("state.db"))
                .unwrap()
                .get(request_id)
                .unwrap()
                .unwrap()
                .permit_lease_generation,
            Some(lease_generation)
        );
        assert_eq!(api.finish(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn new_operation_retry_uses_stable_request_ownership() {
        let request_id = 8123;
        let (dir, mut lane, intent, key, seen, api) =
            adopted_post_provision_retry_fixture(request_id, true, false, 0);
        lane.workers.remove(&key);
        let mut retry = intent.clone();
        retry.operation_id = crate::scaleset::provision_operation_id(7, request_id, 1);

        let result = lane.provision(&retry).await;
        let api_requests = api.finish();
        assert!(result.is_ok(), "new-operation retry failed: {result:?}");
        assert_eq!(
            api_requests, 0,
            "health-only retry fetched another JIT config"
        );
        assert_no_docker_starts(&seen);
        let row = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::Running);
        assert_eq!(row.operation_id, intent.operation_id);
        assert_eq!(lane.live_workers(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn terminal_demand_is_fenced_before_health_only_retry() {
        let request_id = 8124;
        let (dir, mut lane, intent, key, seen, api) =
            adopted_post_provision_retry_fixture(request_id, true, true, 0);
        DemandStore::open(&dir.join("state.db"))
            .unwrap()
            .set_state(
                request_id,
                LocalDemandState::Terminal,
                None,
                lane.ledger.generation().unwrap(),
            )
            .unwrap();
        lane.workers.remove(&key);
        seen.lock().unwrap().clear();

        let result = lane.provision(&intent).await;
        let api_requests = api.finish();
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.to_string().contains("before provision retry")),
            "terminal demand entered health-only retry: {result:?}"
        );
        assert_eq!(api_requests, 0, "terminal retry fetched a new JIT");
        assert!(
            seen.lock().unwrap().is_empty(),
            "terminal retry ticked Docker"
        );
        assert_eq!(lane.live_workers(), 0);
        assert_eq!(
            lane.tick_worker(&key).unwrap(),
            SupervisionOutcome::Healthy,
            "late assignment observation must not supervise a terminal demand"
        );
        assert!(
            seen.lock().unwrap().is_empty(),
            "terminal demand restarted DinD"
        );
        assert_eq!(
            lane.registry.get(&key).unwrap().unwrap().worker_state,
            ScaleSetWorkerState::Running
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_adopts_attempted_provision_intent_instead_of_skipping_it() {
        let request_id = 8125;
        let (dir, mut lane, _intent, key, seen, api) =
            adopted_post_provision_retry_fixture(request_id, true, true, 0);
        let _api_requests = api.finish();
        lane.workers.remove(&key);
        lane.registry
            .set_state(&key, ScaleSetWorkerState::ProvisionIntent)
            .unwrap();
        lane.registry
            .set_runner_start_deadline_if_none(
                &key,
                crate::scaleset::worker::supervise::epoch_seconds().saturating_add(60),
            )
            .unwrap();

        let report = lane.adopt_live_workers().unwrap();
        assert_eq!(report.adopted, 1);
        assert_eq!(report.awaiting_provision, 0);
        assert_eq!(lane.live_workers(), 1);
        assert_eq!(
            lane.registry.get(&key).unwrap().unwrap().worker_state,
            ScaleSetWorkerState::RunnerConnected
        );
        assert_no_docker_starts(&seen);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn same_operation_retry_keeps_exhausted_dind_budget_and_never_restarts() {
        let request_id = 8121;
        let (dir, mut lane, intent, key, seen, api) = adopted_post_provision_retry_fixture(
            request_id,
            false,
            true,
            crate::scaleset::worker::supervise::MAX_DIND_RESTARTS,
        );
        assert!(matches!(
            lane.tick_worker(&key).unwrap(),
            SupervisionOutcome::WorkerFailed { reason } if reason.contains("budget")
        ));
        lane.workers.remove(&key);
        assert_eq!(lane.live_workers(), 0);

        let result = lane.provision(&intent).await;
        let api_requests = api.finish();
        assert!(result.is_ok(), "retry failed: {result:?}");
        assert_eq!(api_requests, 0, "post-provision retry fetched a new JIT");
        assert_no_docker_starts(&seen);

        let worker = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(worker.worker_state, ScaleSetWorkerState::Running);
        assert_eq!(worker.operation_id, intent.operation_id);
        assert_eq!(
            worker.dind_restarts_used,
            crate::scaleset::worker::supervise::MAX_DIND_RESTARTS
        );
        assert_eq!(
            DemandStore::open(&dir.join("state.db"))
                .unwrap()
                .get(request_id)
                .unwrap()
                .unwrap()
                .state,
            LocalDemandState::Acquired
        );
        assert_eq!(lane.live_workers(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn provision_intent_after_runner_start_attempt_is_health_only() {
        let request_id = 8122;
        let (dir, mut lane, intent, key, seen, api) =
            adopted_post_provision_retry_fixture(request_id, true, true, 0);
        lane.workers.remove(&key);
        lane.registry
            .set_state(&key, ScaleSetWorkerState::ProvisionIntent)
            .unwrap();
        lane.registry
            .set_runner_start_deadline_if_none(
                &key,
                crate::scaleset::worker::supervise::epoch_seconds().saturating_add(60),
            )
            .unwrap();
        assert!(
            lane.registry
                .get(&key)
                .unwrap()
                .unwrap()
                .runner_start_attempted
        );

        let result = lane.provision(&intent).await;
        let api_requests = api.finish();
        assert!(result.is_ok(), "retry failed: {result:?}");
        assert_eq!(api_requests, 0, "ambiguous runner start fetched a new JIT");
        assert_no_docker_starts(&seen);
        let worker = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(worker.worker_state, ScaleSetWorkerState::RunnerConnected);
        assert!(!worker.runner_start_attempted);
        assert_eq!(worker.operation_id, intent.operation_id);
        assert_eq!(lane.live_workers(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn same_operation_retry_rejects_terminal_cleanup_rows() {
        for (offset, state) in [
            ScaleSetWorkerState::Terminal,
            ScaleSetWorkerState::DiagnosticExport,
            ScaleSetWorkerState::OwnedCleanup,
            ScaleSetWorkerState::PermitReleased,
        ]
        .into_iter()
        .enumerate()
        {
            let request_id = 8130 + i64::try_from(offset).unwrap();
            let (dir, mut lane, intent, key, seen, api) =
                adopted_post_provision_retry_fixture(request_id, true, true, 0);
            lane.registry.set_state(&key, state).unwrap();
            lane.workers.remove(&key);

            let result = lane.provision(&intent).await;
            let api_requests = api.finish();
            assert!(
                result
                    .as_ref()
                    .is_err_and(|error| error.to_string().contains("already terminal")),
                "same-operation retry for {state:?} was not rejected: {result:?}"
            );
            assert_eq!(api_requests, 0, "terminal retry fetched a new JIT");
            assert_no_docker_starts(&seen);
            assert_eq!(
                lane.registry.get(&key).unwrap().unwrap().worker_state,
                state
            );
            let _ = std::fs::remove_dir_all(&dir);
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
        assert!(state_dir_cleanup_pending(&registry, "7/velnor-7-4244"));
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
        registry
            .set_state(key, ScaleSetWorkerState::ProvisionIntent)
            .unwrap();
        registry
            .set_dind_restart_runtime(key, 2, Some(2_500))
            .unwrap();
        registry.set_awaiting_upstream_completion(key).unwrap();
        drop(registry);

        let mut registry = WorkerRegistry::open(&db).unwrap();
        let row = registry.get(key).unwrap().unwrap();
        assert_eq!(row.runner_start_deadline_epoch, Some(1_000));
        assert_eq!(row.dind_restarts_used, 2);
        assert_eq!(row.dind_restart_ready_deadline_epoch, Some(2_500));
        assert!(row.awaiting_upstream_completion);
        registry.set_dind_restart_runtime(key, 3, None).unwrap();
        drop(registry);

        let registry = WorkerRegistry::open(&db).unwrap();
        let row = registry.get(key).unwrap().unwrap();
        assert_eq!(row.dind_restarts_used, 3);
        assert_eq!(row.dind_restart_ready_deadline_epoch, None);
        assert!(row.awaiting_upstream_completion);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_capability_is_bound_to_locked_registry_owner() {
        use crate::scaleset::worker::runner::{DockerCreateTarget as Target, DockerLifecycleEvent};

        let dir = unique_test_dir("worker-lifecycle-capability");
        let db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        std::fs::File::create(&ledger_path).unwrap();
        let key = {
            let mut registry = WorkerRegistry::open(&db).unwrap();
            seed_registry_worker(
                &mut registry,
                &state_root,
                4293,
                "velnor-7-4293",
                ScaleSetWorkerState::ProvisionIntent,
            )
        };
        let lock =
            crate::scaleset::reconcile::lock_worker_docker_lifecycle(&ledger_path, &key).unwrap();
        let mut registry = WorkerRegistry::open(&db).unwrap();
        let mut deadline = None;
        let mut lifecycle =
            WorkerLifecycleCapability::for_provision(&lock, &mut registry, &key, &mut deadline)
                .unwrap();

        assert!(lifecycle.validate_provision_owner(&key).is_ok());
        assert!(lifecycle
            .validate_provision_owner("7/velnor-7-4294")
            .is_err());
        assert!(lifecycle
            .record_docker_lifecycle_event(
                "7/velnor-7-4294",
                DockerLifecycleEvent::BeforeCreateRequest(Target::Runner),
            )
            .is_err());

        let other_identity = WorkerIdentity::new(OwnershipId::bind(7, "velnor-7-4294"));
        let other_plan = ProvisionPlan {
            identity: other_identity.clone(),
            profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
            state_dir: state_root.join(other_identity.ownership().slug()),
            jit_config: "unused-in-mismatched-owner-test".to_owned(),
            ready_attempts: 1,
        };
        let runner_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut other_runner = CleanupRunner::for_identity(&other_identity, runner_calls.clone());
        let mut other_supervision = Supervision::new(other_identity.clone(), &other_plan.state_dir);
        assert!(other_supervision
            .tick_with_runtime(
                &mut lifecycle,
                &mut other_runner,
                ScaleSetWorkerState::DindReady,
                0,
            )
            .is_err());
        assert!(other_supervision
            .prepare_cleanup(&lifecycle, &mut other_runner)
            .is_err());
        assert!(other_supervision
            .complete_diagnostics_without_runner(&lifecycle, &mut other_runner)
            .is_err());
        assert!(!other_supervision
            .teardown_owned_resources(&lifecycle, &mut other_runner)
            .is_empty());
        assert!(provision_worker(
            &mut lifecycle,
            &mut other_runner,
            &crate::scaleset::worker::DockerToolContentHook,
            &other_plan,
            &std::thread::sleep,
        )
        .is_err());
        assert!(runner_calls.lock().unwrap().is_empty());
        assert_eq!(
            lifecycle
                .registry
                .get(&key)
                .unwrap()
                .unwrap()
                .pending_docker_create,
            None
        );

        lifecycle
            .record_docker_lifecycle_event(
                &key,
                DockerLifecycleEvent::BeforeCreateRequest(Target::Runner),
            )
            .unwrap();
        assert!(
            lifecycle
                .record_docker_lifecycle_event(
                    &key,
                    DockerLifecycleEvent::CreateResolved(Target::Dind),
                )
                .is_err()
        );
        assert_eq!(
            lifecycle
                .registry
                .get(&key)
                .unwrap()
                .unwrap()
                .pending_docker_create,
            Some(Target::Runner)
        );
        lifecycle
            .record_docker_lifecycle_event(
                &key,
                DockerLifecycleEvent::CreateResolved(Target::Runner),
            )
            .unwrap();
        assert_eq!(
            lifecycle
                .registry
                .get(&key)
                .unwrap()
                .unwrap()
                .pending_docker_create,
            None
        );
        drop(lifecycle);
        assert!(deadline.is_some());
        drop(registry);
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn obsolete_docker_create_marker_is_removed_when_clear() {
        let dir = unique_test_dir("clear-obsolete-docker-create-marker");
        let db = dir.join("state.db");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        let runner_name = "velnor-7-4290";
        let key = OwnershipId::bind(7, runner_name).as_str().to_owned();

        let mut registry = WorkerRegistry::open(&db).unwrap();
        seed_registry_worker(
            &mut registry,
            &state_root,
            4290,
            runner_name,
            ScaleSetWorkerState::ProvisionIntent,
        );
        drop(registry);
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "ALTER TABLE scaleset_worker_runtime
                   ADD COLUMN legacy_docker_create_stage_unknown INTEGER NOT NULL DEFAULT 0
                     CHECK (legacy_docker_create_stage_unknown IN (0, 1));",
            )
            .unwrap();

        let registry = WorkerRegistry::open(&db).unwrap();
        assert_eq!(
            registry.get(&key).unwrap().unwrap().worker_state,
            ScaleSetWorkerState::ProvisionIntent
        );
        let columns = runtime_columns(&db);
        assert!(!columns
            .iter()
            .any(|name| name == "legacy_docker_create_stage_unknown"));
        drop(registry);
        let reopened = WorkerRegistry::open(&db).unwrap();
        assert!(!runtime_columns(&db)
            .iter()
            .any(|name| name == "legacy_docker_create_stage_unknown"));
        drop(reopened);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ambiguous_legacy_docker_create_markers_fail_closed_without_mutation() {
        let dir = unique_test_dir("ambiguous-obsolete-docker-create-marker");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        let db = dir.join("legacy-stage.db");
        let runner_name = "velnor-7-4291";
        let key = OwnershipId::bind(7, runner_name).as_str().to_owned();

        let mut registry = WorkerRegistry::open(&db).unwrap();
        seed_registry_worker(
            &mut registry,
            &state_root,
            4291,
            runner_name,
            ScaleSetWorkerState::ProvisionIntent,
        );
        drop(registry);
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "ALTER TABLE scaleset_worker_runtime
                   ADD COLUMN legacy_docker_create_stage_unknown INTEGER NOT NULL DEFAULT 1
                     CHECK (legacy_docker_create_stage_unknown IN (0, 1));",
            )
            .unwrap();
        let columns_before = runtime_columns(&db);

        let error = WorkerRegistry::open(&db).unwrap_err();
        assert!(error.to_string().contains("ambiguous Docker create stage"));
        assert!(error.to_string().contains("repair or reset"));
        assert_eq!(runtime_columns(&db), columns_before);
        let marker: i64 = Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT legacy_docker_create_stage_unknown
                 FROM scaleset_worker_runtime WHERE ownership_id = ?1",
                params![key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            marker, 1,
            "failed migration must leave the ambiguous row intact"
        );

        let runner_marker_db = dir.join("runner-only.db");
        let mut registry = WorkerRegistry::open(&runner_marker_db).unwrap();
        seed_registry_worker(
            &mut registry,
            &state_root,
            4292,
            "velnor-7-4292",
            ScaleSetWorkerState::ProvisionIntent,
        );
        drop(registry);
        Connection::open(&runner_marker_db)
            .unwrap()
            .execute_batch(
                "ALTER TABLE scaleset_worker_runtime
                   ADD COLUMN runner_create_pending INTEGER NOT NULL DEFAULT 1
                     CHECK (runner_create_pending IN (0, 1));",
            )
            .unwrap();
        let columns_before = runtime_columns(&runner_marker_db);
        let error = WorkerRegistry::open(&runner_marker_db).unwrap_err();
        assert!(error
            .to_string()
            .contains("runner-only Docker create marker"));
        assert_eq!(runtime_columns(&runner_marker_db), columns_before);
        let marker: i64 = Connection::open(&runner_marker_db)
            .unwrap()
            .query_row(
                "SELECT runner_create_pending FROM scaleset_worker_runtime WHERE ownership_id = ?1",
                params![OwnershipId::bind(7, "velnor-7-4292").as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            marker, 1,
            "failed migration must leave the old marker intact"
        );

        let no_target_db = dir.join("no-create-intent.db");
        let mut registry = WorkerRegistry::open(&no_target_db).unwrap();
        seed_registry_worker(
            &mut registry,
            &state_root,
            4295,
            "velnor-7-4295",
            ScaleSetWorkerState::ProvisionIntent,
        );
        drop(registry);
        Connection::open(&no_target_db)
            .unwrap()
            .execute_batch("ALTER TABLE scaleset_worker_runtime DROP COLUMN pending_docker_create;")
            .unwrap();
        let columns_before = runtime_columns(&no_target_db);
        let error = WorkerRegistry::open(&no_target_db).unwrap_err();
        assert!(error
            .to_string()
            .contains("predates durable Docker create intent"));
        assert!(error.to_string().contains("repair or reset"));
        assert_eq!(runtime_columns(&no_target_db), columns_before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_replay_rechecks_exact_demand_before_releasing_permit() {
        use velnor_control::permit_ledger::{
            demand_source_roster_path, read_demand_source_roster, AcquireOutcome, PermitLane,
            PermitLedger, PermitState,
        };

        let dir = unique_test_dir("terminal-demand-proof-replay");
        let db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        let request_id = 4294;
        let runner_name = crate::scaleset::runner_name(7, request_id);
        let key = OwnershipId::bind(7, &runner_name).as_str().to_owned();
        let holder = permit_holder(7, request_id);

        let mut demand = DemandStore::open(&db).unwrap();
        demand
            .submit_offer(7, &eligible_offer(request_id), 0)
            .unwrap();
        WorkerRegistry::open(&db).unwrap();
        std::fs::File::create(&ledger_path).unwrap();
        let canonical_ledger = ledger_path.canonicalize().unwrap();
        let canonical_state_db = db.canonicalize().unwrap();
        std::fs::write(
            demand_source_roster_path(&canonical_ledger),
            format!(
                "permit-ledger {}\nstate-db {}\ndemand-db {}\n",
                canonical_ledger.display(),
                canonical_state_db.display(),
                canonical_state_db.display(),
            ),
        )
        .unwrap();
        let mut global = PermitLedger::open(&ledger_path).unwrap();
        global.set_max_jobs(1).unwrap();
        let roster = read_demand_source_roster(&canonical_ledger).unwrap();
        global.configure_demand_source_roster(&roster).unwrap();
        let generation = global.begin_epoch().unwrap();
        global
            .reconcile_host_roster(&roster, generation, &[])
            .unwrap();
        assert_eq!(
            global
                .acquire(
                    &holder,
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        demand
            .set_state(request_id, LocalDemandState::Terminal, None, generation)
            .unwrap();
        drop(demand);
        drop(global);

        let mut registry = WorkerRegistry::open(&db).unwrap();
        seed_registry_worker(
            &mut registry,
            &state_root,
            request_id,
            &runner_name,
            ScaleSetWorkerState::Terminal,
        );
        drop(registry);
        let runner_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::for_identity(
            &WorkerIdentity::new(OwnershipId::bind(7, &runner_name)),
            runner_calls.clone(),
        );
        let mut lane = test_lane(&db, &ledger_path, &state_root, Box::new(runner));
        let terminal_row = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(
            lane.require_terminal_demand_proof(&key, Some(&terminal_row), false)
                .unwrap(),
            request_id
        );

        // Simulate a later intent pass resurrecting the durable demand state.
        // The terminal registry phase cannot stand in for fresh completion
        // proof and the exact permit lease must remain held.
        DemandStore::open(&db)
            .unwrap()
            .set_state(request_id, LocalDemandState::Acquired, None, generation)
            .unwrap();
        let error = lane.drive_terminal_inner(&key, true, false).unwrap_err();
        assert!(error.to_string().contains("exact terminal demand proof"));
        assert_eq!(
            lane.registry.get(&key).unwrap().unwrap().worker_state,
            ScaleSetWorkerState::Terminal
        );
        assert_eq!(lane.ledger.occupied().unwrap(), 1);
        assert!(lane
            .ledger
            .permit_lease_generation(&holder)
            .unwrap()
            .is_some());
        assert!(lane.release_terminal_permit(request_id).is_err());
        assert_eq!(lane.ledger.occupied().unwrap(), 1);
        assert!(runner_calls.lock().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn released_state_dir_cleanup_migration_is_one_time_and_replay_safe() {
        let dir = unique_test_dir("released-state-migration");
        let db = dir.join("state.db");
        let ledger = dir.join("permit-ledger.db");
        let recorded_root = dir.join("recorded-workers");
        let configured_root = dir.join("configured-workers");

        let released_runner = "velnor-7-4248";
        let released_ownership = OwnershipId::bind(7, released_runner);
        let released_identity = WorkerIdentity::new(released_ownership.clone());
        let released_key = released_ownership.as_str().to_owned();
        let released_state_dir = recorded_root.join(released_ownership.slug());
        std::fs::create_dir_all(&released_state_dir).unwrap();
        std::fs::write(released_state_dir.join("raw-job.log"), "legacy state").unwrap();

        let configured_decoy = configured_root.join(released_ownership.slug());
        std::fs::create_dir_all(&configured_decoy).unwrap();
        std::fs::write(configured_decoy.join("keep"), "not the recorded path").unwrap();

        let old_cleanup_runner = "velnor-7-4249";
        let old_cleanup_key = OwnershipId::bind(7, old_cleanup_runner).as_str().to_owned();

        // Build the worker rows, then remove runtime metadata to model the
        // database immediately before the compatibility migration existed.
        {
            let mut registry = WorkerRegistry::open(&db).unwrap();
            assert_eq!(
                seed_registry_worker(
                    &mut registry,
                    &recorded_root,
                    4248,
                    released_runner,
                    ScaleSetWorkerState::PermitReleased,
                ),
                released_key
            );
            assert_eq!(
                seed_registry_worker(
                    &mut registry,
                    &recorded_root,
                    4249,
                    old_cleanup_runner,
                    ScaleSetWorkerState::OwnedCleanup,
                ),
                old_cleanup_key
            );
        }
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "DROP TABLE IF EXISTS scaleset_worker_migrations;
                 DROP TABLE IF EXISTS scaleset_worker_runtime;",
            )
            .unwrap();

        {
            let registry = WorkerRegistry::open(&db).unwrap();
            assert!(state_dir_cleanup_pending(&registry, &released_key));
            let old_cleanup = registry.get(&old_cleanup_key).unwrap().unwrap();
            assert_eq!(old_cleanup.worker_state, ScaleSetWorkerState::OwnedCleanup);
            assert!(old_cleanup.state_dir_cleanup_pending);
        }

        let current_cleanup_runner = "velnor-7-4250";
        let current_released_runner = "velnor-7-4251";
        let current_cleanup_key = OwnershipId::bind(7, current_cleanup_runner)
            .as_str()
            .to_owned();
        let current_released_key = OwnershipId::bind(7, current_released_runner)
            .as_str()
            .to_owned();

        // New lifecycle rows keep their normal phase markers. Reopening must
        // not run the compatibility migration over them.
        {
            let mut registry = WorkerRegistry::open(&db).unwrap();
            assert_eq!(
                seed_registry_worker(
                    &mut registry,
                    &recorded_root,
                    4250,
                    current_cleanup_runner,
                    ScaleSetWorkerState::OwnedCleanup,
                ),
                current_cleanup_key
            );
            assert_eq!(
                seed_registry_worker(
                    &mut registry,
                    &recorded_root,
                    4251,
                    current_released_runner,
                    ScaleSetWorkerState::PermitReleased,
                ),
                current_released_key
            );
        }
        {
            let registry = WorkerRegistry::open(&db).unwrap();
            assert!(state_dir_cleanup_pending(&registry, &released_key));
            assert!(state_dir_cleanup_pending(&registry, &old_cleanup_key));
            assert!(state_dir_cleanup_pending(&registry, &current_cleanup_key));
            assert!(state_dir_cleanup_pending(&registry, &current_released_key));
        }

        // A PermitReleased row can still hold its exact lease while the
        // one-time migration's state-directory cleanup fence is pending.
        {
            let mut ledger = velnor_control::permit_ledger::PermitLedger::open(&ledger).unwrap();
            ledger.set_max_jobs(1).unwrap();
            let generation = ledger.generation().unwrap();
            assert_eq!(
                ledger
                    .acquire(
                        &permit_holder(7, 4248),
                        velnor_control::permit_ledger::PermitLane::ScaleSet,
                        velnor_control::permit_ledger::PermitState::Running,
                        generation,
                        None,
                    )
                    .unwrap(),
                velnor_control::permit_ledger::AcquireOutcome::Acquired
            );
        }

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::missing(released_identity.runner_container(), seen);
        let mut lane = test_lane(&db, &ledger, &configured_root, Box::new(runner));
        let released = lane.registry.get(&released_key).unwrap().unwrap();
        assert_eq!(
            lane.recorded_state_dir(&released).unwrap(),
            released_state_dir
        );
        lane.cleanup_released_state_dir(&released).unwrap();
        assert!(!released_state_dir.exists());
        assert!(configured_decoy.exists());
        assert!(state_dir_cleanup_pending(&lane.registry, &released_key));
        let holder = permit_holder(7, 4248);
        let lease_generation = lane
            .ledger
            .permit_lease_generation(&holder)
            .unwrap()
            .unwrap();
        assert!(lane
            .ledger
            .release_if_generation(&holder, lease_generation)
            .unwrap());
        lane.registry
            .clear_state_dir_cleanup_pending(&released_key)
            .unwrap();

        // A stale snapshot cannot authorize a second deletion after the
        // registry clears its cleanup fence after permit release. Reopening
        // keeps that completed row clear because the migration is one-time.
        assert!(lane.cleanup_released_state_dir(&released).is_err());
        assert!(!state_dir_cleanup_pending(&lane.registry, &released_key));
        lane.registry
            .set_state(&released_key, ScaleSetWorkerState::PermitReleased)
            .unwrap();
        assert!(!state_dir_cleanup_pending(&lane.registry, &released_key));
        drop(lane);

        let registry = WorkerRegistry::open(&db).unwrap();
        assert!(!state_dir_cleanup_pending(&registry, &released_key));
        assert!(state_dir_cleanup_pending(&registry, &old_cleanup_key));
        assert!(state_dir_cleanup_pending(&registry, &current_cleanup_key));
        assert!(state_dir_cleanup_pending(&registry, &current_released_key));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_released_and_owned_cleanup_rows_migrate_and_replay() {
        use velnor_control::permit_ledger::{
            demand_source_roster_path, read_demand_source_roster, AcquireOutcome, PermitLane,
            PermitLedger, PermitState,
        };

        let dir = unique_test_dir("legacy-cleanup-row-replay");
        let db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();

        let released_request = 4252;
        let released_name = crate::scaleset::runner_name(7, released_request);
        let released_identity = WorkerIdentity::new(OwnershipId::bind(7, &released_name));
        let released_key = released_identity.ownership().as_str();
        let released_state_dir = state_root.join(released_identity.ownership().slug());
        std::fs::create_dir_all(&released_state_dir).unwrap();
        std::fs::write(released_state_dir.join("legacy.log"), "old released state").unwrap();

        let owned_request = 4253;
        let owned_name = crate::scaleset::runner_name(7, owned_request);
        let owned_identity = WorkerIdentity::new(OwnershipId::bind(7, &owned_name));
        let owned_key = owned_identity.ownership().as_str();
        let owned_state_dir = state_root.join(owned_identity.ownership().slug());
        std::fs::create_dir_all(&owned_state_dir).unwrap();
        std::fs::write(owned_state_dir.join("legacy.log"), "owned cleanup state").unwrap();

        let mut demand = DemandStore::open(&db).unwrap();
        demand
            .submit_offer(7, &eligible_offer(released_request), 0)
            .unwrap();
        demand
            .submit_offer(7, &eligible_offer(owned_request), 0)
            .unwrap();
        WorkerRegistry::open(&db).unwrap();

        std::fs::File::create(&ledger_path).unwrap();
        let canonical_ledger = ledger_path.canonicalize().unwrap();
        let canonical_state_db = db.canonicalize().unwrap();
        std::fs::write(
            demand_source_roster_path(&canonical_ledger),
            format!(
                "permit-ledger {}\nstate-db {}\ndemand-db {}\n",
                canonical_ledger.display(),
                canonical_state_db.display(),
                canonical_state_db.display(),
            ),
        )
        .unwrap();
        let mut ledger = PermitLedger::open(&ledger_path).unwrap();
        ledger.set_max_jobs(2).unwrap();
        let roster = read_demand_source_roster(&canonical_ledger).unwrap();
        ledger.configure_demand_source_roster(&roster).unwrap();
        let generation = ledger.begin_epoch().unwrap();
        ledger
            .reconcile_host_roster(&roster, generation, &[])
            .unwrap();
        let owned_holder = permit_holder(7, owned_request);
        let released_holder = permit_holder(7, released_request);
        assert_eq!(
            ledger
                .acquire(
                    &released_holder,
                    PermitLane::ScaleSet,
                    PermitState::Running,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            ledger
                .acquire(
                    &owned_holder,
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        demand
            .set_state(
                released_request,
                LocalDemandState::Terminal,
                None,
                generation,
            )
            .unwrap();
        demand
            .set_state(owned_request, LocalDemandState::Terminal, None, generation)
            .unwrap();
        drop(demand);
        drop(ledger);

        let mut registry = WorkerRegistry::open(&db).unwrap();
        seed_registry_worker(
            &mut registry,
            &state_root,
            released_request,
            &released_name,
            ScaleSetWorkerState::PermitReleased,
        );
        seed_registry_worker(
            &mut registry,
            &state_root,
            owned_request,
            &owned_name,
            ScaleSetWorkerState::OwnedCleanup,
        );
        // Model a database where the released-row migration already ran but
        // the one-time OwnedCleanup repair has not yet run.
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "UPDATE scaleset_worker_runtime SET state_dir_cleanup_pending = 0
                   WHERE ownership_id LIKE '%velnor-7-4253';
                 INSERT OR IGNORE INTO scaleset_worker_migrations (migration_id)
                   VALUES ('released_state_dir_cleanup_v2');
                 DELETE FROM scaleset_worker_migrations
                   WHERE migration_id = 'terminal_state_dir_cleanup_v3';",
            )
            .unwrap();
        drop(registry);

        let registry = WorkerRegistry::open(&db).unwrap();
        assert!(state_dir_cleanup_pending(&registry, &released_key));
        assert!(state_dir_cleanup_pending(&registry, &owned_key));
        drop(registry);

        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::missing(owned_identity.runner_container(), calls)
            .with_identity(&owned_identity);
        let mut lane = test_lane(&db, &ledger_path, &state_root, Box::new(runner));
        // Startup recovery discovers both preexisting cleanup rows after
        // their one-time migrations restore the pending fence.
        let report = lane.adopt_live_workers().unwrap();
        assert!(report.resumed_cleanup >= 2);
        assert!(!released_state_dir.exists());
        assert!(!state_dir_cleanup_pending(&lane.registry, &released_key));

        let owned_before = lane.registry.get(&owned_key).unwrap().unwrap();
        assert_eq!(
            owned_before.worker_state,
            ScaleSetWorkerState::PermitReleased
        );
        assert!(!owned_before.state_dir_cleanup_pending);
        assert!(!owned_state_dir.exists());
        lane.drive_terminal(&owned_key).unwrap();
        let owned_after = lane.registry.get(&owned_key).unwrap().unwrap();
        assert_eq!(
            owned_after.worker_state,
            ScaleSetWorkerState::PermitReleased
        );
        assert!(!owned_after.state_dir_cleanup_pending);
        assert!(owned_after.diagnostics_complete);
        assert!(!owned_state_dir.exists());
        assert_eq!(lane.ledger.occupied().unwrap(), 0);
        drop(lane);

        let registry = WorkerRegistry::open(&db).unwrap();
        assert!(!state_dir_cleanup_pending(&registry, &released_key));
        assert!(!state_dir_cleanup_pending(&registry, &owned_key));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn started_replay_with_persisted_provision_intent_defers_to_provision_retry() {
        let dir = unique_test_dir("started-provision-replay");
        let db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        let request_id = 4270;
        let name = crate::scaleset::runner_name(7, request_id);
        let ownership = OwnershipId::bind(7, &name);
        let operation = crate::scaleset::provision_operation_id(7, request_id, 0);
        let mut lane = test_lane(
            &db,
            &ledger_path,
            &state_root,
            Box::new(CleanupRunner::missing(
                name.clone(),
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            )),
        );
        let generation = lane.ledger.generation().unwrap();
        lane.intents
            .record_intent(
                &operation,
                &crate::scaleset::provision_ownership_id(7, &name),
                7,
                request_id,
                &name,
                "sha256:runner",
                "sha256:dind",
                generation,
            )
            .unwrap();
        let key = seed_registry_worker(
            &mut lane.registry,
            &state_root,
            request_id,
            &name,
            ScaleSetWorkerState::ProvisionIntent,
        );
        assert_eq!(key, ownership.as_str());
        assert_eq!(lane.live_workers(), 0);

        lane.note_started(&ScaleSetJobStarted {
            runner_id: 11,
            runner_name: name,
            base: velnor_model::ScaleSetJobMessage {
                runner_request_id: request_id,
                ..Default::default()
            },
        })
        .unwrap();

        assert_eq!(lane.live_workers(), 0);
        assert_eq!(
            lane.registry.get(&key).unwrap().unwrap().worker_state,
            ScaleSetWorkerState::ProvisionIntent
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_provision_intent_replay_captures_dind_before_release_and_state_cleanup() {
        use velnor_control::permit_ledger::{
            demand_source_roster_path, read_demand_source_roster, AcquireOutcome, PermitLane,
            PermitLedger, PermitState,
        };

        let dir = unique_test_dir("terminal-pre-runner-replay");
        let db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();

        let request_id = 4291;
        let runner_name = crate::scaleset::runner_name(7, request_id);
        let ownership = OwnershipId::bind(7, &runner_name);
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str().to_owned();
        let holder = permit_holder(7, request_id);
        let state_dir = state_root.join(ownership.slug());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("jit.env"), "JIT_SECRET=sentinel").unwrap();

        let mut local_demand = DemandStore::open(&db).unwrap();
        local_demand
            .submit_offer(7, &eligible_offer(request_id), 0)
            .unwrap();
        WorkerRegistry::open(&db).unwrap();

        std::fs::File::create(&ledger_path).unwrap();
        let canonical_ledger = ledger_path.canonicalize().unwrap();
        let canonical_state_db = db.canonicalize().unwrap();
        std::fs::write(
            demand_source_roster_path(&canonical_ledger),
            format!(
                "permit-ledger {}\nstate-db {}\ndemand-db {}\n",
                canonical_ledger.display(),
                canonical_state_db.display(),
                canonical_state_db.display(),
            ),
        )
        .unwrap();
        let mut global = PermitLedger::open(&ledger_path).unwrap();
        global.set_max_jobs(1).unwrap();
        let roster = read_demand_source_roster(&canonical_ledger).unwrap();
        global.configure_demand_source_roster(&roster).unwrap();
        let generation = global.begin_epoch().unwrap();
        global
            .reconcile_host_roster(&roster, generation, &[])
            .unwrap();
        assert_eq!(
            global
                .acquire(
                    &holder,
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        local_demand
            .set_state(request_id, LocalDemandState::Terminal, None, generation)
            .unwrap();
        drop(local_demand);
        drop(global);

        let mut registry = WorkerRegistry::open(&db).unwrap();
        assert_eq!(
            seed_registry_worker(
                &mut registry,
                &state_root,
                request_id,
                &runner_name,
                ScaleSetWorkerState::ProvisionIntent,
            ),
            key
        );
        drop(registry);

        // Simulate a crash boundary after the host-only capture marker lands
        // but before the registry records diagnostics_complete.
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_diagnostics_checkpoint
                 BEFORE UPDATE OF diagnostics_complete ON scaleset_worker_runtime
                 WHEN NEW.diagnostics_complete = 1
                 BEGIN SELECT RAISE(ABORT, 'injected checkpoint failure'); END;",
            )
            .unwrap();
        let capture_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        // No create is pending, so terminal recovery can prove the runner is
        // absent and capture the still-owned DinD diagnostics.
        let capture_runner =
            CleanupRunner::runner_absent_for_identity(&identity, capture_calls.clone());
        let mut first_lane = test_lane(&db, &ledger_path, &state_root, Box::new(capture_runner));
        let first_report = first_lane.adopt_live_workers().unwrap();

        assert_eq!(first_report.resumed_cleanup, 1);
        let first_row = first_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(first_row.worker_state, ScaleSetWorkerState::ProvisionIntent);
        assert!(!first_row.diagnostics_complete);
        assert!(Supervision::from_runtime(
            identity.clone(),
            &state_dir,
            HomogeneousProfile::for_arch("x86_64").unwrap(),
            0,
            None,
            None,
        )
        .diagnostics_complete()
        .unwrap());
        assert_eq!(first_lane.ledger.occupied().unwrap(), 1);
        assert!(state_dir.exists());
        drop(first_lane);

        let replay_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let replay_runner =
            CleanupRunner::all_containers_absent_for_identity(&identity, replay_calls.clone());
        let mut lane = test_lane(&db, &ledger_path, &state_root, Box::new(replay_runner));
        let report = lane.adopt_live_workers().unwrap();

        assert_eq!(report.resumed_cleanup, 1);
        let still_pending = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(
            still_pending.worker_state,
            ScaleSetWorkerState::ProvisionIntent
        );
        assert!(!still_pending.diagnostics_complete);
        assert_eq!(lane.ledger.occupied().unwrap(), 1);
        assert!(state_dir.exists());

        // The next idle sweep must retry the durable pre-runner cleanup even
        // without another broker message or process restart.
        Connection::open(&db)
            .unwrap()
            .execute_batch("DROP TRIGGER fail_diagnostics_checkpoint")
            .unwrap();
        lane.opportunistic_sweep();

        let released = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(released.worker_state, ScaleSetWorkerState::PermitReleased);
        assert!(released.diagnostics_complete);
        assert!(!released.state_dir_cleanup_pending);
        assert_eq!(lane.ledger.occupied().unwrap(), 0);
        assert!(!state_dir.exists());
        let basename = state_dir.file_name().unwrap().to_string_lossy();
        assert!(!state_root.join(format!(".velnor-jit-{basename}")).exists());
        assert!(!state_root
            .join(format!(".velnor-diagnostics-{basename}"))
            .exists());

        let identity_dind = "dind-container-id";
        let calls = capture_calls.lock().unwrap();
        assert!(calls.iter().any(|args| {
            args.first().is_some_and(|arg| arg == "logs")
                && args.iter().any(|arg| arg == identity_dind)
        }));
        assert!(!calls.iter().any(|args| {
            args.first().is_some_and(|arg| arg == "logs")
                && args.iter().any(|arg| arg == &identity.runner_container())
        }));
        let replay_calls = replay_calls.lock().unwrap();
        assert!(replay_calls
            .iter()
            .all(|args| !args.iter().any(|arg| arg == "logs")));
        for expected in [identity.runner_container(), identity.dind_container()] {
            assert!(replay_calls.iter().any(|args| {
                args.first().is_some_and(|arg| arg == "inspect")
                    && args.iter().any(|arg| arg == &expected)
            }));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_provision_intent_delayed_runner_create_is_fenced_and_replayed() {
        use velnor_control::permit_ledger::{
            demand_source_roster_path, read_demand_source_roster, AcquireOutcome, PermitLane,
            PermitLedger, PermitState,
        };

        let dir = unique_test_dir("terminal-provision-intent-runner-present");
        let db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        let request_id = 4292;
        let runner_name = crate::scaleset::runner_name(7, request_id);
        let ownership = OwnershipId::bind(7, &runner_name);
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str().to_owned();
        let holder = permit_holder(7, request_id);
        let state_dir = state_root.join(ownership.slug());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("jit.env"), "JIT_SECRET=sentinel").unwrap();

        let mut local_demand = DemandStore::open(&db).unwrap();
        local_demand
            .submit_offer(7, &eligible_offer(request_id), 0)
            .unwrap();
        WorkerRegistry::open(&db).unwrap();

        std::fs::File::create(&ledger_path).unwrap();
        let canonical_ledger = ledger_path.canonicalize().unwrap();
        let canonical_state_db = db.canonicalize().unwrap();
        std::fs::write(
            demand_source_roster_path(&canonical_ledger),
            format!(
                "permit-ledger {}\nstate-db {}\ndemand-db {}\n",
                canonical_ledger.display(),
                canonical_state_db.display(),
                canonical_state_db.display(),
            ),
        )
        .unwrap();
        let mut global = PermitLedger::open(&ledger_path).unwrap();
        global.set_max_jobs(1).unwrap();
        let roster = read_demand_source_roster(&canonical_ledger).unwrap();
        global.configure_demand_source_roster(&roster).unwrap();
        let generation = global.begin_epoch().unwrap();
        global
            .reconcile_host_roster(&roster, generation, &[])
            .unwrap();
        assert_eq!(
            global
                .acquire(
                    &holder,
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        local_demand
            .set_state(request_id, LocalDemandState::Terminal, None, generation)
            .unwrap();
        drop(local_demand);
        drop(global);

        let mut registry = WorkerRegistry::open(&db).unwrap();
        seed_registry_worker(
            &mut registry,
            &state_root,
            request_id,
            &runner_name,
            ScaleSetWorkerState::ProvisionIntent,
        );
        // This is the crash window where a no-runner completion was
        // persisted while a Docker create could still finish later.
        let cleanup_lock =
            crate::scaleset::reconcile::lock_worker_docker_lifecycle(&ledger_path, &key).unwrap();
        let lifecycle =
            WorkerLifecycleCapability::for_cleanup(&cleanup_lock, &mut registry, &key).unwrap();
        let absent_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut absent_runner = CleanupRunner::runner_absent_for_identity(&identity, absent_calls);
        let no_runner_export = Supervision::new(identity.clone(), &state_dir)
            .complete_diagnostics_without_runner(&lifecycle, &mut absent_runner)
            .unwrap();
        assert!(no_runner_export.failures.is_empty());
        drop(lifecycle);
        drop(cleanup_lock);
        registry.set_diagnostics_complete(&key).unwrap();
        registry
            .begin_docker_create(
                &key,
                crate::scaleset::worker::runner::DockerCreateTarget::Runner,
                1_000,
            )
            .unwrap();
        drop(registry);

        // Model Engine accepting POST /containers/create, the first locked
        // inspect still seeing absence, then the container appearing after
        // that CLI/lane attempt is gone.
        let runner_live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let absent_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let first_runner = CleanupRunner::dynamically_present(
            &identity,
            absent_calls.clone(),
            runner_live.clone(),
        );
        let mut first_lane = test_lane(&db, &ledger_path, &state_root, Box::new(first_runner));
        let first_report = first_lane.adopt_live_workers().unwrap();
        assert_eq!(first_report.resumed_cleanup, 1);
        let first_row = first_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(first_row.worker_state, ScaleSetWorkerState::ProvisionIntent);
        assert_eq!(
            first_row.pending_docker_create,
            Some(crate::scaleset::worker::runner::DockerCreateTarget::Runner)
        );
        assert!(!first_row.diagnostics_complete);
        assert_eq!(first_lane.ledger.occupied().unwrap(), 1);
        assert!(state_dir.exists());
        assert!(
            absent_calls
                .lock()
                .unwrap()
                .iter()
                .all(|args| args.first().is_none_or(|arg| arg != "logs")),
            "ambiguous create must not be waived as runner absent"
        );
        drop(first_lane);

        runner_live.store(true, std::sync::atomic::Ordering::SeqCst);
        // A later inspect sees the accepted create. Full diagnostics then
        // fail once; durable phase must already have left ProvisionIntent so
        // a disappearing runner cannot use the no-runner waiver.
        let failed_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut failed_runner =
            CleanupRunner::dynamically_present(&identity, failed_calls, runner_live.clone());
        failed_runner.fail_runner_logs = true;
        let mut failed_lane = test_lane(&db, &ledger_path, &state_root, Box::new(failed_runner));
        let failed_report = failed_lane.adopt_live_workers().unwrap();
        assert_eq!(failed_report.resumed_cleanup, 1);
        let retained = failed_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(retained.worker_state, ScaleSetWorkerState::DiagnosticExport);
        assert_eq!(retained.pending_docker_create, None);
        assert!(!retained.diagnostics_complete);
        assert_eq!(
            Supervision::from_runtime(
                identity.clone(),
                &state_dir,
                HomogeneousProfile::for_arch("x86_64").unwrap(),
                0,
                None,
                None,
            )
            .diagnostic_completion_kind()
            .unwrap(),
            None
        );
        assert_eq!(failed_lane.ledger.occupied().unwrap(), 1);
        assert!(state_dir.exists());
        drop(failed_lane);

        // Runner disappearance plus another artifact failure must stay on
        // the persisted full-capture phase; it cannot publish RunnerAbsent.
        let mut disappeared_runner = CleanupRunner::runner_absent_for_identity(
            &identity,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        );
        disappeared_runner.fail_dind_logs = true;
        let disappeared_calls = disappeared_runner.seen.clone();
        let mut disappeared_lane =
            test_lane(&db, &ledger_path, &state_root, Box::new(disappeared_runner));
        disappeared_lane.adopt_live_workers().unwrap();
        let disappeared = disappeared_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(
            disappeared.worker_state,
            ScaleSetWorkerState::DiagnosticExport,
            "Docker calls: {:?}",
            disappeared_calls.lock().unwrap()
        );
        assert!(!disappeared.diagnostics_complete);
        assert_eq!(
            Supervision::from_runtime(
                identity.clone(),
                &state_dir,
                HomogeneousProfile::for_arch("x86_64").unwrap(),
                0,
                None,
                None,
            )
            .diagnostic_completion_kind()
            .unwrap(),
            None
        );
        assert_eq!(disappeared_lane.ledger.occupied().unwrap(), 1);
        drop(disappeared_lane);

        runner_live.store(true, std::sync::atomic::Ordering::SeqCst);
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::dynamically_present(&identity, calls.clone(), runner_live);
        let mut lane = test_lane(&db, &ledger_path, &state_root, Box::new(runner));
        lane.adopt_live_workers().unwrap();

        let released = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(released.worker_state, ScaleSetWorkerState::PermitReleased);
        assert!(released.diagnostics_complete);
        assert!(!released.state_dir_cleanup_pending);
        assert_eq!(lane.ledger.occupied().unwrap(), 0);
        assert!(!state_dir.exists());
        let calls = calls.lock().unwrap();
        for (container_name, container_id) in [
            (identity.runner_container(), "runner-container-id"),
            (identity.dind_container(), "dind-container-id"),
        ] {
            assert!(calls.iter().any(|args| {
                args.first().is_some_and(|arg| arg == "logs")
                    && args.iter().any(|arg| arg == container_id)
            }));
            assert!(calls.iter().any(|args| {
                args.iter().any(|arg| arg == "inspect")
                    && args.iter().any(|arg| arg == &container_name)
            }));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn no_runner_probe_and_capture_errors_retain_terminal_cleanup_for_retry() {
        use velnor_control::permit_ledger::{
            demand_source_roster_path, read_demand_source_roster, AcquireOutcome, DemandState,
            PermitLane, PermitLedger, PermitState,
        };

        let dir = unique_test_dir("no-runner-inspect-error");
        let db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        let request_id = 4293;
        let runner_name = crate::scaleset::runner_name(7, request_id);
        let ownership = OwnershipId::bind(7, &runner_name);
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str().to_owned();
        let holder = permit_holder(7, request_id);
        let state_dir = state_root.join(ownership.slug());
        std::fs::create_dir_all(&state_dir).unwrap();

        let mut local_demand = DemandStore::open(&db).unwrap();
        local_demand
            .submit_offer(7, &eligible_offer(request_id), 0)
            .unwrap();
        WorkerRegistry::open(&db).unwrap();

        std::fs::File::create(&ledger_path).unwrap();
        let canonical_ledger = ledger_path.canonicalize().unwrap();
        let canonical_state_db = db.canonicalize().unwrap();
        std::fs::write(
            demand_source_roster_path(&canonical_ledger),
            format!(
                "permit-ledger {}\nstate-db {}\ndemand-db {}\n",
                canonical_ledger.display(),
                canonical_state_db.display(),
                canonical_state_db.display(),
            ),
        )
        .unwrap();
        let mut global = PermitLedger::open(&ledger_path).unwrap();
        global.set_max_jobs(1).unwrap();
        let roster = read_demand_source_roster(&canonical_ledger).unwrap();
        global.configure_demand_source_roster(&roster).unwrap();
        let generation = global.begin_epoch().unwrap();
        global
            .reconcile_host_roster(&roster, generation, &[])
            .unwrap();
        assert_eq!(
            global
                .acquire(
                    &holder,
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        local_demand
            .set_state(request_id, LocalDemandState::Terminal, None, generation)
            .unwrap();
        drop(local_demand);
        drop(global);

        let mut registry = WorkerRegistry::open(&db).unwrap();
        seed_registry_worker(
            &mut registry,
            &state_root,
            request_id,
            &runner_name,
            ScaleSetWorkerState::ProvisionIntent,
        );
        drop(registry);

        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::fail_no_runner_cleanup_for_identity(&identity, calls);
        let mut lane = test_lane(&db, &ledger_path, &state_root, Box::new(runner));
        assert!(lane.drive_terminal(&key).is_err());
        let retained = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(retained.worker_state, ScaleSetWorkerState::ProvisionIntent);
        assert!(!retained.diagnostics_complete);
        assert!(state_dir.exists());
        assert_eq!(lane.ledger.occupied().unwrap(), 1);
        let demand = PermitLedger::open(&ledger_path)
            .unwrap()
            .demand(&holder)
            .unwrap()
            .unwrap();
        assert_eq!(demand.state, DemandState::Terminal);
        assert_eq!(
            PermitLedger::open(&ledger_path)
                .unwrap()
                .holder_state(&holder)
                .unwrap(),
            Some(PermitState::Uncertain)
        );

        // A later exact absence result reaches diagnostics, where an I/O
        // failure must keep the same cleanup intent and permit fenced.
        assert!(lane.drive_terminal(&key).is_err());
        let capture_failed = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(
            capture_failed.worker_state,
            ScaleSetWorkerState::ProvisionIntent
        );
        assert!(!capture_failed.diagnostics_complete);
        assert_eq!(lane.ledger.occupied().unwrap(), 1);
        let replayed_demand = PermitLedger::open(&ledger_path)
            .unwrap()
            .demand(&holder)
            .unwrap()
            .unwrap();
        assert_eq!(replayed_demand.state, DemandState::Terminal);
        assert_eq!(
            PermitLedger::open(&ledger_path)
                .unwrap()
                .holder_state(&holder)
                .unwrap(),
            Some(PermitState::Uncertain)
        );

        // Neither failed inspect nor capture lost the completion replay.
        // A successful retry captures DinD evidence and releases state.
        lane.drive_terminal(&key).unwrap();
        let released = lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(released.worker_state, ScaleSetWorkerState::PermitReleased);
        assert!(!released.state_dir_cleanup_pending);
        assert_eq!(lane.ledger.occupied().unwrap(), 0);
        assert!(!state_dir.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn worker_health_failure_retains_permit_until_completion_oracle() {
        use velnor_control::permit_ledger::{DemandState, PermitLane, PermitLedger, PermitState};

        let dir = unique_test_dir("health-failure-retains-permit");
        let db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        std::fs::create_dir_all(&state_root).unwrap();
        let request_id = 4280;
        let runner_name = crate::scaleset::runner_name(7, request_id);
        let identity = WorkerIdentity::new(OwnershipId::bind(7, &runner_name));
        let key = identity.ownership().as_str().to_owned();
        let holder = permit_holder(7, request_id);
        std::fs::create_dir_all(
            state_root
                .join(identity.ownership().slug())
                .join("buildkit-cache"),
        )
        .unwrap();
        let mut ledger = PermitLedger::open(&ledger_path).unwrap();
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();
        assert_eq!(
            ledger
                .acquire(
                    &holder,
                    PermitLane::ScaleSet,
                    PermitState::Running,
                    generation,
                    None,
                )
                .unwrap(),
            velnor_control::permit_ledger::AcquireOutcome::Acquired
        );
        let lease_generation = ledger.permit_lease_generation(&holder).unwrap().unwrap();
        drop(ledger);

        let mut demand = DemandStore::open(&db).unwrap();
        demand
            .submit_offer(7, &eligible_offer(request_id), generation)
            .unwrap();
        demand
            .set_state_with_permit_lease(
                request_id,
                LocalDemandState::Acquired,
                None,
                generation,
                lease_generation,
            )
            .unwrap();
        drop(demand);

        let state_dir = state_root.join(identity.ownership().slug());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = RetryObservationRunner {
            identity: identity.clone(),
            profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
            state_dir,
            dind_running: true,
            runner_running: false,
            dind_ready: true,
            session_established: true,
            seen,
        };
        let mut lane = test_lane(&db, &ledger_path, &state_root, Box::new(runner));
        seed_registry_worker(
            &mut lane.registry,
            &state_root,
            request_id,
            &runner_name,
            ScaleSetWorkerState::Running,
        );

        assert!(matches!(
            lane.tick_worker(&key).unwrap(),
            SupervisionOutcome::WorkerFailed { .. }
        ));
        let ledger = PermitLedger::open(&ledger_path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.holder_state(&holder).unwrap(),
            Some(PermitState::Running)
        );
        assert_eq!(
            ledger.demand(&holder).unwrap().unwrap().state,
            DemandState::Granted
        );
        assert_eq!(
            lane.registry.get(&key).unwrap().unwrap().worker_state,
            ScaleSetWorkerState::Running
        );
        assert!(lane.workers.contains_key(&key));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_backfills_persisted_eligible_demand_before_native_admission() {
        use velnor_control::permit_ledger::{
            demand_source_roster_path, read_demand_source_roster, DemandState as GlobalDemandState,
            PermitLedger,
        };

        let dir = unique_test_dir("eligible-demand-backfill");
        let state_db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let relative_state_root = PathBuf::from(format!(
            ".velnor-lane-relative-root-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let expected_state_root = std::env::current_dir().unwrap().join(&relative_state_root);
        let request_id = 4260;
        let scaleset_holder = permit_holder(7, request_id);
        let first_seen_at = velnor_model::Timestamp::now()
            .minus(Duration::from_secs(30))
            .to_rfc3339()
            .unwrap();
        let first_seen_instant = velnor_model::Timestamp::parse(&first_seen_at)
            .unwrap()
            .as_offset_datetime();
        let first_seen_unix = u64::try_from(first_seen_instant.unix_timestamp()).unwrap();
        let first_seen_subsec_nanos = first_seen_instant.nanosecond();

        DemandStore::open(&state_db).unwrap();
        std::fs::File::create(&ledger_path).unwrap();
        let canonical_ledger = ledger_path.canonicalize().unwrap();
        let canonical_state_db = state_db.canonicalize().unwrap();
        std::fs::write(
            demand_source_roster_path(&canonical_ledger),
            format!(
                "permit-ledger {}\nstate-db {}\ndemand-db {}\n",
                canonical_ledger.display(),
                canonical_state_db.display(),
                canonical_state_db.display(),
            ),
        )
        .unwrap();

        let generation = {
            let mut ledger = PermitLedger::open(&ledger_path).unwrap();
            ledger.set_max_jobs(1).unwrap();
            let roster = read_demand_source_roster(&canonical_ledger).unwrap();
            ledger.configure_demand_source_roster(&roster).unwrap();
            let generation = ledger.begin_epoch().unwrap();
            ledger
                .reconcile_host_roster(&roster, generation, &[])
                .unwrap();
            assert!(ledger.demand(&scaleset_holder).unwrap().is_none());
            generation
        };

        {
            let mut demand = DemandStore::open(&state_db).unwrap();
            demand
                .submit_offer(7, &eligible_offer(request_id), 0)
                .unwrap();
        }
        Connection::open(&state_db)
            .unwrap()
            .execute(
                "UPDATE scaleset_demand SET first_seen_at = ?1 WHERE request_id = ?2",
                rusqlite::params![first_seen_at, request_id],
            )
            .unwrap();

        let open_lane = || {
            let client = ScaleSetClient::new_with_pat(
                "http://127.0.0.1/octo-org",
                "test-token",
                crate::scaleset::client::SystemInfo::default(),
                crate::scaleset::backoff::RetryPolicy::default(),
            )
            .unwrap();
            DaemonWorkerLane::open(
                client,
                LaneConfig {
                    scale_set_id: 7,
                    profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
                    state_root: relative_state_root.clone(),
                    ready_attempts: 1,
                    sweep_interval: Duration::ZERO,
                },
                &state_db,
                &ledger_path,
                Box::new(CleanupRunner::missing(
                    "unused-worker".to_owned(),
                    std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                )),
                Box::new(crate::scaleset::worker::DockerToolContentHook),
            )
            .unwrap()
        };

        let lane = open_lane();
        assert_eq!(lane.config.state_root, expected_state_root);
        let mirrored = PermitLedger::open(&ledger_path)
            .unwrap()
            .demand(&scaleset_holder)
            .unwrap()
            .unwrap();
        assert_eq!(mirrored.first_seen_unix, first_seen_unix);
        assert_eq!(mirrored.first_seen_subsec_nanos, first_seen_subsec_nanos);
        assert_eq!(
            mirrored.lane,
            velnor_control::permit_ledger::PermitLane::ScaleSet
        );
        assert_eq!(mirrored.state, GlobalDemandState::Eligible);
        let original_sequence = mirrored.sequence;
        drop(lane);

        // Startup replay observes the same holder again without changing its
        // original age or global queue position.
        let mut lane = open_lane();
        let replayed = PermitLedger::open(&ledger_path)
            .unwrap()
            .demand(&scaleset_holder)
            .unwrap()
            .unwrap();
        assert_eq!(replayed.first_seen_unix, first_seen_unix);
        assert_eq!(replayed.first_seen_subsec_nanos, first_seen_subsec_nanos);
        assert_eq!(replayed.sequence, original_sequence);

        let native_holder =
            crate::permit_guard::native_permit_holder("native/octo-org/velnor", "later");
        let native = crate::permit_guard::NativePermitGuard::acquire(
            &ledger_path,
            native_holder.clone(),
            "native/octo-org/velnor",
        )
        .unwrap();
        assert!(native.is_none());
        let native_demand = PermitLedger::open(&ledger_path)
            .unwrap()
            .demand(&native_holder)
            .unwrap()
            .unwrap();
        assert!(replayed.first_seen_unix < native_demand.first_seen_unix);
        assert!(replayed.sequence < native_demand.sequence);

        let (outcome, lease_generation) = lane
            .ledger
            .acquire_with_lease_generation(
                &scaleset_holder,
                crate::scaleset::capacity::LedgerLane::ScaleSet,
                LedgerPermitState::Reserved,
                generation,
            )
            .unwrap();
        let raw = PermitLedger::open(&ledger_path).unwrap();
        let ledger_state = format!(
            "generation={:?} reconciled={:?} max={:?} ready={:?} occupied={:?} holders={:?} scale_demand={:?} native_demand={:?}",
            raw.generation(),
            raw.reconciled(),
            raw.max_jobs(),
            raw.demand_sources_ready(),
            raw.occupied(),
            raw.holders(),
            raw.demand(&scaleset_holder),
            raw.demand(&native_holder),
        );
        assert_eq!(
            outcome,
            crate::scaleset::capacity::AcquireOutcome::Acquired,
            "{ledger_state}"
        );
        assert!(lane
            .ledger
            .release_if_generation(&scaleset_holder, lease_generation.unwrap())
            .unwrap());
        drop(lane);
        std::fs::remove_dir_all(&expected_state_root).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_backfill_sorts_rfc3339_instants_and_preserves_fractional_age() {
        use velnor_control::permit_ledger::{
            demand_source_roster_path, read_demand_source_roster, PermitLedger,
        };

        let dir = unique_test_dir("eligible-demand-fractional-order");
        let state_db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let older_request_id = 4270;
        let younger_request_id = 4271;
        let older_holder = permit_holder(7, older_request_id);
        let younger_holder = permit_holder(7, younger_request_id);
        // Same UTC second, but the local SQL lexical order puts the younger
        // instant first because it uses Z instead of a +01:00 offset.
        let older_first_seen = "2026-09-18T01:00:00.100+01:00";
        let younger_first_seen = "2026-09-18T00:00:00.900Z";
        let older_instant = velnor_model::Timestamp::parse(older_first_seen)
            .unwrap()
            .as_offset_datetime();
        let younger_instant = velnor_model::Timestamp::parse(younger_first_seen)
            .unwrap()
            .as_offset_datetime();
        assert_eq!(
            older_instant.unix_timestamp(),
            younger_instant.unix_timestamp()
        );

        DemandStore::open(&state_db).unwrap();
        std::fs::File::create(&ledger_path).unwrap();
        let canonical_ledger = ledger_path.canonicalize().unwrap();
        let canonical_state_db = state_db.canonicalize().unwrap();
        std::fs::write(
            demand_source_roster_path(&canonical_ledger),
            format!(
                "permit-ledger {}\nstate-db {}\ndemand-db {}\n",
                canonical_ledger.display(),
                canonical_state_db.display(),
                canonical_state_db.display(),
            ),
        )
        .unwrap();

        let generation = {
            let mut ledger = PermitLedger::open(&ledger_path).unwrap();
            ledger.set_max_jobs(1).unwrap();
            let roster = read_demand_source_roster(&canonical_ledger).unwrap();
            ledger.configure_demand_source_roster(&roster).unwrap();
            let generation = ledger.begin_epoch().unwrap();
            ledger
                .reconcile_host_roster(&roster, generation, &[])
                .unwrap();
            assert!(ledger.demand(&older_holder).unwrap().is_none());
            assert!(ledger.demand(&younger_holder).unwrap().is_none());
            generation
        };

        {
            let mut demand = DemandStore::open(&state_db).unwrap();
            demand
                .submit_offer(7, &eligible_offer(older_request_id), 0)
                .unwrap();
            demand
                .submit_offer(7, &eligible_offer(younger_request_id), 0)
                .unwrap();
        }
        Connection::open(&state_db)
            .unwrap()
            .execute(
                "UPDATE scaleset_demand SET first_seen_at = ?1 WHERE request_id = ?2",
                rusqlite::params![older_first_seen, older_request_id],
            )
            .unwrap();
        Connection::open(&state_db)
            .unwrap()
            .execute(
                "UPDATE scaleset_demand SET first_seen_at = ?1 WHERE request_id = ?2",
                rusqlite::params![younger_first_seen, younger_request_id],
            )
            .unwrap();

        let client = ScaleSetClient::new_with_pat(
            "http://127.0.0.1/octo-org",
            "test-token",
            crate::scaleset::client::SystemInfo::default(),
            crate::scaleset::backoff::RetryPolicy::default(),
        )
        .unwrap();
        let mut lane = DaemonWorkerLane::open(
            client,
            LaneConfig {
                scale_set_id: 7,
                profile: HomogeneousProfile::for_arch("x86_64").unwrap(),
                state_root,
                ready_attempts: 1,
                sweep_interval: Duration::ZERO,
            },
            &state_db,
            &ledger_path,
            Box::new(CleanupRunner::missing(
                "unused-worker".to_owned(),
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            )),
            Box::new(crate::scaleset::worker::DockerToolContentHook),
        )
        .unwrap();

        let global = PermitLedger::open(&ledger_path).unwrap();
        let older = global.demand(&older_holder).unwrap().unwrap();
        let younger = global.demand(&younger_holder).unwrap().unwrap();
        assert_eq!(older.first_seen_unix, younger.first_seen_unix);
        assert_eq!(older.first_seen_subsec_nanos, 100_000_000);
        assert_eq!(younger.first_seen_subsec_nanos, 900_000_000);
        assert!(older.sequence < younger.sequence);
        drop(global);

        let native_holder =
            crate::permit_guard::native_permit_holder("native/octo-org/velnor", "fractional-later");
        let native = crate::permit_guard::NativePermitGuard::acquire(
            &ledger_path,
            native_holder.clone(),
            "native/octo-org/velnor",
        )
        .unwrap();
        assert!(native.is_none());
        let (outcome, lease_generation) = lane
            .ledger
            .acquire_with_lease_generation(
                &older_holder,
                crate::scaleset::capacity::LedgerLane::ScaleSet,
                LedgerPermitState::Reserved,
                generation,
            )
            .unwrap();
        let mut raw = PermitLedger::open(&ledger_path).unwrap();
        let ledger_state = format!(
            "generation={:?} reconciled={:?} max={:?} ready={:?} occupied={:?} holders={:?} older_demand={:?} younger_demand={:?} native_demand={:?}",
            raw.generation(),
            raw.reconciled(),
            raw.max_jobs(),
            raw.demand_sources_ready(),
            raw.occupied(),
            raw.holders(),
            raw.demand(&older_holder),
            raw.demand(&younger_holder),
            raw.demand(&native_holder),
        );
        drop(raw);
        assert_eq!(
            outcome,
            crate::scaleset::capacity::AcquireOutcome::Acquired,
            "{ledger_state}"
        );
        assert!(lane
            .ledger
            .release_if_generation(&older_holder, lease_generation.unwrap())
            .unwrap());
        drop(lane);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn released_slot_stays_closed_until_state_cleanup_finishes_after_restart() {
        use std::os::unix::fs::symlink;
        use velnor_control::permit_ledger::{
            demand_source_roster_path, read_demand_source_roster, unix_now_parts, AcquireOutcome,
            PermitLane, PermitLedger, PermitState,
        };

        let dir = unique_test_dir("released-state-admission-gate");
        let db = dir.join("state.db");
        let ledger_path = dir.join("permit-ledger.db");
        let state_root = dir.join("workers");
        let request_id = 4280;
        let holder = permit_holder(7, request_id);
        let runner_name = format!("velnor-7-{request_id}");
        let ownership = OwnershipId::bind(7, &runner_name);
        let identity = WorkerIdentity::new(ownership.clone());
        let key = ownership.as_str().to_owned();
        let state_dir = state_root.join(ownership.slug());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("raw-job.log"), "old generation diagnostics").unwrap();
        let completion_marker = write_test_diagnostics_marker(&identity, &state_dir);
        std::fs::create_dir_all(&db.parent().unwrap()).unwrap();
        let mut local_demand = DemandStore::open(&db).unwrap();
        local_demand
            .submit_offer(7, &eligible_offer(request_id), 0)
            .unwrap();
        WorkerRegistry::open(&db).unwrap();

        std::fs::File::create(&ledger_path).unwrap();
        let canonical_ledger = ledger_path.canonicalize().unwrap();
        let canonical_state_db = db.canonicalize().unwrap();
        let roster_path = demand_source_roster_path(&canonical_ledger);
        std::fs::write(
            &roster_path,
            format!(
                "permit-ledger {}\nstate-db {}\ndemand-db {}\n",
                canonical_ledger.display(),
                canonical_state_db.display(),
                canonical_state_db.display(),
            ),
        )
        .unwrap();
        let mut global = PermitLedger::open(&ledger_path).unwrap();
        global.set_max_jobs(2).unwrap();
        let roster = read_demand_source_roster(&canonical_ledger).unwrap();
        global.configure_demand_source_roster(&roster).unwrap();
        let generation = global.begin_epoch().unwrap();
        global
            .reconcile_host_roster(&roster, generation, &[])
            .unwrap();
        let (first_seen, first_seen_subsec_nanos) = unix_now_parts();
        global
            .observe_demand_with_subsecond(
                &holder,
                PermitLane::ScaleSet,
                "scaleset/7",
                first_seen,
                first_seen_subsec_nanos,
                first_seen,
            )
            .unwrap();
        assert_eq!(
            global
                .acquire(
                    &holder,
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        local_demand
            .set_state(request_id, LocalDemandState::Terminal, None, generation)
            .unwrap();
        drop(local_demand);
        drop(global);

        let mut registry = WorkerRegistry::open(&db).unwrap();
        registry
            .upsert(
                &key,
                "op-cleanup-gate",
                request_id,
                &runner_name,
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

        let saved_state_dir = dir.join("saved-old-generation-state");
        let victim_dir = dir.join("state-path-victim");
        std::fs::create_dir_all(&victim_dir).unwrap();
        std::fs::write(victim_dir.join("keep"), "must not follow symlink").unwrap();
        let action_state_dir = state_dir.clone();
        let action_saved_state_dir = saved_state_dir.clone();
        let action_victim_dir = victim_dir.clone();
        let first_seen_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = CleanupRunner::after_remove_for_identity(
            &identity,
            first_seen_calls.clone(),
            move || {
                std::fs::rename(&action_state_dir, &action_saved_state_dir).unwrap();
                symlink(&action_victim_dir, &action_state_dir).unwrap();
            },
        );
        let mut first_lane = test_lane(&db, &ledger_path, &state_root, Box::new(runner));
        assert!(first_lane.drive_terminal(&key).is_err());
        assert!(first_seen_calls.lock().unwrap().iter().any(|args| {
            args.first().is_some_and(|arg| arg == "rm")
                && args.iter().any(|arg| arg == "runner-container-id")
        }));
        let released = first_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(released.worker_state, ScaleSetWorkerState::PermitReleased);
        assert!(released.state_dir_cleanup_pending);
        assert!(std::fs::symlink_metadata(&state_dir)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(saved_state_dir.join("raw-job.log").exists());
        assert!(completion_marker.exists());
        assert_eq!(
            std::fs::read_to_string(victim_dir.join("keep")).unwrap(),
            "must not follow symlink"
        );
        assert_eq!(first_lane.ledger.occupied().unwrap(), 1);
        drop(first_lane);

        // The lease remains held while the recorded state path is unsafe to
        // delete. The source cleanup fence also keeps unrelated Native work
        // closed until startup repair succeeds.
        let mut blocked = PermitLedger::open(&ledger_path).unwrap();
        assert_eq!(blocked.advertised_free().unwrap(), None);
        assert_eq!(
            blocked
                .acquire(
                    "native/cleanup-gate-probe",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(std::process::id()),
                )
                .unwrap(),
            AcquireOutcome::NotReady
        );
        drop(blocked);

        // Startup must fail before session creation while durable cleanup
        // remains pending. The shared ledger independently keeps peer
        // acquisitions closed during that failed startup.
        let failed_replay_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut failed_replay_lane = test_lane(
            &db,
            &ledger_path,
            &state_root,
            Box::new(
                CleanupRunner::missing(identity.runner_container(), failed_replay_calls)
                    .with_identity(&identity),
            ),
        );
        assert!(failed_replay_lane.adopt_live_workers().is_err());
        assert!(state_dir_cleanup_pending(
            &failed_replay_lane.registry,
            &key
        ));
        assert_eq!(failed_replay_lane.ledger.occupied().unwrap(), 1);
        drop(failed_replay_lane);

        // Restart repair removes the symlink without touching its target,
        // restores the recorded tree, then retries durable cleanup. Only
        // after files are gone and pending=0 does admission reopen.
        std::fs::remove_file(&state_dir).unwrap();
        std::fs::rename(&saved_state_dir, &state_dir).unwrap();
        let replay_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut replay_lane = test_lane(
            &db,
            &ledger_path,
            &state_root,
            Box::new(
                CleanupRunner::missing(identity.runner_container(), replay_calls)
                    .with_identity(&identity),
            ),
        );
        let report = replay_lane.adopt_live_workers().unwrap();
        assert!(report.resumed_cleanup >= 1);
        let released = replay_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(released.worker_state, ScaleSetWorkerState::PermitReleased);
        assert!(!released.state_dir_cleanup_pending);
        assert!(!state_dir.exists());
        assert!(!completion_marker.exists());
        assert_eq!(replay_lane.ledger.occupied().unwrap(), 0);
        drop(replay_lane);

        let mut reopened = PermitLedger::open(&ledger_path).unwrap();
        assert_eq!(reopened.advertised_free().unwrap(), Some(2));
        assert_eq!(
            reopened
                .acquire(
                    "native/cleanup-gate-probe",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(std::process::id()),
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        let probe_lease = reopened
            .permit_lease_generation("native/cleanup-gate-probe")
            .unwrap()
            .unwrap();
        assert!(reopened
            .release_if_generation("native/cleanup-gate-probe", probe_lease)
            .unwrap());
        drop(reopened);
        assert_eq!(
            std::fs::read_to_string(victim_dir.join("keep")).unwrap(),
            "must not follow symlink"
        );
        assert!(first_seen_calls
            .lock()
            .unwrap()
            .iter()
            .any(|args| args.iter().any(|arg| arg == "rm")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn owned_cleanup_replay_removes_state_before_releasing_permit() {
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
        let completion_marker = write_test_diagnostics_marker(&identity, &state_dir);

        let holder = permit_holder(7, 4244);
        let mut local_demand = DemandStore::open(&db).unwrap();
        local_demand
            .submit_offer(7, &eligible_offer(4244), 0)
            .unwrap();
        {
            let mut global = velnor_control::permit_ledger::PermitLedger::open(&ledger).unwrap();
            global.set_max_jobs(1).unwrap();
            let generation = global.generation().unwrap();
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
            local_demand
                .set_state(4244, LocalDemandState::Terminal, None, generation)
                .unwrap();
            local_demand
                .record_permit_lease_generation(
                    4244,
                    global.permit_lease_generation(&holder).unwrap().unwrap(),
                )
                .unwrap();
        }
        drop(local_demand);

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
            CleanupRunner::fail_runner_remove_for_identity(&identity, first_seen.clone());
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
        let first_calls = first_seen.lock().unwrap();
        assert!(first_calls
            .iter()
            .all(|args| !args.iter().any(|arg| arg == "logs")));
        assert!(first_calls
            .iter()
            .any(|args| { args.iter().any(|arg| arg.contains(".Config.Labels")) }));
        assert!(first_calls.iter().any(|args| {
            args.first().is_some_and(|arg| arg == "rm")
                && args.iter().any(|arg| arg == "runner-container-id")
        }));
        assert!(completion_marker.exists());
        drop(first_lane);

        // The host-only completion marker stays outside the writable worker
        // tree and survives restart until authorized cleanup removes it.

        // Inject failure after secure state deletion but before permit release.
        // The row and pending gate must preserve replay evidence, and the
        // still-held permit must keep occupancy closed.
        Connection::open(&ledger)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_permit_release BEFORE DELETE ON permits
                 BEGIN SELECT RAISE(ABORT, 'injected permit release failure'); END;",
            )
            .unwrap();
        let release_failure_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let release_failure_runner =
            CleanupRunner::missing(identity.runner_container(), release_failure_seen.clone())
                .with_identity(&identity);
        let mut release_failure_lane =
            test_lane(&db, &ledger, &state_root, Box::new(release_failure_runner));
        assert!(release_failure_lane.drive_terminal(&key).is_err());
        let row = release_failure_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::PermitReleased);
        assert!(row.state_dir_cleanup_pending);
        assert!(!state_dir.exists());
        assert!(!completion_marker.exists());
        assert_eq!(release_failure_lane.ledger.occupied().unwrap(), 1);
        drop(release_failure_lane);
        Connection::open(&ledger)
            .unwrap()
            .execute_batch("DROP TRIGGER fail_permit_release;")
            .unwrap();

        // A corrupted host-only marker must not strand a PermitReleased row:
        // the durable registry authorization still permits no-follow cleanup,
        // and the symlink target must remain untouched.
        let external_marker_target = dir.join("external-marker-target");
        std::fs::write(&external_marker_target, "keep outside cleanup").unwrap();
        std::fs::create_dir_all(completion_marker.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&external_marker_target, &completion_marker).unwrap();

        // Startup retries the idempotent deletion, then releases the permit
        // and clears pending. No Docker work is needed after PermitReleased.
        let replay_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let replay_runner =
            CleanupRunner::missing(identity.runner_container(), replay_seen.clone())
                .with_identity(&identity);
        let mut replay_lane = test_lane(&db, &ledger, &state_root, Box::new(replay_runner));
        let report = replay_lane.adopt_live_workers().unwrap();
        assert!(report.resumed_cleanup >= 1);
        let row = replay_lane.registry.get(&key).unwrap().unwrap();
        assert_eq!(row.worker_state, ScaleSetWorkerState::PermitReleased);
        assert!(row.diagnostics_complete);
        assert!(!row.state_dir_cleanup_pending);
        assert!(!state_dir.exists());
        assert!(!completion_marker.exists());
        assert_eq!(
            std::fs::read_to_string(&external_marker_target).unwrap(),
            "keep outside cleanup"
        );
        assert_eq!(replay_lane.ledger.occupied().unwrap(), 0);
        assert!(replay_seen.lock().unwrap().is_empty());

        // A duplicate terminal observation is idempotent and performs no
        // Docker work after the durable release checkpoint.
        replay_lane.drive_terminal(&key).unwrap();
        assert!(replay_seen.lock().unwrap().is_empty());
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
        velnor_control::permit_ledger::PermitLedger::open(&ledger)
            .unwrap()
            .set_max_jobs(1)
            .unwrap();

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
        let runner = CleanupRunner::for_identity(&identity, seen.clone());
        let mut lane = test_lane(&db, &ledger, &state_root, Box::new(runner));
        let generation = lane.ledger.generation().unwrap();
        let holder = permit_holder(7, 4245);
        let mut demand = DemandStore::open(&db).unwrap();
        demand
            .submit_offer(7, &eligible_offer(4245), generation)
            .unwrap();
        lane.ledger
            .observe_demand(
                &holder,
                crate::scaleset::capacity::LedgerLane::ScaleSet,
                "scaleset/7",
                1,
                2,
            )
            .unwrap();
        let (outcome, lease_generation) = lane
            .ledger
            .acquire_with_lease_generation(
                &holder,
                crate::scaleset::capacity::LedgerLane::ScaleSet,
                LedgerPermitState::Acquiring,
                generation,
            )
            .unwrap();
        assert_eq!(outcome, crate::scaleset::capacity::AcquireOutcome::Acquired);
        demand
            .set_state_with_permit_lease(
                4245,
                LocalDemandState::Terminal,
                None,
                generation,
                lease_generation.unwrap(),
            )
            .unwrap();
        drop(demand);
        lane.drive_terminal(&key).unwrap();

        let calls = seen.lock().unwrap();
        let diagnostic_calls = calls
            .iter()
            .filter(|args| {
                args.first().is_some_and(|arg| arg == "logs")
                    || (args.first().is_some_and(|arg| arg == "inspect")
                        && args.get(1).is_some_and(|arg| arg == "--"))
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

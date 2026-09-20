//! Runner+DinD supervision: death detection, restart-vs-fail policy,
//! diagnostic export, and owned cleanup.
//!
//! Reconciliation at every boundary: each tick observes the live pair
//! ([`observe_pair`]) and compares against the recorded worker state
//! before deciding. The supervisor never assumes the recorded state is
//! still true — a container that died since the last tick is observed
//! dead, and the decision follows the observation.
//!
//! Restart-vs-fail policy:
//! * DinD down while the worker is live → restart the daemon (idempotent
//!   `start`), within a per-worker [`RestartBudget`]. DinD death loses no
//!   job outcome: the runner reconnects to the same socket path.
//! * Budget exhausted → the worker fails to `terminal` (provision defect).
//! * Runner down while recorded `running`/`runner_connected` → the worker
//!   goes to `terminal` WITHOUT restarting the runner. A dead runner may
//!   have finished its job; only the GitHub `JobCompleted` oracle (or its
//!   absence at reconcile) decides the outcome. Restarting would risk
//!   double-execution.
//! * Runner `starting` past its deadline → fail to `terminal` (the JIT
//!   blob or image is defective; redelivery re-offers the work).
//!
//! The lane stages owned cleanup under its lifecycle capability: stop runner
//! → export diagnostics → remove runner → stop DinD → remove DinD → remove
//! network → remove volumes. Diagnostics are exported BEFORE any deletion;
//! any failure in export or removal keeps the permit held for replay instead
//! of releasing fictitious capacity.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(unix)]
use std::{ffi::OsString, fs::File};

use anyhow::{Context, Result};

use super::dind::{DindSpec, DIND_READY_OPERATION_TIMEOUT, DIND_RESTART_READY_TIMEOUT};
use super::ownership::{WorkerIdentity, ROLE_DIND, ROLE_RUNNER};
use super::runner::{runner_connection_by_id, HomogeneousProfile, RunnerConnection, RunnerSpec};
use super::WorkerRunner;

/// How many DinD restarts one worker tolerates before failing.
pub const MAX_DIND_RESTARTS: u32 = 3;
/// JIT runner startup deadline, persisted as an absolute epoch time.
pub const RUNNER_START_TIMEOUT: Duration = Duration::from_secs(120);

/// Per-worker DinD restart budget, rebuilt from durable worker runtime and
/// persisted before restart side effects.
#[derive(Debug, Clone)]
pub struct RestartBudget {
    used: u32,
    max: u32,
}

impl RestartBudget {
    #[must_use]
    pub fn new(max: u32) -> Self {
        Self { used: 0, max }
    }

    /// Rebuild a persisted budget; corrupt values cannot grant extra tries.
    #[must_use]
    pub fn from_used(max: u32, used: u32) -> Self {
        Self {
            used: used.min(max),
            max,
        }
    }

    /// Record a count that was persisted before the corresponding command.
    pub fn record_used(&mut self, used: u32) {
        self.used = self.used.max(used.min(self.max));
    }

    /// Spend one restart. `false` means the budget is exhausted.
    pub fn spend(&mut self) -> bool {
        if self.used >= self.max {
            return false;
        }
        self.used += 1;
        true
    }

    #[must_use]
    pub fn used(&self) -> u32 {
        self.used
    }
}

/// Live observation of one worker pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedPair {
    pub dind_running: bool,
    pub dind_ready: bool,
    pub runner: RunnerConnection,
}

/// Observe the live pair: DinD running state + runner connectivity.
pub(crate) fn observe_pair(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
    profile: &HomogeneousProfile,
) -> Result<ObservedPair> {
    let network_id = super::dind::inspect_owned_network(runner, identity)
        .context("verify worker network ownership before supervision")?;
    if let Some(network_id) = network_id.as_deref() {
        super::dind::validate_network_spec(runner, identity, network_id)?;
    }
    let dind_spec = DindSpec::new(identity.clone(), profile.dind().clone(), state_dir);
    let dind_id = super::dind::inspect_owned_container(runner, identity, ROLE_DIND)
        .context("verify DinD ownership before supervision")?;
    let dind_running = if let Some(dind_id) = dind_id.as_deref() {
        let network_id = network_id
            .as_deref()
            .context("owned DinD exists without its expected worker network")?;
        if !super::dind::validate_dind_runtime(runner, &dind_spec, dind_id, network_id)? {
            anyhow::bail!("DinD disappeared during structural inspection");
        }
        let running = runner
            .run_timeout(
                "docker",
                &crate::docker::client::running_args(dind_id),
                DIND_READY_OPERATION_TIMEOUT,
            )
            .with_context(|| format!("inspect DinD container {dind_id}"))?;
        if running.code != 0 {
            if crate::docker::client::daemon_reports_missing(&running.stderr) {
                false
            } else {
                anyhow::bail!(
                    "inspect DinD container {dind_id} exited {}: {}",
                    running.code,
                    running.stderr.trim()
                );
            }
        } else {
            running.stdout.trim() == "true"
        }
    } else {
        false
    };
    let dind_ready = if dind_running {
        let Some(dind_id) = dind_id.as_deref() else {
            unreachable!("running DinD requires an inspected immutable ID")
        };
        super::dind::dind_ready_with_timeout(
            runner,
            &dind_spec,
            dind_id,
            DIND_READY_OPERATION_TIMEOUT,
        )
        .unwrap_or(false)
    } else {
        false
    };
    let runner_id = super::dind::inspect_owned_container(runner, identity, ROLE_RUNNER)
        .context("verify runner ownership before supervision")?;
    let connection = match runner_id.as_deref() {
        Some(runner_id) => {
            if let Some(dind_id) = dind_id.as_deref() {
                let runner_spec =
                    RunnerSpec::new(identity.clone(), profile.runner().clone(), state_dir, "");
                if !super::dind::validate_runner_runtime(
                    runner,
                    identity,
                    runner_spec.image(),
                    runner_spec.state_dir(),
                    runner_id,
                    dind_id,
                    identity.ownership().runner_name(),
                    None,
                )? {
                    anyhow::bail!("runner disappeared during structural inspection");
                }
                runner_connection_by_id(runner, runner_id)?
            } else {
                RunnerConnection::Down
            }
        }
        None => RunnerConnection::Down,
    };
    Ok(ObservedPair {
        dind_running,
        dind_ready,
        runner: connection,
    })
}

/// What one supervision tick decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisionOutcome {
    /// Live state matches recorded state; nothing to do.
    Healthy,
    /// DinD was down and got restarted (budget spent).
    DindRestarted { restarts_used: u32 },
    /// A restart was durably admitted, but dockerd has not yet answered its
    /// private socket probe. The lane must retain the worker and poll again.
    DindRestartPending {
        restarts_used: u32,
        ready_deadline_epoch: u64,
    },
    /// A formerly starting runner reached the connected marker.
    RunnerConnected,
    /// Startup expired before a successful runner session existed. This is
    /// distinct from post-session health failure so the lane uses its
    /// authoritative cancellation/completion settlement path.
    ProvisioningFailedBeforeSession { reason: String },
    /// The worker must move to `terminal`: restart would be wrong.
    WorkerFailed { reason: String },
}

/// Decide one tick from the observation + recorded state.
///
/// `recorded` is the worker's recorded lifecycle state; the tick
/// reconciles it against `observed` and either repairs (DinD restart)
/// or fails the worker toward `terminal`. Pure decision + the DinD
/// restart effect; the caller performs the lifecycle edge.
#[cfg(test)]
fn supervise_tick(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    recorded: velnor_model::ScaleSetWorkerState,
    observed: &ObservedPair,
    restarts: &mut RestartBudget,
) -> Result<SupervisionOutcome> {
    let profile = HomogeneousProfile::for_arch("x86_64")
        .context("load pinned x86_64 worker profile for test supervisor")?;
    let dind_spec = DindSpec::new(identity.clone(), profile.dind().clone(), Path::new("/tmp"));
    supervise_tick_with_runtime(
        runner,
        identity,
        &dind_spec,
        recorded,
        observed,
        restarts,
        None,
        None,
        0,
        &mut |_, _| Ok(()),
    )
}

/// Runtime-aware supervision. `persist_restarts` commits a restart count
/// before the corresponding Docker `start`, so process death cannot restore
/// the consumed budget.
#[allow(
    clippy::too_many_arguments,
    reason = "single runtime decision boundary"
)]
fn supervise_tick_with_runtime(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    dind_spec: &DindSpec,
    recorded: velnor_model::ScaleSetWorkerState,
    observed: &ObservedPair,
    restarts: &mut RestartBudget,
    runner_start_deadline_epoch: Option<u64>,
    dind_restart_ready_deadline_epoch: Option<u64>,
    now_epoch: u64,
    persist_dind_runtime: &mut dyn FnMut(u32, Option<u64>) -> Result<()>,
) -> Result<SupervisionOutcome> {
    use velnor_model::ScaleSetWorkerState as S;
    // Terminal-side states are owned by the cleanup path, not the tick.
    if matches!(
        recorded,
        S::Terminal | S::DiagnosticExport | S::OwnedCleanup | S::PermitReleased
    ) {
        return Ok(SupervisionOutcome::Healthy);
    }
    let not_yet_connected = !matches!(recorded, S::RunnerConnected | S::Running);
    let session_established = observed.runner == RunnerConnection::Connected;
    if not_yet_connected && session_established && observed.dind_ready {
        return Ok(SupervisionOutcome::RunnerConnected);
    }
    if not_yet_connected
        && !session_established
        && runner_start_deadline_epoch.is_some_and(|deadline| now_epoch >= deadline)
    {
        return Ok(SupervisionOutcome::ProvisioningFailedBeforeSession {
            reason: format!(
                "runner container {} did not establish a session before its startup deadline",
                identity.runner_container()
            ),
        });
    }
    // Runner death decides first: a dead runner is never restarted.
    if matches!(recorded, S::RunnerConnected | S::Running)
        && observed.runner == RunnerConnection::Down
    {
        return Ok(SupervisionOutcome::WorkerFailed {
            reason: format!(
                "runner container {} is down while recorded {}; job outcome comes from the GitHub oracle, not a restart",
                identity.runner_container(),
                recorded.as_str()
            ),
        });
    }
    let pending_deadline = dind_restart_ready_deadline_epoch;
    if observed.dind_ready {
        if pending_deadline.is_some() {
            persist_dind_runtime(restarts.used(), None)?;
            return Ok(SupervisionOutcome::DindRestarted {
                restarts_used: restarts.used(),
            });
        }
        return Ok(SupervisionOutcome::Healthy);
    }

    if pending_deadline.is_some_and(|deadline| now_epoch >= deadline) {
        let reason = format!(
            "DinD container {} did not answer its private socket before the restart readiness deadline",
            identity.dind_container()
        );
        return Ok(if not_yet_connected && !session_established {
            SupervisionOutcome::ProvisioningFailedBeforeSession { reason }
        } else {
            SupervisionOutcome::WorkerFailed { reason }
        });
    }

    let pending = pending_deadline.is_some();
    let next_restart = if pending {
        restarts.used()
    } else {
        restarts.used().saturating_add(1)
    };
    if next_restart > MAX_DIND_RESTARTS || next_restart > restarts.max {
        let reason = format!(
            "DinD container {} is not ready and the restart budget ({}) is exhausted",
            identity.dind_container(),
            restarts.used()
        );
        return Ok(if not_yet_connected && !session_established {
            SupervisionOutcome::ProvisioningFailedBeforeSession { reason }
        } else {
            SupervisionOutcome::WorkerFailed { reason }
        });
    }

    let Some(dind_id) = super::dind::inspect_owned_container(runner, identity, ROLE_DIND)
        .context("verify DinD ownership before restart")?
    else {
        let reason = format!(
            "DinD container {} is absent and cannot be restarted",
            identity.dind_container()
        );
        return Ok(if not_yet_connected && !session_established {
            SupervisionOutcome::ProvisioningFailedBeforeSession { reason }
        } else {
            SupervisionOutcome::WorkerFailed { reason }
        });
    };
    let ready_deadline_epoch = pending_deadline
        .unwrap_or_else(|| now_epoch.saturating_add(DIND_RESTART_READY_TIMEOUT.as_secs()));
    if !pending {
        persist_dind_runtime(next_restart, Some(ready_deadline_epoch))?;
        restarts.record_used(next_restart);
    }

    // A pending, still-running container may merely be waiting for dockerd
    // to finish booting. Do not restart it repeatedly; probe again until the
    // durable total deadline expires. A stopped container can safely retry
    // its idempotent start request.
    if !pending || !observed.dind_running {
        let mut args = if observed.dind_running {
            vec![
                "restart".to_string(),
                "--time".to_string(),
                "10".to_string(),
            ]
        } else {
            vec!["start".to_string()]
        };
        args.push("--".to_string());
        args.push(dind_id.clone());
        let started = runner.run_timeout("docker", &args, DIND_READY_OPERATION_TIMEOUT);
        if started.is_ok_and(|started| started.code == 0)
            && super::dind::dind_ready_with_timeout(
                runner,
                dind_spec,
                &dind_id,
                DIND_READY_OPERATION_TIMEOUT,
            )
            .unwrap_or(false)
        {
            persist_dind_runtime(restarts.used(), None)?;
            return Ok(SupervisionOutcome::DindRestarted {
                restarts_used: restarts.used(),
            });
        }
    }
    Ok(SupervisionOutcome::DindRestartPending {
        restarts_used: restarts.used(),
        ready_deadline_epoch,
    })
}

/// Wall-clock seconds used for durable startup deadlines.
#[must_use]
pub fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// Exported diagnostics: log + inspect files for both containers.
///
/// `failures` contains stop, capture, and artifact-publication failures (for
/// example, a container that vanished mid-export or an I/O error). A
/// non-empty list keeps cleanup uncertain so the evidence gap is visible.
#[derive(Debug, Clone)]
pub struct DiagnosticExport {
    pub dir: PathBuf,
    pub runner_log: PathBuf,
    pub dind_log: PathBuf,
    pub runner_inspect: PathBuf,
    pub dind_inspect: PathBuf,
    pub failures: Vec<String>,
}

/// The durable capture proof kind. Lifecycle recovery must not use a
/// runner-absent capture as proof that a full runner capture completed, or
/// vice versa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiagnosticCompletionKind {
    FullCapture,
    RunnerAbsent,
}

const DIAGNOSTICS_COMPLETE_MARKER: &[u8] = b"velnor-diagnostics-v1\n";
const NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER: &[u8] = b"velnor-diagnostics-v1-runner-absent\n";
const DIAGNOSTIC_ARTIFACT_COMPLETE_PREFIX: &str = "velnor-diagnostic-artifact-v2:";

/// Capture logs + inspect of both containers into a host-only sibling of
/// `state_dir`, outside the writable bind mounted into runner and DinD.
///
/// Runs AFTER the runner stops (logs are complete) and BEFORE any
/// deletion. Every capture is attempted even when an earlier one fails;
/// failures are collected, not raised, so one missing container cannot
/// hide the surviving container's evidence.
///
/// Successful per-file captures are durable and reused on retry. A later
/// capture failure cannot erase earlier evidence. The completion marker is
/// written only after all required files exist and sync successfully.
///
/// Secrecy: inspect output is redacted before it touches disk (labels,
/// `State`, and `NetworkSettings` only — `Config.Env`, which carries the
/// JIT blob, is dropped), and every file is owner-only (`0600`, dir
/// `0700`). Logs stay byte-identical — this layer owns no mask registry
/// — so the host-only diagnostics sibling and shared state dir are deleted
/// once the permit releases (see [`Supervision::release_state`]).
#[cfg(test)]
fn export_diagnostics(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
) -> Result<DiagnosticExport> {
    let runner_container = identity.runner_container();
    let dind_container = identity.dind_container();
    export_diagnostics_inner(
        runner,
        identity,
        state_dir,
        Some(&runner_container),
        Some(&dind_container),
        false,
        true,
    )
}

fn export_diagnostics_inner(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
    runner_container: Option<&str>,
    dind_container: Option<&str>,
    recapture_existing: bool,
    write_completion_marker: bool,
) -> Result<DiagnosticExport> {
    let dir = SecureDiagnosticDir::open(state_dir)?;
    let dir_path = dir.path.clone();
    let runner_log = dir.file_path("runner.log");
    let dind_log = dir.file_path("dind.log");
    let runner_inspect = dir.file_path("runner.inspect.json");
    let dind_inspect = dir.file_path("dind.inspect.json");
    match dir.read_file("capture.complete")? {
        Some(contents) if contents == DIAGNOSTICS_COMPLETE_MARKER && !recapture_existing => {
            dir.sync()
                .context("persist existing diagnostics completion marker")?;
            return Ok(DiagnosticExport {
                dir: dir_path,
                runner_log,
                dind_log,
                runner_inspect,
                dind_inspect,
                failures: Vec::new(),
            });
        }
        Some(contents) if contents == DIAGNOSTICS_COMPLETE_MARKER => {
            // `prepare_cleanup` calls this only after attempting to stop the
            // runner. Invalidate an earlier capture before replacing artifacts:
            // a failed stop must not leave a marker that recovery can trust, and
            // a restarted container must not reuse logs captured before restart.
            dir.remove_file("capture.complete")
                .context("invalidate diagnostics marker before recapture")?;
            dir.sync()
                .context("persist diagnostics marker removal before recapture")?;
        }
        Some(contents)
            if contents == NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER && !recapture_existing =>
        {
            anyhow::bail!("runner-absent diagnostic marker cannot certify a full runner capture");
        }
        Some(contents) if contents == NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER => {
            // A runner may appear after no-runner recovery observed absence.
            // `prepare_cleanup` has attempted to stop it before entering this
            // recapture path, so discard the old proof and recapture all files.
            // A fresh full marker is written only if that stop and every
            // capture succeeded.
            dir.remove_file("capture.complete")
                .context("invalidate runner-absent marker before full recapture")?;
            dir.sync()
                .context("persist runner-absent marker removal before recapture")?;
        }
        Some(_) => anyhow::bail!(
            "invalid diagnostic completion marker {}",
            dir.file_path("capture.complete").display()
        ),
        None => {}
    }

    let mut failures = Vec::new();

    for (container, container_name, name, capture) in [
        (
            runner_container,
            identity.runner_container(),
            "runner.log",
            true,
        ),
        (dind_container, identity.dind_container(), "dind.log", true),
        (
            runner_container,
            identity.runner_container(),
            "runner.inspect.json",
            false,
        ),
        (
            dind_container,
            identity.dind_container(),
            "dind.inspect.json",
            false,
        ),
    ] {
        if recapture_existing || !dir.owner_only_file_exists(name)? {
            match (container, capture) {
                (Some(container), true) => {
                    capture_logs(runner, container, &dir, name, &mut failures);
                }
                (Some(container), false) => {
                    capture_inspect(runner, container, &dir, name, &mut failures);
                }
                (None, _) => failures.push(format!(
                    "worker container {container_name} was absent before diagnostics capture"
                )),
            }
        }
    }

    if write_completion_marker
        && failures.is_empty()
        && let Err(error) = dir.write_file("capture.complete", DIAGNOSTICS_COMPLETE_MARKER)
    {
        failures.push(format!(
            "write {}: {error}",
            dir.file_path("capture.complete").display()
        ));
    }

    Ok(DiagnosticExport {
        dir: dir_path,
        runner_log,
        dind_log,
        runner_inspect,
        dind_inspect,
        failures,
    })
}

fn diagnostic_export(dir: &SecureDiagnosticDir, failures: Vec<String>) -> DiagnosticExport {
    DiagnosticExport {
        dir: dir.path.clone(),
        runner_log: dir.file_path("runner.log"),
        dind_log: dir.file_path("dind.log"),
        runner_inspect: dir.file_path("runner.inspect.json"),
        dind_inspect: dir.file_path("dind.inspect.json"),
        failures,
    }
}

fn capture_logs(
    runner: &mut dyn WorkerRunner,
    container: &str,
    dir: &SecureDiagnosticDir,
    name: &str,
    failures: &mut Vec<String>,
) {
    let logs = runner.run(
        "docker",
        &["logs".to_string(), "--".to_string(), container.to_string()],
    );
    match logs {
        Ok(output) if output.code == 0 => {
            // `docker logs` splits streams; both are evidence.
            let combined = format!("{}{}", output.stdout, output.stderr);
            if let Err(error) = dir.write_file(name, combined.as_bytes()) {
                failures.push(format!("write {}: {error}", dir.file_path(name).display()));
            }
        }
        Ok(output) => failures.push(format!(
            "logs {container} exited {}: {}",
            output.code,
            output.stderr.trim()
        )),
        Err(error) => failures.push(format!("logs {container}: {error:#}")),
    }
}

fn capture_inspect(
    runner: &mut dyn WorkerRunner,
    container: &str,
    dir: &SecureDiagnosticDir,
    name: &str,
    failures: &mut Vec<String>,
) {
    let inspect = runner.run(
        "docker",
        &[
            "inspect".to_string(),
            "--".to_string(),
            container.to_string(),
        ],
    );
    match inspect {
        Ok(output) if output.code == 0 => match redact_inspect(&output.stdout) {
            Some(redacted) => {
                if let Err(error) = dir.write_file(name, redacted.as_bytes()) {
                    failures.push(format!("write {}: {error}", dir.file_path(name).display()));
                }
            }
            // Fail closed: unparseable inspect output is withheld, never
            // persisted raw — raw bytes may carry `Config.Env`.
            None => failures.push(format!(
                "inspect {container}: output withheld (unparseable; refusing to persist unredacted bytes)"
            )),
        },
        Ok(output) => failures.push(format!(
            "inspect {container} exited {}: {}",
            output.code,
            output.stderr.trim()
        )),
        Err(error) => failures.push(format!("inspect {container}: {error:#}")),
    }
}

/// Redact a `docker inspect` array before it touches disk: keep the
/// container identity, labels, `State`, and `NetworkSettings`; drop
/// everything else, notably `Config.Env` (which carries the JIT blob).
/// `None` means the output is not an inspect array — withheld, never
/// persisted raw.
fn redact_inspect(raw: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    let mut redacted = Vec::new();
    for object in parsed.as_array()? {
        let map = object.as_object()?;
        let mut kept = serde_json::Map::new();
        for key in ["Id", "Name", "State", "NetworkSettings"] {
            if let Some(value) = map.get(key) {
                let safe = if key == "State" {
                    let mut state = value.clone();
                    if let Some(health) = state.get_mut("Health").and_then(|v| v.as_object_mut()) {
                        // Healthcheck output is container-controlled and can
                        // echo process environment, including the one-shot
                        // JIT secret. Preserve health status/metadata only.
                        health.remove("Log");
                    }
                    state
                } else {
                    value.clone()
                };
                kept.insert(key.to_string(), safe);
            }
        }
        if let Some(labels) = map.get("Config").and_then(|config| config.get("Labels")) {
            kept.insert(
                "Config".to_string(),
                serde_json::json!({ "Labels": labels }),
            );
        }
        redacted.push(serde_json::Value::Object(kept));
    }
    serde_json::to_string_pretty(&redacted).ok()
}

struct SecureDiagnosticDir {
    path: PathBuf,
    #[cfg(unix)]
    directory: File,
}

impl SecureDiagnosticDir {
    fn open(state_dir: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            let parent_path = state_dir.parent().with_context(|| {
                format!("worker state dir has no parent: {}", state_dir.display())
            })?;
            let parent =
                super::secure_fs::open_absolute_directory(parent_path).with_context(|| {
                    format!("open host worker-state parent {}", parent_path.display())
                })?;
            super::secure_fs::verify_host_parent(&parent).with_context(|| {
                format!("verify host worker-state parent {}", parent_path.display())
            })?;
            let name = diagnostics_dir_name(state_dir)?;
            let directory = super::secure_fs::open_or_create_private_directory_at(&parent, &name)
                .with_context(|| {
                format!(
                    "open host-only diagnostics dir {}",
                    parent_path.join(&name).display()
                )
            })?;
            Ok(Self {
                path: parent_path.join(name),
                directory,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = state_dir;
            anyhow::bail!("secure host-only worker diagnostics require Unix dirfd support")
        }
    }

    fn open_existing(state_dir: &Path) -> Result<Option<Self>> {
        #[cfg(unix)]
        {
            let parent_path = state_dir.parent().with_context(|| {
                format!("worker state dir has no parent: {}", state_dir.display())
            })?;
            let parent =
                super::secure_fs::open_absolute_directory(parent_path).with_context(|| {
                    format!("open host worker-state parent {}", parent_path.display())
                })?;
            super::secure_fs::verify_host_parent(&parent).with_context(|| {
                format!("verify host worker-state parent {}", parent_path.display())
            })?;
            let name = diagnostics_dir_name(state_dir)?;
            let directory = match super::secure_fs::open_private_directory_at(&parent, &name) {
                Ok(directory) => directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "open existing host-only diagnostics dir {}",
                            parent_path.join(&name).display()
                        )
                    });
                }
            };
            Ok(Some(Self {
                path: parent_path.join(name),
                directory,
            }))
        }
        #[cfg(not(unix))]
        {
            let _ = state_dir;
            anyhow::bail!("secure host-only worker diagnostics require Unix dirfd support")
        }
    }

    fn file_path(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    fn write_file(&self, name: &str, contents: &[u8]) -> io::Result<()> {
        #[cfg(unix)]
        {
            super::secure_fs::write_file_at(
                &self.directory,
                std::ffi::OsStr::new(name),
                contents,
                0o600,
            )
        }
        #[cfg(not(unix))]
        {
            let _ = (name, contents);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "secure host-only worker diagnostics require Unix dirfd support",
            ))
        }
    }

    fn read_file(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        #[cfg(unix)]
        {
            super::secure_fs::read_file_at(&self.directory, std::ffi::OsStr::new(name))
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "secure host-only worker diagnostics require Unix dirfd support",
            ))
        }
    }

    fn owner_only_file_exists(&self, name: &str) -> io::Result<bool> {
        #[cfg(unix)]
        {
            super::secure_fs::owner_only_regular_file_exists_at(
                &self.directory,
                std::ffi::OsStr::new(name),
            )
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "secure host-only worker diagnostics require Unix dirfd support",
            ))
        }
    }

    fn owner_only_file_digest(&self, name: &str) -> io::Result<Option<[u8; 32]>> {
        #[cfg(unix)]
        {
            super::secure_fs::blake3_digest_owner_only_file_at(
                &self.directory,
                std::ffi::OsStr::new(name),
            )
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "secure host-only worker diagnostics require Unix dirfd support",
            ))
        }
    }

    fn remove_file(&self, name: &str) -> io::Result<()> {
        #[cfg(unix)]
        {
            super::secure_fs::remove_file_at(&self.directory, std::ffi::OsStr::new(name))
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "secure host-only worker diagnostics require Unix dirfd support",
            ))
        }
    }

    fn artifact_capture_complete(&self, name: &str) -> Result<bool> {
        let marker_name = artifact_marker_name(name);
        let Some(marker) = self.read_file(&marker_name)? else {
            return Ok(false);
        };
        let Some(digest) = self.owner_only_file_digest(name)? else {
            return Ok(false);
        };
        Ok(marker == artifact_marker_contents(name, &digest))
    }

    fn mark_artifact_complete(&self, name: &str) -> Result<()> {
        let marker_name = artifact_marker_name(name);
        let digest = self
            .owner_only_file_digest(name)?
            .with_context(|| format!("diagnostic artifact {name} is not present"))?;
        self.write_file(&marker_name, &artifact_marker_contents(name, &digest))
            .with_context(|| format!("persist completion proof for diagnostic {name}"))
    }

    /// Accept a previously atomically published final if its receipt is
    /// missing or stale. This directory is host-only, and all final artifacts
    /// are published by `write_file_at` only after the complete temporary file
    /// and its directory entry are synced. The content-bound receipt repairs
    /// the crash window without deleting useful evidence on retry.
    fn ensure_artifact_complete(&self, name: &str) -> Result<bool> {
        if self.artifact_capture_complete(name)? {
            return Ok(true);
        }
        if !self.owner_only_file_exists(name)? {
            return Ok(false);
        }
        self.mark_artifact_complete(name)?;
        Ok(true)
    }

    fn clear_artifact(&self, name: &str) -> Result<()> {
        self.remove_file(name)
            .with_context(|| format!("remove incomplete diagnostic {name}"))?;
        let marker_name = artifact_marker_name(name);
        self.remove_file(&marker_name)
            .with_context(|| format!("remove diagnostic completion proof {marker_name}"))
    }

    fn sync(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            super::secure_fs::sync_directory(&self.directory)
        }
        #[cfg(not(unix))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "secure host-only worker diagnostics require Unix dirfd support",
            ))
        }
    }
}

fn artifact_marker_name(name: &str) -> String {
    format!("{name}.complete")
}

fn artifact_marker_contents(name: &str, digest: &[u8; 32]) -> Vec<u8> {
    format!(
        "{DIAGNOSTIC_ARTIFACT_COMPLETE_PREFIX}{name}:{}\n",
        blake3::Hash::from_bytes(*digest).to_hex()
    )
    .into_bytes()
}

#[cfg(unix)]
fn diagnostics_dir_name(state_dir: &Path) -> Result<OsString> {
    let base = state_dir.file_name().with_context(|| {
        format!(
            "worker state dir has no final component: {}",
            state_dir.display()
        )
    })?;
    let mut name = OsString::from(".velnor-diagnostics-");
    name.push(base);
    Ok(name)
}

/// Owned teardown report: diagnostics export and resource-removal results.
///
/// `failures` contains teardown failures only. Stop/capture failures live in
/// `export.failures`; callers must use [`Self::confirmed`] as the permit
/// release gate instead of checking either list alone.
#[cfg(test)]
#[derive(Debug, Clone)]
struct CleanupReport {
    export: DiagnosticExport,
    /// Teardown failures only. Use [`Self::confirmed`] as the release gate;
    /// stop/capture failures are stored separately in `export.failures`.
    failures: Vec<String>,
}

#[cfg(test)]
impl CleanupReport {
    /// Return true only when stop, diagnostic capture, and teardown all
    /// succeeded. Capacity or permit release must be gated on this result.
    #[must_use]
    fn confirmed(&self) -> bool {
        self.export.failures.is_empty() && self.failures.is_empty()
    }
}

/// Stop the runner and persist a complete diagnostics capture before any
/// owned resource can be deleted.
fn prepare_cleanup(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
) -> Result<DiagnosticExport> {
    let runner_id = super::dind::inspect_owned_container(runner, identity, ROLE_RUNNER)
        .context("verify runner ownership before cleanup")?;
    let dind_id = super::dind::inspect_owned_container(runner, identity, ROLE_DIND)
        .context("verify DinD ownership before cleanup")?;
    if runner_id.is_none()
        && let Some(export) = reuse_full_capture_after_runner_removal(state_dir)?
    {
        return Ok(export);
    }
    let mut stop_failures = Vec::new();
    if let Some(runner_id) = runner_id.as_deref() {
        stop_container(runner, runner_id, &mut stop_failures);
    }
    let stop_succeeded = stop_failures.is_empty();
    // A markerless retry captures all four artifacts again. Atomic publication
    // keeps a prior artifact until a complete replacement is ready. If the
    // directory sync fails after rename, the new complete file may be visible,
    // but this path reports failure and leaves the completion marker absent.
    // Recapture prevents a pre-stop runner log from certifying the final set.
    let mut export = export_diagnostics_inner(
        runner,
        identity,
        state_dir,
        runner_id.as_deref(),
        dind_id.as_deref(),
        true,
        false,
    )?;
    if stop_succeeded && export.failures.is_empty() {
        let diagnostics = SecureDiagnosticDir::open(state_dir)?;
        if let Err(error) = diagnostics.write_file("capture.complete", DIAGNOSTICS_COMPLETE_MARKER)
        {
            export.failures.push(format!(
                "write {}: {error}",
                diagnostics.file_path("capture.complete").display()
            ));
        }
    }
    export.failures.extend(stop_failures);
    Ok(export)
}

/// Reuse the durable full-capture proof when a teardown replay finds the
/// runner already removed. The full marker is written only after all four
/// artifacts have been atomically published; check each artifact is still an
/// owner-only regular file before trusting that proof. Without this path, a
/// crash immediately after runner removal makes recapture impossible and
/// strands the remaining owned resources.
fn reuse_full_capture_after_runner_removal(state_dir: &Path) -> Result<Option<DiagnosticExport>> {
    let Some(diagnostics) = SecureDiagnosticDir::open_existing(state_dir)? else {
        return Ok(None);
    };
    if diagnostics.read_file("capture.complete")?.as_deref() != Some(DIAGNOSTICS_COMPLETE_MARKER) {
        return Ok(None);
    }

    let mut missing_artifacts = Vec::new();
    for name in [
        "runner.log",
        "dind.log",
        "runner.inspect.json",
        "dind.inspect.json",
    ] {
        if !diagnostics.owner_only_file_exists(name)? {
            missing_artifacts.push(name);
        }
    }
    if !missing_artifacts.is_empty() {
        diagnostics
            .remove_file("capture.complete")
            .context("invalidate diagnostics marker with missing artifacts")?;
        diagnostics
            .sync()
            .context("persist invalid diagnostics marker removal")?;
        return Ok(Some(diagnostic_export(
            &diagnostics,
            vec![format!(
                "full diagnostics capture proof has missing or insecure artifacts: {}",
                missing_artifacts.join(", ")
            )],
        )));
    }

    diagnostics
        .sync()
        .context("persist reused diagnostics completion marker")?;
    Ok(Some(diagnostic_export(&diagnostics, Vec::new())))
}

/// Remove every owned object after diagnostics are durably complete.
#[cfg(test)]
fn finish_cleanup(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    export: DiagnosticExport,
) -> CleanupReport {
    if !export.failures.is_empty() {
        return CleanupReport {
            export,
            failures: Vec::new(),
        };
    }
    let failures = teardown_owned_resources(runner, identity);
    CleanupReport { export, failures }
}

/// Remove every Docker resource owned by one worker. Missing objects count
/// as removed, making replay safe after a process crash mid-teardown.
fn teardown_owned_resources(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
) -> Vec<String> {
    // Prove every resource before the first mutation. This keeps a foreign
    // same-name network or volume from being discovered only after containers
    // have already been stopped or removed.
    let ownership = (|| -> Result<_> {
        let runner_id = super::dind::inspect_owned_container(runner, identity, ROLE_RUNNER)
            .context("verify runner ownership")?;
        let dind_id = super::dind::inspect_owned_container(runner, identity, ROLE_DIND)
            .context("verify DinD ownership")?;
        let network_id = super::dind::inspect_owned_network(runner, identity)
            .context("verify worker network ownership")?;
        let workspace_volume = super::dind::verify_owned_volume_unique(
            runner,
            identity,
            super::runner::DockerCreateTarget::WorkspaceVolume,
        )
        .context("verify workspace volume ownership")?;
        let dind_data_volume = super::dind::verify_owned_volume_unique(
            runner,
            identity,
            super::runner::DockerCreateTarget::DindDataVolume,
        )
        .context("verify DinD data volume ownership")?;
        Ok((
            runner_id,
            dind_id,
            network_id,
            workspace_volume,
            dind_data_volume,
        ))
    })();
    let (runner_id, dind_id, network_id, workspace_volume, dind_data_volume) = match ownership {
        Ok(resources) => resources,
        Err(error) => {
            return vec![format!(
                "verify resource ownership before teardown: {error:#}"
            )]
        }
    };

    let mut failures = Vec::new();
    if let Some(runner_id) = runner_id.as_deref() {
        remove_container(runner, runner_id, &mut failures);
    }
    if let Some(dind_id) = dind_id.as_deref() {
        stop_container(runner, dind_id, &mut failures);
        remove_container(runner, dind_id, &mut failures);
    }
    if let Some(network_id) = network_id.as_deref() {
        remove_network(runner, network_id, &mut failures);
    }
    if workspace_volume.is_some() {
        prune_volume(
            runner,
            identity,
            super::runner::DockerCreateTarget::WorkspaceVolume,
            &mut failures,
        );
    }
    if dind_data_volume.is_some() {
        prune_volume(
            runner,
            identity,
            super::runner::DockerCreateTarget::DindDataVolume,
            &mut failures,
        );
    }
    failures
}

/// Tear down every object the worker owns, in dependency order. An export
/// failure prevents deletion, preserving evidence for a replay.
#[cfg(test)]
fn owned_cleanup(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
) -> Result<CleanupReport> {
    let export = prepare_cleanup(runner, identity, state_dir)?;
    Ok(finish_cleanup(runner, identity, export))
}

fn stop_container(runner: &mut dyn WorkerRunner, container: &str, failures: &mut Vec<String>) {
    match runner.run(
        "docker",
        &crate::docker::client::container_stop_args(container, Some(30)),
    ) {
        Ok(output) if output.code == 0 => {}
        Ok(output) if crate::docker::client::daemon_reports_missing(&output.stderr) => {}
        Ok(output) => failures.push(format!(
            "stop {container} exited {}: {}",
            output.code,
            output.stderr.trim()
        )),
        Err(error) => failures.push(format!("stop {container}: {error:#}")),
    }
}

fn remove_container(runner: &mut dyn WorkerRunner, container: &str, failures: &mut Vec<String>) {
    match runner.run(
        "docker",
        &crate::docker::client::container_remove_args(container, true, false),
    ) {
        Ok(output) if output.code == 0 => {}
        Ok(output) if crate::docker::client::daemon_reports_missing(&output.stderr) => {}
        Ok(output) => failures.push(format!(
            "remove {container} exited {}: {}",
            output.code,
            output.stderr.trim()
        )),
        Err(error) => failures.push(format!("remove {container}: {error:#}")),
    }
}

fn remove_network(runner: &mut dyn WorkerRunner, network_id: &str, failures: &mut Vec<String>) {
    match runner.run(
        "docker",
        &[
            "network".to_string(),
            "rm".to_string(),
            "--".to_string(),
            network_id.to_string(),
        ],
    ) {
        Ok(output) if output.code == 0 => {}
        Ok(output) if crate::docker::client::daemon_reports_missing(&output.stderr) => {}
        Ok(output) => failures.push(format!(
            "remove network {network_id} exited {}: {}",
            output.code,
            output.stderr.trim()
        )),
        Err(error) => failures.push(format!("remove network {network_id}: {error:#}")),
    }
}

fn prune_volume(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    target: super::runner::DockerCreateTarget,
    failures: &mut Vec<String>,
) {
    if let Err(error) = super::dind::prune_owned_volume(runner, identity, target) {
        failures.push(format!("prune owned worker volume {target:?}: {error:#}"));
    }
}

/// Supervision handle: identity + state dir + durable runtime counters.
///
/// Mutating ticks and teardown run through the scale-set lane, which holds
/// the worker lifecycle lock and persists runtime state.
///
/// ```compile_fail
/// use velnor_runner::scaleset::worker::{Supervision, WorkerRunner};
///
/// fn bypass_tick(supervision: &mut Supervision, runner: &mut dyn WorkerRunner) {
///     let _ = supervision.tick_with_runtime(
///         runner,
///         velnor_model::ScaleSetWorkerState::Running,
///         0,
///         &mut |_, _| Ok(()),
///     );
/// }
/// ```
///
/// ```compile_fail
/// use velnor_runner::scaleset::worker::{Supervision, WorkerRunner};
///
/// fn bypass_cleanup(supervision: &Supervision, runner: &mut dyn WorkerRunner) {
///     let _ = supervision.prepare_cleanup(runner);
///     let _ = supervision.teardown_owned_resources(runner);
///     let _ = supervision.complete_diagnostics_without_runner(runner);
/// }
/// ```
///
/// ```compile_fail
/// use velnor_runner::scaleset::worker::{Supervision, WorkerRunner};
///
/// fn bypass_durable_cleanup(supervision: &Supervision, runner: &mut dyn WorkerRunner) {
///     let _ = supervision.cleanup(runner);
/// }
/// ```
#[derive(Debug)]
pub struct Supervision {
    identity: WorkerIdentity,
    state_dir: PathBuf,
    profile: HomogeneousProfile,
    restarts: RestartBudget,
    runner_start_deadline_epoch: Option<u64>,
    dind_restart_ready_deadline_epoch: Option<u64>,
}

impl Supervision {
    #[cfg(test)]
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "the pinned x86_64 test profile is required"
    )]
    pub fn new(identity: WorkerIdentity, state_dir: &Path) -> Self {
        let profile = HomogeneousProfile::for_arch("x86_64")
            .expect("the pinned x86_64 worker profile must be available in tests");
        Self {
            identity,
            state_dir: state_dir.to_path_buf(),
            profile,
            restarts: RestartBudget::new(MAX_DIND_RESTARTS),
            runner_start_deadline_epoch: None,
            dind_restart_ready_deadline_epoch: None,
        }
    }

    /// Rebuild supervision from the durable worker registry after restart.
    #[must_use]
    pub fn from_runtime(
        identity: WorkerIdentity,
        state_dir: &Path,
        profile: HomogeneousProfile,
        restarts_used: u32,
        runner_start_deadline_epoch: Option<u64>,
        dind_restart_ready_deadline_epoch: Option<u64>,
    ) -> Self {
        Self {
            identity,
            state_dir: state_dir.to_path_buf(),
            profile,
            restarts: RestartBudget::from_used(MAX_DIND_RESTARTS, restarts_used),
            runner_start_deadline_epoch,
            dind_restart_ready_deadline_epoch,
        }
    }

    /// Observe + decide one tick while persisting runtime state before
    /// commands with external side effects.
    pub(in crate::scaleset) fn tick_with_runtime(
        &mut self,
        lifecycle: &mut crate::scaleset::lane::WorkerLifecycleCapability<'_>,
        runner: &mut dyn WorkerRunner,
        recorded: velnor_model::ScaleSetWorkerState,
        now_epoch: u64,
    ) -> Result<SupervisionOutcome> {
        let ownership_id = self.identity.ownership().as_str();
        lifecycle.validate_tick_owner(&ownership_id)?;
        let observed = observe_pair(runner, &self.identity, &self.state_dir, &self.profile)?;
        let dind_spec = DindSpec::new(
            self.identity.clone(),
            self.profile.dind().clone(),
            &self.state_dir,
        );
        let mut persist_dind_runtime =
            |used, deadline| lifecycle.persist_dind_runtime(&ownership_id, used, deadline);
        let outcome = supervise_tick_with_runtime(
            runner,
            &self.identity,
            &dind_spec,
            recorded,
            &observed,
            &mut self.restarts,
            self.runner_start_deadline_epoch,
            self.dind_restart_ready_deadline_epoch,
            now_epoch,
            &mut persist_dind_runtime,
        )?;
        match outcome {
            SupervisionOutcome::DindRestartPending {
                ready_deadline_epoch,
                ..
            } => self.dind_restart_ready_deadline_epoch = Some(ready_deadline_epoch),
            SupervisionOutcome::DindRestarted { .. } => {
                self.dind_restart_ready_deadline_epoch = None;
            }
            _ => {}
        }
        Ok(outcome)
    }

    pub fn clear_runner_start_deadline(&mut self) {
        self.runner_start_deadline_epoch = None;
    }

    pub fn set_runner_start_deadline(&mut self, deadline_epoch: u64) {
        self.runner_start_deadline_epoch = Some(deadline_epoch);
    }

    pub(in crate::scaleset) fn prepare_cleanup(
        &self,
        lifecycle: &crate::scaleset::lane::WorkerLifecycleCapability<'_>,
        runner: &mut dyn WorkerRunner,
    ) -> Result<DiagnosticExport> {
        let ownership_id = self.identity.ownership().as_str();
        lifecycle.validate_cleanup_owner(&ownership_id)?;
        prepare_cleanup(runner, &self.identity, &self.state_dir)
    }

    pub(in crate::scaleset) fn teardown_owned_resources(
        &self,
        lifecycle: &crate::scaleset::lane::WorkerLifecycleCapability<'_>,
        runner: &mut dyn WorkerRunner,
    ) -> Vec<String> {
        let ownership_id = self.identity.ownership().as_str();
        if let Err(error) = lifecycle.validate_owned_cleanup(&ownership_id) {
            return vec![format!(
                "lifecycle authorization before teardown: {error:#}"
            )];
        }
        teardown_owned_resources(runner, &self.identity)
    }

    pub(crate) fn state_dir_exists(&self) -> Result<bool> {
        #[cfg(unix)]
        {
            let parent_path = self.state_dir.parent().with_context(|| {
                format!(
                    "worker state dir has no parent: {}",
                    self.state_dir.display()
                )
            })?;
            let parent =
                super::secure_fs::open_absolute_directory(parent_path).with_context(|| {
                    format!("open host worker-state parent {}", parent_path.display())
                })?;
            super::secure_fs::verify_host_parent(&parent).with_context(|| {
                format!("verify host worker-state parent {}", parent_path.display())
            })?;
            let state_name = self.state_dir.file_name().with_context(|| {
                format!(
                    "worker state dir has no final component: {}",
                    self.state_dir.display()
                )
            })?;
            super::secure_fs::directory_exists_at(&parent, state_name)
                .with_context(|| format!("inspect worker state dir {}", self.state_dir.display()))
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!("secure worker-state inspection requires Unix dirfd support")
        }
    }

    pub(crate) fn diagnostics_complete(&self) -> Result<bool> {
        Ok(self.diagnostic_completion_kind()?.is_some())
    }

    pub(crate) fn diagnostic_completion_kind(&self) -> Result<Option<DiagnosticCompletionKind>> {
        let Some(diagnostics) = SecureDiagnosticDir::open_existing(&self.state_dir)? else {
            return Ok(None);
        };
        match diagnostics.read_file("capture.complete")? {
            None => Ok(None),
            Some(contents) if contents == DIAGNOSTICS_COMPLETE_MARKER => {
                diagnostics
                    .sync()
                    .context("persist diagnostics completion marker")?;
                Ok(Some(DiagnosticCompletionKind::FullCapture))
            }
            Some(contents) if contents == NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER => {
                diagnostics
                    .sync()
                    .context("persist diagnostics completion marker")?;
                Ok(Some(DiagnosticCompletionKind::RunnerAbsent))
            }
            Some(_) => anyhow::bail!(
                "invalid diagnostic completion marker {}",
                diagnostics.file_path("capture.complete").display()
            ),
        }
    }

    /// Capture available DinD evidence and durably record diagnostics as
    /// complete when a persisted `ProvisionIntent` has no runner container.
    /// This method verifies runner absence, checks DinD's full ownership
    /// labels, and captures by immutable Docker ID. Callers must still prove
    /// the worker's terminal-side registry phase through the lifecycle
    /// capability before invoking it.
    pub(in crate::scaleset) fn complete_diagnostics_without_runner(
        &self,
        lifecycle: &crate::scaleset::lane::WorkerLifecycleCapability<'_>,
        runner: &mut dyn WorkerRunner,
    ) -> Result<DiagnosticExport> {
        let ownership_id = self.identity.ownership().as_str();
        lifecycle.validate_cleanup_owner(&ownership_id)?;
        self.complete_diagnostics_without_runner_unchecked(runner)
    }

    fn complete_diagnostics_without_runner_unchecked(
        &self,
        runner: &mut dyn WorkerRunner,
    ) -> Result<DiagnosticExport> {
        if super::dind::inspect_owned_container(runner, &self.identity, ROLE_RUNNER)?.is_some() {
            anyhow::bail!(
                "refusing no-runner diagnostics completion while runner {} exists",
                self.identity.runner_container()
            );
        }

        let diagnostics = SecureDiagnosticDir::open(&self.state_dir)?;
        match diagnostics.read_file("capture.complete")? {
            Some(contents) if contents == NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER => {
                // A durable receipt can be replayed only while any same-name
                // DinD still proves the recorded worker identity.
                super::dind::inspect_owned_container(runner, &self.identity, ROLE_DIND)?;
                diagnostics
                    .sync()
                    .context("persist existing diagnostics completion marker")?;
                return Ok(diagnostic_export(&diagnostics, Vec::new()));
            }
            Some(contents) if contents == DIAGNOSTICS_COMPLETE_MARKER => {
                anyhow::bail!("full diagnostics marker cannot certify runner-absent capture");
            }
            Some(_) => anyhow::bail!(
                "invalid diagnostic completion marker {}",
                diagnostics.file_path("capture.complete").display()
            ),
            None => {}
        }

        let dind_id = super::dind::inspect_owned_container(runner, &self.identity, ROLE_DIND)?;

        for name in [
            "runner.log",
            "runner.inspect.json",
            "runner.log.complete",
            "runner.inspect.json.complete",
        ] {
            diagnostics.remove_file(name).with_context(|| {
                format!(
                    "remove incomplete diagnostic {}",
                    diagnostics.file_path(name).display()
                )
            })?;
        }
        diagnostics
            .sync()
            .context("persist removed incomplete diagnostics")?;

        let mut failures = Vec::new();
        if let Some(dind_id) = dind_id.as_deref() {
            if !diagnostics.ensure_artifact_complete("dind.log")? {
                diagnostics.clear_artifact("dind.log")?;
                let before = failures.len();
                capture_logs(runner, dind_id, &diagnostics, "dind.log", &mut failures);
                if failures.len() == before {
                    diagnostics.mark_artifact_complete("dind.log")?;
                }
            }
            if !diagnostics.ensure_artifact_complete("dind.inspect.json")? {
                diagnostics.clear_artifact("dind.inspect.json")?;
                let before = failures.len();
                capture_inspect(
                    runner,
                    dind_id,
                    &diagnostics,
                    "dind.inspect.json",
                    &mut failures,
                );
                if failures.len() == before {
                    diagnostics.mark_artifact_complete("dind.inspect.json")?;
                }
            }
        } else {
            for name in ["dind.log", "dind.inspect.json"] {
                if !diagnostics.ensure_artifact_complete(name)? {
                    diagnostics.clear_artifact(name)?;
                }
            }
        }
        if failures.is_empty()
            && let Err(error) =
                diagnostics.write_file("capture.complete", NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER)
        {
            failures.push(format!(
                "write {}: {error}",
                diagnostics.file_path("capture.complete").display()
            ));
        }
        if failures.is_empty()
            && let Err(error) = diagnostics.sync()
        {
            failures.push(format!(
                "sync {}: {error}",
                diagnostics.file_path("capture.complete").display()
            ));
        }

        Ok(diagnostic_export(&diagnostics, failures))
    }

    /// Delete worker state and host-only sibling data while the permit is
    /// still held. State is removed first so a partial failure leaves private
    /// data available for an idempotent retry. Every removal is relative to
    /// an opened, no-follow parent descriptor. A live state tree must carry
    /// the host-only completion marker before this path can delete it.
    pub(crate) fn release_state(
        &self,
        authorization: &crate::scaleset::lane::ReleasedStateCleanupAuthorization,
    ) -> Result<()> {
        self.verify_cleanup_authorization(authorization)?;
        if self.state_dir_exists()? && !self.diagnostics_complete()? {
            anyhow::bail!(
                "refusing to delete {} without complete host-only diagnostics",
                self.state_dir.display()
            );
        }
        self.release_state_inner()
    }

    /// Delete an owned worker state tree when the durable ownership registry
    /// authorizes cleanup without a completed diagnostics marker.
    pub(crate) fn release_owned_state(
        &self,
        authorization: &crate::scaleset::lane::ReleasedStateCleanupAuthorization,
    ) -> Result<()> {
        self.verify_cleanup_authorization(authorization)?;
        self.release_state_inner()
    }

    fn verify_cleanup_authorization(
        &self,
        authorization: &crate::scaleset::lane::ReleasedStateCleanupAuthorization,
    ) -> Result<()> {
        let ownership_id = self.identity.ownership().as_str();
        if !authorization.authorizes(&ownership_id, &self.state_dir) {
            anyhow::bail!(
                "released-state authorization does not match worker {ownership_id:?} at {}",
                self.state_dir.display()
            );
        }
        Ok(())
    }

    fn release_state_inner(&self) -> Result<()> {
        #[cfg(unix)]
        {
            let parent_path = self.state_dir.parent().with_context(|| {
                format!(
                    "worker state dir has no parent: {}",
                    self.state_dir.display()
                )
            })?;
            let parent =
                super::secure_fs::open_absolute_directory(parent_path).with_context(|| {
                    format!("open host worker-state parent {}", parent_path.display())
                })?;
            super::secure_fs::verify_host_parent(&parent).with_context(|| {
                format!("verify host worker-state parent {}", parent_path.display())
            })?;
            let state_name = self.state_dir.file_name().with_context(|| {
                format!(
                    "worker state dir has no final component: {}",
                    self.state_dir.display()
                )
            })?;
            let worker_private_name =
                super::secure_fs::private_jit_directory_name(&self.state_dir)?;
            let diagnostic_name = diagnostics_dir_name(&self.state_dir)?;
            release_paths(
                &parent,
                state_name,
                &worker_private_name,
                &diagnostic_name,
                super::secure_fs::remove_tree_at,
            )
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!("secure worker-state release requires Unix dirfd support")
        }
    }
}

#[cfg(unix)]
fn release_paths(
    parent: &File,
    state_name: &std::ffi::OsStr,
    worker_private_name: &std::ffi::OsStr,
    diagnostic_name: &std::ffi::OsStr,
    mut remove: impl FnMut(&File, &std::ffi::OsStr) -> io::Result<()>,
) -> Result<()> {
    remove(parent, state_name)
        .with_context(|| format!("delete released worker state {state_name:?}"))?;
    remove(parent, worker_private_name)
        .with_context(|| format!("delete host-only worker files {worker_private_name:?}"))?;
    remove(parent, diagnostic_name)
        .with_context(|| format!("delete host-only diagnostics {diagnostic_name:?}"))?;
    super::secure_fs::sync_directory(parent).context("persist released worker-state removals")?;
    Ok(())
}

#[cfg(all(test, unix))]
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
    use super::super::WorkerOutput;
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use velnor_model::ScaleSetWorkerState as S;

    struct ScriptRunner {
        results: VecDeque<WorkerOutput>,
        seen: Vec<Vec<String>>,
        timeouts: Vec<Duration>,
    }

    impl ScriptRunner {
        fn scripted(results: Vec<WorkerOutput>) -> Self {
            Self {
                results: results.into(),
                seen: Vec::new(),
                timeouts: Vec::new(),
            }
        }

        fn ok(stdout: &str) -> WorkerOutput {
            WorkerOutput {
                code: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
            }
        }

        fn fail(code: i32, stderr: &str) -> WorkerOutput {
            WorkerOutput {
                code,
                stdout: String::new(),
                stderr: stderr.to_string(),
            }
        }
    }

    impl WorkerRunner for ScriptRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<WorkerOutput> {
            assert_eq!(program, "docker");
            self.seen.push(args.to_vec());
            self.results
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("script exhausted at docker {}", args.join(" ")))
        }

        fn run_timeout(
            &mut self,
            program: &str,
            args: &[String],
            timeout: Duration,
        ) -> Result<WorkerOutput> {
            self.timeouts.push(timeout);
            self.run(program, args)
        }
    }

    fn identity() -> WorkerIdentity {
        WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ))
    }

    fn test_profile() -> HomogeneousProfile {
        HomogeneousProfile::for_arch("x86_64").unwrap()
    }

    fn test_dind_spec() -> DindSpec {
        let profile = test_profile();
        DindSpec::new(
            identity(),
            profile.dind().clone(),
            Path::new("/tmp/velnor-test-sup"),
        )
    }

    fn owned_container_inspect(id: &str, role: &str) -> WorkerOutput {
        let identity = identity();
        let mut labels = identity.labels();
        labels.insert(
            super::super::ownership::WORKER_ROLE_LABEL.to_string(),
            role.to_string(),
        );
        ScriptRunner::ok(&format!(
            "{id}\n{}",
            serde_json::to_string(&labels).unwrap()
        ))
    }

    fn owned_network_inspect(id: &str) -> WorkerOutput {
        ScriptRunner::ok(&format!(
            "{id}\n{}",
            serde_json::to_string(&identity().labels()).unwrap()
        ))
    }

    fn owned_volume_inspect(target: super::super::runner::DockerCreateTarget) -> WorkerOutput {
        let identity = identity();
        let (kind, name) = match target {
            super::super::runner::DockerCreateTarget::WorkspaceVolume => {
                ("workspace", identity.workspace_volume())
            }
            super::super::runner::DockerCreateTarget::DindDataVolume => {
                ("dind-data", identity.dind_data_volume())
            }
            _ => panic!("not a volume target: {target:?}"),
        };
        let mut labels = identity.labels();
        labels.insert("velnor.scaleset.volume".to_string(), kind.to_string());
        labels.insert("velnor.scaleset.volume-name".to_string(), name.clone());
        ScriptRunner::ok(&format!(
            "{name}\n{}",
            serde_json::to_string(&labels).unwrap()
        ))
    }

    fn owned_volume_snapshot(target: super::super::runner::DockerCreateTarget) -> WorkerOutput {
        let identity = identity();
        let (kind, name) = match target {
            super::super::runner::DockerCreateTarget::WorkspaceVolume => {
                ("workspace", identity.workspace_volume())
            }
            super::super::runner::DockerCreateTarget::DindDataVolume => {
                ("dind-data", identity.dind_data_volume())
            }
            _ => panic!("not a volume target: {target:?}"),
        };
        let mut labels = identity.labels();
        labels.insert("velnor.scaleset.volume".to_string(), kind.to_string());
        labels.insert("velnor.scaleset.volume-name".to_string(), name.clone());
        ScriptRunner::ok(
            &serde_json::json!({
                "Name": name,
                "Driver": "local",
                "Options": null,
                "Labels": labels
            })
            .to_string(),
        )
    }

    fn missing_volume(name: &str) -> WorkerOutput {
        ScriptRunner::fail(1, &format!("Error: No such volume: {name}"))
    }

    #[derive(Clone)]
    struct MockContainer {
        name: String,
        id: String,
        labels: BTreeMap<String, String>,
    }

    struct MockNetwork {
        name: String,
        id: String,
        labels: BTreeMap<String, String>,
    }

    struct MockVolume {
        name: String,
        labels: BTreeMap<String, String>,
        active: bool,
    }

    struct OwnershipRunner {
        containers: Vec<MockContainer>,
        networks: Vec<MockNetwork>,
        volumes: Vec<MockVolume>,
        seen: Vec<Vec<String>>,
        replace_dind_after_lookup: bool,
        replace_dind_after_preflight: bool,
        replace_network_after_preflight: bool,
        replace_workspace_after_preflight: bool,
    }

    impl OwnershipRunner {
        fn with_dind_labels(labels: BTreeMap<String, String>) -> Self {
            Self {
                containers: vec![MockContainer {
                    name: identity().dind_container(),
                    id: "dind-id".to_string(),
                    labels,
                }],
                networks: Vec::new(),
                volumes: Vec::new(),
                seen: Vec::new(),
                replace_dind_after_lookup: false,
                replace_dind_after_preflight: false,
                replace_network_after_preflight: false,
                replace_workspace_after_preflight: false,
            }
        }

        fn with_owned_resources() -> Self {
            let worker = identity();
            let mut runner = Self::with_dind_labels(container_labels(ROLE_DIND));
            runner.containers.push(MockContainer {
                name: worker.runner_container(),
                id: "runner-id".to_string(),
                labels: container_labels(ROLE_RUNNER),
            });
            runner.networks.push(MockNetwork {
                name: worker.network(),
                id: "network-id".to_string(),
                labels: worker.labels(),
            });
            for (kind, name) in [
                ("workspace", worker.workspace_volume()),
                ("dind-data", worker.dind_data_volume()),
            ] {
                let mut labels = worker.labels();
                labels.insert("velnor.scaleset.volume".to_string(), kind.to_string());
                labels.insert("velnor.scaleset.volume-name".to_string(), name.clone());
                runner.volumes.push(MockVolume {
                    name,
                    labels,
                    active: false,
                });
            }
            runner
        }

        fn dind_exists(&self) -> bool {
            self.containers
                .iter()
                .any(|container| container.name == identity().dind_container())
        }

        fn dind_id(&self) -> Option<&str> {
            self.containers
                .iter()
                .find(|container| container.name == identity().dind_container())
                .map(|container| container.id.as_str())
        }

        fn volume_exists(&self, name: &str) -> bool {
            self.volumes.iter().any(|volume| volume.name == name)
        }

        fn saw(&self, verb: &str) -> bool {
            self.seen
                .iter()
                .any(|args| args.first().is_some_and(|arg| arg == verb))
        }

        fn saw_pair(&self, first: &str, second: &str) -> bool {
            self.seen.iter().any(|args| {
                args.first().is_some_and(|arg| arg == first)
                    && args.get(1).is_some_and(|arg| arg == second)
            })
        }

        fn missing(name: &str) -> WorkerOutput {
            ScriptRunner::fail(1, &format!("Error: No such object: {name}"))
        }

        fn volume_labels_match(volume: &MockVolume, filters: &[String]) -> bool {
            filters.iter().all(|filter| {
                let Some(label_filter) = filter.strip_prefix("label=") else {
                    return false;
                };
                let Some((key, value)) = label_filter.split_once('=') else {
                    return false;
                };
                volume.labels.get(key).is_some_and(|found| found == value)
            })
        }
    }

    impl WorkerRunner for OwnershipRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<WorkerOutput> {
            assert_eq!(program, "docker");
            self.seen.push(args.to_vec());
            let target = args.last().map(String::as_str).unwrap_or_default();
            match args.first().map(String::as_str) {
                Some("inspect") if args.iter().any(|arg| arg == "--type") => {
                    let Some(container) = self
                        .containers
                        .iter()
                        .find(|container| container.name == target || container.id == target)
                    else {
                        return Ok(Self::missing(target));
                    };
                    let output = ScriptRunner::ok(&format!(
                        "{}\n{}",
                        container.id,
                        serde_json::to_string(&container.labels)?
                    ));
                    if target == identity().dind_container() && self.replace_dind_after_lookup {
                        self.replace_dind_after_lookup = false;
                        if let Some(dind) = self
                            .containers
                            .iter_mut()
                            .find(|container| container.name == identity().dind_container())
                        {
                            dind.id = "foreign-replacement-id".to_string();
                            dind.labels.insert(
                                super::super::ownership::OWNERSHIP_LABEL.to_string(),
                                "7/foreign".to_string(),
                            );
                        }
                    }
                    Ok(output)
                }
                Some("inspect") => {
                    let Some(container) = self
                        .containers
                        .iter()
                        .find(|container| container.id == target)
                    else {
                        return Ok(Self::missing(target));
                    };
                    Ok(ScriptRunner::ok(&format!(
                        r#"[{{"Id":"{}","Config":{{"Labels":{}}},"State":{{"Status":"running"}}}}]"#,
                        container.id,
                        serde_json::to_string(&container.labels)?
                    )))
                }
                Some("logs") => {
                    if self
                        .containers
                        .iter()
                        .any(|container| container.id == target)
                    {
                        Ok(ScriptRunner::ok("owned dind log\n"))
                    } else {
                        Ok(Self::missing(target))
                    }
                }
                Some("start") => {
                    if self
                        .containers
                        .iter()
                        .any(|container| container.id == target)
                    {
                        Ok(ScriptRunner::ok(target))
                    } else {
                        Ok(Self::missing(target))
                    }
                }
                Some("stop") => {
                    if self
                        .containers
                        .iter()
                        .any(|container| container.id == target)
                    {
                        Ok(ScriptRunner::ok("dind-id\n"))
                    } else {
                        Ok(Self::missing(target))
                    }
                }
                Some("rm") => {
                    let Some(index) = self
                        .containers
                        .iter()
                        .position(|container| container.id == target)
                    else {
                        return Ok(Self::missing(target));
                    };
                    self.containers.remove(index);
                    Ok(ScriptRunner::ok("dind-id\n"))
                }
                Some("network") if args.get(1).is_some_and(|arg| arg == "inspect") => {
                    let Some(network) = self
                        .networks
                        .iter()
                        .find(|network| network.name == target || network.id == target)
                    else {
                        return Ok(ScriptRunner::fail(1, "Error: No such network"));
                    };
                    if args.iter().any(|arg| arg == "{{json .}}") {
                        return Ok(ScriptRunner::ok(
                            &serde_json::json!({
                                "Id": network.id,
                                "Name": network.name,
                                "Labels": network.labels,
                                "Driver": "bridge",
                                "Scope": "local",
                                "Internal": false,
                                "Attachable": false,
                                "Ingress": false,
                                "ConfigOnly": false,
                                "EnableIPv6": false,
                                "Options": null,
                                "IPAM": {
                                    "Driver": "default",
                                    "Options": null,
                                    "Config": [{
                                        "Subnet": "172.30.0.0/16",
                                        "IPRange": "",
                                        "Gateway": "172.30.0.1"
                                    }]
                                }
                            })
                            .to_string(),
                        ));
                    }
                    Ok(ScriptRunner::ok(&format!(
                        "{}\n{}",
                        network.id,
                        serde_json::to_string(&network.labels)?
                    )))
                }
                Some("network") if args.get(1).is_some_and(|arg| arg == "rm") => {
                    let Some(index) = self
                        .networks
                        .iter()
                        .position(|network| network.id == target)
                    else {
                        return Ok(ScriptRunner::fail(1, "Error: No such network"));
                    };
                    self.networks.remove(index);
                    Ok(ScriptRunner::ok(target))
                }
                Some("volume") if args.get(1).is_some_and(|arg| arg == "inspect") => {
                    let Some(volume) = self.volumes.iter().find(|volume| volume.name == target)
                    else {
                        return Ok(ScriptRunner::fail(1, "Error: No such volume"));
                    };
                    let output = if args.iter().any(|arg| arg == "{{json .}}") {
                        ScriptRunner::ok(
                            &serde_json::json!({
                                "Name": volume.name,
                                "Driver": "local",
                                "Options": null,
                                "Labels": volume.labels
                            })
                            .to_string(),
                        )
                    } else {
                        ScriptRunner::ok(&format!(
                            "{}\n{}",
                            volume.name,
                            serde_json::to_string(&volume.labels)?
                        ))
                    };
                    if target == identity().dind_data_volume() {
                        if self.replace_network_after_preflight {
                            self.replace_network_after_preflight = false;
                            if let Some(network) = self
                                .networks
                                .iter_mut()
                                .find(|network| network.name == identity().network())
                            {
                                network.id = "foreign-network-replacement-id".to_string();
                                network.labels.insert(
                                    super::super::ownership::OWNERSHIP_LABEL.to_string(),
                                    "7/foreign".to_string(),
                                );
                            }
                        }
                        if self.replace_dind_after_preflight {
                            self.replace_dind_after_preflight = false;
                            if let Some(dind) = self
                                .containers
                                .iter_mut()
                                .find(|container| container.name == identity().dind_container())
                            {
                                dind.id = "foreign-replacement-id".to_string();
                                dind.labels.insert(
                                    super::super::ownership::OWNERSHIP_LABEL.to_string(),
                                    "7/foreign".to_string(),
                                );
                            }
                        }
                        if self.replace_workspace_after_preflight {
                            self.replace_workspace_after_preflight = false;
                            if let Some(workspace) = self
                                .volumes
                                .iter_mut()
                                .find(|volume| volume.name == identity().workspace_volume())
                            {
                                workspace.labels.insert(
                                    super::super::ownership::OWNERSHIP_LABEL.to_string(),
                                    "7/foreign".to_string(),
                                );
                            }
                        }
                    }
                    Ok(output)
                }
                Some("volume") if args.get(1).is_some_and(|arg| arg == "ls") => {
                    let filters: Vec<_> = args
                        .windows(2)
                        .filter(|pair| pair[0] == "--filter")
                        .map(|pair| pair[1].clone())
                        .collect();
                    let names = self
                        .volumes
                        .iter()
                        .filter(|volume| Self::volume_labels_match(volume, &filters))
                        .map(|volume| volume.name.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(ScriptRunner::ok(&names))
                }
                Some("volume") if args.get(1).is_some_and(|arg| arg == "prune") => {
                    let filters: Vec<_> = args
                        .windows(2)
                        .filter(|pair| pair[0] == "--filter")
                        .map(|pair| pair[1].clone())
                        .collect();
                    self.volumes.retain(|volume| {
                        volume.active || !Self::volume_labels_match(volume, &filters)
                    });
                    Ok(ScriptRunner::ok("Deleted Volumes:\n"))
                }
                Some("network") => Ok(ScriptRunner::fail(1, "Error: No such network")),
                Some("volume") => Ok(ScriptRunner::fail(1, "Error: No such volume")),
                verb => Err(anyhow::anyhow!("unexpected docker command {verb:?}")),
            }
        }
    }

    fn container_labels(role: &str) -> BTreeMap<String, String> {
        let mut labels = identity().labels();
        labels.insert(
            super::super::ownership::WORKER_ROLE_LABEL.to_string(),
            role.to_string(),
        );
        labels
    }

    fn test_cleanup_authorization(
        state_dir: &Path,
    ) -> crate::scaleset::lane::ReleasedStateCleanupAuthorization {
        crate::scaleset::lane::test_release_authorization("7/velnor-set-0007", state_dir)
    }

    fn temp_state(name: &str) -> PathBuf {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!("velnor-supervise-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let state = root.join("worker");
        std::fs::create_dir(&state).unwrap();
        state
    }

    #[test]
    fn healthy_pair_ticks_healthy() {
        let observed = ObservedPair {
            dind_running: true,
            dind_ready: true,
            runner: RunnerConnection::Connected,
        };
        let mut runner = ScriptRunner::scripted(vec![]);
        let mut restarts = RestartBudget::new(MAX_DIND_RESTARTS);
        let outcome = supervise_tick(
            &mut runner,
            &identity(),
            S::Running,
            &observed,
            &mut restarts,
        )
        .unwrap();
        assert_eq!(outcome, SupervisionOutcome::Healthy);
        assert!(runner.seen.is_empty());
    }

    #[test]
    fn dind_death_restarts_within_budget() {
        let observed = ObservedPair {
            dind_running: false,
            dind_ready: false,
            runner: RunnerConnection::Connected,
        };
        let mut runner = ScriptRunner::scripted(vec![
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::ok("started\n"),
            ScriptRunner::ok("28.5.2\n"),
        ]);
        let mut restarts = RestartBudget::new(MAX_DIND_RESTARTS);
        let mut persisted = Vec::new();
        let outcome = supervise_tick_with_runtime(
            &mut runner,
            &identity(),
            &test_dind_spec(),
            S::Running,
            &observed,
            &mut restarts,
            None,
            None,
            100,
            &mut |used, deadline| {
                persisted.push((used, deadline));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            outcome,
            SupervisionOutcome::DindRestarted { restarts_used: 1 }
        );
        assert_eq!(runner.seen[1], ["start", "--", "dind-id"]);
        assert_eq!(runner.seen[2][..2], ["exec", "dind-id"]);
        assert_eq!(
            runner.timeouts,
            [
                super::super::dind::DIND_INSPECT_OPERATION_TIMEOUT,
                super::super::dind::DIND_INSPECT_OPERATION_TIMEOUT,
                DIND_READY_OPERATION_TIMEOUT,
            ],
            "restart, immutable-ID inspection, and dockerd probe must each be bounded"
        );
        assert_eq!(persisted, [(1, Some(220)), (1, None)]);
    }

    #[test]
    fn established_session_waits_for_dind_probe_and_restart_deadline() {
        let observed = ObservedPair {
            dind_running: false,
            dind_ready: false,
            runner: RunnerConnection::Connected,
        };
        let mut runner = ScriptRunner::scripted(vec![
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::ok("started\n"),
            ScriptRunner::fail(1, "dockerd is still starting"),
        ]);
        let mut restarts = RestartBudget::new(MAX_DIND_RESTARTS);
        let mut persisted = Vec::new();
        let pending = supervise_tick_with_runtime(
            &mut runner,
            &identity(),
            &test_dind_spec(),
            S::DindReady,
            &observed,
            &mut restarts,
            Some(100),
            None,
            100,
            &mut |used, deadline| {
                persisted.push((used, deadline));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            pending,
            SupervisionOutcome::DindRestartPending {
                restarts_used: 1,
                ready_deadline_epoch: 220,
            }
        );
        assert_eq!(persisted, [(1, Some(220))]);
        assert_eq!(runner.seen[1], ["start", "--", "dind-id"]);
        assert_eq!(runner.seen[2][..2], ["exec", "dind-id"]);

        let still_unready = ObservedPair {
            dind_running: true,
            dind_ready: false,
            runner: RunnerConnection::Connected,
        };
        let mut pending_runner =
            ScriptRunner::scripted(vec![owned_container_inspect("dind-id", ROLE_DIND)]);
        let pending_again = supervise_tick_with_runtime(
            &mut pending_runner,
            &identity(),
            &test_dind_spec(),
            S::Running,
            &still_unready,
            &mut restarts,
            None,
            Some(220),
            120,
            &mut |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(pending_again, pending);
        assert!(!pending_runner.seen.iter().any(|args| args
            .first()
            .is_some_and(|arg| arg == "start" || arg == "restart")));

        let ready = ObservedPair {
            dind_running: true,
            dind_ready: true,
            runner: RunnerConnection::Connected,
        };
        let mut ready_runner = ScriptRunner::scripted(vec![]);
        let mut cleared = Vec::new();
        let repaired = supervise_tick_with_runtime(
            &mut ready_runner,
            &identity(),
            &test_dind_spec(),
            S::Running,
            &ready,
            &mut restarts,
            None,
            Some(220),
            121,
            &mut |used, deadline| {
                cleared.push((used, deadline));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            repaired,
            SupervisionOutcome::DindRestarted { restarts_used: 1 }
        );
        assert_eq!(cleared, [(1, None)]);
        assert!(ready_runner.seen.is_empty());
    }

    #[test]
    fn recovered_supervision_waits_on_persisted_dind_restart_deadline() {
        // The lane rebuilds this handle from the worker registry after a
        // process restart. A still-pending restart must not spend the budget
        // again or issue another Docker start before its durable deadline.
        let mut supervision = Supervision::from_runtime(
            identity(),
            Path::new("/tmp/velnor-test-sup"),
            test_profile(),
            1,
            None,
            Some(220),
        );
        let observed = ObservedPair {
            dind_running: true,
            dind_ready: false,
            runner: RunnerConnection::Connected,
        };
        let mut runner =
            ScriptRunner::scripted(vec![owned_container_inspect("dind-id", ROLE_DIND)]);
        let mut persisted = Vec::new();
        let outcome = supervise_tick_with_runtime(
            &mut runner,
            &identity(),
            &test_dind_spec(),
            S::Running,
            &observed,
            &mut supervision.restarts,
            supervision.runner_start_deadline_epoch,
            supervision.dind_restart_ready_deadline_epoch,
            120,
            &mut |used, deadline| {
                persisted.push((used, deadline));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            SupervisionOutcome::DindRestartPending {
                restarts_used: 1,
                ready_deadline_epoch: 220,
            }
        );
        assert_eq!(supervision.restarts.used(), 1);
        assert_eq!(supervision.dind_restart_ready_deadline_epoch, Some(220));
        assert_eq!(persisted, []);
        assert!(!runner.seen.iter().any(|args| args
            .first()
            .is_some_and(|arg| arg == "start" || arg == "restart")));
    }

    #[test]
    fn dind_death_fails_when_budget_exhausted() {
        let observed = ObservedPair {
            dind_running: false,
            dind_ready: false,
            runner: RunnerConnection::Connected,
        };
        let mut runner = ScriptRunner::scripted(vec![]);
        let mut restarts = RestartBudget::new(1);
        assert!(restarts.spend());
        let outcome = supervise_tick(
            &mut runner,
            &identity(),
            S::Running,
            &observed,
            &mut restarts,
        )
        .unwrap();
        assert!(matches!(outcome, SupervisionOutcome::WorkerFailed { .. }));
        // No restart attempted: the script is empty and nothing ran.
        assert!(runner.seen.is_empty());
    }

    #[test]
    fn runner_death_is_never_restarted() {
        let observed = ObservedPair {
            dind_running: true,
            dind_ready: true,
            runner: RunnerConnection::Down,
        };
        let mut runner = ScriptRunner::scripted(vec![]);
        let mut restarts = RestartBudget::new(MAX_DIND_RESTARTS);
        let outcome = supervise_tick(
            &mut runner,
            &identity(),
            S::Running,
            &observed,
            &mut restarts,
        )
        .unwrap();
        match outcome {
            SupervisionOutcome::WorkerFailed { reason } => {
                assert!(reason.contains("oracle, not a restart"), "{reason}");
            }
            other => panic!("expected WorkerFailed, got {other:?}"),
        }
        assert!(runner.seen.is_empty());
        assert_eq!(restarts.used(), 0);
    }

    #[test]
    fn runner_start_deadline_is_enforced_and_durable() {
        let observed = ObservedPair {
            dind_running: true,
            dind_ready: false,
            runner: RunnerConnection::Starting,
        };
        let mut runner = ScriptRunner::scripted(vec![]);
        let mut restarts = RestartBudget::new(MAX_DIND_RESTARTS);
        let expired = supervise_tick_with_runtime(
            &mut runner,
            &identity(),
            &test_dind_spec(),
            S::DindReady,
            &observed,
            &mut restarts,
            Some(100),
            None,
            100,
            &mut |_, _| Ok(()),
        )
        .unwrap();
        assert!(matches!(
            expired,
            SupervisionOutcome::ProvisioningFailedBeforeSession { .. }
        ));
        assert!(runner.seen.is_empty());

        let observed = ObservedPair {
            dind_running: true,
            dind_ready: true,
            runner: RunnerConnection::Connected,
        };
        let within_deadline = supervise_tick_with_runtime(
            &mut runner,
            &identity(),
            &test_dind_spec(),
            S::DindReady,
            &observed,
            &mut restarts,
            Some(100),
            None,
            99,
            &mut |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(within_deadline, SupervisionOutcome::RunnerConnected);
    }

    #[test]
    fn restart_budget_persists_before_docker_start() {
        let observed = ObservedPair {
            dind_running: false,
            dind_ready: false,
            runner: RunnerConnection::Connected,
        };
        let mut runner =
            ScriptRunner::scripted(vec![owned_container_inspect("dind-id", ROLE_DIND)]);
        let mut restarts = RestartBudget::new(MAX_DIND_RESTARTS);
        let error = supervise_tick_with_runtime(
            &mut runner,
            &identity(),
            &test_dind_spec(),
            S::Running,
            &observed,
            &mut restarts,
            None,
            None,
            100,
            &mut |_, _| anyhow::bail!("registry unavailable"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("registry unavailable"));
        assert_eq!(
            runner.seen.len(),
            1,
            "DinD must be ownership-checked, then persistence must precede start"
        );
        assert_eq!(runner.seen[0].last().unwrap(), &identity().dind_container());
        assert_eq!(restarts.used(), 0);
        assert_eq!(
            RestartBudget::from_used(MAX_DIND_RESTARTS, u32::MAX).used(),
            MAX_DIND_RESTARTS
        );
    }

    #[test]
    fn supervision_rejects_foreign_same_name_container_before_state_checks() {
        let mut labels = container_labels(ROLE_DIND);
        labels.insert(
            super::super::ownership::OWNERSHIP_LABEL.to_string(),
            "7/foreign".to_string(),
        );
        let mut runner = OwnershipRunner::with_dind_labels(labels);
        let error = observe_pair(
            &mut runner,
            &identity(),
            Path::new("/tmp/velnor-test-sup"),
            &test_profile(),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("foreign ownership"),
            "{error:#}"
        );
        assert_eq!(runner.seen.len(), 2);
        assert!(runner.seen[0].first().is_some_and(|arg| arg == "network"));
        assert!(runner.seen[1].first().is_some_and(|arg| arg == "inspect"));
        assert!(runner.seen[1]
            .iter()
            .any(|arg| arg == &identity().dind_container()));
        assert!(!runner.saw("start"));
        assert!(!runner.saw("stop"));
        assert!(!runner.saw("rm"));
    }

    #[test]
    fn restart_targets_inspected_id_if_same_name_is_replaced() {
        let observed = ObservedPair {
            dind_running: false,
            dind_ready: false,
            runner: RunnerConnection::Connected,
        };
        let mut runner = OwnershipRunner::with_dind_labels(container_labels(ROLE_DIND));
        runner.replace_dind_after_lookup = true;
        let mut restarts = RestartBudget::new(MAX_DIND_RESTARTS);
        let mut persisted = Vec::new();
        let outcome = supervise_tick_with_runtime(
            &mut runner,
            &identity(),
            &test_dind_spec(),
            S::Running,
            &observed,
            &mut restarts,
            None,
            None,
            100,
            &mut |used, deadline| {
                persisted.push((used, deadline));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            outcome,
            SupervisionOutcome::DindRestartPending {
                restarts_used: 1,
                ready_deadline_epoch: 220,
            }
        );
        assert_eq!(persisted, [(1, Some(220))]);
        assert_eq!(restarts.used(), 1);
        assert_eq!(runner.dind_id(), Some("foreign-replacement-id"));
        assert!(runner.seen.iter().any(|args| {
            args.first().is_some_and(|arg| arg == "start")
                && args.last().is_some_and(|arg| arg == "dind-id")
        }));
        assert!(!runner.seen.iter().any(|args| {
            args.iter().any(|arg| arg == "foreign-replacement-id")
                && args.first().is_some_and(|arg| arg == "start")
        }));
    }

    #[test]
    fn terminal_side_states_skip_the_tick() {
        let observed = ObservedPair {
            dind_running: false,
            dind_ready: false,
            runner: RunnerConnection::Down,
        };
        for recorded in [
            S::Terminal,
            S::DiagnosticExport,
            S::OwnedCleanup,
            S::PermitReleased,
        ] {
            let mut runner = ScriptRunner::scripted(vec![]);
            let mut restarts = RestartBudget::new(MAX_DIND_RESTARTS);
            let outcome =
                supervise_tick(&mut runner, &identity(), recorded, &observed, &mut restarts)
                    .unwrap();
            assert_eq!(outcome, SupervisionOutcome::Healthy);
        }
    }

    #[test]
    fn cleanup_exports_before_first_deletion_in_order() {
        let state = temp_state("order");
        let mut runner = ScriptRunner::scripted(vec![
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::ok("runner stopped\n"),
            ScriptRunner::ok("LOGS-R\n"),
            ScriptRunner::ok("LOGS-D\n"),
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("[{}]\n"),
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            owned_network_inspect("network-id"),
            owned_volume_inspect(super::super::runner::DockerCreateTarget::WorkspaceVolume),
            ScriptRunner::ok(&format!("{}\n", identity().workspace_volume())),
            owned_volume_snapshot(super::super::runner::DockerCreateTarget::WorkspaceVolume),
            owned_volume_inspect(super::super::runner::DockerCreateTarget::DindDataVolume),
            ScriptRunner::ok(&format!("{}\n", identity().dind_data_volume())),
            owned_volume_snapshot(super::super::runner::DockerCreateTarget::DindDataVolume),
            ScriptRunner::ok("runner removed\n"),
            ScriptRunner::ok("dind stopped\n"),
            ScriptRunner::ok("dind removed\n"),
            ScriptRunner::ok("network-id\n"),
            owned_volume_inspect(super::super::runner::DockerCreateTarget::WorkspaceVolume),
            ScriptRunner::ok(&format!("{}\n", identity().workspace_volume())),
            owned_volume_snapshot(super::super::runner::DockerCreateTarget::WorkspaceVolume),
            ScriptRunner::ok("workspace\n"),
            missing_volume(&identity().workspace_volume()),
            ScriptRunner::ok(""),
            owned_volume_inspect(super::super::runner::DockerCreateTarget::DindDataVolume),
            ScriptRunner::ok(&format!("{}\n", identity().dind_data_volume())),
            owned_volume_snapshot(super::super::runner::DockerCreateTarget::DindDataVolume),
            ScriptRunner::ok("dind-data\n"),
            missing_volume(&identity().dind_data_volume()),
            ScriptRunner::ok(""),
        ]);
        let report = owned_cleanup(&mut runner, &identity(), &state).unwrap();
        assert!(report.confirmed(), "{report:?}");
        let verbs: Vec<String> = runner.seen.iter().map(|argv| argv.join(" ")).collect();
        let position = |needle: &str| verbs.iter().position(|v| v.contains(needle)).unwrap();
        // Ownership probes precede actions; immutable IDs carry the proof
        // through stop/capture/remove even if the name is reused.
        assert!(position("stop -t 30 -- runner-id") < position("logs -- runner-id"));
        assert!(position("logs -- runner-id") < position("rm --force -- runner-id"));
        assert!(position("rm --force -- runner-id") < position("stop -t 30 -- dind-id"));
        assert!(position("stop -t 30 -- dind-id") < position("rm --force -- dind-id"));
        assert!(position("rm --force -- dind-id") < position("network rm -- network-id"));
        let prune_workspace = position("volume prune --all --force");
        let prune_dind = verbs
            .iter()
            .enumerate()
            .filter(|(_, command)| command.contains("volume prune --all --force"))
            .nth(1)
            .map(|(index, _)| index)
            .unwrap();
        assert!(position("network rm -- network-id") < prune_workspace);
        assert!(prune_workspace < prune_dind);
        assert!(verbs.iter().all(|command| !command.contains("volume rm")));
        // Evidence landed on disk.
        assert_eq!(
            std::fs::read_to_string(&report.export.runner_log).unwrap(),
            "LOGS-R\n"
        );
        assert_eq!(
            std::fs::read_to_string(&report.export.dind_log).unwrap(),
            "LOGS-D\n"
        );
        Supervision::new(identity(), &state)
            .release_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn cleanup_replay_reuses_full_capture_after_runner_removal() {
        let state = temp_state("cleanup-replay-after-runner-removal");
        let mut first_runner = ScriptRunner::scripted(vec![
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::ok("runner stopped\n"),
            ScriptRunner::ok("captured runner logs\n"),
            ScriptRunner::ok("captured DinD logs\n"),
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let captured = prepare_cleanup(&mut first_runner, &identity(), &state).unwrap();
        assert!(captured.failures.is_empty(), "{captured:?}");

        // Teardown accepted runner removal, then the process crashed before
        // removing DinD. Recreate that Docker boundary and replay cleanup.
        let mut crash_boundary = ScriptRunner::scripted(vec![ScriptRunner::ok("runner removed\n")]);
        let mut removal_failures = Vec::new();
        remove_container(&mut crash_boundary, "runner-id", &mut removal_failures);
        assert!(removal_failures.is_empty());
        assert_eq!(
            crash_boundary.seen[0].last().map(String::as_str),
            Some("runner-id")
        );

        let runner_missing = format!("Error: No such object: {}", identity().runner_container());
        let mut retry_runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, &runner_missing),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::fail(1, &runner_missing),
            owned_container_inspect("dind-id", ROLE_DIND),
            owned_network_inspect("network-id"),
            owned_volume_inspect(super::super::runner::DockerCreateTarget::WorkspaceVolume),
            ScriptRunner::ok(&format!("{}\n", identity().workspace_volume())),
            owned_volume_snapshot(super::super::runner::DockerCreateTarget::WorkspaceVolume),
            owned_volume_inspect(super::super::runner::DockerCreateTarget::DindDataVolume),
            ScriptRunner::ok(&format!("{}\n", identity().dind_data_volume())),
            owned_volume_snapshot(super::super::runner::DockerCreateTarget::DindDataVolume),
            ScriptRunner::ok("dind stopped\n"),
            ScriptRunner::ok("dind removed\n"),
            ScriptRunner::ok("network-id\n"),
            owned_volume_inspect(super::super::runner::DockerCreateTarget::WorkspaceVolume),
            ScriptRunner::ok(&format!("{}\n", identity().workspace_volume())),
            owned_volume_snapshot(super::super::runner::DockerCreateTarget::WorkspaceVolume),
            ScriptRunner::ok("workspace\n"),
            missing_volume(&identity().workspace_volume()),
            ScriptRunner::ok(""),
            owned_volume_inspect(super::super::runner::DockerCreateTarget::DindDataVolume),
            ScriptRunner::ok(&format!("{}\n", identity().dind_data_volume())),
            owned_volume_snapshot(super::super::runner::DockerCreateTarget::DindDataVolume),
            ScriptRunner::ok("dind-data\n"),
            missing_volume(&identity().dind_data_volume()),
            ScriptRunner::ok(""),
        ]);
        let report = owned_cleanup(&mut retry_runner, &identity(), &state).unwrap();
        assert!(report.confirmed(), "{report:?}");
        assert_eq!(
            std::fs::read(&report.export.runner_log).unwrap(),
            b"captured runner logs\n"
        );
        assert_eq!(
            std::fs::read(&report.export.dir.join("capture.complete")).unwrap(),
            DIAGNOSTICS_COMPLETE_MARKER
        );
        let commands: Vec<String> = retry_runner
            .seen
            .iter()
            .map(|args| args.join(" "))
            .collect();
        assert!(commands.iter().all(|command| !command.starts_with("logs ")));
        assert!(!commands
            .iter()
            .any(|command| command.contains("stop -t 30 -- runner-id")));
        assert!(!commands
            .iter()
            .any(|command| command.contains("rm --force -- runner-id")));
        assert!(commands
            .iter()
            .any(|command| command.contains("stop -t 30 -- dind-id")));
        assert!(commands
            .iter()
            .any(|command| command.contains("rm --force -- dind-id")));

        Supervision::new(identity(), &state)
            .release_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn cleanup_replay_rejects_full_marker_with_missing_artifact() {
        let state = temp_state("cleanup-replay-invalid-full-capture");
        let diagnostics = SecureDiagnosticDir::open(&state).unwrap();
        diagnostics
            .write_file("capture.complete", DIAGNOSTICS_COMPLETE_MARKER)
            .unwrap();
        for (name, contents) in [
            ("runner.log", b"runner logs".as_slice()),
            ("dind.log", b"dind logs".as_slice()),
            ("dind.inspect.json", b"[]".as_slice()),
        ] {
            diagnostics.write_file(name, contents).unwrap();
        }

        let runner_missing = format!("Error: No such object: {}", identity().runner_container());
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, &runner_missing),
            owned_container_inspect("dind-id", ROLE_DIND),
        ]);
        let export = prepare_cleanup(&mut runner, &identity(), &state).unwrap();
        assert_eq!(export.failures.len(), 1, "{export:?}");
        assert!(export.failures[0].contains("missing or insecure artifacts"));
        assert!(!export.dir.join("capture.complete").exists());
        assert_eq!(
            runner.seen.len(),
            2,
            "invalid proof blocks all cleanup calls"
        );

        let report = finish_cleanup(&mut runner, &identity(), export);
        assert!(!report.confirmed());
        assert!(report.failures.is_empty());
        assert_eq!(runner.seen.len(), 2);
        Supervision::new(identity(), &state)
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn cleanup_preserves_resources_when_diagnostics_fail() {
        let state = temp_state("failures");
        let mut runner = ScriptRunner::scripted(vec![
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::fail(1, "boom"), // stop runner fails
            ScriptRunner::ok("LOGS-R\n"),
            ScriptRunner::fail(1, "gone"), // logs dind fails
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let export = prepare_cleanup(&mut runner, &identity(), &state).unwrap();
        assert_eq!(export.failures.len(), 2);
        let report = finish_cleanup(&mut runner, &identity(), export);
        assert!(!report.confirmed());
        assert_eq!(report.export.failures.len(), 2);
        assert!(report.failures.is_empty());
        // Capture/stop ran, but the release-gated finish method skipped every
        // removal. Teardown failures alone are not the cleanup result.
        assert_eq!(runner.seen.len(), 7);
        assert!(runner
            .seen
            .iter()
            .all(|args| !args.iter().any(|arg| arg == "rm")));
        Supervision::new(identity(), &state)
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_runner_stop_never_completes_diagnostics_and_retry_recaptures() {
        let state = temp_state("stop-failure-retry");
        let mut first_runner = ScriptRunner::scripted(vec![
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::fail(1, "runner stop failed"),
            ScriptRunner::ok("runner logs before stop\n"),
            ScriptRunner::ok("dind logs before stop\n"),
            ScriptRunner::ok(r#"[{"Id":"runner-before-stop"}]"#),
            ScriptRunner::ok(r#"[{"Id":"dind-before-stop"}]"#),
        ]);
        let first = prepare_cleanup(&mut first_runner, &identity(), &state).unwrap();
        assert!(first
            .failures
            .iter()
            .any(|failure| failure.contains("stop")));
        assert!(!Supervision::new(identity(), &state)
            .diagnostics_complete()
            .unwrap());
        assert!(!first.dir.join("capture.complete").exists());
        assert_eq!(
            std::fs::read(&first.runner_log).unwrap(),
            b"runner logs before stop\n"
        );

        let mut retry_runner = ScriptRunner::scripted(vec![
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::ok("runner stopped\n"),
            ScriptRunner::ok("runner logs after stop\n"),
            ScriptRunner::ok("dind logs after stop\n"),
            ScriptRunner::ok(r#"[{"Id":"runner-after-stop"}]"#),
            ScriptRunner::ok(r#"[{"Id":"dind-after-stop"}]"#),
        ]);
        let retry = prepare_cleanup(&mut retry_runner, &identity(), &state).unwrap();
        assert!(retry.failures.is_empty(), "{retry:?}");
        assert_eq!(retry_runner.seen.len(), 7);
        assert!(Supervision::new(identity(), &state)
            .diagnostics_complete()
            .unwrap());
        assert_eq!(
            std::fs::read(&retry.runner_log).unwrap(),
            b"runner logs after stop\n"
        );
        assert_eq!(
            std::fs::read(&retry.dind_log).unwrap(),
            b"dind logs after stop\n"
        );
        assert!(std::fs::read_to_string(&retry.runner_inspect)
            .unwrap()
            .contains("runner-after-stop"));
        Supervision::new(identity(), &state)
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_runner_stop_invalidates_prior_marker_and_recaptures() {
        let state = temp_state("stop-failure-invalidates-marker");
        let mut first_runner = ScriptRunner::scripted(vec![
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::ok("runner stopped\n"),
            ScriptRunner::ok("runner logs before restart\n"),
            ScriptRunner::ok("dind logs before restart\n"),
            ScriptRunner::ok(r#"[{"Id":"runner-before-restart"}]"#),
            ScriptRunner::ok(r#"[{"Id":"dind-before-restart"}]"#),
        ]);
        let first = prepare_cleanup(&mut first_runner, &identity(), &state).unwrap();
        assert!(first.failures.is_empty(), "{first:?}");
        assert!(first.dir.join("capture.complete").is_file());

        let mut retry_runner = ScriptRunner::scripted(vec![
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::fail(1, "runner stop failed"),
            ScriptRunner::ok("runner logs while stop is uncertain\n"),
            ScriptRunner::ok("dind logs while stop is uncertain\n"),
            ScriptRunner::ok(r#"[{"Id":"runner-after-restart"}]"#),
            ScriptRunner::ok(r#"[{"Id":"dind-after-restart"}]"#),
        ]);
        let retry = prepare_cleanup(&mut retry_runner, &identity(), &state).unwrap();
        assert!(retry
            .failures
            .iter()
            .any(|failure| failure.contains("stop")));
        assert_eq!(
            retry_runner.seen.len(),
            7,
            "retry must recapture every artifact"
        );
        assert!(!retry.dir.join("capture.complete").exists());
        assert!(!Supervision::new(identity(), &state)
            .diagnostics_complete()
            .unwrap());
        assert_eq!(
            std::fs::read(&retry.runner_log).unwrap(),
            b"runner logs while stop is uncertain\n"
        );
        assert!(std::fs::read_to_string(&retry.runner_inspect)
            .unwrap()
            .contains("runner-after-restart"));

        Supervision::new(identity(), &state)
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn diagnostics_redact_container_env_before_writing() {
        let state = temp_state("redact");
        let inspect = r#"[{
            "Id": "abc123", "Name": "/velnor-scaleset-runner-x",
            "Config": {
                "Labels": {"velnor.scaleset.ownership": "7/velnor-set-0007"},
                "Env": ["ACTIONS_RUNNER_INPUT_JITCONFIG=live-jit-blob-bytes", "PATH=/usr/bin"]
            },
            "State": {
                "Status": "exited", "ExitCode": 0,
                "Health": {
                    "Status": "healthy", "FailingStreak": 0,
                    "Log": [{"Output": "healthcheck-echoed-jit-secret"}]
                }
            },
            "NetworkSettings": {"Networks": {}},
            "HostConfig": {"Binds": []}
        }]"#;
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("LOGS-R\n"),
            ScriptRunner::ok("LOGS-D\n"),
            ScriptRunner::ok(inspect),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let export = export_diagnostics(&mut runner, &identity(), &state).unwrap();
        assert!(export.failures.is_empty(), "{export:?}");
        let persisted = std::fs::read_to_string(&export.runner_inspect).unwrap();
        assert!(
            !persisted.contains("live-jit-blob-bytes"),
            "JIT blob must not reach disk: {persisted}"
        );
        assert!(
            !persisted.contains("healthcheck-echoed-jit-secret"),
            "healthcheck output must not reach disk: {persisted}"
        );
        assert!(!persisted.contains("\"Log\""), "{persisted}");
        assert!(persisted.contains("\"Status\": \"healthy\""), "{persisted}");
        assert!(!persisted.contains("\"Env\""), "{persisted}");
        assert!(!persisted.contains("HostConfig"), "{persisted}");
        // Evidence survives: identity, labels, state, networks.
        assert!(persisted.contains("abc123"), "{persisted}");
        assert!(
            persisted.contains("velnor.scaleset.ownership"),
            "{persisted}"
        );
        assert!(persisted.contains("\"State\""), "{persisted}");
        assert!(persisted.contains("\"NetworkSettings\""), "{persisted}");
        let persisted_json: serde_json::Value = serde_json::from_str(&persisted).unwrap();
        let health = &persisted_json[0]["State"]["Health"];
        assert!(health.get("Log").is_none(), "{persisted_json}");
        assert_eq!(health["Status"], "healthy", "{persisted_json}");
        assert_eq!(health["FailingStreak"], 0, "{persisted_json}");
        Supervision::new(identity(), &state)
            .release_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn diagnostic_completion_marker_is_host_only_and_persisted() {
        let state = temp_state("diagnostics-marker");
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("LOGS-R\n"),
            ScriptRunner::ok("LOGS-D\n"),
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let export = export_diagnostics(&mut runner, &identity(), &state).unwrap();
        let supervision = Supervision::new(identity(), &state);
        let marker = export.dir.join("capture.complete");

        assert!(export.failures.is_empty(), "{export:?}");
        assert_eq!(std::fs::read(&marker).unwrap(), b"velnor-diagnostics-v1\n");
        assert!(!state.join("diagnostics/capture.complete").exists());
        assert!(supervision.diagnostics_complete().unwrap());
        assert_eq!(
            supervision.diagnostic_completion_kind().unwrap(),
            Some(DiagnosticCompletionKind::FullCapture)
        );
        assert!(marker.exists());

        supervision
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn no_runner_completion_marker_is_durable_and_replayable() {
        let state = temp_state("no-runner-complete");
        let diagnostics = SecureDiagnosticDir::open(&state).unwrap();
        diagnostics
            .write_file("runner.log", b"partial runner output")
            .unwrap();
        diagnostics
            .write_file("dind.log", b"partial dind output")
            .unwrap();

        let supervision = Supervision::new(identity(), &state);
        const DIND_ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let dind_inspect = format!(
            r#"[{{"Id":"{DIND_ID}","Config":{{"Env":["DIND_SECRET=dind-launch-secret"]}},"State":{{"Status":"running"}}}}]"#
        );
        assert!(redact_inspect(&dind_inspect).is_some(), "{dind_inspect}");
        let runner_missing = format!("Error: No such object: {}", identity().runner_container());
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, &runner_missing),
            owned_container_inspect(DIND_ID, ROLE_DIND),
            ScriptRunner::ok(&dind_inspect),
        ]);
        let export = supervision
            .complete_diagnostics_without_runner_unchecked(&mut runner)
            .unwrap();
        assert!(export.failures.is_empty(), "{export:?}");
        assert!(supervision.diagnostics_complete().unwrap());
        assert_eq!(
            supervision.diagnostic_completion_kind().unwrap(),
            Some(DiagnosticCompletionKind::RunnerAbsent)
        );
        let diagnostics = SecureDiagnosticDir::open_existing(&state).unwrap().unwrap();
        assert_eq!(
            diagnostics.read_file("capture.complete").unwrap().unwrap(),
            NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER
        );
        assert!(diagnostics.read_file("runner.log").unwrap().is_none());
        assert_eq!(
            diagnostics.read_file("dind.log").unwrap().unwrap(),
            b"partial dind output"
        );
        let persisted_dind = diagnostics.read_file("dind.inspect.json").unwrap().unwrap();
        let persisted_dind = String::from_utf8(persisted_dind).unwrap();
        assert!(persisted_dind.contains(DIND_ID), "{persisted_dind}");
        assert!(!persisted_dind.contains("DIND_SECRET"), "{persisted_dind}");
        assert!(
            !persisted_dind.contains("dind-launch-secret"),
            "{persisted_dind}"
        );
        assert!(!persisted_dind.contains("\"Env\""), "{persisted_dind}");

        // Recovery rechecks runner absence, then reuses the no-runner marker
        // without recapturing any artifacts.
        let runner_missing = format!("Error: No such object: {}", identity().runner_container());
        let mut replay = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, &runner_missing),
            owned_container_inspect(DIND_ID, ROLE_DIND),
        ]);
        let replayed = supervision
            .complete_diagnostics_without_runner_unchecked(&mut replay)
            .unwrap();
        assert!(replayed.failures.is_empty(), "{replayed:?}");
        assert_eq!(
            replay.seen.len(),
            2,
            "runner absence and DinD ownership must be rechecked"
        );
        supervision
            .release_state(&test_cleanup_authorization(&state))
            .unwrap();
        assert!(!state.exists());
        assert!(!diagnostics.path.exists());
    }

    #[test]
    fn no_runner_diagnostics_capture_only_an_exactly_owned_dind() {
        let exact_state = temp_state("no-runner-exact-owner");
        let exact = Supervision::new(identity(), &exact_state);
        let mut owned = OwnershipRunner::with_dind_labels(container_labels(ROLE_DIND));
        let export = exact
            .complete_diagnostics_without_runner_unchecked(&mut owned)
            .unwrap();
        assert!(export.failures.is_empty(), "{export:?}");
        assert!(exact.diagnostics_complete().unwrap());
        assert!(
            owned.dind_exists(),
            "capture must not remove the owned DinD"
        );
        assert!(owned.saw("logs"));
        assert!(owned
            .seen
            .iter()
            .any(|args| args.first().is_some_and(|verb| verb == "inspect")
                && args.last().is_some_and(|id| id == "dind-id")));
        exact
            .release_owned_state(&test_cleanup_authorization(&exact_state))
            .unwrap();
        std::fs::remove_dir_all(exact_state.parent().unwrap()).unwrap();

        let mut foreign_owner = container_labels(ROLE_DIND);
        foreign_owner.insert(
            super::super::ownership::OWNERSHIP_LABEL.to_string(),
            "7/another-runner".to_string(),
        );
        let mut missing_owner = container_labels(ROLE_DIND);
        missing_owner.remove(super::super::ownership::OWNERSHIP_LABEL);
        let mut wrong_role = container_labels(ROLE_DIND);
        wrong_role.insert(
            super::super::ownership::WORKER_ROLE_LABEL.to_string(),
            ROLE_RUNNER.to_string(),
        );
        let mut wrong_runner = container_labels(ROLE_DIND);
        wrong_runner.insert(
            super::super::ownership::RUNNER_LABEL.to_string(),
            "another-runner".to_string(),
        );
        for (name, labels) in [
            ("foreign-owner", foreign_owner),
            ("missing-owner", missing_owner),
            ("wrong-role", wrong_role),
            ("wrong-runner", wrong_runner),
        ] {
            let state = temp_state(&format!("no-runner-{name}"));
            let supervision = Supervision::new(identity(), &state);
            let mut foreign = OwnershipRunner::with_dind_labels(labels);
            assert!(
                supervision
                    .complete_diagnostics_without_runner_unchecked(&mut foreign)
                    .is_err(),
                "{name} labels must fail closed"
            );
            assert!(foreign.dind_exists(), "foreign same-name DinD was changed");
            assert!(!foreign.saw("logs"), "foreign logs must not be captured");
            assert!(!foreign.saw("rm"), "foreign DinD must not be removed");
            assert!(!supervision.diagnostics_complete().unwrap());
            supervision
                .release_owned_state(&test_cleanup_authorization(&state))
                .unwrap();
            std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn terminal_teardown_checks_both_container_owners_before_any_mutation() {
        let mut foreign_owner = container_labels(ROLE_DIND);
        foreign_owner.insert(
            super::super::ownership::OWNERSHIP_LABEL.to_string(),
            "7/another-runner".to_string(),
        );
        let mut missing_owner = container_labels(ROLE_DIND);
        missing_owner.remove(super::super::ownership::OWNERSHIP_LABEL);
        let mut wrong_role = container_labels(ROLE_DIND);
        wrong_role.insert(
            super::super::ownership::WORKER_ROLE_LABEL.to_string(),
            ROLE_RUNNER.to_string(),
        );

        for (name, labels) in [
            ("foreign-owner", foreign_owner),
            ("missing-owner", missing_owner),
            ("wrong-role", wrong_role),
        ] {
            let mut runner = OwnershipRunner::with_dind_labels(labels);
            let failures = teardown_owned_resources(&mut runner, &identity());
            assert!(!failures.is_empty(), "{name} labels must block teardown");
            assert!(runner.dind_exists(), "foreign same-name DinD was removed");
            assert!(!runner.saw("stop"), "foreign DinD must not be stopped");
            assert!(!runner.saw("rm"), "foreign DinD must not be removed");
        }

        let mut owned = OwnershipRunner::with_dind_labels(container_labels(ROLE_DIND));
        let failures = teardown_owned_resources(&mut owned, &identity());
        assert!(failures.is_empty(), "{failures:?}");
        assert!(!owned.dind_exists(), "exact-owner DinD should be removed");
        assert!(owned.saw("stop"));
        assert!(owned.saw("rm"));

        let mut absent = OwnershipRunner::with_dind_labels(container_labels(ROLE_DIND));
        absent.containers.clear();
        let failures = teardown_owned_resources(&mut absent, &identity());
        assert!(
            failures.is_empty(),
            "missing containers are already removed"
        );
        assert!(!absent.saw("stop"));
        assert!(!absent.saw("rm"));
    }

    #[test]
    fn terminal_teardown_preflights_network_and_volume_owners_before_mutation() {
        let mut foreign_network = OwnershipRunner::with_owned_resources();
        foreign_network.networks[0].labels.insert(
            super::super::ownership::OWNERSHIP_LABEL.to_string(),
            "7/foreign".to_string(),
        );
        let failures = teardown_owned_resources(&mut foreign_network, &identity());
        assert!(!failures.is_empty(), "foreign network must block teardown");
        assert_eq!(foreign_network.networks[0].id, "network-id");
        assert!(foreign_network.dind_exists());
        assert!(foreign_network.volume_exists(&identity().workspace_volume()));
        assert!(!foreign_network.saw("stop"));
        assert!(!foreign_network.saw("rm"));
        assert!(!foreign_network.saw_pair("network", "rm"));
        assert!(!foreign_network.saw_pair("volume", "prune"));

        let mut foreign_volume = OwnershipRunner::with_owned_resources();
        foreign_volume.volumes[0].labels.insert(
            super::super::ownership::OWNERSHIP_LABEL.to_string(),
            "7/foreign".to_string(),
        );
        let failures = teardown_owned_resources(&mut foreign_volume, &identity());
        assert!(!failures.is_empty(), "foreign volume must block teardown");
        assert!(foreign_volume.volume_exists(&identity().workspace_volume()));
        assert!(foreign_volume.dind_exists());
        assert!(!foreign_volume.saw("stop"));
        assert!(!foreign_volume.saw("rm"));
        assert!(!foreign_volume.saw_pair("network", "rm"));
        assert!(!foreign_volume.saw_pair("volume", "prune"));
    }

    #[test]
    fn terminal_teardown_uses_inspected_ids_after_same_name_replacements() {
        let mut container_replacement = OwnershipRunner::with_owned_resources();
        container_replacement.replace_dind_after_preflight = true;
        let failures = teardown_owned_resources(&mut container_replacement, &identity());
        assert!(
            failures.is_empty(),
            "missing original ID is already removed: {failures:?}"
        );
        assert!(container_replacement.dind_exists());
        assert_eq!(
            container_replacement.dind_id(),
            Some("foreign-replacement-id")
        );
        assert!(container_replacement.seen.iter().any(|args| {
            args.first().is_some_and(|arg| arg == "stop")
                && args.last().is_some_and(|arg| arg == "dind-id")
        }));
        assert!(!container_replacement.seen.iter().any(|args| {
            args.iter().any(|arg| arg == "foreign-replacement-id")
                && (args.first().is_some_and(|arg| arg == "stop" || arg == "rm"))
        }));

        let mut network_replacement = OwnershipRunner::with_owned_resources();
        network_replacement.replace_network_after_preflight = true;
        let failures = teardown_owned_resources(&mut network_replacement, &identity());
        assert!(
            failures.is_empty(),
            "missing original network ID is removed: {failures:?}"
        );
        assert_eq!(
            network_replacement.networks[0].id,
            "foreign-network-replacement-id"
        );
        assert!(network_replacement.seen.iter().any(|args| {
            args.first().is_some_and(|arg| arg == "network")
                && args.get(1).is_some_and(|arg| arg == "rm")
                && args.last().is_some_and(|arg| arg == "network-id")
        }));
        assert!(!network_replacement.seen.iter().any(|args| {
            args.first().is_some_and(|arg| arg == "network")
                && args.get(1).is_some_and(|arg| arg == "rm")
                && args
                    .last()
                    .is_some_and(|arg| arg == "foreign-network-replacement-id")
        }));
    }

    #[test]
    fn volume_prune_uses_full_exact_labels_and_preserves_active_or_duplicate_volumes() {
        let worker = identity();
        let mut runner = OwnershipRunner::with_owned_resources();
        runner.volumes[0].active = true;
        let mut duplicate_labels = worker.labels();
        duplicate_labels.insert(
            "velnor.scaleset.volume".to_string(),
            "workspace".to_string(),
        );
        duplicate_labels.insert(
            "velnor.scaleset.volume-name".to_string(),
            "duplicate-workspace-volume".to_string(),
        );
        runner.volumes.push(MockVolume {
            name: "duplicate-workspace-volume".to_string(),
            labels: duplicate_labels,
            active: false,
        });

        let failures = teardown_owned_resources(&mut runner, &worker);
        assert!(
            !failures.is_empty(),
            "active workspace volume keeps cleanup unconfirmed"
        );
        assert!(runner.volume_exists(&worker.workspace_volume()));
        assert!(runner.volume_exists("duplicate-workspace-volume"));
        let workspace_prune = runner
            .seen
            .iter()
            .find(|args| {
                args.first().is_some_and(|arg| arg == "volume")
                    && args.get(1).is_some_and(|arg| arg == "prune")
                    && args.iter().any(|arg| {
                        arg == &format!(
                            "label=velnor.scaleset.volume-name={}",
                            worker.workspace_volume()
                        )
                    })
            })
            .expect("workspace prune uses the deterministic name label");
        assert!(workspace_prune.iter().any(|arg| arg == "--all"));
        assert!(workspace_prune.iter().any(|arg| arg == "--force"));
        for (key, value) in worker.labels() {
            assert!(
                workspace_prune
                    .iter()
                    .any(|arg| arg == &format!("label={key}={value}")),
                "missing exact owner filter {key}={value}: {workspace_prune:?}"
            );
        }
        for (key, value) in [
            ("velnor.scaleset.volume", "workspace".to_string()),
            ("velnor.scaleset.volume-name", worker.workspace_volume()),
        ] {
            assert!(workspace_prune
                .iter()
                .any(|arg| arg == &format!("label={key}={value}")));
        }
    }

    #[test]
    fn terminal_teardown_rejects_duplicate_exact_volume_claim_before_mutation() {
        let worker = identity();
        let mut runner = OwnershipRunner::with_owned_resources();
        runner.volumes.push(MockVolume {
            name: "duplicate-docker-volume-id".to_string(),
            labels: runner.volumes[0].labels.clone(),
            active: false,
        });

        let failures = teardown_owned_resources(&mut runner, &worker);
        assert!(
            !failures.is_empty(),
            "duplicate exact-label volume claim must block teardown"
        );
        assert!(runner.volume_exists(&worker.workspace_volume()));
        assert!(runner.volume_exists("duplicate-docker-volume-id"));
        assert!(runner.dind_exists());
        assert!(!runner.saw("stop"));
        assert!(!runner.saw("rm"));
        assert!(!runner.saw_pair("network", "rm"));
        assert!(!runner.saw_pair("volume", "prune"));
        assert!(runner.saw_pair("volume", "ls"));
    }

    #[test]
    fn foreign_workspace_replacement_after_preflight_is_not_pruned() {
        let worker = identity();
        let mut runner = OwnershipRunner::with_owned_resources();
        runner.replace_workspace_after_preflight = true;
        let failures = teardown_owned_resources(&mut runner, &worker);
        assert!(
            !failures.is_empty(),
            "replacement must keep cleanup unconfirmed"
        );
        assert!(runner.volume_exists(&worker.workspace_volume()));
        assert!(runner.volumes.iter().any(|volume| {
            volume.name == worker.workspace_volume()
                && volume.labels.get(super::super::ownership::OWNERSHIP_LABEL)
                    == Some(&"7/foreign".to_string())
        }));
        assert!(!runner.seen.iter().any(|args| {
            args.first().is_some_and(|arg| arg == "volume")
                && args.get(1).is_some_and(|arg| arg == "prune")
                && args.iter().any(|arg| {
                    arg == &format!(
                        "label=velnor.scaleset.volume-name={}",
                        worker.workspace_volume()
                    )
                })
        }));
    }

    #[test]
    fn diagnostic_retry_preserves_each_successful_capture() {
        let state = temp_state("diagnostics-partial-retry");
        let mut first_runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("runner evidence from first attempt\n"),
            ScriptRunner::fail(1, "dind logs temporarily unavailable"),
            ScriptRunner::ok(r#"[{"Id":"runner-first"}]"#),
            ScriptRunner::ok(r#"[{"Id":"dind-first"}]"#),
        ]);
        let first = export_diagnostics(&mut first_runner, &identity(), &state).unwrap();
        assert_eq!(first.failures.len(), 1, "{first:?}");
        assert_eq!(
            std::fs::read(&first.runner_log).unwrap(),
            b"runner evidence from first attempt\n"
        );
        assert!(!first.dir.join("capture.complete").exists());

        let mut retry_runner =
            ScriptRunner::scripted(vec![ScriptRunner::ok("dind evidence from retry\n")]);
        let retry = export_diagnostics(&mut retry_runner, &identity(), &state).unwrap();
        assert!(retry.failures.is_empty(), "{retry:?}");
        assert_eq!(
            retry_runner.seen.len(),
            1,
            "only missing dind logs recaptured"
        );
        assert_eq!(
            std::fs::read(&retry.runner_log).unwrap(),
            b"runner evidence from first attempt\n"
        );
        assert_eq!(
            std::fs::read(&retry.dind_log).unwrap(),
            b"dind evidence from retry\n"
        );
        assert!(std::fs::read_to_string(&retry.runner_inspect)
            .unwrap()
            .contains("runner-first"));
        assert!(std::fs::read_to_string(&retry.dind_inspect)
            .unwrap()
            .contains("dind-first"));
        assert!(Supervision::new(identity(), &state)
            .release_owned_state(&test_cleanup_authorization(&state))
            .is_ok());
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn no_runner_retry_adopts_published_dind_log_and_preserves_it_on_capture_failure() {
        let state = temp_state("no-runner-partial-retry");
        let diagnostics = SecureDiagnosticDir::open(&state).unwrap();
        diagnostics
            .write_file("dind.log", b"previous atomically published evidence\n")
            .unwrap();
        let supervision = Supervision::new(identity(), &state);
        let mut first_runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, "Error: No such object: velnor-scaleset-runner"),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::fail(1, "temporary inspect failure"),
        ]);
        let first = supervision
            .complete_diagnostics_without_runner_unchecked(&mut first_runner)
            .unwrap();
        assert_eq!(first.failures.len(), 1, "{first:?}");
        assert_eq!(
            std::fs::read(&first.dind_log).unwrap(),
            b"previous atomically published evidence\n"
        );
        assert_eq!(
            std::fs::read(first.dir.join("dind.log.complete")).unwrap(),
            artifact_marker_contents(
                "dind.log",
                blake3::hash(b"previous atomically published evidence\n").as_bytes()
            )
        );
        assert_eq!(first_runner.seen.len(), 3);
        assert!(first_runner
            .seen
            .iter()
            .all(|args| !args.iter().any(|arg| arg == "logs")));
        assert!(!first.dir.join("capture.complete").exists());

        let mut retry_runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, "Error: No such object: velnor-scaleset-runner"),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::ok(r#"[{"Id":"dind-retry"}]"#),
        ]);
        let retry = supervision
            .complete_diagnostics_without_runner_unchecked(&mut retry_runner)
            .unwrap();
        assert!(retry.failures.is_empty(), "{retry:?}");
        assert_eq!(
            retry_runner.seen.len(),
            3,
            "only missing dind inspect recaptured"
        );
        assert_eq!(
            std::fs::read(&retry.dind_log).unwrap(),
            b"previous atomically published evidence\n"
        );
        assert!(std::fs::read_to_string(&retry.dind_inspect)
            .unwrap()
            .contains("dind-retry"));
        assert!(SecureDiagnosticDir::open(&state)
            .unwrap()
            .artifact_capture_complete("dind.inspect.json")
            .unwrap());
        assert!(supervision.diagnostics_complete().unwrap());
        assert!(supervision
            .release_owned_state(&test_cleanup_authorization(&state))
            .is_ok());
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn no_runner_completion_adopts_atomic_dind_log_when_dind_is_gone() {
        let state = temp_state("no-runner-dind-gone");
        let diagnostics = SecureDiagnosticDir::open(&state).unwrap();
        diagnostics
            .write_file("dind.log", b"captured before DinD disappeared\n")
            .unwrap();

        let dind_missing = format!("Error: No such object: {}", identity().dind_container());
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, "Error: No such object: velnor-scaleset-runner"),
            ScriptRunner::fail(1, &dind_missing),
        ]);
        let supervision = Supervision::new(identity(), &state);
        let export = supervision
            .complete_diagnostics_without_runner_unchecked(&mut runner)
            .unwrap();

        assert!(export.failures.is_empty(), "{export:?}");
        assert_eq!(runner.seen.len(), 2);
        assert_eq!(
            std::fs::read(&export.dind_log).unwrap(),
            b"captured before DinD disappeared\n"
        );
        assert!(!export.dind_inspect.exists());
        assert_eq!(
            std::fs::read(export.dir.join("dind.log.complete")).unwrap(),
            artifact_marker_contents(
                "dind.log",
                blake3::hash(b"captured before DinD disappeared\n").as_bytes()
            )
        );
        assert!(supervision.diagnostics_complete().unwrap());
        supervision
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn completion_marker_kinds_are_not_interchangeable() {
        let no_runner_state = temp_state("marker-kind-no-runner");
        let no_runner_diagnostics = SecureDiagnosticDir::open(&no_runner_state).unwrap();
        no_runner_diagnostics
            .write_file("capture.complete", NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER)
            .unwrap();
        let no_runner_supervision = Supervision::new(identity(), &no_runner_state);
        assert_eq!(
            no_runner_supervision.diagnostic_completion_kind().unwrap(),
            Some(DiagnosticCompletionKind::RunnerAbsent)
        );
        let mut full_capture = ScriptRunner::scripted(vec![]);
        let full_capture_result =
            export_diagnostics(&mut full_capture, &identity(), &no_runner_state);
        assert!(full_capture_result
            .unwrap_err()
            .to_string()
            .contains("runner-absent"));
        assert!(full_capture.seen.is_empty());
        Supervision::new(identity(), &no_runner_state)
            .release_owned_state(&test_cleanup_authorization(&no_runner_state))
            .unwrap();
        std::fs::remove_dir_all(no_runner_state.parent().unwrap()).unwrap();

        let full_state = temp_state("marker-kind-full");
        let full_diagnostics = SecureDiagnosticDir::open(&full_state).unwrap();
        full_diagnostics
            .write_file("capture.complete", DIAGNOSTICS_COMPLETE_MARKER)
            .unwrap();
        let supervision = Supervision::new(identity(), &full_state);
        assert_eq!(
            supervision.diagnostic_completion_kind().unwrap(),
            Some(DiagnosticCompletionKind::FullCapture)
        );
        let mut no_runner_capture = ScriptRunner::scripted(vec![ScriptRunner::fail(
            1,
            "Error: No such object: velnor-scaleset-runner",
        )]);
        let no_runner_result =
            supervision.complete_diagnostics_without_runner_unchecked(&mut no_runner_capture);
        assert!(no_runner_result
            .unwrap_err()
            .to_string()
            .contains("full diagnostics marker"));
        assert_eq!(no_runner_capture.seen.len(), 1);
        supervision
            .release_owned_state(&test_cleanup_authorization(&full_state))
            .unwrap();
        std::fs::remove_dir_all(full_state.parent().unwrap()).unwrap();
    }

    #[test]
    fn full_capture_recapture_failure_preserves_prior_dind_log() {
        let state = temp_state("full-capture-dind-retry");
        let diagnostics = SecureDiagnosticDir::open(&state).unwrap();
        diagnostics
            .write_file("dind.log", b"previous complete dind capture\n")
            .unwrap();

        let mut first_runner = ScriptRunner::scripted(vec![
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::ok("runner stopped\n"),
            ScriptRunner::ok("runner logs first attempt\n"),
            ScriptRunner::fail(1, "temporary dind log failure"),
            ScriptRunner::ok(r#"[{"Id":"runner-first"}]"#),
            ScriptRunner::ok(r#"[{"Id":"dind-first"}]"#),
        ]);
        let first = prepare_cleanup(&mut first_runner, &identity(), &state).unwrap();
        assert_eq!(first.failures.len(), 1, "{first:?}");
        assert_eq!(
            std::fs::read(&first.dind_log).unwrap(),
            b"previous complete dind capture\n"
        );
        assert!(!first.dir.join("capture.complete").exists());

        let mut retry_runner = ScriptRunner::scripted(vec![
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::ok("runner stopped\n"),
            ScriptRunner::ok("runner logs final attempt\n"),
            ScriptRunner::ok("dind logs final attempt\n"),
            ScriptRunner::ok(r#"[{"Id":"runner-final"}]"#),
            ScriptRunner::ok(r#"[{"Id":"dind-final"}]"#),
        ]);
        let retry = prepare_cleanup(&mut retry_runner, &identity(), &state).unwrap();
        assert!(retry.failures.is_empty(), "{retry:?}");
        assert_eq!(
            std::fs::read(&retry.dind_log).unwrap(),
            b"dind logs final attempt\n"
        );
        assert_eq!(
            Supervision::new(identity(), &state)
                .diagnostic_completion_kind()
                .unwrap(),
            Some(DiagnosticCompletionKind::FullCapture)
        );

        Supervision::new(identity(), &state)
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn full_capture_replaces_runner_absent_marker_only_after_stop_and_recapture() {
        let state = temp_state("marker-kind-refresh-full");
        let diagnostics = SecureDiagnosticDir::open(&state).unwrap();
        diagnostics
            .write_file("capture.complete", NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER)
            .unwrap();
        let supervision = Supervision::new(identity(), &state);

        let mut failed_stop = ScriptRunner::scripted(vec![
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::fail(1, "runner still stopping"),
            ScriptRunner::ok("runner logs during uncertain stop\n"),
            ScriptRunner::ok("dind logs during uncertain stop\n"),
            ScriptRunner::ok(r#"[{"Id":"runner-uncertain"}]"#),
            ScriptRunner::ok(r#"[{"Id":"dind-uncertain"}]"#),
        ]);
        let incomplete = prepare_cleanup(&mut failed_stop, &identity(), &state).unwrap();
        assert!(!incomplete.failures.is_empty());
        assert!(!incomplete.dir.join("capture.complete").exists());
        assert_eq!(supervision.diagnostic_completion_kind().unwrap(), None);

        let mut stopped = ScriptRunner::scripted(vec![
            owned_container_inspect("runner-id", ROLE_RUNNER),
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::ok("runner stopped\n"),
            ScriptRunner::ok("runner logs after stop\n"),
            ScriptRunner::ok("dind logs after stop\n"),
            ScriptRunner::ok(r#"[{"Id":"runner-final"}]"#),
            ScriptRunner::ok(r#"[{"Id":"dind-final"}]"#),
        ]);
        let complete = prepare_cleanup(&mut stopped, &identity(), &state).unwrap();
        assert!(complete.failures.is_empty(), "{complete:?}");
        assert_eq!(
            supervision.diagnostic_completion_kind().unwrap(),
            Some(DiagnosticCompletionKind::FullCapture)
        );
        assert_eq!(stopped.seen.len(), 7);
        assert!(std::fs::read_to_string(&complete.runner_inspect)
            .unwrap()
            .contains("runner-final"));

        supervision
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn release_rejects_authorization_for_another_worker() {
        let state = temp_state("release-authorization-mismatch");
        let diagnostics = SecureDiagnosticDir::open(&state).unwrap();
        diagnostics
            .write_file("capture.complete", DIAGNOSTICS_COMPLETE_MARKER)
            .unwrap();
        let jit =
            super::super::create_owner_only_file_next_to(&state, b"other worker secret").unwrap();
        let jit_dir = jit.parent().unwrap().to_path_buf();
        let foreign = crate::scaleset::lane::test_release_authorization("7/other-worker", &state);
        assert!(Supervision::new(identity(), &state)
            .release_state(&foreign)
            .is_err());
        assert!(state.is_dir());
        assert!(diagnostics.path.is_dir());
        assert!(jit.is_file());
        assert!(jit_dir.is_dir());
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn completed_diagnostics_are_reused_after_containers_disappear() {
        let state = temp_state("diagnostics-replay");
        let mut first_runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("LOGS-R\n"),
            ScriptRunner::ok("LOGS-D\n"),
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let first = export_diagnostics(&mut first_runner, &identity(), &state).unwrap();
        assert!(first.failures.is_empty(), "{first:?}");

        // Docker may already have removed a container after the durable
        // capture. Replays trust the host-only marker and keep the evidence.
        let mut retry_runner = ScriptRunner::scripted(vec![]);
        let retry = export_diagnostics(&mut retry_runner, &identity(), &state).unwrap();
        assert!(retry.failures.is_empty(), "{retry:?}");
        assert!(retry_runner.seen.is_empty());
        assert_eq!(std::fs::read(&retry.runner_log).unwrap(), b"LOGS-R\n");
        assert_eq!(std::fs::read(&retry.dind_log).unwrap(), b"LOGS-D\n");

        Supervision::new(identity(), &state)
            .release_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_diagnostics_have_no_completion_marker_and_registry_cleanup_can_release() {
        let state = temp_state("diagnostics-incomplete");
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, "logs failed"),
            ScriptRunner::ok("LOGS-D\n"),
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let export = export_diagnostics(&mut runner, &identity(), &state).unwrap();
        let supervision = Supervision::new(identity(), &state);

        assert_eq!(export.failures.len(), 1);
        assert!(!supervision.diagnostics_complete().unwrap());
        assert!(!export.dir.join("capture.complete").exists());
        assert!(supervision
            .release_state(&test_cleanup_authorization(&state))
            .is_err());

        // Crash recovery may release incomplete diagnostics only after the
        // durable ownership registry authorizes cleanup.
        supervision
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        assert!(!state.exists());
        assert!(!export.dir.exists());
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn diagnostics_land_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let state = temp_state("perms");
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("LOGS-R\n"),
            ScriptRunner::ok("LOGS-D\n"),
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let export = export_diagnostics(&mut runner, &identity(), &state).unwrap();
        assert!(export.failures.is_empty(), "{export:?}");
        for file in [
            &export.runner_log,
            &export.dind_log,
            &export.runner_inspect,
            &export.dind_inspect,
        ] {
            let mode = std::fs::metadata(file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} has mode {mode:o}", file.display());
        }
        let dir_mode = std::fs::metadata(&export.dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "diagnostics dir has mode {dir_mode:o}");
        Supervision::new(identity(), &state)
            .release_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn diagnostics_ignore_worker_precreated_directory_symlink() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let state = temp_state("worker-diagnostics-symlink");
        let victim = state.parent().unwrap().join("victim");
        std::fs::create_dir(&victim).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sentinel = victim.join("sentinel");
        std::fs::write(&sentinel, b"untouched").unwrap();
        let sentinel_mode = std::fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777;
        symlink(&victim, state.join("diagnostics")).unwrap();

        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("LOGS-R\n"),
            ScriptRunner::ok("LOGS-D\n"),
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let export = export_diagnostics(&mut runner, &identity(), &state).unwrap();
        assert!(export.failures.is_empty(), "{export:?}");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"untouched");
        assert_eq!(
            std::fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777,
            sentinel_mode
        );
        assert_eq!(
            std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!victim.join("runner.log").exists());
        assert_eq!(export.dir.parent(), state.parent());
        assert_ne!(export.dir, state.join("diagnostics"));
        assert!(std::fs::symlink_metadata(state.join("diagnostics"))
            .unwrap()
            .file_type()
            .is_symlink());

        Supervision::new(identity(), &state)
            .release_state(&test_cleanup_authorization(&state))
            .unwrap();
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"untouched");
        assert_eq!(
            std::fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777,
            sentinel_mode
        );
        assert!(victim.is_dir());
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn diagnostics_fail_closed_on_host_sibling_symlink() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let state = temp_state("host-diagnostics-symlink");
        let victim = state.parent().unwrap().join("victim");
        std::fs::create_dir(&victim).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sentinel = victim.join("sentinel");
        std::fs::write(&sentinel, b"untouched").unwrap();
        let sentinel_mode = std::fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777;
        let diagnostics_name = diagnostics_dir_name(&state).unwrap();
        symlink(&victim, state.parent().unwrap().join(&diagnostics_name)).unwrap();

        let mut runner = ScriptRunner::scripted(vec![]);
        assert!(export_diagnostics(&mut runner, &identity(), &state).is_err());
        assert!(runner.seen.is_empty());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"untouched");
        assert_eq!(
            std::fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777,
            sentinel_mode
        );
        assert_eq!(
            std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o755
        );

        std::fs::remove_file(state.parent().unwrap().join(diagnostics_name)).unwrap();
        std::fs::remove_dir_all(&victim).unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn diagnostics_dir_fd_resists_parent_path_swap() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let state = temp_state("parent-swap");
        let parent = state.parent().unwrap().to_path_buf();
        let moved = parent.with_extension("moved");
        let victim = parent.with_extension("victim");
        std::fs::create_dir(&victim).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(victim.join("sentinel"), b"untouched").unwrap();
        let diagnostics = SecureDiagnosticDir::open(&state).unwrap();
        let name = diagnostics_dir_name(&state).unwrap();

        std::fs::rename(&parent, &moved).unwrap();
        symlink(&victim, &parent).unwrap();
        diagnostics
            .write_file("runner.log", b"captured logs")
            .unwrap();

        assert_eq!(
            std::fs::read(victim.join("sentinel")).unwrap(),
            b"untouched"
        );
        assert!(!victim.join(&name).exists());
        assert_eq!(
            std::fs::read(moved.join(name).join("runner.log")).unwrap(),
            b"captured logs"
        );

        std::fs::remove_file(parent).unwrap();
        std::fs::remove_dir_all(moved).unwrap();
        std::fs::remove_dir_all(victim).unwrap();
    }

    #[test]
    fn release_state_rejects_parent_path_swap() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let state = temp_state("release-parent-swap");
        let parent = state.parent().unwrap().to_path_buf();
        let moved = parent.with_extension("moved");
        let victim = parent.with_extension("victim");
        std::fs::create_dir(&victim).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o700)).unwrap();
        let sentinel = victim.join("sentinel");
        std::fs::write(&sentinel, b"untouched").unwrap();
        let sentinel_mode = std::fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777;
        let _diagnostics = SecureDiagnosticDir::open(&state).unwrap();

        std::fs::rename(&parent, &moved).unwrap();
        symlink(&victim, &parent).unwrap();
        assert!(Supervision::new(identity(), &state)
            .release_state(&test_cleanup_authorization(&state))
            .is_err());

        assert!(moved.join("worker").is_dir());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"untouched");
        assert_eq!(
            std::fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777,
            sentinel_mode
        );
        assert!(!victim.join(".velnor-diagnostics-worker").exists());
        assert!(moved.join(".velnor-diagnostics-worker").is_dir());

        std::fs::remove_file(&parent).unwrap();
        std::fs::remove_dir_all(&moved).unwrap();
        std::fs::remove_dir_all(&victim).unwrap();
    }

    #[test]
    fn unparseable_inspect_is_withheld_never_persisted_raw() {
        let state = temp_state("withhold");
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("LOGS-R\n"),
            ScriptRunner::ok("LOGS-D\n"),
            ScriptRunner::ok("NOT-JSON ACTIONS_RUNNER_INPUT_JITCONFIG=live-jit-blob-bytes\n"),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let export = export_diagnostics(&mut runner, &identity(), &state).unwrap();
        assert_eq!(export.failures.len(), 1);
        assert!(export.failures[0].contains("withheld"), "{export:?}");
        assert!(!export.runner_inspect.exists());
        // The raw bytes exist nowhere under the state dir.
        let mut raw_survived = false;
        for entry in std::fs::read_dir(&export.dir).unwrap() {
            let body = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            raw_survived |= body.contains("live-jit-blob-bytes");
        }
        assert!(!raw_survived);
        Supervision::new(identity(), &state)
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn release_state_removes_jit_file_left_by_process_crash() {
        let state = temp_state("release");
        let supervision = Supervision::new(identity(), &state);
        let jit_file =
            super::super::create_owner_only_file_next_to(&state, b"crash-secret").unwrap();
        let jit_dir = jit_file.parent().unwrap().to_path_buf();
        assert!(jit_file.is_file());

        // Simulates a process crash after writing the env file but before
        // Docker returned and the normal unlink ran. Diagnostics need not
        // have been created yet.
        supervision
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        assert!(!state.exists());
        assert!(!jit_file.exists());
        assert!(!jit_dir.exists());
        // Repeated recovery is safe after partial/previous cleanup.
        supervision
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn release_state_retry_keeps_private_data_until_state_removal_succeeds() {
        let state = temp_state("release-retry");
        let already_removed = state.join("already-removed");
        std::fs::write(&already_removed, b"partial state").unwrap();
        let jit_file =
            super::super::create_owner_only_file_next_to(&state, b"crash-secret").unwrap();
        let jit_dir = jit_file.parent().unwrap().to_path_buf();
        let diagnostics = SecureDiagnosticDir::open(&state).unwrap();
        let parent_path = state.parent().unwrap();
        let parent = super::super::secure_fs::open_absolute_directory(parent_path).unwrap();
        let state_name = state.file_name().unwrap();
        let private_name = super::super::secure_fs::private_jit_directory_name(&state).unwrap();
        let diagnostic_name = diagnostics_dir_name(&state).unwrap();
        let mut fail_once = true;

        let first = release_paths(
            &parent,
            state_name,
            &private_name,
            &diagnostic_name,
            |directory, name| {
                if name == state_name && fail_once {
                    fail_once = false;
                    let state_directory =
                        super::super::secure_fs::open_absolute_directory(&state).unwrap();
                    super::super::secure_fs::remove_tree_at(
                        &state_directory,
                        std::ffi::OsStr::new("already-removed"),
                    )
                    .unwrap();
                    Err(io::Error::other("injected state removal failure"))
                } else {
                    super::super::secure_fs::remove_tree_at(directory, name)
                }
            },
        );
        assert!(first.is_err());
        assert!(state.is_dir());
        assert!(!already_removed.exists());
        assert!(jit_file.is_file());
        assert!(diagnostics.path.is_dir());

        release_paths(
            &parent,
            state_name,
            &private_name,
            &diagnostic_name,
            super::super::secure_fs::remove_tree_at,
        )
        .unwrap();
        assert!(!state.exists());
        assert!(!jit_dir.exists());
        assert!(!diagnostics.path.exists());
        std::fs::remove_dir_all(parent_path).unwrap();
    }

    #[test]
    fn missing_objects_read_as_already_cleaned() {
        let state = temp_state("missing");
        let missing = || ScriptRunner::fail(1, "Error: No such container");
        let mut runner = ScriptRunner::scripted(vec![
            missing(), // runner ownership probe: already gone
            owned_container_inspect("dind-id", ROLE_DIND),
            ScriptRunner::ok("LOGS-D\n"),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let report = owned_cleanup(&mut runner, &identity(), &state).unwrap();
        // Missing runner evidence remains a gap; the owned DinD evidence
        // still captures, while terminal teardown waits for complete proof.
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.export.failures.len(), 2);
        assert_eq!(runner.seen.len(), 4);
        assert!(!report.confirmed());
        Supervision::new(identity(), &state)
            .release_owned_state(&test_cleanup_authorization(&state))
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }
}

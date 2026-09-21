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
//! Owned cleanup ordering (see [`owned_cleanup`]): stop runner → export
//! diagnostics → remove runner → stop DinD → remove DinD → remove
//! network → remove volumes. Diagnostics are exported BEFORE any
//! deletion; any failure in export or removal is collected into the
//! [`CleanupReport`] and the caller retains the permit as uncertain
//! instead of releasing fictitious capacity.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use super::ownership::WorkerIdentity;
use super::runner::{runner_connection, RunnerConnection};
use super::WorkerRunner;

/// How many DinD restarts one worker tolerates before failing.
pub const MAX_DIND_RESTARTS: u32 = 3;
/// JIT runner startup deadline, persisted as an absolute epoch time.
pub const RUNNER_START_TIMEOUT: Duration = Duration::from_secs(120);

/// Per-worker DinD restart budget (persisted with the worker record once
/// the journal extension lands; until then owned by the tick caller).
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
    pub runner: RunnerConnection,
}

/// Observe the live pair: DinD running state + runner connectivity.
pub(crate) fn observe_pair(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
) -> Result<ObservedPair> {
    let dind = identity.dind_container();
    let running = runner
        .run("docker", &crate::docker::client::running_args(&dind))
        .with_context(|| format!("inspect DinD container {dind}"))?;
    let dind_running = if running.code != 0 {
        if crate::docker::client::daemon_reports_missing(&running.stderr) {
            false
        } else {
            anyhow::bail!(
                "inspect DinD container {dind} exited {}: {}",
                running.code,
                running.stderr.trim()
            );
        }
    } else {
        running.stdout.trim() == "true"
    };
    let connection = runner_connection(runner, identity)?;
    Ok(ObservedPair {
        dind_running,
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
    /// A formerly starting runner reached the connected marker.
    RunnerConnected,
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
pub(crate) fn supervise_tick(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    recorded: velnor_model::ScaleSetWorkerState,
    observed: &ObservedPair,
    restarts: &mut RestartBudget,
) -> Result<SupervisionOutcome> {
    supervise_tick_with_runtime(
        runner,
        identity,
        recorded,
        observed,
        restarts,
        None,
        0,
        &mut |_| Ok(()),
    )
}

/// Runtime-aware supervision. `persist_restarts` commits a restart count
/// before the corresponding Docker `start`, so process death cannot restore
/// the consumed budget.
#[allow(
    clippy::too_many_arguments,
    reason = "single runtime decision boundary"
)]
pub(crate) fn supervise_tick_with_runtime(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    recorded: velnor_model::ScaleSetWorkerState,
    observed: &ObservedPair,
    restarts: &mut RestartBudget,
    runner_start_deadline_epoch: Option<u64>,
    now_epoch: u64,
    persist_restarts: &mut dyn FnMut(u32) -> Result<()>,
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
    if not_yet_connected
        && runner_start_deadline_epoch.is_some_and(|deadline| now_epoch >= deadline)
    {
        return Ok(SupervisionOutcome::WorkerFailed {
            reason: format!(
                "runner container {} did not connect before its startup deadline",
                identity.runner_container()
            ),
        });
    }
    if not_yet_connected && observed.runner == RunnerConnection::Connected {
        return Ok(SupervisionOutcome::RunnerConnected);
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
    // DinD death is repairable within budget.
    if !observed.dind_running {
        let next_restart = restarts.used().saturating_add(1);
        if next_restart <= MAX_DIND_RESTARTS && next_restart <= restarts.max {
            persist_restarts(next_restart)?;
            restarts.record_used(next_restart);
            let name = identity.dind_container();
            let started = runner
                .run(
                    "docker",
                    &["start".to_string(), "--".to_string(), name.clone()],
                )
                .with_context(|| format!("restart DinD container {name}"))?;
            if started.code != 0 {
                anyhow::bail!(
                    "restart DinD container {name} exited {}: {}",
                    started.code,
                    started.stderr.trim()
                );
            }
            return Ok(SupervisionOutcome::DindRestarted {
                restarts_used: restarts.used(),
            });
        }
        return Ok(SupervisionOutcome::WorkerFailed {
            reason: format!(
                "DinD container {} is down and the restart budget ({}) is exhausted",
                identity.dind_container(),
                restarts.used()
            ),
        });
    }
    Ok(SupervisionOutcome::Healthy)
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
/// `failures` names the files that could not be captured (a container
/// that vanished mid-export, an I/O error). A non-empty `failures` list
/// makes the cleanup report failed: the permit stays uncertain so the
/// evidence gap is visible instead of silently released.
#[derive(Debug, Clone)]
pub struct DiagnosticExport {
    pub dir: PathBuf,
    pub runner_log: PathBuf,
    pub dind_log: PathBuf,
    pub runner_inspect: PathBuf,
    pub dind_inspect: PathBuf,
    pub failures: Vec<String>,
}

/// Capture logs + inspect of both containers into `state_dir/diagnostics`.
///
/// Runs AFTER the runner stops (logs are complete) and BEFORE any
/// deletion. Every capture is attempted even when an earlier one fails;
/// failures are collected, not raised, so one missing container cannot
/// hide the surviving container's evidence.
///
/// Secrecy: inspect output is redacted before it touches disk (labels,
/// `State`, and `NetworkSettings` only — `Config.Env`, which carries the
/// JIT blob, is dropped), and every file is owner-only (`0600`, dir
/// `0700`). Logs stay byte-identical — this layer owns no mask registry
/// — so the state dir itself is deleted once the permit releases (see
/// [`Supervision::release_state`]); raw logs never rest on shared disk
/// past the worker's lifetime.
pub(crate) fn export_diagnostics(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
) -> Result<DiagnosticExport> {
    let dir = state_dir.join("diagnostics");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create diagnostics dir {}", dir.display()))?;
    restrict_diagnostic_dir(&dir)
        .with_context(|| format!("restrict diagnostics dir {}", dir.display()))?;
    let mut failures = Vec::new();

    let runner_log = dir.join("runner.log");
    let dind_log = dir.join("dind.log");
    let runner_inspect = dir.join("runner.inspect.json");
    let dind_inspect = dir.join("dind.inspect.json");
    let complete = dir.join("capture.complete");
    if complete.exists() {
        let marker = std::fs::read(&complete)
            .with_context(|| format!("read diagnostic marker {}", complete.display()))?;
        if marker != b"velnor-diagnostics-v1\n" {
            anyhow::bail!(
                "invalid diagnostic completion marker {}",
                complete.display()
            );
        }
        return Ok(DiagnosticExport {
            dir,
            runner_log,
            dind_log,
            runner_inspect,
            dind_inspect,
            failures,
        });
    }

    // No completion marker means a prior capture was interrupted. The
    // cleanup phase never removes containers before the marker lands, so
    // incomplete artifacts can be discarded and captured again safely.
    for artifact in [&runner_log, &dind_log, &runner_inspect, &dind_inspect] {
        match std::fs::remove_file(artifact) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                failures.push(format!("remove incomplete {}: {error}", artifact.display()))
            }
        }
    }
    if !failures.is_empty() {
        return Ok(DiagnosticExport {
            dir,
            runner_log,
            dind_log,
            runner_inspect,
            dind_inspect,
            failures,
        });
    }
    capture_logs(
        runner,
        &identity.runner_container(),
        &runner_log,
        &mut failures,
    );
    capture_logs(runner, &identity.dind_container(), &dind_log, &mut failures);
    capture_inspect(
        runner,
        &identity.runner_container(),
        &runner_inspect,
        &mut failures,
    );
    capture_inspect(
        runner,
        &identity.dind_container(),
        &dind_inspect,
        &mut failures,
    );
    if failures.is_empty()
        && let Err(error) = write_diagnostic(&complete, b"velnor-diagnostics-v1\n")
    {
        failures.push(format!("write {}: {error}", complete.display()));
    }

    Ok(DiagnosticExport {
        dir,
        runner_log,
        dind_log,
        runner_inspect,
        dind_inspect,
        failures,
    })
}

fn capture_logs(
    runner: &mut dyn WorkerRunner,
    container: &str,
    dest: &Path,
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
            if let Err(error) = write_diagnostic(dest, combined.as_bytes()) {
                failures.push(format!("write {}: {error}", dest.display()));
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
    dest: &Path,
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
                if let Err(error) = write_diagnostic(dest, redacted.as_bytes()) {
                    failures.push(format!("write {}: {error}", dest.display()));
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

/// Write one diagnostic file with owner-only permissions.
fn write_diagnostic(dest: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::time::SystemTime;

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let file_name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("diagnostic");
    let temporary =
        dest.with_file_name(format!(".{file_name}.{}.{}.tmp", std::process::id(), stamp));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let write_result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&temporary, dest)?;
        if let Some(parent) = dest.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

/// Restrict the diagnostics dir to the owner. Best-effort on non-unix,
/// where no permission primitive exists.
fn restrict_diagnostic_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Owned teardown report: what was removed and what failed.
///
/// `failures.is_empty()` means confirmed cleanup (permit releases).
/// Anything else means retained-uncertain: every removal was still
/// attempted (a stuck object must not pin the rest), but the permit
/// stays occupied so the residue is visible and converged by recovery.
#[derive(Debug, Clone)]
pub struct CleanupReport {
    pub export: DiagnosticExport,
    pub failures: Vec<String>,
}

impl CleanupReport {
    #[must_use]
    pub fn confirmed(&self) -> bool {
        self.export.failures.is_empty() && self.failures.is_empty()
    }
}

/// Stop the runner and persist a complete diagnostics capture before any
/// owned resource can be deleted.
pub(crate) fn prepare_cleanup(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
) -> Result<DiagnosticExport> {
    let mut stop_failures = Vec::new();
    stop_container(runner, &identity.runner_container(), &mut stop_failures);
    let mut export = export_diagnostics(runner, identity, state_dir)?;
    export.failures.extend(stop_failures);
    Ok(export)
}

/// Remove every owned object after diagnostics are durably complete.
pub(crate) fn finish_cleanup(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    export: DiagnosticExport,
) -> CleanupReport {
    let failures = teardown_owned_resources(runner, identity);
    CleanupReport { export, failures }
}

/// Remove every Docker resource owned by one worker. Missing objects count
/// as removed, making replay safe after a process crash mid-teardown.
pub(crate) fn teardown_owned_resources(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
) -> Vec<String> {
    let mut failures = Vec::new();
    remove_container(runner, &identity.runner_container(), &mut failures);
    stop_container(runner, &identity.dind_container(), &mut failures);
    remove_container(runner, &identity.dind_container(), &mut failures);
    remove_network(runner, &identity.network(), &mut failures);
    remove_volume(runner, &identity.workspace_volume(), &mut failures);
    remove_volume(runner, &identity.dind_data_volume(), &mut failures);
    failures
}

/// Tear down every object the worker owns, in dependency order. An export
/// failure prevents deletion, preserving evidence for a replay.
pub(crate) fn owned_cleanup(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
) -> Result<CleanupReport> {
    let export = prepare_cleanup(runner, identity, state_dir)?;
    if !export.failures.is_empty() {
        return Ok(CleanupReport {
            export,
            failures: Vec::new(),
        });
    }
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

fn remove_network(runner: &mut dyn WorkerRunner, network: &str, failures: &mut Vec<String>) {
    match runner.run(
        "docker",
        &[
            "network".to_string(),
            "rm".to_string(),
            "--".to_string(),
            network.to_string(),
        ],
    ) {
        Ok(output) if output.code == 0 => {}
        Ok(output)
            if crate::docker::client::daemon_reports_missing(&output.stderr)
                || output.stderr.contains("not found")
                || output.stderr.contains("Not found") => {}
        Ok(output) => failures.push(format!(
            "remove network {network} exited {}: {}",
            output.code,
            output.stderr.trim()
        )),
        Err(error) => failures.push(format!("remove network {network}: {error:#}")),
    }
}

fn remove_volume(runner: &mut dyn WorkerRunner, volume: &str, failures: &mut Vec<String>) {
    match runner.run(
        "docker",
        &[
            "volume".to_string(),
            "rm".to_string(),
            "--".to_string(),
            volume.to_string(),
        ],
    ) {
        Ok(output) if output.code == 0 => {}
        Ok(output)
            if crate::docker::client::daemon_reports_missing(&output.stderr)
                || output.stderr.contains("not found")
                || output.stderr.contains("Not found") => {}
        Ok(output) => failures.push(format!(
            "remove volume {volume} exited {}: {}",
            output.code,
            output.stderr.trim()
        )),
        Err(error) => failures.push(format!("remove volume {volume}: {error:#}")),
    }
}

/// Supervision handle: identity + state dir + durable runtime counters.
#[derive(Debug)]
pub struct Supervision {
    identity: WorkerIdentity,
    state_dir: PathBuf,
    restarts: RestartBudget,
    runner_start_deadline_epoch: Option<u64>,
}

impl Supervision {
    #[must_use]
    pub fn new(identity: WorkerIdentity, state_dir: &Path) -> Self {
        Self {
            identity,
            state_dir: state_dir.to_path_buf(),
            restarts: RestartBudget::new(MAX_DIND_RESTARTS),
            runner_start_deadline_epoch: None,
        }
    }

    /// Rebuild supervision from the durable worker registry after restart.
    #[must_use]
    pub fn from_runtime(
        identity: WorkerIdentity,
        state_dir: &Path,
        restarts_used: u32,
        runner_start_deadline_epoch: Option<u64>,
    ) -> Self {
        Self {
            identity,
            state_dir: state_dir.to_path_buf(),
            restarts: RestartBudget::from_used(MAX_DIND_RESTARTS, restarts_used),
            runner_start_deadline_epoch,
        }
    }

    /// Observe + decide one tick (reconciles recorded vs live).
    pub fn tick(
        &mut self,
        runner: &mut dyn WorkerRunner,
        recorded: velnor_model::ScaleSetWorkerState,
    ) -> Result<SupervisionOutcome> {
        self.tick_with_runtime(runner, recorded, epoch_seconds(), &mut |_| Ok(()))
    }

    /// Observe + decide one tick while persisting runtime state before
    /// commands with external side effects.
    pub fn tick_with_runtime(
        &mut self,
        runner: &mut dyn WorkerRunner,
        recorded: velnor_model::ScaleSetWorkerState,
        now_epoch: u64,
        persist_restarts: &mut dyn FnMut(u32) -> Result<()>,
    ) -> Result<SupervisionOutcome> {
        let observed = observe_pair(runner, &self.identity)?;
        supervise_tick_with_runtime(
            runner,
            &self.identity,
            recorded,
            &observed,
            &mut self.restarts,
            self.runner_start_deadline_epoch,
            now_epoch,
            persist_restarts,
        )
    }

    pub fn clear_runner_start_deadline(&mut self) {
        self.runner_start_deadline_epoch = None;
    }

    pub fn set_runner_start_deadline(&mut self, deadline_epoch: u64) {
        self.runner_start_deadline_epoch = Some(deadline_epoch);
    }

    /// Run the owned teardown sequence.
    pub fn cleanup(&self, runner: &mut dyn WorkerRunner) -> Result<CleanupReport> {
        owned_cleanup(runner, &self.identity, &self.state_dir)
    }

    pub(crate) fn prepare_cleanup(
        &self,
        runner: &mut dyn WorkerRunner,
    ) -> Result<DiagnosticExport> {
        prepare_cleanup(runner, &self.identity, &self.state_dir)
    }

    pub(crate) fn teardown_owned_resources(&self, runner: &mut dyn WorkerRunner) -> Vec<String> {
        teardown_owned_resources(runner, &self.identity)
    }

    pub(crate) fn state_dir_exists(&self) -> Result<bool> {
        match std::fs::symlink_metadata(&self.state_dir) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
            Ok(_) => anyhow::bail!(
                "refusing worker state path {}: not a real directory",
                self.state_dir.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("inspect worker state dir {}", self.state_dir.display())),
        }
    }

    pub(crate) fn diagnostics_complete(&self) -> Result<bool> {
        let marker = self.state_dir.join("diagnostics").join("capture.complete");
        let contents = match std::fs::read(&marker) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read diagnostic marker {}", marker.display()));
            }
        };
        if contents != b"velnor-diagnostics-v1\n" {
            anyhow::bail!("invalid diagnostic completion marker {}", marker.display());
        }
        Ok(true)
    }

    pub(crate) fn clear_diagnostic_completion_marker(&self) -> Result<()> {
        let diagnostics = self.state_dir.join("diagnostics");
        let marker = diagnostics.join("capture.complete");
        match std::fs::remove_file(&marker) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("remove diagnostic marker {}", marker.display()));
            }
        }
        std::fs::File::open(&diagnostics)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("sync diagnostics dir {}", diagnostics.display()))?;
        Ok(())
    }

    /// Delete the worker's state dir. Call once, after the permit
    /// releases: the dir holds the raw job logs, which must not rest on
    /// shared disk past the worker's lifetime. Fails closed on a dir this
    /// worker's export did not create (no `diagnostics/` child) rather
    /// than deleting an unrelated tree. Idempotent, including after a
    /// prior partial directory removal.
    pub fn release_state(&self) -> Result<()> {
        super::runner::RunnerSpec::scrub_jit_env_files_for_state(&self.state_dir)
            .context("scrub JIT env file before deleting worker state")?;
        let metadata = match std::fs::symlink_metadata(&self.state_dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect worker state dir {}", self.state_dir.display())
                });
            }
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            anyhow::bail!(
                "refusing to delete {}: worker state path is not a real directory",
                self.state_dir.display()
            );
        }
        if !self.state_dir.join("diagnostics").is_dir() {
            anyhow::bail!(
                "refusing to delete {}: no diagnostics dir (not a released worker state dir)",
                self.state_dir.display()
            );
        }
        std::fs::remove_dir_all(&self.state_dir).with_context(|| {
            format!(
                "delete released worker state dir {}",
                self.state_dir.display()
            )
        })
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
    use super::super::WorkerOutput;
    use super::*;
    use std::collections::VecDeque;
    use velnor_model::ScaleSetWorkerState as S;

    struct ScriptRunner {
        results: VecDeque<WorkerOutput>,
        seen: Vec<Vec<String>>,
    }

    impl ScriptRunner {
        fn scripted(results: Vec<WorkerOutput>) -> Self {
            Self {
                results: results.into(),
                seen: Vec::new(),
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
    }

    fn identity() -> WorkerIdentity {
        WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ))
    }

    fn temp_state(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("velnor-supervise-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn healthy_pair_ticks_healthy() {
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("true\n"),                // dind running
            ScriptRunner::ok("true\n"),                // runner running
            ScriptRunner::ok("Connected to GitHub\n"), // runner logs
        ]);
        let mut supervision = Supervision::new(identity(), Path::new("/tmp/velnor-test-sup"));
        let outcome = supervision.tick(&mut runner, S::Running).unwrap();
        assert_eq!(outcome, SupervisionOutcome::Healthy);
    }

    #[test]
    fn dind_death_restarts_within_budget() {
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("false\n"),               // dind stopped
            ScriptRunner::ok("true\n"),                // runner running
            ScriptRunner::ok("Connected to GitHub\n"), // runner logs
            ScriptRunner::ok("velnor-scaleset-dind-s7-velnor-set-0007-2ad92676\n"), // start dind
        ]);
        let mut supervision = Supervision::new(identity(), Path::new("/tmp/velnor-test-sup"));
        let outcome = supervision.tick(&mut runner, S::Running).unwrap();
        assert_eq!(
            outcome,
            SupervisionOutcome::DindRestarted { restarts_used: 1 }
        );
    }

    #[test]
    fn dind_death_fails_when_budget_exhausted() {
        let observed = ObservedPair {
            dind_running: false,
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
            runner: RunnerConnection::Connected,
        };
        let mut runner = ScriptRunner::scripted(vec![]);
        let mut restarts = RestartBudget::new(MAX_DIND_RESTARTS);
        let expired = supervise_tick_with_runtime(
            &mut runner,
            &identity(),
            S::DindReady,
            &observed,
            &mut restarts,
            Some(100),
            100,
            &mut |_| Ok(()),
        )
        .unwrap();
        assert!(matches!(expired, SupervisionOutcome::WorkerFailed { .. }));
        assert!(runner.seen.is_empty());

        let within_deadline = supervise_tick_with_runtime(
            &mut runner,
            &identity(),
            S::DindReady,
            &observed,
            &mut restarts,
            Some(100),
            99,
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(within_deadline, SupervisionOutcome::RunnerConnected);
    }

    #[test]
    fn restart_budget_persists_before_docker_start() {
        let observed = ObservedPair {
            dind_running: false,
            runner: RunnerConnection::Connected,
        };
        let mut runner = ScriptRunner::scripted(vec![]);
        let mut restarts = RestartBudget::new(MAX_DIND_RESTARTS);
        let error = supervise_tick_with_runtime(
            &mut runner,
            &identity(),
            S::Running,
            &observed,
            &mut restarts,
            None,
            100,
            &mut |_| anyhow::bail!("registry unavailable"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("registry unavailable"));
        assert!(
            runner.seen.is_empty(),
            "DinD must not start before persistence"
        );
        assert_eq!(restarts.used(), 0);
        assert_eq!(
            RestartBudget::from_used(MAX_DIND_RESTARTS, u32::MAX).used(),
            MAX_DIND_RESTARTS
        );
    }

    #[test]
    fn terminal_side_states_skip_the_tick() {
        let observed = ObservedPair {
            dind_running: false,
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
    fn inspect_redaction_drops_healthcheck_output_that_can_echo_jit() {
        let raw = r#"[
            {
                "Id": "container-id",
                "Config": {
                    "Env": ["ACTIONS_RUNNER_INPUT_JITCONFIG=JIT_ENV_SENTINEL"],
                    "Labels": {"owner": "velnor"}
                },
                "State": {
                    "Status": "running",
                    "Health": {
                        "Status": "healthy",
                        "FailingStreak": 0,
                        "Log": [{"Output": "HEALTH_OUTPUT_SENTINEL"}]
                    }
                },
                "NetworkSettings": {}
            }
        ]"#;
        let redacted = redact_inspect(raw).unwrap();
        assert!(redacted.contains("healthy"));
        assert!(redacted.contains("FailingStreak"));
        assert!(!redacted.contains("HEALTH_OUTPUT_SENTINEL"));
        assert!(!redacted.contains("JIT_ENV_SENTINEL"));
        assert!(!redacted.contains("\"Log\""));
        assert!(!redacted.contains("\"Env\""));
    }

    #[test]
    fn cleanup_exports_before_first_deletion_in_order() {
        let state = temp_state("order");
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("runner\n"),  // stop runner
            ScriptRunner::ok("LOGS-R\n"),  // logs runner
            ScriptRunner::ok("LOGS-D\n"),  // logs dind
            ScriptRunner::ok("[{}]\n"),    // inspect runner
            ScriptRunner::ok("[{}]\n"),    // inspect dind
            ScriptRunner::ok("runner\n"),  // rm runner
            ScriptRunner::ok("dind\n"),    // stop dind
            ScriptRunner::ok("dind\n"),    // rm dind
            ScriptRunner::ok("net\n"),     // rm network
            ScriptRunner::ok("work\n"),    // rm workspace volume
            ScriptRunner::ok("dindata\n"), // rm dind data volume
        ]);
        let report = owned_cleanup(&mut runner, &identity(), &state).unwrap();
        assert!(report.confirmed(), "{report:?}");
        let verbs: Vec<String> = runner.seen.iter().map(|argv| argv.join(" ")).collect();
        let position = |needle: &str| verbs.iter().position(|v| v.contains(needle)).unwrap();
        // Order: stop runner < logs < rm runner < stop dind < rm dind < network < volumes.
        assert!(position("stop -t 30 -- velnor-scaleset-runner") < position("logs --"));
        assert!(position("logs --") < position("rm --force -- velnor-scaleset-runner"));
        assert!(
            position("rm --force -- velnor-scaleset-runner")
                < position("stop -t 30 -- velnor-scaleset-dind")
        );
        assert!(
            position("stop -t 30 -- velnor-scaleset-dind")
                < position("rm --force -- velnor-scaleset-dind")
        );
        assert!(position("rm --force -- velnor-scaleset-dind") < position("network rm"));
        assert!(position("network rm") < position("volume rm"));
        // Evidence landed on disk.
        assert_eq!(
            std::fs::read_to_string(&report.export.runner_log).unwrap(),
            "LOGS-R\n"
        );
        assert_eq!(
            std::fs::read_to_string(&report.export.dind_log).unwrap(),
            "LOGS-D\n"
        );
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn cleanup_collects_failures_without_aborting() {
        let state = temp_state("failures");
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, "boom"), // stop runner fails
            ScriptRunner::ok("LOGS-R\n"),
            ScriptRunner::fail(1, "gone"), // logs dind fails
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("runner\n"),
            ScriptRunner::ok("dind\n"),
            ScriptRunner::fail(1, "boom"), // rm dind fails
            ScriptRunner::ok("net\n"),
            ScriptRunner::ok("work\n"),
            ScriptRunner::ok("dindata\n"),
        ]);
        let report = owned_cleanup(&mut runner, &identity(), &state).unwrap();
        assert!(!report.confirmed());
        assert_eq!(report.export.failures.len(), 2);
        assert!(report.failures.is_empty());
        // Every export step ran, but no deletion ran because evidence was
        // incomplete: stop + four captures only.
        assert_eq!(runner.seen.len(), 5);
        assert!(!runner
            .seen
            .iter()
            .any(|args| args.iter().any(|arg| arg == "rm")));
        std::fs::remove_dir_all(&state).unwrap();
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
            "State": {"Status": "exited", "ExitCode": 0},
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
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn completed_diagnostics_replay_without_recapturing() {
        let state = temp_state("diagnostics-replay");
        let mut first = ScriptRunner::scripted(vec![
            ScriptRunner::ok("LOGS-R\n"),
            ScriptRunner::ok("LOGS-D\n"),
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let export = export_diagnostics(&mut first, &identity(), &state).unwrap();
        assert!(export.failures.is_empty());
        let mut replay = ScriptRunner::scripted(vec![]);
        let replayed = export_diagnostics(&mut replay, &identity(), &state).unwrap();
        assert!(replayed.failures.is_empty());
        assert_eq!(replay.seen.len(), 0);
        assert_eq!(replayed.runner_log, export.runner_log);
        assert_eq!(
            std::fs::read_to_string(state.join("diagnostics/capture.complete")).unwrap(),
            "velnor-diagnostics-v1\n"
        );
        std::fs::remove_dir_all(&state).unwrap();
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
        std::fs::remove_dir_all(&state).unwrap();
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
        for entry in std::fs::read_dir(state.join("diagnostics")).unwrap() {
            let body = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            raw_survived |= body.contains("live-jit-blob-bytes");
        }
        assert!(!raw_survived);
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn release_state_deletes_only_exported_dirs() {
        let state = temp_state("release");
        let supervision = Supervision::new(identity(), &state);
        // No export ran here: deletion refuses rather than removing an
        // unrelated tree.
        assert!(supervision.release_state().is_err());
        assert!(state.is_dir());

        std::fs::create_dir_all(state.join("diagnostics")).unwrap();
        std::fs::write(state.join("diagnostics/runner.log"), "LOGS\n").unwrap();
        supervision.release_state().unwrap();
        assert!(!state.exists());
    }

    #[test]
    fn missing_objects_read_as_already_cleaned() {
        let state = temp_state("missing");
        let missing = || ScriptRunner::fail(1, "Error: No such container");
        let mut runner = ScriptRunner::scripted(vec![
            missing(), // stop runner: already gone
            missing(), // logs runner: gone → export failure (evidence gap is real)
            ScriptRunner::ok("LOGS-D\n"),
            missing(), // inspect runner: gone → export failure
            ScriptRunner::ok("[{}]\n"),
            missing(), // rm runner: already gone → fine
            ScriptRunner::ok("dind\n"),
            ScriptRunner::ok("dind\n"),
            ScriptRunner::fail(1, "Error: No such network"),
            ScriptRunner::fail(1, "Error: No such volume"),
            ScriptRunner::fail(1, "Error: No such volume"),
        ]);
        let report = owned_cleanup(&mut runner, &identity(), &state).unwrap();
        // Removals of missing objects are clean; missing EVIDENCE is a gap.
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.export.failures.len(), 2);
        assert!(!report.confirmed());
        std::fs::remove_dir_all(&state).unwrap();
    }
}

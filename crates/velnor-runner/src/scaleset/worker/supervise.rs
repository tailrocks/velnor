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

use anyhow::{Context, Result};
use std::io;
#[cfg(unix)]
use std::{ffi::OsString, fs::File};

use super::ownership::WorkerIdentity;
use super::runner::{runner_connection, RunnerConnection};
use super::WorkerRunner;

/// How many DinD restarts one worker tolerates before failing.
pub const MAX_DIND_RESTARTS: u32 = 3;

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
    /// The worker must move to `terminal`: restart would be wrong.
    WorkerFailed { reason: String },
}

/// Decide one tick from the observation + recorded state.
///
/// `recorded` is the worker's recorded lifecycle state; the tick
/// reconciles it against `observed` and either repairs (DinD restart)
/// or fails the worker toward `terminal`. Pure decision + the DinD
/// restart effect; the caller performs the lifecycle edge.
pub(crate) fn supervise_tick(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    recorded: velnor_model::ScaleSetWorkerState,
    observed: &ObservedPair,
    restarts: &mut RestartBudget,
) -> Result<SupervisionOutcome> {
    use velnor_model::ScaleSetWorkerState as S;
    // Terminal-side states are owned by the cleanup path, not the tick.
    if matches!(
        recorded,
        S::Terminal | S::DiagnosticExport | S::OwnedCleanup | S::PermitReleased
    ) {
        return Ok(SupervisionOutcome::Healthy);
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
        if restarts.spend() {
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

const DIAGNOSTICS_COMPLETE_MARKER: &[u8] = b"velnor-diagnostics-v1\n";
const NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER: &[u8] = b"velnor-diagnostics-v1-runner-absent\n";
const DIAGNOSTIC_ARTIFACT_COMPLETE_PREFIX: &[u8] = b"velnor-diagnostic-artifact-v1:";

/// Capture logs + inspect of both containers into a host-only sibling of
/// `state_dir`, outside the writable bind mounted into runner and DinD.
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
/// — so the host-only diagnostics sibling and shared state dir are deleted
/// once the permit releases (see [`Supervision::release_state`]).
pub(crate) fn export_diagnostics(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
) -> Result<DiagnosticExport> {
    export_diagnostics_inner(runner, identity, state_dir, true)
}

fn export_diagnostics_inner(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
    write_completion_marker: bool,
) -> Result<DiagnosticExport> {
    let dir = SecureDiagnosticDir::open(state_dir)?;
    let dir_path = dir.path.clone();
    let runner_log = dir.file_path("runner.log");
    let dind_log = dir.file_path("dind.log");
    let runner_inspect = dir.file_path("runner.inspect.json");
    let dind_inspect = dir.file_path("dind.inspect.json");
    match dir.read_file("capture.complete")? {
        Some(contents) if contents == DIAGNOSTICS_COMPLETE_MARKER => {
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
        Some(contents) if contents == NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER => {
            anyhow::bail!("runner-absent diagnostic marker cannot certify a full runner capture");
        }
        Some(_) => anyhow::bail!(
            "invalid diagnostic completion marker {}",
            dir.file_path("capture.complete").display()
        ),
        None => {}
    }

    // A missing marker means the previous capture did not finish. Discard
    // partial files through the pinned directory handle before recapturing.
    for name in [
        "runner.log",
        "dind.log",
        "runner.inspect.json",
        "dind.inspect.json",
        "runner.log.complete",
        "dind.log.complete",
        "runner.inspect.json.complete",
        "dind.inspect.json.complete",
    ] {
        dir.remove_file(name).with_context(|| {
            format!(
                "remove incomplete diagnostic {}",
                dir.file_path(name).display()
            )
        })?;
    }
    dir.sync()
        .context("persist removed incomplete diagnostics")?;
    let mut failures = Vec::new();

    capture_logs(
        runner,
        &identity.runner_container(),
        &dir,
        "runner.log",
        &mut failures,
    );
    capture_logs(
        runner,
        &identity.dind_container(),
        &dir,
        "dind.log",
        &mut failures,
    );
    capture_inspect(
        runner,
        &identity.runner_container(),
        &dir,
        "runner.inspect.json",
        &mut failures,
    );
    capture_inspect(
        runner,
        &identity.dind_container(),
        &dir,
        "dind.inspect.json",
        &mut failures,
    );

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

fn inspect_container_exists(runner: &mut dyn WorkerRunner, container: &str) -> Result<bool> {
    let inspect = runner
        .run(
            "docker",
            &[
                "inspect".to_string(),
                "--type".to_string(),
                "container".to_string(),
                "--format".to_string(),
                "{{.Id}}".to_string(),
                "--".to_string(),
                container.to_string(),
            ],
        )
        .with_context(|| format!("inspect container {container}"))?;
    if inspect.code == 0 {
        if inspect.stdout.trim().is_empty() {
            anyhow::bail!("inspect container {container} returned an empty id");
        }
        return Ok(true);
    }
    if crate::docker::client::daemon_reports_missing(&inspect.stderr) {
        return Ok(false);
    }
    anyhow::bail!(
        "inspect container {container} exited {}: {}",
        inspect.code,
        inspect.stderr.trim()
    )
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
        let expected = artifact_marker_contents(name);
        if self.read_file(&marker_name)?.as_deref() != Some(expected.as_slice()) {
            return Ok(false);
        }
        #[cfg(unix)]
        {
            super::secure_fs::owner_only_regular_file_exists_at(
                &self.directory,
                std::ffi::OsStr::new(name),
            )
            .map_err(anyhow::Error::from)
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!("secure host-only worker diagnostics require Unix dirfd support")
        }
    }

    fn mark_artifact_complete(&self, name: &str) -> Result<()> {
        let marker_name = artifact_marker_name(name);
        self.write_file(&marker_name, &artifact_marker_contents(name))
            .with_context(|| format!("persist completion proof for diagnostic {name}"))
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

fn artifact_marker_contents(name: &str) -> Vec<u8> {
    let mut contents =
        Vec::with_capacity(DIAGNOSTIC_ARTIFACT_COMPLETE_PREFIX.len() + name.len() + 1);
    contents.extend_from_slice(DIAGNOSTIC_ARTIFACT_COMPLETE_PREFIX);
    contents.extend_from_slice(name.as_bytes());
    contents.push(b'\n');
    contents
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

/// Stop the runner and ensure a complete diagnostic capture before any
/// owned resource can be deleted.
pub(crate) fn prepare_cleanup(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
) -> Result<DiagnosticExport> {
    let mut stop_failures = Vec::new();
    stop_container(runner, &identity.runner_container(), &mut stop_failures);
    let mut export =
        export_diagnostics_inner(runner, identity, state_dir, stop_failures.is_empty())?;
    export.failures.extend(stop_failures);
    Ok(export)
}

/// Remove the worker's Docker resources after diagnostics are complete.
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

/// Tear down every object the worker owns after diagnostic capture succeeds.
/// A failed export preserves its source containers and evidence for replay.
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
    let failures = teardown_owned_resources(runner, identity);
    Ok(CleanupReport { export, failures })
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
        Ok(output) if crate::docker::client::daemon_reports_missing(&output.stderr) => {}
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
        Ok(output) if crate::docker::client::daemon_reports_missing(&output.stderr) => {}
        Ok(output) => failures.push(format!(
            "remove volume {volume} exited {}: {}",
            output.code,
            output.stderr.trim()
        )),
        Err(error) => failures.push(format!("remove volume {volume}: {error:#}")),
    }
}

/// Supervision handle: identity + state dir + restart budget.
#[derive(Debug)]
pub struct Supervision {
    identity: WorkerIdentity,
    state_dir: PathBuf,
    restarts: RestartBudget,
}

impl Supervision {
    #[must_use]
    pub fn new(identity: WorkerIdentity, state_dir: &Path) -> Self {
        Self {
            identity,
            state_dir: state_dir.to_path_buf(),
            restarts: RestartBudget::new(MAX_DIND_RESTARTS),
        }
    }

    /// Observe + decide one tick (reconciles recorded vs live).
    pub fn tick(
        &mut self,
        runner: &mut dyn WorkerRunner,
        recorded: velnor_model::ScaleSetWorkerState,
    ) -> Result<SupervisionOutcome> {
        let observed = observe_pair(runner, &self.identity)?;
        supervise_tick(
            runner,
            &self.identity,
            recorded,
            &observed,
            &mut self.restarts,
        )
    }

    /// Run the owned teardown sequence.
    pub fn cleanup(&self, runner: &mut dyn WorkerRunner) -> Result<CleanupReport> {
        owned_cleanup(runner, &self.identity, &self.state_dir)
    }

    pub(crate) fn prepare_cleanup(
        &self,
        runner: &mut dyn WorkerRunner,
    ) -> Result<DiagnosticExport> {
        self::prepare_cleanup(runner, &self.identity, &self.state_dir)
    }

    pub(crate) fn teardown_owned_resources(&self, runner: &mut dyn WorkerRunner) -> Vec<String> {
        self::teardown_owned_resources(runner, &self.identity)
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
        let Some(diagnostics) = SecureDiagnosticDir::open_existing(&self.state_dir)? else {
            return Ok(false);
        };
        match diagnostics.read_file("capture.complete")? {
            None => Ok(false),
            Some(contents)
                if contents == DIAGNOSTICS_COMPLETE_MARKER
                    || contents == NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER =>
            {
                diagnostics
                    .sync()
                    .context("persist diagnostics completion marker")?;
                Ok(true)
            }
            Some(_) => anyhow::bail!(
                "invalid diagnostic completion marker {}",
                diagnostics.file_path("capture.complete").display()
            ),
        }
    }

    /// Capture available DinD evidence and durably record diagnostics as
    /// complete when a persisted `ProvisionIntent` has no runner container.
    /// This method verifies runner absence and distinguishes a missing DinD
    /// container from Docker errors. Callers must still prove the durable
    /// worker stage is `ProvisionIntent` before invoking it.
    pub(crate) fn complete_diagnostics_without_runner(
        &self,
        runner: &mut dyn WorkerRunner,
    ) -> Result<DiagnosticExport> {
        if inspect_container_exists(runner, &self.identity.runner_container())? {
            anyhow::bail!(
                "refusing no-runner diagnostics completion while runner {} exists",
                self.identity.runner_container()
            );
        }

        let diagnostics = SecureDiagnosticDir::open(&self.state_dir)?;
        match diagnostics.read_file("capture.complete")? {
            Some(contents) if contents == NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER => {
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

        let dind_exists = inspect_container_exists(runner, &self.identity.dind_container())?;

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
        if dind_exists {
            if !diagnostics.artifact_capture_complete("dind.log")? {
                diagnostics.clear_artifact("dind.log")?;
                let before = failures.len();
                capture_logs(
                    runner,
                    &self.identity.dind_container(),
                    &diagnostics,
                    "dind.log",
                    &mut failures,
                );
                if failures.len() == before {
                    diagnostics.mark_artifact_complete("dind.log")?;
                }
            }
            if !diagnostics.artifact_capture_complete("dind.inspect.json")? {
                diagnostics.clear_artifact("dind.inspect.json")?;
                let before = failures.len();
                capture_inspect(
                    runner,
                    &self.identity.dind_container(),
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
                if !diagnostics.artifact_capture_complete(name)? {
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

    pub(crate) fn clear_diagnostic_completion_marker(&self) -> Result<()> {
        let Some(diagnostics) = SecureDiagnosticDir::open_existing(&self.state_dir)? else {
            return Ok(());
        };
        diagnostics
            .remove_file("capture.complete")
            .context("remove diagnostic completion marker")?;
        diagnostics
            .sync()
            .context("persist removed diagnostic completion marker")
    }

    /// Delete worker state and host-only sibling data after the permit
    /// releases. State is removed first so a partial failure leaves private
    /// data available for an idempotent retry. Every removal is relative to
    /// an opened, no-follow parent descriptor. A live state tree must carry
    /// the host-only completion marker before this path can delete it.
    pub fn release_state(&self) -> Result<()> {
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
    pub(crate) fn release_owned_state(&self) -> Result<()> {
        self.release_state_inner()
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
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!("velnor-supervise-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let state = root.join("worker");
        std::fs::create_dir(&state).unwrap();
        state
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
        Supervision::new(identity(), &state)
            .release_state()
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn cleanup_preserves_resources_when_diagnostics_fail() {
        let state = temp_state("failures");
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, "boom"), // stop runner fails
            ScriptRunner::ok("LOGS-R\n"),
            ScriptRunner::fail(1, "gone"), // logs dind fails
            ScriptRunner::ok("[{}]\n"),
            ScriptRunner::ok("[{}]\n"),
        ]);
        let report = owned_cleanup(&mut runner, &identity(), &state).unwrap();
        assert!(!report.confirmed());
        assert_eq!(report.export.failures.len(), 2);
        assert!(!report.export.dir.join("capture.complete").exists());
        assert!(report.failures.is_empty());
        // Capture/stop ran, but no resource removal followed failed evidence.
        assert_eq!(runner.seen.len(), 5);
        assert!(runner
            .seen
            .iter()
            .all(|args| !args.iter().any(|arg| arg == "rm")));
        Supervision::new(identity(), &state)
            .release_owned_state()
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_runner_stop_cannot_complete_diagnostics_and_retry_recaptures_logs() {
        let state = temp_state("stop-failure-marker");
        let first = prepare_cleanup(
            &mut ScriptRunner::scripted(vec![
                ScriptRunner::fail(1, "runner stop failed"),
                ScriptRunner::ok("before-stop runner logs\n"),
                ScriptRunner::ok("before-stop dind logs\n"),
                ScriptRunner::ok("[{}]\n"),
                ScriptRunner::ok("[{}]\n"),
            ]),
            &identity(),
            &state,
        )
        .unwrap();
        assert_eq!(first.failures.len(), 1, "{first:?}");
        assert!(!first.dir.join("capture.complete").exists());

        let retry = prepare_cleanup(
            &mut ScriptRunner::scripted(vec![
                ScriptRunner::ok("runner stopped\n"),
                ScriptRunner::ok("after-stop runner logs\n"),
                ScriptRunner::ok("after-stop dind logs\n"),
                ScriptRunner::ok("[{}]\n"),
                ScriptRunner::ok("[{}]\n"),
            ]),
            &identity(),
            &state,
        )
        .unwrap();
        assert!(retry.failures.is_empty(), "{retry:?}");
        assert_eq!(
            std::fs::read(&retry.runner_log).unwrap(),
            b"after-stop runner logs\n"
        );
        assert_eq!(
            std::fs::read(&retry.dind_log).unwrap(),
            b"after-stop dind logs\n"
        );
        assert_eq!(
            std::fs::read(retry.dir.join("capture.complete")).unwrap(),
            DIAGNOSTICS_COMPLETE_MARKER
        );
        Supervision::new(identity(), &state)
            .release_owned_state()
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
            .release_state()
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn diagnostic_completion_marker_is_host_only_and_clearable() {
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

        supervision.clear_diagnostic_completion_marker().unwrap();
        assert!(!supervision.diagnostics_complete().unwrap());
        assert!(!marker.exists());

        supervision.release_owned_state().unwrap();
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
        let dind_inspect =
            r#"[{"Id":"dind123","Config":{"Env":["DIND_SECRET"]},"State":{"Status":"running"}}]"#;
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, "Error: No such object: velnor-scaleset-runner"),
            ScriptRunner::ok("dind-id\n"),
            ScriptRunner::ok("dind output\n"),
            ScriptRunner::ok(dind_inspect),
        ]);
        let export = supervision
            .complete_diagnostics_without_runner(&mut runner)
            .unwrap();
        assert!(export.failures.is_empty(), "{export:?}");
        assert!(supervision.diagnostics_complete().unwrap());
        let diagnostics = SecureDiagnosticDir::open_existing(&state).unwrap().unwrap();
        assert_eq!(
            diagnostics.read_file("capture.complete").unwrap().unwrap(),
            NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER
        );
        assert!(diagnostics.read_file("runner.log").unwrap().is_none());
        assert_eq!(
            diagnostics.read_file("dind.log").unwrap().unwrap(),
            b"dind output\n"
        );
        let persisted_dind = diagnostics.read_file("dind.inspect.json").unwrap().unwrap();
        let persisted_dind = String::from_utf8(persisted_dind).unwrap();
        assert!(persisted_dind.contains("dind123"), "{persisted_dind}");
        assert!(!persisted_dind.contains("DIND_SECRET"), "{persisted_dind}");
        assert!(!persisted_dind.contains("\"Env\""), "{persisted_dind}");

        // Recovery verifies the runner remains absent before trusting its
        // durable no-runner marker. It does not recapture the DinD artifacts.
        let mut replay = ScriptRunner::scripted(vec![ScriptRunner::fail(
            1,
            "Error: No such object: velnor-scaleset-runner",
        )]);
        let replayed = supervision
            .complete_diagnostics_without_runner(&mut replay)
            .unwrap();
        assert!(replayed.failures.is_empty(), "{replayed:?}");
        assert_eq!(replay.seen.len(), 1);
        supervision.release_state().unwrap();
        assert!(!state.exists());
        assert!(!diagnostics.path.exists());
    }

    #[test]
    fn no_runner_retry_preserves_completed_dind_artifacts() {
        let state = temp_state("no-runner-retry-artifacts");
        let supervision = Supervision::new(identity(), &state);
        let mut first = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, "Error: No such object: velnor-scaleset-runner"),
            ScriptRunner::ok("dind-id\n"),
            ScriptRunner::ok("captured dind logs\n"),
            ScriptRunner::ok("not-json ACTIONS_RUNNER_INPUT_JITCONFIG=secret\n"),
        ]);
        let partial = supervision
            .complete_diagnostics_without_runner(&mut first)
            .unwrap();
        assert_eq!(partial.failures.len(), 1, "{partial:?}");
        assert!(!supervision.diagnostics_complete().unwrap());
        assert!(!partial.dir.join("runner.log").exists());
        assert_eq!(
            std::fs::read(&partial.dind_log).unwrap(),
            b"captured dind logs\n"
        );
        assert_eq!(
            std::fs::read(partial.dir.join("dind.log.complete")).unwrap(),
            artifact_marker_contents("dind.log")
        );
        assert!(!partial.dir.join("dind.inspect.json").exists());
        assert!(!partial.dir.join("capture.complete").exists());

        let dind_inspect = r#"[{"Id":"dind123","State":{"Status":"running"}}]"#;
        let mut retry = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, "Error: No such object: velnor-scaleset-runner"),
            ScriptRunner::ok("dind-id\n"),
            ScriptRunner::ok(dind_inspect),
        ]);
        let complete = supervision
            .complete_diagnostics_without_runner(&mut retry)
            .unwrap();
        assert!(complete.failures.is_empty(), "{complete:?}");
        assert_eq!(retry.seen.len(), 3);
        assert!(!retry
            .seen
            .iter()
            .any(|args| args.first().is_some_and(|verb| verb == "logs")));
        assert_eq!(
            std::fs::read(&complete.dind_log).unwrap(),
            b"captured dind logs\n"
        );
        assert!(supervision.diagnostics_complete().unwrap());
        supervision.release_owned_state().unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }

    #[test]
    fn completion_marker_kinds_are_not_interchangeable() {
        let no_runner_state = temp_state("marker-kind-no-runner");
        let no_runner_diagnostics = SecureDiagnosticDir::open(&no_runner_state).unwrap();
        no_runner_diagnostics
            .write_file("capture.complete", NO_RUNNER_DIAGNOSTICS_COMPLETE_MARKER)
            .unwrap();
        let mut full_capture = ScriptRunner::scripted(vec![]);
        let full_capture_result =
            export_diagnostics(&mut full_capture, &identity(), &no_runner_state);
        assert!(full_capture_result
            .unwrap_err()
            .to_string()
            .contains("runner-absent"));
        assert!(full_capture.seen.is_empty());
        Supervision::new(identity(), &no_runner_state)
            .release_owned_state()
            .unwrap();
        std::fs::remove_dir_all(no_runner_state.parent().unwrap()).unwrap();

        let full_state = temp_state("marker-kind-full");
        let full_diagnostics = SecureDiagnosticDir::open(&full_state).unwrap();
        full_diagnostics
            .write_file("capture.complete", DIAGNOSTICS_COMPLETE_MARKER)
            .unwrap();
        let supervision = Supervision::new(identity(), &full_state);
        let mut no_runner_capture = ScriptRunner::scripted(vec![ScriptRunner::fail(
            1,
            "Error: No such object: velnor-scaleset-runner",
        )]);
        let no_runner_result =
            supervision.complete_diagnostics_without_runner(&mut no_runner_capture);
        assert!(no_runner_result
            .unwrap_err()
            .to_string()
            .contains("full diagnostics marker"));
        assert_eq!(no_runner_capture.seen.len(), 1);
        supervision.release_owned_state().unwrap();
        std::fs::remove_dir_all(full_state.parent().unwrap()).unwrap();
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
            .release_state()
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
        assert!(supervision.release_state().is_err());

        // Crash recovery may release incomplete diagnostics only after the
        // durable ownership registry authorizes cleanup.
        supervision.release_owned_state().unwrap();
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
            .release_state()
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
            .release_state()
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
            .release_state()
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
    fn release_paths_remove_only_from_the_pinned_parent_after_swap() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let base = std::env::temp_dir().canonicalize().unwrap();
        let parent = base.join(format!(
            "velnor-release-pinned-parent-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let moved = parent.with_extension("moved");
        let victim = parent.with_extension("victim");
        std::fs::create_dir(&parent).unwrap();
        std::fs::create_dir(&victim).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o700)).unwrap();

        let state_name = std::ffi::OsString::from("worker");
        let private_name = std::ffi::OsString::from(".velnor-jit-worker");
        let diagnostic_name = std::ffi::OsString::from(".velnor-diagnostics-worker");
        let artifact_names = [&state_name, &private_name, &diagnostic_name];
        for artifact_name in artifact_names {
            let host_artifact = parent.join(artifact_name);
            let victim_artifact = victim.join(artifact_name);
            std::fs::create_dir(&host_artifact).unwrap();
            std::fs::create_dir(&victim_artifact).unwrap();
            std::fs::write(host_artifact.join("sentinel"), b"host-owned").unwrap();
            std::fs::write(victim_artifact.join("sentinel"), b"victim-data").unwrap();
        }

        let pinned_parent = super::super::secure_fs::open_absolute_directory(&parent).unwrap();
        super::super::secure_fs::verify_host_parent(&pinned_parent).unwrap();
        std::fs::rename(&parent, &moved).unwrap();
        symlink(&victim, &parent).unwrap();

        release_paths(
            &pinned_parent,
            &state_name,
            &private_name,
            &diagnostic_name,
            super::super::secure_fs::remove_tree_at,
        )
        .unwrap();

        for artifact_name in artifact_names {
            assert!(!moved.join(artifact_name).exists());
            assert_eq!(
                std::fs::read(victim.join(artifact_name).join("sentinel")).unwrap(),
                b"victim-data"
            );
        }

        std::fs::remove_file(&parent).unwrap();
        std::fs::remove_dir(&moved).unwrap();
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
            .release_owned_state()
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
        supervision.release_owned_state().unwrap();
        assert!(!state.exists());
        assert!(!jit_file.exists());
        assert!(!jit_dir.exists());
        // Repeated recovery is safe after partial/previous cleanup.
        supervision.release_owned_state().unwrap();
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
        Supervision::new(identity(), &state)
            .release_owned_state()
            .unwrap();
        std::fs::remove_dir_all(state.parent().unwrap()).unwrap();
    }
}

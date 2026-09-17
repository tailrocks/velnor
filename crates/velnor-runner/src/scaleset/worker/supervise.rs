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

/// Capture logs + inspect of both containers into `state_dir/diagnostics`.
///
/// Runs AFTER the runner stops (logs are complete) and BEFORE any
/// deletion. Every capture is attempted even when an earlier one fails;
/// failures are collected, not raised, so one missing container cannot
/// hide the surviving container's evidence.
pub(crate) fn export_diagnostics(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
) -> Result<DiagnosticExport> {
    let dir = state_dir.join("diagnostics");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create diagnostics dir {}", dir.display()))?;
    let mut failures = Vec::new();

    let runner_log = dir.join("runner.log");
    capture_logs(
        runner,
        &identity.runner_container(),
        &runner_log,
        &mut failures,
    );
    let dind_log = dir.join("dind.log");
    capture_logs(runner, &identity.dind_container(), &dind_log, &mut failures);
    let runner_inspect = dir.join("runner.inspect.json");
    capture_inspect(
        runner,
        &identity.runner_container(),
        &runner_inspect,
        &mut failures,
    );
    let dind_inspect = dir.join("dind.inspect.json");
    capture_inspect(
        runner,
        &identity.dind_container(),
        &dind_inspect,
        &mut failures,
    );

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
            if let Err(error) = std::fs::write(dest, combined) {
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
        Ok(output) if output.code == 0 => {
            if let Err(error) = std::fs::write(dest, &output.stdout) {
                failures.push(format!("write {}: {error}", dest.display()));
            }
        }
        Ok(output) => failures.push(format!(
            "inspect {container} exited {}: {}",
            output.code,
            output.stderr.trim()
        )),
        Err(error) => failures.push(format!("inspect {container}: {error:#}")),
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

/// Tear down every object the worker owns, in dependency order.
///
/// Order: stop runner → export diagnostics → remove runner → stop DinD →
/// remove DinD → remove network → remove volumes. The runner stops first
/// (it shares the DinD netns; removing DinD first would strand it) and
/// diagnostics export before the first deletion. Every step is attempted;
/// failures collect into the report instead of aborting the sequence.
pub(crate) fn owned_cleanup(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    state_dir: &Path,
) -> Result<CleanupReport> {
    let mut failures = Vec::new();
    stop_container(runner, &identity.runner_container(), &mut failures);
    let export = export_diagnostics(runner, identity, state_dir)?;
    remove_container(runner, &identity.runner_container(), &mut failures);
    stop_container(runner, &identity.dind_container(), &mut failures);
    remove_container(runner, &identity.dind_container(), &mut failures);
    remove_network(runner, &identity.network(), &mut failures);
    remove_volume(runner, &identity.workspace_volume(), &mut failures);
    remove_volume(runner, &identity.dind_data_volume(), &mut failures);
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
            ScriptRunner::ok("velnor-scaleset-dind-s7-velnor-set-0007\n"), // start dind
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
            ScriptRunner::ok("{}\n"),      // inspect runner
            ScriptRunner::ok("{}\n"),      // inspect dind
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
            ScriptRunner::ok("{}\n"),
            ScriptRunner::ok("{}\n"),
            ScriptRunner::ok("runner\n"),
            ScriptRunner::ok("dind\n"),
            ScriptRunner::fail(1, "boom"), // rm dind fails
            ScriptRunner::ok("net\n"),
            ScriptRunner::ok("work\n"),
            ScriptRunner::ok("dindata\n"),
        ]);
        let report = owned_cleanup(&mut runner, &identity(), &state).unwrap();
        assert!(!report.confirmed());
        assert_eq!(report.export.failures.len(), 1);
        assert_eq!(report.failures.len(), 2);
        // Every step still ran: 11 docker calls.
        assert_eq!(runner.seen.len(), 11);
        std::fs::remove_dir_all(&state).unwrap();
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
            ScriptRunner::ok("{}\n"),
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

//! One cancellation model for a running job.
//!
//! Velnor previously had no cancellation model at all: a broker cancellation
//! message ran an unbounded `docker kill` against two container names and set a
//! boolean that was only read *after* the executor had already returned. The
//! job kept running its remaining steps, `cancelled()` was hardcoded false,
//! host child processes were never signalled, service containers were never
//! touched, and the MicroVM backend was uncancellable.
//!
//! This module is the whole model. It has three parts:
//!
//! * [`JobCancellation`] — the job token. Two levels, `Requested` then
//!   `Forced`, matching how `JobDispatcher` first cancels the worker and only
//!   then hard kills it (`src/Runner.Listener/JobDispatcher.cs:1280-1285`). A
//!   request first updates job status; the active step re-evaluates its own
//!   condition and only a false/error result cancels that step's work, matching
//!   `StepsRunner` and `CompositeActionHandler`.
//! * [`TerminationTarget`] — everything a cancelled job can still be holding.
//!   Targets register themselves as they are created and deregister when they
//!   are gone, so the fan-out set is derived from what actually exists rather
//!   than from a hardcoded list of two container names.
//! * [`ActiveStepGuard`] — a running step's cancellation callback. The guard
//!   re-evaluates the condition on a job request and deregisters at the step
//!   boundary, so `always()` and `cancelled()` work can survive job cancellation.
//! * [`terminate`] — the one ladder, SIGINT then SIGTERM then SIGKILL, with
//!   upstream's timings (`src/Runner.Sdk/ProcessInvoker.cs:32-33`, escalated in
//!   `CancelAndKillProcessTree`, `:443-447`).
//!
//! The token is in-process by construction. Velnor is a three-tier process
//! fleet, so a cancellation that has to cross a process boundary — the broker
//! message arriving at the slot process, the host telling a guest agent to stop
//! — travels as a message (broker poll, vsock `Cancel`) and is *converted* into
//! this token at the boundary. Nothing here is expected to be visible to
//! another process.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Why a job is being cancelled.
///
/// Job `timeout-minutes` is deliberately one of these rather than a separate
/// mechanism: upstream enforces it on the server and delivers it to the runner
/// as an ordinary cancellation, so it shares this whole ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    /// GitHub sent `JobCancellation` for this job.
    ServerRequested,
    /// The job's own `timeout-minutes` wall clock elapsed.
    JobTimeout,
    /// The runner registration backing this job disappeared, so no further
    /// control message can ever arrive.
    RegistrationLost,
    /// The daemon is shutting down under it.
    DaemonShutdown,
}

impl CancelReason {
    /// Stable label for logs and step messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ServerRequested => "server-requested",
            Self::JobTimeout => "job-timeout",
            Self::RegistrationLost => "registration-lost",
            Self::DaemonShutdown => "daemon-shutdown",
        }
    }

    /// Operator-facing sentence used as the cancelled job's reason.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::ServerRequested => "The job was cancelled by GitHub.",
            Self::JobTimeout => {
                "The job exceeded its `timeout-minutes` wall clock and was cancelled."
            }
            Self::RegistrationLost => {
                "The runner registration for this job disappeared; the job was cancelled because broker control messages can no longer be received."
            }
            Self::DaemonShutdown => "The runner is shutting down; the job was cancelled.",
        }
    }
}

/// How far cancellation has escalated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CancelLevel {
    /// Not cancelled.
    None,
    /// Cancellation requested. Job conditions now read as cancelled; the
    /// active step re-evaluates its condition and is terminated only if that
    /// condition is false or cannot be evaluated. Later eligible work can run.
    Requested,
    /// The grace period is spent. Everything still alive is killed outright.
    Forced,
}

impl CancelLevel {
    const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::None,
            1 => Self::Requested,
            _ => Self::Forced,
        }
    }

    const fn as_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Requested => 1,
            Self::Forced => 2,
        }
    }
}

/// `ProcessInvoker` waits 7.5s after SIGINT before SIGTERM
/// (`src/Runner.Sdk/ProcessInvoker.cs:32`).
pub const DEFAULT_SIGINT_GRACE: Duration = Duration::from_millis(7500);
/// …and 2.5s after SIGTERM before killing the tree
/// (`src/Runner.Sdk/ProcessInvoker.cs:33`).
pub const DEFAULT_SIGTERM_GRACE: Duration = Duration::from_millis(2500);
/// `JobDispatcher` floors the server-supplied cancel timeout at 60s
/// (`src/Runner.Listener/JobDispatcher.cs:1280-1283`).
pub const MIN_CANCEL_GRACE: Duration = Duration::from_secs(60);
/// …and hard kills 15s before it expires
/// (`src/Runner.Listener/JobDispatcher.cs:1285`).
pub const HARD_KILL_LEAD: Duration = Duration::from_secs(15);
/// GitHub's default `timeout-minutes` (360): the collective job wall clock.
/// Upstream enforces job timeouts on the server and delivers them as ordinary
/// cancellations; the [`arm_job_timeout`] enforcer below is the local backstop
/// for when that message never arrives.
pub const DEFAULT_JOB_TIMEOUT: Duration = Duration::from_secs(360 * 60);

/// Effective forced-kill deadline for a server-supplied cancel timeout:
/// `max(timeout, 60s) - 15s` (`src/Runner.Listener/JobDispatcher.cs:1280-1285`).
#[must_use]
pub fn forced_kill_delay(cancel_timeout: Option<Duration>) -> Duration {
    cancel_timeout
        .unwrap_or(MIN_CANCEL_GRACE)
        .max(MIN_CANCEL_GRACE)
        .saturating_sub(HARD_KILL_LEAD)
}

fn env_duration_ms(name: &str, fallback: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(fallback, Duration::from_millis)
}

/// The one escalation ladder's timings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminationLadder {
    /// Wait after SIGINT before escalating to SIGTERM.
    pub sigint_grace: Duration,
    /// Wait after SIGTERM before escalating to SIGKILL.
    pub sigterm_grace: Duration,
}

impl Default for TerminationLadder {
    /// Upstream's timings, overridable per host for operators whose images need
    /// longer to flush. Never unbounded: a missing or unparsable value keeps
    /// the upstream default.
    fn default() -> Self {
        Self {
            sigint_grace: env_duration_ms("VELNOR_CANCEL_SIGINT_TIMEOUT_MS", DEFAULT_SIGINT_GRACE),
            sigterm_grace: env_duration_ms(
                "VELNOR_CANCEL_SIGTERM_TIMEOUT_MS",
                DEFAULT_SIGTERM_GRACE,
            ),
        }
    }
}

/// One step of the ladder, in escalation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminationSignal {
    /// Ctrl+C equivalent. A well-behaved build tool flushes and exits.
    Interrupt,
    /// The conventional "stop now, you may clean up" signal.
    Terminate,
    /// Unignorable.
    Kill,
}

impl TerminationSignal {
    const fn posix(self) -> i32 {
        match self {
            Self::Interrupt => libc::SIGINT,
            Self::Terminate => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
        }
    }

    /// Name accepted by `docker kill --signal`.
    const fn docker(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }

    const ORDER: [Self; 3] = [Self::Interrupt, Self::Terminate, Self::Kill];
}

/// Something a cancelled job can still be holding.
///
/// Every variant is a thing that outlives the Rust value that created it, which
/// is why the set is a registry and not a struct: a host process tree survives
/// its `Command`, a container survives its `docker run` client, a BuildKit
/// solve survives the buildx client that started it.
#[derive(Clone)]
pub enum TerminationTarget {
    /// A host process group. Every host child is spawned as its own group
    /// leader, so signalling `-pgid` reaches the whole tree — the shell, the
    /// compiler it forked, and the daemon that compiler started.
    ProcessGroup {
        /// Group id, equal to the direct child's pid.
        pgid: u32,
        /// What the group is, for logs. Never an argument vector.
        label: String,
    },
    /// A job-level process group that must survive the Requested pass so its
    /// runtime can finish eligible `always()`/`cancelled()` work.
    ProcessGroupAt {
        /// Group id, equal to the direct child's pid.
        pgid: u32,
        /// What the group is, for logs. Never an argument vector.
        label: String,
        /// Earliest cancellation level allowed to terminate this process group.
        terminate_at: CancelLevel,
    },
    /// A Docker container this job owns: the job container, a service
    /// container, a Docker-action sidecar, or a BuildKit builder.
    Container {
        /// Container name.
        name: String,
        /// What the container is, for logs.
        role: ContainerRole,
    },
    /// A target only its owner knows how to stop: a live vsock `Cancel`
    /// followed by `stop_jailer` for the MicroVM backend, or a BuildKit solve
    /// abort. Runs once, at the level the owner registered it for.
    Hook {
        /// What the hook stops, for logs.
        label: String,
        /// Invoked with the level that triggered the fan-out.
        run: Arc<dyn Fn(CancelLevel) -> Result<(), String> + Send + Sync>,
    },
}

impl std::fmt::Debug for TerminationTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProcessGroup { pgid, label } => formatter
                .debug_struct("ProcessGroup")
                .field("pgid", pgid)
                .field("label", label)
                .finish(),
            Self::ProcessGroupAt {
                pgid,
                label,
                terminate_at,
            } => formatter
                .debug_struct("ProcessGroupAt")
                .field("pgid", pgid)
                .field("label", label)
                .field("terminate_at", terminate_at)
                .finish(),
            Self::Container { name, role } => formatter
                .debug_struct("Container")
                .field("name", name)
                .field("role", role)
                .finish(),
            Self::Hook { label, .. } => formatter
                .debug_struct("Hook")
                .field("label", label)
                .finish(),
        }
    }
}

impl TerminationTarget {
    /// The escalation level at which this target may be terminated.
    ///
    /// Not everything a cancelled job holds may die at the first request. The
    /// job container and its service containers must outlive cancellation long
    /// enough for `always()` and `cancelled()` steps to run *against a container
    /// that still exists* — upstream stops them in an `always()` post step,
    /// "Stop containers" (`src/Runner.Worker/ContainerOperationProvider.cs:57-63`),
    /// not on the cancellation callback. Process groups, Docker-action
    /// sidecars, and BuildKit builders are eligible at `Requested`, but the
    /// active step's condition must first be re-evaluated. Upstream calls
    /// `step.ExecutionContext.CancelToken()` only if that condition becomes
    /// false (`src/Runner.Worker/StepsRunner.cs:146-185` and
    /// `src/Runner.Worker/Handlers/CompositeActionHandler.cs:331-371`).
    #[must_use]
    pub const fn terminate_at(&self) -> CancelLevel {
        match self {
            Self::Container {
                role: ContainerRole::Job | ContainerRole::Service,
                ..
            } => CancelLevel::Forced,
            Self::ProcessGroupAt { terminate_at, .. } => *terminate_at,
            _ => CancelLevel::Requested,
        }
    }

    /// Ordering within one fan-out pass: lower runs first.
    ///
    /// A hook is how a target is asked to stop *itself* — the microVM guest's
    /// vsock `Cancel` is one — so hooks precede the process and container
    /// terminations that take the decision away from it.
    #[must_use]
    pub const fn termination_rank(&self) -> u8 {
        match self {
            Self::Hook { .. } => 0,
            Self::ProcessGroup { .. } | Self::ProcessGroupAt { .. } => 1,
            Self::Container {
                role: ContainerRole::DockerAction | ContainerRole::BuildKit,
                ..
            } => 2,
            Self::Container {
                role: ContainerRole::Job | ContainerRole::Service,
                ..
            } => 3,
        }
    }

    /// Deterministic ordering within one fan-out pass.
    fn termination_order(&self) -> (u8, u8) {
        let subrank = match self {
            Self::Hook { .. } | Self::ProcessGroup { .. } | Self::ProcessGroupAt { .. } => 0,
            Self::Container {
                role: ContainerRole::DockerAction,
                ..
            } => 0,
            Self::Container {
                role: ContainerRole::BuildKit,
                ..
            } => 1,
            Self::Container {
                role: ContainerRole::Job,
                ..
            } => 0,
            Self::Container {
                role: ContainerRole::Service,
                ..
            } => 1,
        };
        (self.termination_rank(), subrank)
    }

    /// Stable identity used for deduplication and log lines.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::ProcessGroup { pgid, .. } => format!("pgid:{pgid}"),
            Self::ProcessGroupAt { pgid, .. } => format!("pgid:{pgid}"),
            Self::Container { name, .. } => format!("container:{name}"),
            Self::Hook { label, .. } => format!("hook:{label}"),
        }
    }
}

/// What a container is to the job, so a log line can say which fan-out target
/// did not die without echoing an image reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerRole {
    /// The long-lived job container every `run:` step execs into.
    Job,
    /// A `services:` container.
    Service,
    /// The sidecar a Docker action runs in.
    DockerAction,
    /// The job's BuildKit builder. Killing the buildx client leaves this
    /// sibling daemon solving — and possibly pushing — on its own.
    BuildKit,
}

impl ContainerRole {
    /// Stable label for tracing fields.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Job => "job",
            Self::Service => "service",
            Self::DockerAction => "docker-action",
            Self::BuildKit => "buildkit",
        }
    }
}

/// Outcome of walking one target down the ladder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminationOutcome {
    /// The target's key.
    pub target: String,
    /// The last signal actually delivered, if any.
    pub escalated_to: Option<TerminationSignal>,
    /// Whether the target was observed gone by the time the ladder finished.
    pub gone: bool,
    /// Why the ladder could not finish, when it could not.
    pub error: Option<String>,
}

/// Whether a process group still has any member.
fn process_group_alive(pgid: u32) -> bool {
    // SAFETY: `kill` with signal 0 performs the permission and existence check
    // without delivering anything. A negative pid addresses the process group.
    let result = unsafe { libc::kill(-(pgid as i32), 0) };
    if result == 0 {
        return true;
    }
    // EPERM means it exists but is not ours; ESRCH means it is gone.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn signal_process_group(pgid: u32, signal: TerminationSignal) -> Result<(), String> {
    // SAFETY: a negative pid addresses the process group; every host child is
    // spawned as its own group leader so this can never reach the runner's own
    // group.
    let result = unsafe { libc::kill(-(pgid as i32), signal.posix()) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        // Already gone. Not a failure to terminate.
        return Ok(());
    }
    Err(format!("kill -{} -{pgid}: {error}", signal.label()))
}

/// Bounded `docker` invocation used by the ladder.
///
/// The cancel path is the one place a `docker` call must never be unbounded: an
/// unbounded call against a wedged daemon voids cancellation while GitHub has
/// already been told the job is cancelled. Every call here is classified and
/// bounded by [`crate::docker::deadline_for`].
fn docker_bounded(args: &[String]) -> anyhow::Result<String> {
    let (_, deadline) = crate::docker::deadline_for(args, CONTAINER_FALLBACK_DEADLINE);
    crate::docker::client::host_call_bounded(args, deadline)
}

/// Only reachable if a future `docker` subcommand classifies as `Payload`,
/// which no ladder call does. Named anyway so no seam is unbounded.
const CONTAINER_FALLBACK_DEADLINE: Duration = Duration::from_secs(60);

fn container_alive(name: &str) -> bool {
    // Only a daemon that positively reports the container missing is
    // evidence that it is gone (`NotFound` reads as not running inside the
    // client). Treating *any* inspect failure as "gone" let a wedged or
    // timing-out daemon end the ladder having sent no signal at all, and
    // report the container terminated — the one failure direction
    // cancellation must never take. An unknown state keeps the target alive
    // so the ladder escalates against it.
    crate::docker::Docker::host()
        .container_running(name)
        .unwrap_or(true)
}

fn signal_container(name: &str, signal: TerminationSignal) -> Result<(), String> {
    let args = vec![
        "kill".to_string(),
        "--signal".to_string(),
        signal.docker().to_string(),
        name.to_string(),
    ];
    match docker_bounded(&args) {
        Ok(_) => Ok(()),
        Err(error)
            if crate::docker::client::is_not_found(&error)
                || crate::docker::client::is_not_running(&error) =>
        {
            Ok(())
        }
        Err(error) => Err(format!("{error:#}")),
    }
}

/// Poll interval while waiting for a target to notice a signal. Short enough
/// that a fast exit is observed promptly, long enough not to spin.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

fn deadline_after(start: Instant, duration: Duration) -> Instant {
    start.checked_add(duration).unwrap_or(start)
}

fn wait_until_deadline(deadline: Instant, now: &dyn Fn() -> Instant, sleep: &dyn Fn(Duration)) {
    let remaining = deadline.saturating_duration_since(now());
    if !remaining.is_zero() {
        sleep(remaining);
    }
}

fn wait_until_gone(
    alive: &dyn Fn() -> bool,
    grace: Duration,
    deadline: Instant,
    sleep: &dyn Fn(Duration),
) -> bool {
    let until = deadline_after(Instant::now(), grace).min(deadline);
    loop {
        if !alive() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        sleep(POLL_INTERVAL.min(until.saturating_duration_since(Instant::now())));
    }
}

/// Walk one target down the ladder: SIGINT, then SIGTERM, then SIGKILL.
///
/// `deadline` is the outer bound the whole fan-out shares, so one stubborn
/// target cannot spend the budget of the ones after it. Reaching the deadline
/// jumps straight to SIGKILL rather than giving up: a cancelled job must not
/// leave anything running.
#[must_use]
pub fn terminate(target: &TerminationTarget, deadline: Instant) -> TerminationOutcome {
    terminate_with(
        target,
        deadline,
        TerminationLadder::default(),
        &|duration| {
            std::thread::sleep(duration);
        },
    )
}

/// [`terminate`] with an explicit ladder and sleep, so the escalation order is
/// testable without real signals or real time.
#[must_use]
pub fn terminate_with(
    target: &TerminationTarget,
    deadline: Instant,
    ladder: TerminationLadder,
    sleep: &dyn Fn(Duration),
) -> TerminationOutcome {
    let key = target.key();
    let mut outcome = TerminationOutcome {
        target: key,
        escalated_to: None,
        gone: false,
        error: None,
    };
    match target {
        TerminationTarget::Hook { run, .. } => {
            let level = if Instant::now() >= deadline {
                CancelLevel::Forced
            } else {
                CancelLevel::Requested
            };
            match run(level) {
                Ok(()) => outcome.gone = true,
                Err(detail) => outcome.error = Some(detail),
            }
            return outcome;
        }
        TerminationTarget::ProcessGroup { pgid, .. }
        | TerminationTarget::ProcessGroupAt { pgid, .. } => {
            let pgid = *pgid;
            let alive = move || process_group_alive(pgid);
            run_ladder(
                &mut outcome,
                ladder,
                deadline,
                sleep,
                &alive,
                &move |signal| signal_process_group(pgid, signal),
            );
        }
        TerminationTarget::Container { name, .. } => {
            let name = name.clone();
            let alive_name = name.clone();
            let alive = move || container_alive(&alive_name);
            run_ladder(
                &mut outcome,
                ladder,
                deadline,
                sleep,
                &alive,
                &move |signal| signal_container(&name, signal),
            );
        }
    }
    outcome
}

fn run_ladder(
    outcome: &mut TerminationOutcome,
    ladder: TerminationLadder,
    deadline: Instant,
    sleep: &dyn Fn(Duration),
    alive: &dyn Fn() -> bool,
    signal: &dyn Fn(TerminationSignal) -> Result<(), String>,
) {
    if !alive() {
        outcome.gone = true;
        return;
    }
    for step in TerminationSignal::ORDER {
        // Past the shared deadline nothing but SIGKILL is worth sending.
        if Instant::now() >= deadline && step != TerminationSignal::Kill {
            continue;
        }
        if let Err(detail) = signal(step) {
            outcome.error = Some(detail);
            // A signal that could not be delivered is never treated as
            // success; keep escalating in case the failure was transient.
            continue;
        }
        outcome.escalated_to = Some(step);
        let grace = match step {
            TerminationSignal::Interrupt => ladder.sigint_grace,
            TerminationSignal::Terminate => ladder.sigterm_grace,
            // Nothing survives SIGKILL, but the kernel still needs a moment to
            // tear the group or the Engine to reap the container.
            TerminationSignal::Kill => ladder.sigterm_grace,
        };
        if wait_until_gone(alive, grace, deadline, sleep) {
            outcome.gone = true;
            outcome.error = None;
            return;
        }
    }
    outcome.gone = !alive();
    if outcome.gone {
        outcome.error = None;
    } else if outcome.error.is_none() {
        outcome.error = Some("target survived SIGKILL".to_string());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FanOutScope {
    All,
    Job,
    Step(u64),
}

impl FanOutScope {
    fn includes(self, step_id: Option<u64>) -> bool {
        match self {
            Self::All => true,
            Self::Job => step_id.is_none(),
            Self::Step(expected) => step_id == Some(expected),
        }
    }
}

struct RegisteredTarget {
    id: u64,
    step_id: Option<u64>,
    target: TerminationTarget,
}

#[derive(Default)]
struct Registry {
    targets: Vec<RegisteredTarget>,
    /// Ids already put through the ladder. Termination is idempotent per
    /// target: a fan-out triggered by a late registration must not replay the
    /// ladder against targets an earlier fan-out already terminated, or a
    /// cancelled job sends a second kill to a container that is already gone
    /// and records a duplicate outcome.
    terminated: std::collections::HashSet<u64>,
    /// Targets currently being processed by another fan-out pass. A failed
    /// target is removed from this set and stays eligible for the next pass.
    in_flight: std::collections::HashSet<u64>,
}

struct Inner {
    level: AtomicU8,
    reason: Mutex<Option<CancelReason>>,
    /// Serializes request-time state capture so the absolute forced deadline
    /// is recorded before any fan-out worker can start.
    request_state: Mutex<()>,
    /// Delay from the first request to forced escalation, seeded from the
    /// server-supplied cancel timeout.
    /// Millis until forced escalation. Mutable because the grace arrives with
    /// the server's cancellation message, after the token exists.
    forced_after_ms: AtomicU64,
    /// Absolute monotonic deadline captured at the first cancellation request.
    forced_deadline: Mutex<Option<Instant>>,
    registry: Mutex<Registry>,
    next_id: AtomicU64,
    request_observers: Mutex<Vec<(u64, Arc<RequestObserver>)>>,
    next_observer_id: AtomicU64,
    active_steps: Mutex<Vec<Arc<ActiveStep>>>,
    next_step_id: AtomicU64,
    /// Set once so a repeated request never starts a second deadline watcher.
    deadline_watcher_started: AtomicBool,
    /// Whether this token owns its escalation deadline. Remote workers use
    /// the host's Forced pass as the authority for their grace period.
    auto_force: bool,
    /// Outcomes of the last fan-out, for tests and forensics.
    outcomes: Mutex<Vec<TerminationOutcome>>,
    /// When `false` the token records state and runs registered hooks but
    /// never signals a real process or container. Used by unit tests.
    live: bool,
}

/// A running job's cancellation token.
///
/// Cloning shares one state; the token is a handle, not a copy. It is a
/// *required* field of every type that represents a running job, so a future
/// code path cannot execute a step without one.
#[derive(Clone)]
pub struct JobCancellation(Arc<Inner>);

impl Default for JobCancellation {
    /// A **live** token that nobody has cancelled.
    ///
    /// `JobExecutionState` derives `Default` for builder and test construction,
    /// so this exists to satisfy that, not to be a general constructor. It is
    /// deliberately live rather than recording: if a real job ever ran on a
    /// defaulted token and were then cancelled, a recording token would log the
    /// termination and kill nothing, which is exactly the silent-inertness this
    /// work exists to remove. A live token that is never cancelled behaves as a
    /// job with no cancellation source, which is honest.
    fn default() -> Self {
        Self::new(None)
    }
}

impl std::fmt::Debug for JobCancellation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JobCancellation")
            .field("level", &self.level())
            .field("reason", &self.reason())
            .finish()
    }
}

/// A registration that removes its target when dropped, so the fan-out set
/// only ever names things that still exist.
pub struct TargetRegistration {
    token: JobCancellation,
    id: u64,
}

impl std::fmt::Debug for TargetRegistration {
    /// Names the registration without printing the token, whose `Debug` walks
    /// the whole registry.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TargetRegistration")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for TargetRegistration {
    fn drop(&mut self) {
        self.token.deregister(self.id);
    }
}

/// A registration for a job-status notification callback.
#[derive(Debug)]
pub struct RequestObserverRegistration {
    token: JobCancellation,
    id: u64,
    registered: bool,
}

impl Drop for RequestObserverRegistration {
    fn drop(&mut self) {
        if self.registered {
            self.token.end_request_observer(self.id);
        }
    }
}

type StepConditionRecheck = dyn Fn() -> Result<bool, String> + Send + Sync + 'static;
type RequestObserver = dyn Fn(CancelReason) -> Result<(), String> + Send + Sync + 'static;

struct ActiveStep {
    id: u64,
    /// Absent when the job was already cancelled before this step registered,
    /// matching Runner's callback registration guard.
    recheck: Option<Arc<StepConditionRecheck>>,
    cancelled: AtomicBool,
    condition_error: Mutex<Option<String>>,
}

/// Cancellation callback registration for one running step.
///
/// The callback returns the running step condition after the job token has
/// become cancelled. `Ok(true)` keeps the step running; `Ok(false)` or `Err`
/// cancels its current work. The error is retained for the executor to report
/// on that step, as Runner does through `ExecutionContext.Error`.
pub struct ActiveStepGuard {
    token: JobCancellation,
    step: Arc<ActiveStep>,
}

impl ActiveStepGuard {
    /// Whether the job cancellation callback cancelled this step.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.step.cancelled.load(Ordering::SeqCst)
    }

    /// Condition error from cancellation-time re-evaluation, if any.
    #[must_use]
    pub fn condition_error(&self) -> Option<String> {
        self.step
            .condition_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for ActiveStepGuard {
    fn drop(&mut self) {
        self.token.end_step(self.step.id);
    }
}

impl JobCancellation {
    /// A token for a job that can be cancelled, with the forced-escalation
    /// delay derived from the server-supplied cancel timeout.
    #[must_use]
    pub fn new(cancel_timeout: Option<Duration>) -> Self {
        Self::with_forced_delay(forced_kill_delay(cancel_timeout), true, true)
    }

    /// A token that can never be cancelled.
    ///
    /// This is the honest value for work that is definitionally not the job:
    /// post-completion teardown, cleanup engines, and expression rendering
    /// outside a running job. Upstream uses the same shape for post steps,
    /// which get a fresh unlinked `CancellationTokenSource`
    /// (`src/Runner.Worker/ExecutionContext.cs:436`, reached with `null` from
    /// `CreatePostChild`, `:1384-1395`).
    #[must_use]
    pub fn inert() -> Self {
        Self::with_forced_delay(forced_kill_delay(None), true, true)
    }

    /// A token whose ladder records what it would do without signalling
    /// anything. Registered hooks still run.
    #[must_use]
    pub fn recording(cancel_timeout: Option<Duration>) -> Self {
        Self::with_forced_delay(forced_kill_delay(cancel_timeout), false, true)
    }

    /// A live token whose Forced pass is owned by a remote supervisor.
    ///
    /// Requested cancellation still rechecks step conditions and interrupts
    /// false-condition work. This token does not start its own deadline
    /// watcher; the supervisor's Forced pass is authoritative. Use this for a
    /// guest process controlled by a host cancellation token with its own
    /// server-supplied grace period.
    #[must_use]
    pub fn remote() -> Self {
        Self::with_forced_delay(forced_kill_delay(None), true, false)
    }

    fn with_forced_delay(forced_after: Duration, live: bool, auto_force: bool) -> Self {
        Self(Arc::new(Inner {
            level: AtomicU8::new(CancelLevel::None.as_u8()),
            reason: Mutex::new(None),
            request_state: Mutex::new(()),
            forced_after_ms: AtomicU64::new(
                u64::try_from(forced_after.as_millis()).unwrap_or(u64::MAX),
            ),
            forced_deadline: Mutex::new(None),
            registry: Mutex::new(Registry::default()),
            next_id: AtomicU64::new(0),
            request_observers: Mutex::new(Vec::new()),
            next_observer_id: AtomicU64::new(0),
            active_steps: Mutex::new(Vec::new()),
            next_step_id: AtomicU64::new(0),
            deadline_watcher_started: AtomicBool::new(false),
            auto_force,
            outcomes: Mutex::new(Vec::new()),
            live,
        }))
    }

    /// A fresh token that shares nothing with this one.
    ///
    /// Post steps run under one of these so nothing can cancel them, exactly as
    /// upstream gives every post child a new `CancellationTokenSource`
    /// (`src/Runner.Worker/ExecutionContext.cs:436`, `:1384-1395`).
    #[must_use]
    pub fn unlinked(&self) -> Self {
        Self::with_forced_delay(self.forced_after(), self.0.live, self.0.auto_force)
    }

    /// Current escalation level.
    #[must_use]
    pub fn level(&self) -> CancelLevel {
        CancelLevel::from_u8(self.0.level.load(Ordering::SeqCst))
    }

    /// Whether cancellation has been requested at any level.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.level() != CancelLevel::None
    }

    /// Whether the grace period is spent.
    #[must_use]
    pub fn is_forced(&self) -> bool {
        self.level() == CancelLevel::Forced
    }

    /// Register a one-shot observer for the job-status transition.
    ///
    /// Observers run on every first request, whether or not the current step's
    /// condition permits cancelling that step. A runtime bridge uses this to
    /// tell a remote executor that job status changed; that executor then
    /// re-evaluates its own active step condition. If registration happens
    /// after cancellation, the observer runs immediately with the original
    /// reason.
    #[must_use]
    pub fn register_request_observer<F>(&self, observer: F) -> RequestObserverRegistration
    where
        F: Fn(CancelReason) -> Result<(), String> + Send + Sync + 'static,
    {
        let _request_state = self
            .0
            .request_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = self.0.next_observer_id.fetch_add(1, Ordering::SeqCst);
        let observer = Arc::new(observer) as Arc<RequestObserver>;
        let reason = self.reason();
        if let Some(reason) = reason {
            drop(_request_state);
            Self::notify_request_observer(&observer, reason);
            RequestObserverRegistration {
                token: self.clone(),
                id,
                registered: false,
            }
        } else {
            self.0
                .request_observers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((id, observer));
            RequestObserverRegistration {
                token: self.clone(),
                id,
                registered: true,
            }
        }
    }

    fn notify_request_observer(observer: &Arc<RequestObserver>, reason: CancelReason) {
        if let Err(error) = observer(reason) {
            eprintln!(
                "job cancellation notification failed ({}): {error}",
                reason.label()
            );
        }
    }

    /// Register the active step's condition recheck for job cancellation.
    ///
    /// Call before evaluating the initial condition. A cancellation that
    /// arrives during this scope marks job status cancelled, calls `recheck`,
    /// and terminates Requested targets only for `Ok(false)` or `Err`. If the
    /// token was already cancelled, no callback is registered and the initial
    /// condition evaluation observes the cancelled job state, matching Runner.
    /// Dropping the guard deactivates the callback at the step boundary.
    #[must_use]
    pub fn begin_step<F>(&self, recheck: F) -> ActiveStepGuard
    where
        F: Fn() -> Result<bool, String> + Send + Sync + 'static,
    {
        // Serialize with the first cancellation transition so a cancellation
        // can either capture this callback or observe that Runner would have
        // skipped callback registration because the token was already fired.
        let _request_state = self
            .0
            .request_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let recheck =
            (!self.is_cancelled()).then(|| Arc::new(recheck) as Arc<StepConditionRecheck>);
        let step = Arc::new(ActiveStep {
            id: self.0.next_step_id.fetch_add(1, Ordering::SeqCst),
            recheck,
            cancelled: AtomicBool::new(false),
            condition_error: Mutex::new(None),
        });
        self.0
            .active_steps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::clone(&step));
        ActiveStepGuard {
            token: self.clone(),
            step,
        }
    }

    /// Whether any currently active step was cancelled after re-evaluation.
    #[must_use]
    pub fn active_step_cancelled(&self) -> bool {
        self.0
            .active_steps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|step| step.cancelled.load(Ordering::SeqCst))
    }

    /// The innermost active step, matching nested Runner execution contexts.
    fn current_active_step(&self) -> Option<Arc<ActiveStep>> {
        self.0
            .active_steps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last()
            .cloned()
    }

    /// Whether cancellation should stop the work currently using this token.
    ///
    /// A requested job cancellation does not stop the innermost active step
    /// whose condition still passes (`always()` / `cancelled()` work), even if
    /// an enclosing composite condition fails. With no active step, new
    /// job-scoped work must stop. Forced always stops work that a Requested
    /// pass spared.
    #[must_use]
    pub fn should_abort_work(&self) -> bool {
        match self.level() {
            CancelLevel::None => false,
            CancelLevel::Forced => true,
            CancelLevel::Requested => {
                let Some(step) = self.current_active_step() else {
                    return true;
                };
                self.is_forced() || step.cancelled.load(Ordering::SeqCst)
            }
        }
    }

    /// Whether a spawned process should be registered for cancellation.
    ///
    /// Always register. `register` associates the target with the innermost
    /// active step, then applies the correct Requested or Forced fan-out. This
    /// also closes the spawn/register race for work whose condition just failed.
    #[must_use]
    pub fn should_track_process_group(&self) -> bool {
        true
    }

    /// Why the job is being cancelled, once it is.
    #[must_use]
    pub fn reason(&self) -> Option<CancelReason> {
        *self
            .0
            .reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// How long after the first request the token escalates to `Forced`.
    #[must_use]
    pub fn forced_after(&self) -> Duration {
        Duration::from_millis(self.0.forced_after_ms.load(Ordering::SeqCst))
    }

    fn forced_deadline(&self) -> Option<Instant> {
        *self
            .0
            .forced_deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Replace the grace before escalation with the one the server asked for.
    ///
    /// Upstream's `JobCancelMessage` carries a timeout and the listener honours
    /// it; Velnor discarded the field and always used its own default, so a
    /// server asking for a longer wind-down did not get one. Ignored once the
    /// escalation timer is already running, so a late message cannot extend a
    /// cancellation that is already counting down.
    pub fn set_forced_after(&self, grace: Duration) {
        let _request_state = self
            .0
            .request_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_cancelled() || self.0.deadline_watcher_started.load(Ordering::SeqCst) {
            return;
        }
        self.0.forced_after_ms.store(
            u64::try_from(grace.as_millis()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
    }

    /// Request job cancellation and re-evaluate every active step condition.
    ///
    /// Returns `true` when this call is the one that cancelled the job.
    /// Repeated cancellation is idempotent: the reason of the first request
    /// stands. The active step's work enters the termination ladder only when
    /// its re-evaluated condition is false or errors; Forced still kills every
    /// registered target at the deadline.
    pub fn request(&self, reason: CancelReason) -> bool {
        let (transitioned, observers, current_step) = {
            let _request_state = self
                .0
                .request_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let transitioned = self
                .0
                .level
                .compare_exchange(
                    CancelLevel::None.as_u8(),
                    CancelLevel::Requested.as_u8(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok();
            if transitioned {
                *self
                    .0
                    .reason
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason);
                let deadline = deadline_after(Instant::now(), self.forced_after());
                *self
                    .0
                    .forced_deadline
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(deadline);
            }
            let observers = if transitioned {
                self.0
                    .request_observers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter()
                    .map(|(_, observer)| Arc::clone(observer))
                    .collect()
            } else {
                Vec::new()
            };
            let current_step = transitioned.then(|| self.current_active_step()).flatten();
            (transitioned, observers, current_step)
        };
        if !transitioned {
            return false;
        }

        self.recheck_active_steps(reason);
        let fan_out_scope = Self::requested_fan_out_scope(reason, current_step.as_deref());
        if let Some(scope) = fan_out_scope {
            self.spawn_requested_fan_out(scope);
        }
        if self.0.auto_force {
            self.spawn_deadline_watcher();
        }
        // Remote notification can block on its transport. Complete local
        // condition rechecks and start the forced deadline first so a stalled
        // guest cannot hold up cancellation of the host's active step.
        for observer in observers {
            Self::notify_request_observer(&observer, reason);
        }
        true
    }

    fn requested_fan_out_scope(
        reason: CancelReason,
        current_step: Option<&ActiveStep>,
    ) -> Option<FanOutScope> {
        if reason == CancelReason::DaemonShutdown {
            Some(FanOutScope::All)
        } else if let Some(step) = current_step {
            step.cancelled
                .load(Ordering::SeqCst)
                .then_some(FanOutScope::Step(step.id))
        } else {
            // Job setup has no step callback yet. Stop job-scoped processes;
            // later step work registers under its own condition guard.
            Some(FanOutScope::Job)
        }
    }

    fn recheck_active_steps(&self, reason: CancelReason) {
        let active_steps = self
            .0
            .active_steps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for step in active_steps {
            let result = if reason == CancelReason::DaemonShutdown {
                // Runner shutdown skips condition evaluation and leaves the
                // default recheck result false before calling `CancelToken`.
                Ok(false)
            } else if let Some(recheck) = step.recheck.as_ref() {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| recheck()))
                    .unwrap_or_else(|_| Err("condition re-evaluation panicked".to_string()))
            } else {
                continue;
            };
            let still_active = self
                .0
                .active_steps
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .any(|active| active.id == step.id);
            if !still_active {
                continue;
            }
            match result {
                Ok(true) => {}
                Ok(false) => step.cancelled.store(true, Ordering::SeqCst),
                Err(error) => {
                    *step
                        .condition_error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                    step.cancelled.store(true, Ordering::SeqCst);
                }
            }
        }
    }

    /// Escalate to `Forced` without waiting for the grace period.
    pub fn force(&self) {
        self.force_at(Instant::now());
    }

    fn force_at(&self, deadline: Instant) {
        let _request_state = self
            .0
            .request_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.0
            .level
            .store(CancelLevel::Forced.as_u8(), Ordering::SeqCst);
        drop(_request_state);
        // Escalation means "kill what the request deliberately spared". Setting
        // the level alone would leave the job and service containers alive
        // until something else happened to sweep, which is the leak this level
        // exists to close.
        self.fan_out_once_at(Some(deadline));
    }

    fn end_step(&self, id: u64) {
        self.0
            .active_steps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|step| step.id != id);
    }

    fn end_request_observer(&self, id: u64) {
        self.0
            .request_observers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(existing, _)| *existing != id);
    }

    /// Register a target with the fan-out. The registration removes it on drop.
    #[must_use]
    pub fn register(&self, target: TerminationTarget) -> TargetRegistration {
        let id = self.0.next_id.fetch_add(1, Ordering::SeqCst);
        let active_step = self.current_active_step();
        let step_id = active_step.as_ref().map(|step| step.id);
        self.0
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .targets
            .push(RegisteredTarget {
                id,
                step_id,
                target,
            });
        // A late target belongs to the innermost step that registered it.
        // Kill only that step's work when its condition failed; a cancelled
        // composite wrapper must not kill an eligible embedded child. With no
        // active step, a requested cancellation still stops job-scoped setup.
        let fan_out_scope = if self.is_forced() {
            Some(FanOutScope::All)
        } else if self.is_cancelled() {
            match active_step {
                Some(step) if step.cancelled.load(Ordering::SeqCst) => {
                    Some(FanOutScope::Step(step.id))
                }
                Some(_) => None,
                None => Some(FanOutScope::Job),
            }
        } else {
            None
        };
        if let Some(scope) = fan_out_scope {
            // Registration may happen from inside a running hook. Keep this
            // pass non-blocking so a hook cannot wait on the fan-out thread
            // that is currently invoking it.
            self.fan_out_scope_at(scope, self.forced_deadline());
        }
        TargetRegistration {
            token: self.clone(),
            id,
        }
    }

    fn deregister(&self, id: u64) {
        let mut registry = self
            .0
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.targets.retain(|target| target.id != id);
        // Ids are never reused (`next_id` only increments), so forgetting the
        // termination mark here keeps the set bounded by the live registry
        // rather than by the number of targets the job ever created.
        registry.terminated.remove(&id);
        registry.in_flight.remove(&id);
    }

    /// Targets currently registered, for tests and forensics.
    #[must_use]
    pub fn target_keys(&self) -> Vec<String> {
        self.0
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .targets
            .iter()
            .map(|target| target.target.key())
            .collect()
    }

    /// Outcomes recorded by the fan-out so far.
    #[must_use]
    pub fn outcomes(&self) -> Vec<TerminationOutcome> {
        self.0
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn spawn_requested_fan_out(&self, scope: FanOutScope) {
        let token = self.clone();
        let forced_deadline = self
            .forced_deadline()
            .unwrap_or_else(|| deadline_after(Instant::now(), self.forced_after()));
        let initial_token = token.clone();
        let spawned = std::thread::Builder::new()
            .name("velnor-cancel-ladder".into())
            .spawn(move || {
                initial_token.fan_out_scope_at(scope, Some(forced_deadline));
                // If the deadline watcher had to skip a target still being
                // processed by this pass, retry it after the in-flight work
                // releases its claim.
                if initial_token.is_forced() {
                    initial_token.fan_out_once_at(Some(forced_deadline));
                }
            });
        if let Err(error) = spawned {
            // A host that cannot spawn a thread still has to terminate the
            // cancelled step rather than leaving its process group alive.
            eprintln!("cancellation ladder thread could not be spawned ({error}); running inline");
            self.fan_out_scope_at(scope, Some(forced_deadline));
            if self.is_forced() {
                self.fan_out_once_at(Some(forced_deadline));
            }
        }
    }

    fn spawn_deadline_watcher(&self) {
        if self
            .0
            .deadline_watcher_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let forced_deadline = self
            .forced_deadline()
            .unwrap_or_else(|| deadline_after(Instant::now(), self.forced_after()));
        let timer_token = self.clone();
        let timer_spawned = std::thread::Builder::new()
            .name("velnor-cancel-deadline".into())
            .spawn(move || {
                wait_until_deadline(forced_deadline, &|| Instant::now(), &|duration| {
                    std::thread::sleep(duration)
                });
                if timer_token.is_cancelled() {
                    // Everything that survived the ladder gets one forced pass
                    // at the absolute deadline, mirroring the listener's
                    // cancel-then-hard-kill pair
                    // (`src/Runner.Listener/JobDispatcher.cs:1280-1285`).
                    timer_token.force_at(forced_deadline);
                }
            });
        if let Err(error) = timer_spawned {
            // Preserve the absolute deadline even if the timer thread cannot
            // be created. This rare fallback may block the request caller, but
            // it cannot silently extend or omit forced escalation.
            eprintln!(
                "cancellation deadline thread could not be spawned ({error}); waiting inline"
            );
            wait_until_deadline(forced_deadline, &|| Instant::now(), &|duration| {
                std::thread::sleep(duration)
            });
            if self.is_cancelled() {
                self.force_at(forced_deadline);
            }
        }
    }

    /// Run the ladder over every registered target once.
    ///
    /// Public so the daemon-shutdown path can drive a synchronous fan-out and
    /// observe the outcome before it exits.
    pub fn fan_out_once(&self) {
        loop {
            self.fan_out_once_at(self.forced_deadline());
            let in_flight = !self
                .0
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .in_flight
                .is_empty();
            if !in_flight {
                return;
            }
            // Cancellation starts its condition-approved pass asynchronously. A synchronous
            // caller must not return while that pass still owns a target, or
            // it can observe an incomplete outcome set.
            std::thread::yield_now();
        }
    }

    fn fan_out_once_at(&self, forced_deadline: Option<Instant>) {
        self.fan_out_scope_at(FanOutScope::All, forced_deadline);
    }

    fn fan_out_scope_at(&self, scope: FanOutScope, forced_deadline: Option<Instant>) {
        // Claim the unterminated targets under one lock, marking them before
        // the ladder runs, so a concurrent registration cannot select the same
        // target for a second fan-out.
        let targets: Vec<(u64, TerminationTarget)> = {
            let mut registry = self
                .0
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // A target is eligible only once the escalation has reached the
            // level it is allowed to die at. The Requested pass starts only
            // after the active step's condition re-evaluates false, so a
            // job-level request can preserve `always()`/`cancelled()` work.
            //
            // An explicit fan-out on an uncancelled token is a teardown sweep,
            // not a no-op: floor the comparison at `Requested` so a caller that
            // reaches for the ladder directly still gets the step-scoped
            // targets it asked for.
            let level = match CancelLevel::from_u8(self.0.level.load(Ordering::SeqCst)) {
                CancelLevel::None => CancelLevel::Requested,
                level => level,
            };
            let scope = if level == CancelLevel::Forced {
                FanOutScope::All
            } else {
                scope
            };
            let mut pending: Vec<(u64, TerminationTarget)> = registry
                .targets
                .iter()
                .filter(|registered| {
                    scope.includes(registered.step_id)
                        && !registry.terminated.contains(&registered.id)
                        && !registry.in_flight.contains(&registered.id)
                        && registered.target.terminate_at().as_u8() <= level.as_u8()
                })
                .map(|registered| (registered.id, registered.target.clone()))
                .collect();
            // Registration order is an accident of startup sequencing. Sort
            // all classes explicitly so reverse registration produces the
            // same cancellation order: Hook -> ProcessGroup -> DockerAction /
            // BuildKit -> Job / Service.
            pending.sort_by(|(left_id, left), (right_id, right)| {
                left.termination_order()
                    .cmp(&right.termination_order())
                    .then_with(|| left.key().cmp(&right.key()))
                    .then_with(|| left_id.cmp(right_id))
            });
            for (id, _) in &pending {
                registry.in_flight.insert(*id);
            }
            pending
        };
        if targets.is_empty() {
            return;
        }
        let ladder = TerminationLadder::default();
        // Every target shares one bound, so a wedged Docker daemon cannot make
        // the fan-out itself unbounded.
        let deadline = forced_deadline.unwrap_or_else(|| {
            deadline_after(
                Instant::now(),
                ladder
                    .sigint_grace
                    .saturating_add(ladder.sigterm_grace)
                    .saturating_mul(u32::try_from(targets.len().max(1)).unwrap_or(u32::MAX))
                    .saturating_add(Duration::from_secs(30)),
            )
        });
        for (id, target) in &targets {
            let outcome = if self.0.live {
                terminate_with(target, deadline, ladder, &|duration| {
                    std::thread::sleep(duration);
                })
            } else {
                recorded_outcome(
                    target,
                    if deadline <= Instant::now() {
                        CancelLevel::Forced
                    } else {
                        CancelLevel::Requested
                    },
                )
            };
            if let Some(error) = outcome.error.as_deref() {
                eprintln!(
                    "Cancellation could not terminate {}: {error}",
                    outcome.target
                );
                tracing::warn!(
                    target: "velnor.cancel",
                    fan_out_target = outcome.target.as_str(),
                    "cancellation fan-out target survived the termination ladder"
                );
            }
            let outcome_gone = outcome.gone;
            self.0
                .outcomes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(outcome);
            // Publish the outcome before releasing `in_flight`: synchronous
            // callers use that marker as the completion boundary.
            let mut registry = self
                .0
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.in_flight.remove(id);
            if outcome_gone
                && registry
                    .targets
                    .iter()
                    .any(|registered| registered.id == *id)
            {
                registry.terminated.insert(*id);
            }
        }
    }
}

/// Wall-clock enforcer: requests [`CancelReason::JobTimeout`] on the job's
/// token when `timeout` elapses. Disarms on drop, so a finished job never
/// fires late. One watchdog thread per armed job; it exits on disarm or on
/// firing, so an armed-then-dropped enforcer leaves nothing behind.
pub struct JobTimeoutEnforcer {
    disarm: Option<std::sync::mpsc::Sender<()>>,
}

impl Drop for JobTimeoutEnforcer {
    fn drop(&mut self) {
        // The watchdog exits on its own once disarmed; no join, so dropping
        // from an async worker never blocks job teardown.
        if let Some(disarm) = self.disarm.take() {
            let _ = disarm.send(());
        }
    }
}

/// Arm the job wall clock: `on_timeout` runs and [`CancelReason::JobTimeout`]
/// is requested when `timeout` elapses without the returned guard being
/// dropped. `request` is idempotent, so firing into an already-cancelled job
/// keeps the first reason.
pub fn arm_job_timeout<F>(
    token: &JobCancellation,
    timeout: Duration,
    on_timeout: F,
) -> JobTimeoutEnforcer
where
    F: FnOnce() + Send + 'static,
{
    let (disarm_tx, disarm_rx) = std::sync::mpsc::channel::<()>();
    let token = token.clone();
    let spawned = std::thread::Builder::new()
        .name("velnor-job-timeout".into())
        .spawn(move || {
            if disarm_rx.recv_timeout(timeout).is_err() {
                // Elapsed, not disarmed.
                eprintln!("Job exceeded its timeout-minutes wall clock; requesting cancellation.");
                on_timeout();
                token.request(CancelReason::JobTimeout);
            }
        });
    match spawned {
        Ok(_) => JobTimeoutEnforcer {
            disarm: Some(disarm_tx),
        },
        Err(error) => {
            // A host that cannot spawn a thread cannot enforce the wall
            // clock; say so loudly rather than pretending it is armed.
            eprintln!(
                "job-timeout watchdog thread could not be spawned ({error}); the job wall clock is not enforced"
            );
            JobTimeoutEnforcer { disarm: None }
        }
    }
}

fn recorded_outcome(target: &TerminationTarget, level: CancelLevel) -> TerminationOutcome {
    let mut outcome = TerminationOutcome {
        target: target.key(),
        escalated_to: Some(TerminationSignal::Interrupt),
        gone: true,
        error: None,
    };
    if let TerminationTarget::Hook { run, .. } = target
        && let Err(detail) = run(level)
    {
        outcome.error = Some(detail);
        outcome.gone = false;
    }
    outcome
}

/// The cancellation of the job running on this process, if any.
///
/// `ProcessCommandRunner` is the single seam every host process spawn passes
/// through, and it has no job-shaped context of its own. Installing the active
/// job's token here is what lets every spawned process group register itself
/// without each call site remembering to.
static ACTIVE: OnceLock<Mutex<Option<JobCancellation>>> = OnceLock::new();

fn active_slot() -> &'static Mutex<Option<JobCancellation>> {
    ACTIVE.get_or_init(|| Mutex::new(None))
}

/// The active job's token, if a job is running on this process.
#[must_use]
pub fn active() -> Option<JobCancellation> {
    active_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Install `token` as the active job's cancellation for as long as the returned
/// guard lives.
#[must_use]
pub fn set_active(token: JobCancellation) -> ActiveGuard {
    let previous = active_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .replace(token);
    ActiveGuard { previous }
}

/// Restores the previously active token on drop.
pub struct ActiveGuard {
    previous: Option<JobCancellation>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        *active_slot()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self.previous.take();
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

    /// Wait for the shared outcomes log to reach `expected` entries.
    ///
    /// A failed step-condition recheck starts a detached ladder thread, so a
    /// synchronous `fan_out_once` can observe an empty log while that thread
    /// still holds the targets. Both passes append to the same log in
    /// deterministic order; waiting keeps ordering assertions exact without
    /// racing the scheduler. Times out rather than hanging a wedged ladder.
    fn wait_for_outcomes(token: &JobCancellation, expected: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while token.outcomes().len() < expected && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn forced_kill_delay_floors_at_sixty_seconds_minus_the_lead() {
        // `JobDispatcher.cs:1280-1285`.
        assert_eq!(forced_kill_delay(None), Duration::from_secs(45));
        assert_eq!(
            forced_kill_delay(Some(Duration::from_secs(10))),
            Duration::from_secs(45)
        );
        assert_eq!(
            forced_kill_delay(Some(Duration::from_secs(300))),
            Duration::from_secs(285)
        );
    }

    #[test]
    fn default_ladder_matches_upstream_process_invoker() {
        // `src/Runner.Sdk/ProcessInvoker.cs:32-33`.
        assert_eq!(DEFAULT_SIGINT_GRACE, Duration::from_millis(7500));
        assert_eq!(DEFAULT_SIGTERM_GRACE, Duration::from_millis(2500));
    }

    #[test]
    fn request_is_idempotent_and_keeps_the_first_reason() {
        let token = JobCancellation::recording(None);
        assert!(!token.is_cancelled());
        assert!(token.request(CancelReason::ServerRequested));
        assert!(!token.request(CancelReason::JobTimeout));
        assert_eq!(token.reason(), Some(CancelReason::ServerRequested));
        assert_eq!(token.level(), CancelLevel::Requested);
    }

    #[test]
    fn abort_predicate_tracks_active_step_decision_not_job_status_alone() {
        let token = JobCancellation::recording(None);
        assert!(!token.should_abort_work());
        let always_step = token.begin_step(|| Ok(true));

        assert!(token.request(CancelReason::ServerRequested));
        assert!(token.is_cancelled());
        assert!(!always_step.is_cancelled());
        assert!(
            !token.should_abort_work(),
            "requested cancellation must let eligible active work finish"
        );

        drop(always_step);
        assert!(
            token.should_abort_work(),
            "requested cancellation must prevent new work between steps"
        );

        let ordinary = JobCancellation::recording(None);
        let ordinary_step = ordinary.begin_step(|| Ok(false));
        assert!(ordinary.request(CancelReason::JobTimeout));
        assert!(ordinary_step.is_cancelled());
        assert!(ordinary.should_abort_work());

        let forced = JobCancellation::recording(None);
        let surviving_step = forced.begin_step(|| Ok(true));
        assert!(forced.request(CancelReason::ServerRequested));
        assert!(!surviving_step.is_cancelled());
        assert!(!forced.should_abort_work());
        forced.force();
        assert!(forced.should_abort_work());
    }

    #[test]
    fn nested_always_child_survives_rejected_composite_scope() {
        let token = JobCancellation::recording(None);
        let composite = token.begin_step(|| Ok(false));
        let _composite_process = token.register(TerminationTarget::ProcessGroup {
            pgid: 401,
            label: "composite-scope".into(),
        });
        let child = token.begin_step(|| Ok(true));
        let _child_process = token.register(TerminationTarget::ProcessGroup {
            pgid: 402,
            label: "always-child".into(),
        });

        assert!(token.request(CancelReason::ServerRequested));
        assert!(composite.is_cancelled());
        assert!(!child.is_cancelled());
        assert!(token.active_step_cancelled());
        assert!(
            !token.should_abort_work(),
            "the innermost always child controls the current operation"
        );
        assert_eq!(
            JobCancellation::requested_fan_out_scope(
                CancelReason::ServerRequested,
                Some(&child.step)
            ),
            None,
            "the rejected composite wrapper must not start a Requested fan-out"
        );
        let _late_child_process = token.register(TerminationTarget::ProcessGroup {
            pgid: 403,
            label: "late-always-child".into(),
        });
        assert!(token.outcomes().is_empty());

        token.force();
        wait_for_outcomes(&token, 3);
        assert_eq!(
            token
                .outcomes()
                .iter()
                .map(|outcome| outcome.target.clone())
                .collect::<Vec<_>>(),
            vec![
                "pgid:401".to_string(),
                "pgid:402".to_string(),
                "pgid:403".to_string()
            ]
        );
    }

    #[test]
    fn nested_false_child_fan_out_targets_only_its_own_scope() {
        let token = JobCancellation::recording(None);
        let composite = token.begin_step(|| Ok(false));
        let _composite_process = token.register(TerminationTarget::ProcessGroup {
            pgid: 411,
            label: "composite-scope".into(),
        });
        let child = token.begin_step(|| Ok(false));
        let _child_process = token.register(TerminationTarget::ProcessGroup {
            pgid: 412,
            label: "ordinary-child".into(),
        });

        assert!(token.request(CancelReason::JobTimeout));
        assert!(composite.is_cancelled());
        assert!(child.is_cancelled());
        assert!(token.should_abort_work());
        assert_eq!(
            JobCancellation::requested_fan_out_scope(CancelReason::JobTimeout, Some(&child.step)),
            Some(FanOutScope::Step(child.step.id))
        );
        wait_for_outcomes(&token, 1);
        assert_eq!(token.outcomes()[0].target, "pgid:412");

        let _late_child_process = token.register(TerminationTarget::ProcessGroup {
            pgid: 413,
            label: "late-ordinary-child".into(),
        });
        wait_for_outcomes(&token, 2);
        let requested_targets = token
            .outcomes()
            .iter()
            .map(|outcome| outcome.target.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            requested_targets,
            ["pgid:412".to_string(), "pgid:413".to_string()].into()
        );

        token.force();
        wait_for_outcomes(&token, 3);
        let all_targets = token
            .outcomes()
            .iter()
            .map(|outcome| outcome.target.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            all_targets,
            [
                "pgid:411".to_string(),
                "pgid:412".to_string(),
                "pgid:413".to_string()
            ]
            .into()
        );
    }

    #[test]
    fn remote_token_waits_for_supervisor_forced_pass() {
        let token = JobCancellation::remote();
        let always_step = token.begin_step(|| Ok(true));

        assert!(token.request(CancelReason::ServerRequested));
        assert_eq!(token.level(), CancelLevel::Requested);
        assert!(!always_step.is_cancelled());
        assert!(
            !token.0.deadline_watcher_started.load(Ordering::SeqCst),
            "remote guest must honor the host's server-configured cancellation grace"
        );

        token.force();
        assert_eq!(token.level(), CancelLevel::Forced);
    }

    #[test]
    fn cancellation_rechecks_active_conditions_and_preserves_eligible_work() {
        let token = JobCancellation::recording(None);
        let mut state = crate::executor::JobExecutionState::default();
        state.set_cancellation(token.clone());
        assert!(state.evaluate_condition(Some("always()")).unwrap());
        assert!(!state.evaluate_condition(Some("cancelled()")).unwrap());
        assert!(state.evaluate_condition(None).unwrap());

        let always_cancellation = token.clone();
        let always_step = token.begin_step(move || {
            let mut state = crate::executor::JobExecutionState::default();
            state.set_cancellation(always_cancellation.clone());
            state
                .evaluate_condition(Some("always()"))
                .map_err(|error| error.to_string())
        });
        let _always_process = token.register(TerminationTarget::ProcessGroup {
            pgid: 101,
            label: "always-step".into(),
        });

        assert!(token.request(CancelReason::ServerRequested));
        assert!(!always_step.is_cancelled());
        assert!(token.outcomes().is_empty());
        assert!(state.evaluate_condition(Some("always()")).unwrap());
        assert!(state.evaluate_condition(Some("cancelled()")).unwrap());
        assert!(!state.evaluate_condition(None).unwrap());

        // `cancelled()` work begins after job status changed, so Runner would
        // not attach a cancellation callback to it. It must still run.
        drop(always_step);
        let cancelled_cancellation = token.clone();
        let cancelled_step = token.begin_step(move || {
            let mut state = crate::executor::JobExecutionState::default();
            state.set_cancellation(cancelled_cancellation.clone());
            state
                .evaluate_condition(Some("cancelled()"))
                .map_err(|error| error.to_string())
        });
        let _cancelled_process = token.register(TerminationTarget::ProcessGroup {
            pgid: 102,
            label: "cancelled-step".into(),
        });
        assert!(!cancelled_step.is_cancelled());
        assert!(token.outcomes().is_empty());
        assert!(token.should_track_process_group());
        drop(cancelled_step);
        assert!(
            token.should_track_process_group(),
            "registering a post-step spawn closes the spawn/register race"
        );
    }

    #[test]
    fn request_observers_notify_remote_job_status_when_active_step_survives() {
        let token = JobCancellation::recording(None);
        let always_step = token.begin_step(|| Ok(true));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_by_callback = Arc::clone(&observed);
        let _observer = token.register_request_observer(move |reason| {
            observed_by_callback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(reason);
            Ok(())
        });

        assert!(token.request(CancelReason::ServerRequested));
        assert!(!always_step.is_cancelled());
        assert_eq!(
            *observed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![CancelReason::ServerRequested]
        );
        assert!(token.outcomes().is_empty());

        let late_observer_called = Arc::new(AtomicBool::new(false));
        let late_observer_flag = Arc::clone(&late_observer_called);
        let _late_observer = token.register_request_observer(move |reason| {
            assert_eq!(reason, CancelReason::ServerRequested);
            late_observer_flag.store(true, Ordering::SeqCst);
            Ok(())
        });
        assert!(late_observer_called.load(Ordering::SeqCst));
    }

    #[test]
    fn local_step_recheck_runs_before_remote_observers() {
        let token = JobCancellation::recording(None);
        let _ordinary_step = token.begin_step(|| Ok(false));
        let observed_token = token.clone();
        let _observer = token.register_request_observer(move |_| {
            assert!(
                observed_token.active_step_cancelled(),
                "remote notification must not block the host step recheck"
            );
            Ok(())
        });

        assert!(token.request(CancelReason::ServerRequested));
    }

    #[test]
    fn deferred_process_groups_survive_requested_and_die_at_forced() {
        let token = JobCancellation::recording(None);
        let _step = token.begin_step(|| Ok(false));
        let _ordinary = token.register(TerminationTarget::ProcessGroup {
            pgid: 301,
            label: "step-process".into(),
        });
        let _jailer = token.register(TerminationTarget::ProcessGroupAt {
            pgid: 302,
            label: "microvm-jailer".into(),
            terminate_at: CancelLevel::Forced,
        });

        assert!(token.request(CancelReason::ServerRequested));
        wait_for_outcomes(&token, 1);
        assert_eq!(
            token
                .outcomes()
                .iter()
                .map(|outcome| outcome.target.as_str())
                .collect::<Vec<_>>(),
            vec!["pgid:301"]
        );

        token.force();
        wait_for_outcomes(&token, 2);
        assert_eq!(
            token
                .outcomes()
                .iter()
                .map(|outcome| outcome.target.as_str())
                .collect::<Vec<_>>(),
            vec!["pgid:301", "pgid:302"]
        );
    }

    #[test]
    fn timeout_rechecks_ordinary_step_and_starts_with_interrupt() {
        let token = JobCancellation::recording(None);
        let mut state = crate::executor::JobExecutionState::default();
        state.set_cancellation(token.clone());
        assert!(state.evaluate_condition(None).unwrap());

        let step_cancellation = token.clone();
        let step = token.begin_step(move || {
            let mut state = crate::executor::JobExecutionState::default();
            state.set_cancellation(step_cancellation.clone());
            state
                .evaluate_condition(None)
                .map_err(|error| error.to_string())
        });
        let _ordinary_process = token.register(TerminationTarget::ProcessGroup {
            pgid: 202,
            label: "ordinary-step".into(),
        });

        assert!(token.request(CancelReason::JobTimeout));
        assert_eq!(token.reason(), Some(CancelReason::JobTimeout));
        assert!(step.is_cancelled());
        assert!(token.active_step_cancelled());
        assert!(token.should_track_process_group());
        assert!(!state.evaluate_condition(None).unwrap());
        assert!(state.evaluate_condition(Some("always()")).unwrap());
        assert!(state.evaluate_condition(Some("cancelled()")).unwrap());

        // The recording token proves the same termination ladder starts at
        // SIGINT without signalling a real process group in this unit test.
        token.fan_out_once();
        let outcomes = token.outcomes();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].target, "pgid:202");
        assert_eq!(outcomes[0].escalated_to, Some(TerminationSignal::Interrupt));
        assert!(outcomes[0].gone);
    }

    #[test]
    fn condition_recheck_error_is_retained_and_cancels_the_step() {
        let token = JobCancellation::recording(None);
        let step = token.begin_step(|| Err("invalid expression".to_string()));
        token.request(CancelReason::ServerRequested);

        assert!(step.is_cancelled());
        assert_eq!(
            step.condition_error().as_deref(),
            Some("invalid expression")
        );
    }

    #[test]
    fn daemon_shutdown_cancels_without_rechecking_an_always_step() {
        let token = JobCancellation::recording(None);
        let step = token.begin_step(|| Ok(true));
        let _process = token.register(TerminationTarget::ProcessGroup {
            pgid: 303,
            label: "shutdown-step".into(),
        });

        token.request(CancelReason::DaemonShutdown);

        assert!(step.is_cancelled());
        assert!(step.condition_error().is_none());
        token.fan_out_once();
        assert_eq!(token.outcomes().len(), 1);
        assert_eq!(token.outcomes()[0].target, "pgid:303");
    }

    #[test]
    fn unlinked_token_is_not_cancelled_by_its_parent() {
        let token = JobCancellation::recording(None);
        let post = token.unlinked();
        token.request(CancelReason::ServerRequested);
        assert!(token.is_cancelled());
        assert!(!post.is_cancelled());
    }

    #[test]
    fn registration_removes_its_target_on_drop() {
        let token = JobCancellation::recording(None);
        let registration = token.register(TerminationTarget::Container {
            name: "velnor-job-1".into(),
            role: ContainerRole::Job,
        });
        assert_eq!(token.target_keys(), vec!["container:velnor-job-1"]);
        drop(registration);
        assert!(token.target_keys().is_empty());
    }

    #[test]
    fn fan_out_reaches_every_registered_target_class() {
        let token = JobCancellation::recording(None);
        let _job = token.register(TerminationTarget::Container {
            name: "velnor-job-1".into(),
            role: ContainerRole::Job,
        });
        let _service = token.register(TerminationTarget::Container {
            name: "velnor-service-1-postgres".into(),
            role: ContainerRole::Service,
        });
        let _buildkit = token.register(TerminationTarget::Container {
            name: "velnor-buildkit-1".into(),
            role: ContainerRole::BuildKit,
        });
        let _group = token.register(TerminationTarget::ProcessGroup {
            pgid: 4242,
            label: "step".into(),
        });
        let hook_ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&hook_ran);
        let _hook = token.register(TerminationTarget::Hook {
            label: "vsock-cancel".into(),
            run: Arc::new(move |_| {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            }),
        });
        // The Requested pass represents a running step whose rechecked
        // condition is false; the job and service containers still survive.
        // Forcing reaches the rest. Asserting both phases is the contract, not
        // just that the fan-out visits everything.
        token.fan_out_once();
        let after_request: Vec<String> = token
            .outcomes()
            .into_iter()
            .map(|outcome| outcome.target)
            .collect();
        assert_eq!(
            after_request,
            vec![
                // Hooks first: a hook asks a target to stop itself, so it must
                // precede the terminations that take the decision away.
                "hook:vsock-cancel".to_string(),
                "pgid:4242".to_string(),
                "container:velnor-buildkit-1".to_string(),
            ],
            "a cancellation request must not destroy the job or service containers"
        );
        // `force` fans out on its own; calling it is the whole escalation.
        token.force();
        let reached: Vec<String> = token
            .outcomes()
            .into_iter()
            .map(|outcome| outcome.target)
            .collect();
        assert_eq!(
            reached,
            vec![
                "hook:vsock-cancel",
                "pgid:4242",
                "container:velnor-buildkit-1",
                "container:velnor-job-1",
                "container:velnor-service-1-postgres",
            ],
            "the failed step's work dies at Requested; the containers post steps need die on escalation, and every class is reached exactly once"
        );
        assert!(hook_ran.load(Ordering::SeqCst));
    }

    #[test]
    fn fan_out_order_is_independent_of_registration_order() {
        let token = JobCancellation::recording(None);
        let _service = token.register(TerminationTarget::Container {
            name: "aaa-service".into(),
            role: ContainerRole::Service,
        });
        let _job = token.register(TerminationTarget::Container {
            name: "zzz-job".into(),
            role: ContainerRole::Job,
        });
        let _buildkit = token.register(TerminationTarget::Container {
            name: "aaa-buildkit".into(),
            role: ContainerRole::BuildKit,
        });
        let _docker_action = token.register(TerminationTarget::Container {
            name: "zzz-action".into(),
            role: ContainerRole::DockerAction,
        });
        let _group = token.register(TerminationTarget::ProcessGroup {
            pgid: 42,
            label: "step".into(),
        });
        let _hook = token.register(TerminationTarget::Hook {
            label: "zzz-hook".into(),
            run: Arc::new(|_| Ok(())),
        });

        token.fan_out_once();

        assert_eq!(
            token
                .outcomes()
                .into_iter()
                .map(|outcome| outcome.target)
                .collect::<Vec<_>>(),
            vec![
                "hook:zzz-hook",
                "pgid:42",
                "container:zzz-action",
                "container:aaa-buildkit",
            ]
        );
        token.force();
        assert_eq!(
            token
                .outcomes()
                .into_iter()
                .map(|outcome| outcome.target)
                .collect::<Vec<_>>(),
            vec![
                "hook:zzz-hook",
                "pgid:42",
                "container:zzz-action",
                "container:aaa-buildkit",
                "container:zzz-job",
                "container:aaa-service",
            ]
        );
    }

    #[test]
    fn failed_target_remains_retryable_until_termination_succeeds() {
        let token = JobCancellation::recording(None);
        let attempts = Arc::new(AtomicU8::new(0));
        let attempts_for_hook = Arc::clone(&attempts);
        let _hook = token.register(TerminationTarget::Hook {
            label: "retryable".into(),
            run: Arc::new(move |_| {
                if attempts_for_hook.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err("transient hook failure".into())
                } else {
                    Ok(())
                }
            }),
        });

        token.fan_out_once();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(token.target_keys(), vec!["hook:retryable"]);
        assert!(!token.outcomes()[0].gone);

        token.fan_out_once();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(token.outcomes()[1].gone);

        token.fan_out_once();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(token.outcomes().len(), 2);
    }

    #[test]
    fn deadline_watcher_does_not_sleep_after_a_slow_initial_fan_out() {
        let request_started = Instant::now();
        let forced_deadline = deadline_after(request_started, Duration::from_secs(5));
        let initial_fan_out_finished = deadline_after(forced_deadline, Duration::from_secs(1));
        let slept = std::cell::Cell::new(false);

        wait_until_deadline(forced_deadline, &|| initial_fan_out_finished, &|_| {
            slept.set(true)
        });

        assert!(!slept.get());
    }

    #[test]
    fn ladder_escalates_interrupt_then_terminate_then_kill() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&sent);
        let mut outcome = TerminationOutcome {
            target: "pgid:1".into(),
            escalated_to: None,
            gone: false,
            error: None,
        };
        run_ladder(
            &mut outcome,
            TerminationLadder {
                sigint_grace: Duration::from_millis(1),
                sigterm_grace: Duration::from_millis(1),
            },
            Instant::now() + Duration::from_secs(5),
            &|_| {},
            &|| true,
            &move |signal| {
                record
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(signal);
                Ok(())
            },
        );
        assert_eq!(
            *sent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![
                TerminationSignal::Interrupt,
                TerminationSignal::Terminate,
                TerminationSignal::Kill,
            ]
        );
        assert_eq!(outcome.error.as_deref(), Some("target survived SIGKILL"));
    }

    #[test]
    fn ladder_stops_at_the_first_signal_that_works() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&sent);
        let alive = Arc::new(AtomicBool::new(true));
        let alive_probe = Arc::clone(&alive);
        let mut outcome = TerminationOutcome {
            target: "pgid:1".into(),
            escalated_to: None,
            gone: false,
            error: None,
        };
        run_ladder(
            &mut outcome,
            TerminationLadder {
                sigint_grace: Duration::from_millis(1),
                sigterm_grace: Duration::from_millis(1),
            },
            Instant::now() + Duration::from_secs(5),
            &|_| {},
            &move || alive_probe.load(Ordering::SeqCst),
            &move |signal| {
                record
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(signal);
                alive.store(false, Ordering::SeqCst);
                Ok(())
            },
        );
        assert_eq!(
            *sent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![TerminationSignal::Interrupt]
        );
        assert!(outcome.gone);
        assert_eq!(outcome.escalated_to, Some(TerminationSignal::Interrupt));
    }

    #[test]
    fn ladder_past_its_deadline_goes_straight_to_kill() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&sent);
        let mut outcome = TerminationOutcome {
            target: "container:velnor-job-1".into(),
            escalated_to: None,
            gone: false,
            error: None,
        };
        run_ladder(
            &mut outcome,
            TerminationLadder::default(),
            Instant::now() - Duration::from_secs(1),
            &|_| {},
            &|| true,
            &move |signal| {
                record
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(signal);
                Ok(())
            },
        );
        assert_eq!(
            *sent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![TerminationSignal::Kill]
        );
    }

    /// Registration order must not decide termination order: a cooperative
    /// hook always runs before process and container termination targets.
    #[test]
    fn a_hook_runs_before_terminations_registered_earlier() {
        let token = JobCancellation::recording(None);
        let _step = token.begin_step(|| Ok(false));
        let _jailer = token.register(TerminationTarget::ProcessGroup {
            pgid: 4242,
            label: "microvm-jailer".into(),
        });
        let _guest = token.register(TerminationTarget::Hook {
            label: "microvm-guest-cancel".into(),
            run: Arc::new(|_| Ok(())),
        });
        token.request(CancelReason::ServerRequested);
        token.fan_out_once();
        wait_for_outcomes(&token, 2);
        assert_eq!(
            token
                .outcomes()
                .into_iter()
                .map(|outcome| outcome.target)
                .collect::<Vec<_>>(),
            vec!["hook:microvm-guest-cancel", "pgid:4242"],
            "the guest is asked to stop before its VM is killed, though it registered second"
        );
    }

    /// A MicroVM cancellation first updates remote job status. The guest then
    /// re-evaluates its own active step; the jailer survives Requested so later
    /// `always()`/`cancelled()` work can finish.
    #[test]
    fn a_microvm_cancels_the_guest_before_it_kills_the_jailer() {
        let token = JobCancellation::recording(None);
        let _step = token.begin_step(|| Ok(false));
        let guest_cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&guest_cancelled);
        let _guest = token.register_request_observer(move |_| {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        });
        let _jailer = token.register(TerminationTarget::ProcessGroupAt {
            pgid: 4242,
            label: "microvm-jailer".into(),
            terminate_at: CancelLevel::Forced,
        });

        token.request(CancelReason::ServerRequested);
        assert!(
            guest_cancelled.load(Ordering::SeqCst),
            "remote job status is updated even when the host step's condition was rechecked"
        );
        assert!(token.outcomes().is_empty(), "jailer survives Requested");

        token.force();
        assert_eq!(token.outcomes()[0].target, "pgid:4242");
    }

    /// A MicroVM jailer owns the job runtime and survives Requested, like the
    /// job and service containers. Forced still reaches it as a process group.
    #[test]
    fn a_microvm_jailer_is_reachable_from_the_fan_out() {
        let token = JobCancellation::recording(None);
        let _step = token.begin_step(|| Ok(false));
        let jailer = TerminationTarget::ProcessGroupAt {
            pgid: 4242,
            label: "microvm-jailer".into(),
            terminate_at: CancelLevel::Forced,
        };
        assert_eq!(
            jailer.terminate_at(),
            CancelLevel::Forced,
            "the guest must outlive Requested for later eligible steps"
        );
        let registration = token.register(jailer);
        token.request(CancelReason::ServerRequested);
        assert!(token.outcomes().is_empty());
        token.force();
        token.fan_out_once();
        assert_eq!(
            token
                .outcomes()
                .into_iter()
                .map(|outcome| outcome.target)
                .collect::<Vec<_>>(),
            vec!["pgid:4242"]
        );
        // Dropping the session deregisters it, so a finished job leaves nothing
        // on the fan-out to kill.
        drop(registration);
        assert!(token.target_keys().is_empty());
    }

    #[test]
    fn a_target_registered_after_cancellation_is_terminated_immediately() {
        let token = JobCancellation::recording(None);
        let _step = token.begin_step(|| Ok(false));
        token.request(CancelReason::ServerRequested);
        assert!(token.outcomes().is_empty());

        // A target for the failed step that appears after the request lost a
        // race must not be leaked just because the fan-out already ran.
        let _late = token.register(TerminationTarget::ProcessGroup {
            pgid: 4242,
            label: "late-step".into(),
        });
        // `request` starts the detached ladder thread, which can claim the
        // late target before this thread's inline pass runs. Both passes
        // append to the same outcomes log, so wait for the entry rather than
        // asserting the inline pass won the race.
        wait_for_outcomes(&token, 1);
        assert_eq!(
            token
                .outcomes()
                .into_iter()
                .map(|outcome| outcome.target)
                .collect::<Vec<_>>(),
            vec!["pgid:4242"]
        );

        // A service container registered just as late still survives the
        // request, because post steps run against it, and dies on escalation.
        let _late_service = token.register(TerminationTarget::Container {
            name: "velnor-service-1-redis".into(),
            role: ContainerRole::Service,
        });
        assert_eq!(
            token
                .outcomes()
                .into_iter()
                .map(|outcome| outcome.target)
                .collect::<Vec<_>>(),
            vec!["pgid:4242"],
            "a service container must outlive a cancellation request"
        );
        token.force();
        // The detached pass can still be alive here and claim the service
        // container first; the escalation outcome lands in the same log.
        wait_for_outcomes(&token, 2);
        assert_eq!(
            token
                .outcomes()
                .into_iter()
                .map(|outcome| outcome.target)
                .collect::<Vec<_>>(),
            vec![
                "pgid:4242".to_string(),
                "container:velnor-service-1-redis".to_string()
            ],
            "escalation must reach the container the request spared, exactly once"
        );
    }

    #[test]
    fn active_token_is_restored_when_the_guard_drops() {
        // Serialized with the Engine routing tests: while this cancelled
        // token is installed, a concurrent facade API attempt would observe
        // the cancellation and fall back, flaking its scripted runner.
        let _serial = crate::docker::metrics::lock_serial_for_test();
        let token = JobCancellation::recording(None);
        assert!(active().is_none());
        {
            let _guard = set_active(token.clone());
            let installed = active().expect("active token");
            installed.request(CancelReason::JobTimeout);
            assert!(token.is_cancelled());
        }
        assert!(active().is_none());
    }

    /// `CancelReason::JobTimeout` had no producer: a job whose server
    /// cancellation never arrived ran forever. The wall-clock enforcer must
    /// request it when the collective job time exceeds the limit.
    #[test]
    fn job_timeout_enforcer_requests_job_timeout_when_the_wall_clock_elapses() {
        let token = JobCancellation::recording(None);
        let fired = Arc::new(AtomicBool::new(false));
        let _enforcer = arm_job_timeout(&token, Duration::from_millis(20), {
            let fired = Arc::clone(&fired);
            move || fired.store(true, Ordering::SeqCst)
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !token.is_cancelled() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(token.reason(), Some(CancelReason::JobTimeout));
        assert!(fired.load(Ordering::SeqCst));
    }

    /// A finished job disarms the enforcer by dropping it; the wall clock
    /// must never fire late into whatever runs next on the host.
    #[test]
    fn job_timeout_enforcer_disarms_on_drop() {
        let token = JobCancellation::recording(None);
        {
            let _enforcer = arm_job_timeout(&token, Duration::from_millis(50), || {});
        }
        std::thread::sleep(Duration::from_millis(300));
        assert!(!token.is_cancelled());
    }
}

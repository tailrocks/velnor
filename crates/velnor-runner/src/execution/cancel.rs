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
//! * [`JobCancellation`] — the token. Two levels, `Requested` then `Forced`,
//!   matching how `JobDispatcher` first cancels the worker and only then hard
//!   kills it (`src/Runner.Listener/JobDispatcher.cs:1280-1285`).
//! * [`TerminationTarget`] — everything a cancelled job can still be holding.
//!   Targets register themselves as they are created and deregister when they
//!   are gone, so the fan-out set is derived from what actually exists rather
//!   than from a hardcoded list of two container names.
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
    /// Cancellation requested. Steps stop being dispatched, the running step is
    /// walked down the termination ladder, and `always()`/`cancelled()` post
    /// work still runs.
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
    /// Stable identity used for deduplication and log lines.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::ProcessGroup { pgid, .. } => format!("pgid:{pgid}"),
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
fn docker_bounded(args: &[String]) -> Result<String, String> {
    let (_, deadline) = crate::docker::deadline_for(args, CONTAINER_FALLBACK_DEADLINE);
    crate::docker_lease::run_host_docker_bounded(args, deadline)
        .map_err(|error| format!("{error:#}"))
}

/// Only reachable if a future `docker` subcommand classifies as `Payload`,
/// which no ladder call does. Named anyway so no seam is unbounded.
const CONTAINER_FALLBACK_DEADLINE: Duration = Duration::from_secs(60);

fn container_alive(name: &str) -> bool {
    let args = vec![
        "inspect".to_string(),
        "--format".to_string(),
        "{{.State.Running}}".to_string(),
        name.to_string(),
    ];
    match docker_bounded(&args) {
        Ok(output) => output.trim() == "true",
        // "No such container" and any other inspect failure both mean there is
        // nothing left for the ladder to escalate against.
        Err(_) => false,
    }
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
        Err(detail)
            if detail.contains("No such container") || detail.contains("is not running") =>
        {
            Ok(())
        }
        Err(detail) => Err(detail),
    }
}

/// Poll interval while waiting for a target to notice a signal. Short enough
/// that a fast exit is observed promptly, long enough not to spin.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

fn wait_until_gone(
    alive: &dyn Fn() -> bool,
    grace: Duration,
    deadline: Instant,
    sleep: &dyn Fn(Duration),
) -> bool {
    let until = (Instant::now() + grace).min(deadline);
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
        TerminationTarget::ProcessGroup { pgid, .. } => {
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

#[derive(Default)]
struct Registry {
    targets: Vec<(u64, TerminationTarget)>,
    /// Ids already put through the ladder. Termination is idempotent per
    /// target: a fan-out triggered by a late registration must not replay the
    /// ladder against targets an earlier fan-out already terminated, or a
    /// cancelled job sends a second kill to a container that is already gone
    /// and records a duplicate outcome.
    terminated: std::collections::HashSet<u64>,
}

struct Inner {
    level: AtomicU8,
    reason: Mutex<Option<CancelReason>>,
    /// Delay from the first request to forced escalation, seeded from the
    /// server-supplied cancel timeout.
    forced_after: Duration,
    registry: Mutex<Registry>,
    next_id: AtomicU64,
    /// Set once so repeated cancellation never starts a second fan-out.
    fan_out_started: AtomicBool,
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

impl Drop for TargetRegistration {
    fn drop(&mut self) {
        self.token.deregister(self.id);
    }
}

impl JobCancellation {
    /// A token for a job that can be cancelled, with the forced-escalation
    /// delay derived from the server-supplied cancel timeout.
    #[must_use]
    pub fn new(cancel_timeout: Option<Duration>) -> Self {
        Self::with_forced_delay(forced_kill_delay(cancel_timeout), true)
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
        Self::with_forced_delay(forced_kill_delay(None), true)
    }

    /// A token whose ladder records what it would do without signalling
    /// anything. Registered hooks still run.
    #[must_use]
    pub fn recording(cancel_timeout: Option<Duration>) -> Self {
        Self::with_forced_delay(forced_kill_delay(cancel_timeout), false)
    }

    fn with_forced_delay(forced_after: Duration, live: bool) -> Self {
        Self(Arc::new(Inner {
            level: AtomicU8::new(CancelLevel::None.as_u8()),
            reason: Mutex::new(None),
            forced_after,
            registry: Mutex::new(Registry::default()),
            next_id: AtomicU64::new(0),
            fan_out_started: AtomicBool::new(false),
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
        Self::with_forced_delay(self.0.forced_after, self.0.live)
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
        self.0.forced_after
    }

    /// Request cancellation, running the termination fan-out once.
    ///
    /// Returns `true` when this call is the one that cancelled the job.
    /// Repeated cancellation is idempotent: the reason of the first request
    /// stands and no second fan-out starts, which is what keeps a cancel storm
    /// from turning into a signal storm.
    pub fn request(&self, reason: CancelReason) -> bool {
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
        }
        if self
            .0
            .fan_out_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.spawn_fan_out();
        }
        transitioned
    }

    /// Escalate to `Forced` without waiting for the grace period.
    pub fn force(&self) {
        self.0
            .level
            .store(CancelLevel::Forced.as_u8(), Ordering::SeqCst);
    }

    /// Register a target with the fan-out. The registration removes it on drop.
    #[must_use]
    pub fn register(&self, target: TerminationTarget) -> TargetRegistration {
        let id = self.0.next_id.fetch_add(1, Ordering::SeqCst);
        self.0
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .targets
            .push((id, target));
        // A target created after cancellation was requested is still a target:
        // terminate it now rather than leaking it because it lost a race.
        if self.is_cancelled() {
            self.fan_out_once();
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
        registry.targets.retain(|(existing, _)| *existing != id);
        // Ids are never reused (`next_id` only increments), so forgetting the
        // termination mark here keeps the set bounded by the live registry
        // rather than by the number of targets the job ever created.
        registry.terminated.remove(&id);
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
            .map(|(_, target)| target.key())
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

    fn spawn_fan_out(&self) {
        let token = self.clone();
        let forced_after = self.0.forced_after;
        let spawned = std::thread::Builder::new()
            .name("velnor-cancel-ladder".into())
            .spawn(move || {
                token.fan_out_once();
                // Everything that survived the ladder gets one forced pass at
                // the server-supplied deadline, mirroring the listener's
                // cancel-then-hard-kill pair
                // (`src/Runner.Listener/JobDispatcher.cs:1280-1285`).
                std::thread::sleep(forced_after);
                if token.is_cancelled() {
                    token.force();
                    token.fan_out_once();
                }
            });
        if let Err(error) = spawned {
            // A host that cannot spawn a thread still has to terminate the job:
            // run the fan-out inline rather than leaving it running.
            eprintln!("cancellation ladder thread could not be spawned ({error}); running inline");
            self.fan_out_once();
        }
    }

    /// Run the ladder over every registered target once.
    ///
    /// Public so the daemon-shutdown path can drive a synchronous fan-out and
    /// observe the outcome before it exits.
    pub fn fan_out_once(&self) {
        // Claim the unterminated targets under one lock, marking them before
        // the ladder runs, so a concurrent registration cannot select the same
        // target for a second fan-out.
        let targets: Vec<TerminationTarget> = {
            let mut registry = self
                .0
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let pending: Vec<(u64, TerminationTarget)> = registry
                .targets
                .iter()
                .filter(|(id, _)| !registry.terminated.contains(id))
                .map(|(id, target)| (*id, target.clone()))
                .collect();
            for (id, _) in &pending {
                registry.terminated.insert(*id);
            }
            pending.into_iter().map(|(_, target)| target).collect()
        };
        if targets.is_empty() {
            return;
        }
        let ladder = TerminationLadder::default();
        // Every target shares one bound, so a wedged Docker daemon cannot make
        // the fan-out itself unbounded.
        let deadline = Instant::now()
            + ladder
                .sigint_grace
                .saturating_add(ladder.sigterm_grace)
                .saturating_mul(u32::try_from(targets.len().max(1)).unwrap_or(u32::MAX))
                .saturating_add(Duration::from_secs(30));
        for target in &targets {
            let outcome = if self.0.live {
                terminate_with(target, deadline, ladder, &|duration| {
                    std::thread::sleep(duration);
                })
            } else {
                recorded_outcome(target)
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
            self.0
                .outcomes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(outcome);
        }
    }
}

fn recorded_outcome(target: &TerminationTarget) -> TerminationOutcome {
    let mut outcome = TerminationOutcome {
        target: target.key(),
        escalated_to: Some(TerminationSignal::Interrupt),
        gone: true,
        error: None,
    };
    if let TerminationTarget::Hook { run, .. } = target
        && let Err(detail) = run(CancelLevel::Requested)
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
mod tests {
    use super::*;

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
        token.fan_out_once();
        let reached: Vec<String> = token
            .outcomes()
            .into_iter()
            .map(|outcome| outcome.target)
            .collect();
        assert_eq!(
            reached,
            vec![
                "container:velnor-job-1",
                "container:velnor-service-1-postgres",
                "container:velnor-buildkit-1",
                "pgid:4242",
                "hook:vsock-cancel",
            ]
        );
        assert!(hook_ran.load(Ordering::SeqCst));
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

    #[test]
    fn a_target_registered_after_cancellation_is_terminated_immediately() {
        let token = JobCancellation::recording(None);
        token.request(CancelReason::ServerRequested);
        assert!(token.outcomes().is_empty());
        let _late = token.register(TerminationTarget::Container {
            name: "velnor-service-1-redis".into(),
            role: ContainerRole::Service,
        });
        assert_eq!(
            token
                .outcomes()
                .into_iter()
                .map(|outcome| outcome.target)
                .collect::<Vec<_>>(),
            vec!["container:velnor-service-1-redis"]
        );
    }

    #[test]
    fn active_token_is_restored_when_the_guard_drops() {
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
}

//! Per-scope controller: desired state, permits, slot/job child processes.
//!
//! Restarting this process must not stop existing slot or job workers: children
//! are spawned without kill-on-drop, and packaged units must not use
//! `PartOf=controller`. Every journal side effect is executed here.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Args;
use serde_json::json;
use velnor_control::journal::{Event, Journal, SideEffect, SlotRecord};
use velnor_control::lifecycle::LifecycleService;
use velnor_control::store::Store;
use velnor_model::{FleetHealthState, Generation, JobId, JobPhase2, SlotId, SlotPhase2};

use crate::config;
use crate::protocol::{GitHubScope, RegistrationClient};
use crate::runner::daemon_forensic_log;

use super::cleanup;
use super::complete;
use super::exec::load_exec_config;
use super::health::HealthServer;
use super::prove;
use super::slot::slot_id;
use super::watchdog::{feed_after_cycle, LocalCycle};

/// Bound live JIT requests during startup/recovery without making the GitHub
/// API a burst target. This matches the bounded configure path.
const JIT_REGISTRATION_CONCURRENCY: usize = 4;
/// Reconcile remote registration membership at most once per minute. The
/// reconciliation uses one paginated fleet listing per controller, not one
/// request per registered slot, so multi-slot fleets do not exhaust the
/// shared GitHub token budget.
const REGISTRATION_RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
/// Completion-marker and orphan-outbox scans are recovery work, not a steady
/// state tick. The first cycle runs them immediately; retries are bounded.
const OUTBOX_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(30);
/// Reserve half of the controller's 30s watchdog budget for local journal,
/// process, and health work. Every remote operation in one cycle shares this
/// deadline; one slow API cannot consume the budget of later operations.
const CONTROLLER_REMOTE_BUDGET: Duration = Duration::from_secs(15);
/// A canceled JIT POST may have been accepted by GitHub before curl died.
/// Reserve time to remove that deterministic-name orphan before the next
/// controller cycle retries registration.
const JIT_ORPHAN_CLEANUP_BUDGET: Duration = Duration::from_secs(8);
const FENCED_SLOT_TERMINATION_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a draining slot child may take to leave GitHub before SIGKILL.
///
/// An idle slot's graceful exit is bounded network work: the broker
/// long-poll is cancelled on the drain edge, then it deletes its broker
/// session (`BrokerClient::delete_session`, 30s request bound), its own JIT
/// registration and the prewarmed successor's (one REST call each,
/// `GITHUB_MAX_TIME_SECS` = 5s). Killing it sooner leaves those
/// registrations on GitHub — the 5s budget did exactly that on every
/// on-demand drain. Fleet slots are systemd units with their own
/// `TimeoutStopSec`; this bound applies to controller-owned children.
const CONTROLLER_CHILD_DRAIN_TIMEOUT: Duration = Duration::from_secs(30 + 5 + 5 + 5);
/// A slot must publish fresh, generation-bound progress within this startup
/// bound before it can prove session liveness or readiness.
const SLOT_HEARTBEAT_MAX_AGE: Duration = Duration::from_secs(10);
const SUPERVISION_METRICS_INTERVAL: Duration = Duration::from_secs(1);

struct SlotChild {
    generation: Generation,
    child: Child,
}

type SlotChildren = HashMap<String, SlotChild>;

trait ProcessHandle {
    fn process(&self) -> &Child;
    fn process_mut(&mut self) -> &mut Child;
}

impl ProcessHandle for Child {
    fn process(&self) -> &Child {
        self
    }

    fn process_mut(&mut self) -> &mut Child {
        self
    }
}

impl ProcessHandle for SlotChild {
    fn process(&self) -> &Child {
        &self.child
    }

    fn process_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

/// Steady-state floor between live GitHub probes. The reconcile loop ticks
/// every 2s, but several fleets share one PAT with a 5000 req/hr budget:
/// unbounded probing alone (~1800 req/hr/fleet) can exhaust it. One probe
/// per minute per fleet keeps observation cost bounded well under budget.
const GITHUB_PROBE_MIN_INTERVAL: Duration = Duration::from_secs(60);
/// Probe backoff ceiling after repeated unreachable results.
const GITHUB_PROBE_MAX_BACKOFF: Duration = Duration::from_secs(600);
/// Stop probing (and hold JIT retries) while the shared token has fewer
/// requests remaining than this, so registration/DELETE traffic never hits
/// the hard 403 wall.
const RATE_LIMIT_HEADROOM_REMAINING: u64 = 100;
/// Per-slot JIT registration retry backoff ceiling. The first retries are
/// deliberately short (5s doubling) — only sustained failures grow long.
const REGISTRATION_RETRY_MAX_BACKOFF: Duration = Duration::from_secs(600);
/// A fleet that cannot resolve its desired routing policy fails closed
/// forever. Restating that diagnosis once per probe interval keeps it loud in
/// the journal without one line per reconcile cycle.
const UNRESOLVED_POLICY_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Unix seconds of the last unresolved-desired-policy diagnosis.
static UNRESOLVED_POLICY_LOGGED_AT: AtomicU64 = AtomicU64::new(0);

/// Controller-local REST pacing for the shared PAT.
/// Read-only probes and JIT registration retries in this controller draw from the same budget:
/// when GitHub reports exhaustion (403/429 with rate-limit headers), every
/// paced call holds until the reset epoch, so a quota storm cannot feed on
/// its own retries. Health degrades visibly (`github_reachable: false`)
/// instead of silently burning budget.
#[derive(Debug)]
struct GithubPacing {
    next_probe: tokio::time::Instant,
    probe_failures: u32,
    /// Controller-wide hold on JIT registration while the shared PAT is exhausted.
    /// Independent of per-slot retry: a quota 403/429 must not let proven
    /// unregistered slots keep calling `jit_configure_one_slot`.
    rest_hold_until: Option<tokio::time::Instant>,
    /// slot_id -> (next allowed attempt, failure streak)
    registration_retry: HashMap<String, (tokio::time::Instant, u32)>,
}

impl Default for GithubPacing {
    fn default() -> Self {
        Self {
            next_probe: tokio::time::Instant::now(),
            probe_failures: 0,
            rest_hold_until: None,
            registration_retry: HashMap::new(),
        }
    }
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Duration until the epoch deadline, with deterministic per-scope jitter so
/// fleets sharing a token do not resume in lockstep.
fn until_epoch_with_jitter(reset_epoch: u64, salt: u64) -> Option<Duration> {
    let until = reset_epoch.saturating_sub(epoch_now());
    if until == 0 {
        return None;
    }
    let jitter = 1 + salt % 15;
    Some(Duration::from_secs(until + jitter))
}

impl GithubPacing {
    fn probe_due(&self, now: tokio::time::Instant) -> bool {
        now >= self.next_probe
    }

    fn rest_requests_allowed(&self, now: tokio::time::Instant) -> bool {
        self.rest_hold_until.is_none_or(|deadline| now >= deadline)
    }

    /// Record a probe outcome and schedule the next probe. Rate-limited
    /// probes hold this controller's fleet until the reported reset epoch.
    fn record_probe(
        &mut self,
        now: tokio::time::Instant,
        rate_limited: bool,
        remaining: Option<u64>,
        reset_epoch: Option<u64>,
    ) {
        let salt = std::process::id() as u64;
        if rate_limited {
            self.probe_failures += 1;
            self.hold_rest_until(now, reset_epoch);
            return;
        }
        if let (Some(remaining), Some(reset_epoch)) = (remaining, reset_epoch)
            && remaining < RATE_LIMIT_HEADROOM_REMAINING
        {
            // Nearly exhausted: keep the remaining budget for
            // DELETE traffic until the window resets. Do not spend it
            // on new JIT registrations.
            if until_epoch_with_jitter(reset_epoch, salt).is_some() {
                self.hold_rest_until(now, Some(reset_epoch));
                return;
            }
        }
        self.probe_failures = 0;
        self.rest_hold_until = None;
        self.next_probe = now + GITHUB_PROBE_MIN_INTERVAL;
    }

    /// Record an unreachable (but not rate-limited) probe: exponential
    /// backoff, capped, so a GitHub outage does not cost 30 probes/minute.
    fn record_probe_unreachable(&mut self, now: tokio::time::Instant) {
        let exp = 1u32
            .checked_shl(self.probe_failures.min(4))
            .unwrap_or(u32::MAX);
        let backoff = GITHUB_PROBE_MIN_INTERVAL
            .saturating_mul(exp)
            .min(GITHUB_PROBE_MAX_BACKOFF);
        self.probe_failures += 1;
        self.next_probe = now + backoff;
    }

    fn registration_due(&self, slot_id: &str, now: tokio::time::Instant) -> bool {
        if self.rest_hold_until.is_some_and(|deadline| now < deadline) {
            return false;
        }
        self.registration_retry
            .get(slot_id)
            .is_none_or(|(deadline, _)| now >= *deadline)
    }

    fn record_registration_success(&mut self, slot_id: &str) {
        self.registration_retry.remove(slot_id);
    }

    /// Controller-wide REST hold until the GitHub reset epoch. Same duration
    /// formula as a rate-limited probe: jittered remaining window, or the
    /// probe backoff ceiling when GitHub omitted the reset header (429).
    fn hold_rest_until(&mut self, now: tokio::time::Instant, reset_epoch: Option<u64>) {
        let salt = std::process::id() as u64;
        let hold = reset_epoch
            .and_then(|epoch| until_epoch_with_jitter(epoch, salt))
            .unwrap_or(GITHUB_PROBE_MAX_BACKOFF)
            .max(GITHUB_PROBE_MIN_INTERVAL);
        let deadline = now + hold;
        let deadline = self
            .rest_hold_until
            .map_or(deadline, |existing| existing.max(deadline));
        self.next_probe = self.next_probe.max(deadline);
        self.rest_hold_until = Some(deadline);
    }

    /// Failed JIT: per-slot backoff always. Quota 403/429 also parks every
    /// other unregistered slot until reset. Permission 403 with remaining > 0
    /// must not set `rest_hold_until`.
    fn record_registration_error(
        &mut self,
        slot_id: &str,
        now: tokio::time::Instant,
        error: &anyhow::Error,
    ) {
        if let Some(quota) = crate::protocol::github_api_quota_status(error) {
            self.hold_rest_until(now, quota.reset_epoch_or_retry_after(epoch_now()));
        }
        self.record_registration_failure(
            slot_id,
            now,
            crate::protocol::github_api_retry_delay(error),
        );
    }

    /// Failed JIT registration: back off this slot (5s doubling, capped), or
    /// until the GitHub-reported reset when headers carry a delay. Per-slot
    /// only — a Duration hint is not quota evidence (permission 403s also
    /// carry `x-ratelimit-reset`).
    fn record_registration_failure(
        &mut self,
        slot_id: &str,
        now: tokio::time::Instant,
        rate_limit_hint: Option<Duration>,
    ) {
        let streak = self
            .registration_retry
            .get(slot_id)
            .map_or(0, |(_, streak)| streak + 1);
        let backoff = Duration::from_secs(
            5u64.saturating_mul(1u64 << (streak.saturating_sub(1).min(7)))
                .min(REGISTRATION_RETRY_MAX_BACKOFF.as_secs()),
        );
        let hold = rate_limit_hint.map_or(backoff, |hint| hint.max(backoff));
        self.registration_retry
            .insert(slot_id.to_owned(), (now + hold, streak));
    }
}

#[derive(Debug, Clone, Args)]
pub struct ControllerArgs {
    #[arg(long)]
    pub state_dir: PathBuf,
    #[arg(long, default_value = "default")]
    pub scope: String,
    /// Operator-declared minimum ready capacity `M`.
    #[arg(long, default_value_t = 1)]
    pub desired_ready: u32,
    #[arg(long)]
    pub once: bool,
    /// Spawn slot OS processes (production and isolation tests).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub spawn_slots: bool,
    /// Durable lifecycle ledger for unified drain. Daemon-wired only (the
    /// controller CLI leaves this `None` and drains on latch plus journal).
    #[arg(skip)]
    pub lifecycle: Option<ControllerLifecycle>,
}

/// Durable lifecycle identity for one supervised controller.
///
/// `instance` is the lifecycle ledger slug (the hostname-derived daemon
/// slug), which differs from the controller `scope` (the slot-id prefix).
/// The daemon maps both explicitly at the `supervise_from_daemon` call site.
#[derive(Debug, Clone)]
pub struct ControllerLifecycle {
    pub store: Arc<Store>,
    pub instance: String,
}

/// Lifecycle ledger as the drain loop consumes it: a read-through service
/// plus the raw store handle for observed-projection writes.
struct ActiveLifecycle {
    service: LifecycleService,
    store: Arc<Store>,
    instance: String,
}

impl ActiveLifecycle {
    /// Bind the threaded lifecycle handle, or `None` when the configured
    /// slug is not a canonical instance identity (the loop then drains on
    /// latch plus journal only, and says so once).
    fn bind(lifecycle: &ControllerLifecycle) -> Option<Self> {
        match LifecycleService::with_store_for_instance(
            Arc::clone(&lifecycle.store),
            lifecycle.instance.clone(),
        ) {
            Ok(service) => Some(Self {
                service,
                store: Arc::clone(&lifecycle.store),
                instance: lifecycle.instance.clone(),
            }),
            Err(error) => {
                eprintln!(
                    "lifecycle drain unavailable: configured instance is not a canonical identity: {error}; draining on signal plus journal only"
                );
                None
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct MetricsSnapshot {
    slot_processes: usize,
    job_processes: usize,
    waiter_processes: usize,
    last_wall_duration_ms: u64,
    reconcile: ReconcileTelemetry,
}

#[derive(Clone, Copy, Debug, Default)]
struct ReconcileTelemetry {
    completed_cycles: u64,
    wall_duration_ms_total: u64,
    controller_cpu_user_us_at_boundary: u64,
    controller_cpu_system_us_at_boundary: u64,
}

impl Default for MetricsSnapshot {
    fn default() -> Self {
        Self {
            slot_processes: 0,
            job_processes: 0,
            waiter_processes: 0,
            last_wall_duration_ms: 1,
            reconcile: ReconcileTelemetry::default(),
        }
    }
}

struct MetricsPublisherState {
    snapshot: MetricsSnapshot,
    sequence: u64,
    stopped: bool,
}

/// Publish telemetry independently of the local control cycle. The controller
/// retains journal and child ownership. The shared state serializes snapshot
/// updates, sequence allocation, stopping, and the atomic-file writer.
struct MetricsPublisher {
    state: Arc<Mutex<MetricsPublisherState>>,
    stop: Arc<tokio::sync::Notify>,
    state_dir: PathBuf,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl MetricsPublisher {
    fn start(state_dir: &Path) -> Self {
        let state = Arc::new(Mutex::new(MetricsPublisherState {
            snapshot: MetricsSnapshot::default(),
            sequence: 0,
            stopped: false,
        }));
        let stop = Arc::new(tokio::sync::Notify::new());
        let task_state = Arc::clone(&state);
        let task_stop = Arc::clone(&stop);
        let task_state_dir = state_dir.to_owned();
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(SUPERVISION_METRICS_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = task_stop.notified() => break,
                    _ = interval.tick() => {
                        match publish_metrics_snapshot(&task_state_dir, &task_state, false) {
                            Ok(true) => {}
                            Ok(false) => break,
                            Err(error) => {
                                eprintln!("controller metrics publication failed: {error:#}");
                            }
                        }
                    }
                }
            }
        });
        Self {
            state,
            stop,
            state_dir: state_dir.to_owned(),
            task: Some(task),
        }
    }

    fn update(
        &self,
        slots: &SlotChildren,
        jobs: &HashMap<String, Child>,
        last_wall_duration_ms: u64,
    ) {
        let (job_processes, waiter_processes) = job_process_counts(jobs.keys());
        // A poisoned metrics lock must not crash the controller: the state is
        // plain data (counters, a sequence, a flag), so no broken invariant
        // can survive inside it and recovering the guard is always sound.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.snapshot.slot_processes = slots.len();
        state.snapshot.job_processes = job_processes;
        state.snapshot.waiter_processes = waiter_processes;
        state.snapshot.last_wall_duration_ms = last_wall_duration_ms.max(1);
    }

    fn reconcile_completed(
        &self,
        slots: &SlotChildren,
        jobs: &HashMap<String, Child>,
        last_wall_duration_ms: u64,
    ) {
        let (job_processes, waiter_processes) = job_process_counts(jobs.keys());
        let (controller_cpu_user_us, controller_cpu_system_us) = process_cpu_usage();
        // Metrics state is plain data, so recovering a poisoned guard is
        // sound just as it is in `update`.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.snapshot.slot_processes = slots.len();
        state.snapshot.job_processes = job_processes;
        state.snapshot.waiter_processes = waiter_processes;
        let last_wall_duration_ms = last_wall_duration_ms.max(1);
        state.snapshot.last_wall_duration_ms = last_wall_duration_ms;
        let reconcile = &mut state.snapshot.reconcile;
        reconcile.completed_cycles = reconcile.completed_cycles.saturating_add(1);
        reconcile.wall_duration_ms_total = reconcile
            .wall_duration_ms_total
            .saturating_add(last_wall_duration_ms);
        reconcile.controller_cpu_user_us_at_boundary = controller_cpu_user_us;
        reconcile.controller_cpu_system_us_at_boundary = controller_cpu_system_us;
    }

    async fn stop_and_publish(&mut self) -> anyhow::Result<()> {
        // See `update`: metrics state is plain data, so a poisoned lock
        // recovers its guard instead of crashing the controller shutdown.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .stopped = true;
        self.stop.notify_one();
        if let Some(task) = self.task.take() {
            task.await.context("metrics publisher task failed")?;
        }
        publish_metrics_snapshot(&self.state_dir, &self.state, true)?;
        Ok(())
    }
}

impl Drop for MetricsPublisher {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Allocate the next sequence and write the snapshot while holding the shared
/// state lock. This prevents a synchronous final publish from racing the
/// periodic task or regressing its sequence.
fn publish_metrics_snapshot(
    state_dir: &Path,
    state: &Arc<Mutex<MetricsPublisherState>>,
    allow_stopped: bool,
) -> anyhow::Result<bool> {
    // See `MetricsPublisher::update`: metrics state is plain data, so a
    // poisoned lock recovers its guard instead of crashing the publish loop.
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.stopped && !allow_stopped {
        return Ok(false);
    }
    state.sequence = state.sequence.saturating_add(1);
    let current = state.snapshot;
    publish_controller_metrics(
        state_dir,
        state.sequence,
        current.slot_processes,
        current.job_processes,
        current.waiter_processes,
        current.last_wall_duration_ms,
        current.reconcile,
    )?;
    Ok(true)
}

pub async fn run(args: ControllerArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.state_dir)?;
    cleanup::initialize_owned_directory(&args.state_dir)?;
    let mut journal = Journal::open(args.state_dir.join("journal.db"))?;
    let server = HealthServer::bind(&args.state_dir)?;
    journal.apply(Event::ControlLive)?;
    journal.apply(Event::JournalWritable)?;
    journal.apply(Event::DesiredCapacity {
        ready: args.desired_ready,
    })?;
    let mut slots = SlotChildren::new();
    let mut jobs: HashMap<String, Child> = HashMap::new();
    let mut heartbeats: HashMap<String, (u32, u64)> = HashMap::new();
    let mut startup_deadlines: HashMap<String, Instant> = HashMap::new();
    let mut last_registration_reconcile = Instant::now() - REGISTRATION_RECONCILE_INTERVAL;
    let mut last_outbox_reconcile = Instant::now() - OUTBOX_RECONCILIATION_INTERVAL;
    let mut pacing = GithubPacing::default();
    let mut ready_announced = false;
    let mut last_wall_duration_ms = 1;
    publish_controller_metrics(
        &args.state_dir,
        0,
        0,
        0,
        0,
        1,
        ReconcileTelemetry::default(),
    )?;
    let mut metrics = MetricsPublisher::start(&args.state_dir);
    let lifecycle = args.lifecycle.as_ref().and_then(ActiveLifecycle::bind);
    loop {
        // Unified drain: the signal latch, durable journal marker, or fresh
        // lifecycle `draining` desired state all converge on one child-drain
        // exit. The lifecycle ledger is now the production path, not a
        // separately enabled compatibility mode.
        if should_drain(crate::runner::draining(), &journal, lifecycle.as_ref()) {
            drain_edge(&mut journal, lifecycle.as_ref());
            drain_children(&journal, &mut slots, &mut jobs).await?;
            metrics.update(&slots, &jobs, last_wall_duration_ms);
            metrics.stop_and_publish().await?;
            return Ok(());
        }
        let cycle_started = Instant::now();
        let cycle = reconcile_once(
            &args,
            &mut journal,
            &server,
            &mut slots,
            &mut jobs,
            &mut heartbeats,
            &mut startup_deadlines,
            &mut last_registration_reconcile,
            &mut last_outbox_reconcile,
            &mut pacing,
            &metrics,
            lifecycle.as_ref(),
        )
        .await?;
        last_wall_duration_ms = cycle_started.elapsed().as_millis().max(1) as u64;
        metrics.reconcile_completed(&slots, &jobs, last_wall_duration_ms);
        let _ = feed_after_cycle(cycle, !ready_announced);
        ready_announced = true;
        if args.once {
            // Leave children running: a controller restart (or --once exit)
            // must not stop slot or job processes. Reap completed children
            // through the same ownership path used by the normal loop.
            reap(&mut slots);
            reap(&mut jobs);
            metrics.update(&slots, &jobs, last_wall_duration_ms);
            metrics.stop_and_publish().await?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Publish the local supervision proof atomically. This document is telemetry
/// only: a failed write must not change scheduling or child-process ownership.
fn publish_controller_metrics(
    state_dir: &std::path::Path,
    sequence: u64,
    slot_processes: usize,
    job_processes: usize,
    waiter_processes: usize,
    last_wall_duration_ms: u64,
    reconcile: ReconcileTelemetry,
) -> anyhow::Result<()> {
    let wal_bytes = std::fs::metadata(state_dir.join("journal.db-wal"))
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let (user_us, system_us) = process_cpu_usage();
    let zero_cpu_phase = || json!({ "user_us": 0_u64, "system_us": 0_u64 });
    let metrics = json!({
        "sequence": sequence,
        "slot_processes": slot_processes,
        "job_processes": job_processes,
        "waiter_processes": waiter_processes,
        "reconcile": {
            "last_wall_duration_ms": last_wall_duration_ms,
            "completed_cycles": reconcile.completed_cycles,
            "wall_duration_ms_total": reconcile.wall_duration_ms_total,
        },
        "journal": { "wal_bytes": wal_bytes },
        "cpu": {
            "controller": { "user_us": user_us, "system_us": system_us },
            "controller_at_reconcile_boundary": {
                "user_us": reconcile.controller_cpu_user_us_at_boundary,
                "system_us": reconcile.controller_cpu_system_us_at_boundary,
            },
            "phases": {
                "journal": zero_cpu_phase(),
                "filesystem": zero_cpu_phase(),
                "github": zero_cpu_phase(),
                "broker": zero_cpu_phase(),
                "child_supervision": zero_cpu_phase()
            }
        }
    });
    let temporary = state_dir.join(".controller-metrics.json.tmp");
    let destination = state_dir.join("controller-metrics.json");
    std::fs::write(&temporary, serde_json::to_vec(&metrics)?)?;
    std::fs::rename(temporary, destination)?;
    Ok(())
}

/// Return process CPU time for the explicit aggregate controller metric. The
/// phase buckets remain zero until phase-specific accounting exists.
#[cfg(unix)]
fn process_cpu_usage() -> (u64, u64) {
    let mut raw = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` initializes the provided rusage structure on a zero
    // return; the pointer is valid for the duration of the syscall.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, raw.as_mut_ptr()) };
    if result != 0 {
        return (0, 0);
    }
    // SAFETY: the successful syscall initialized every field we read.
    let usage = unsafe { raw.assume_init() };
    (
        timeval_microseconds(&usage.ru_utime),
        timeval_microseconds(&usage.ru_stime),
    )
}

#[cfg(unix)]
fn timeval_microseconds(value: &libc::timeval) -> u64 {
    u64::try_from(value.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000)
        .saturating_add(u64::try_from(value.tv_usec).unwrap_or(0))
}

#[cfg(not(unix))]
fn process_cpu_usage() -> (u64, u64) {
    (0, 0)
}

fn job_process_counts<'a>(job_ids: impl Iterator<Item = &'a String>) -> (usize, usize) {
    job_ids.fold((0, 0), |(jobs, waiters), job_id| {
        if job_id.starts_with("wait-") {
            (jobs, waiters + 1)
        } else {
            (jobs + 1, waiters)
        }
    })
}

/// Durable marker leg of the drain signal. Unreadable is not a drain order
/// here: a store or journal blip must not tear down supervision, while a
/// marker any reader can see still does.
fn journal_marker_latched(journal: &Journal) -> bool {
    journal
        .materialized_state()
        .map(|state| state.drain_active)
        .unwrap_or(false)
}

/// `should_drain` runs every loop cycle, so an instance whose ledger row
/// is missing (or unreadable for any other reason) would spam one
/// forensic line per cycle forever. The diagnosis logs once per process;
/// the decision itself (not a drain order) is recomputed every cycle.
static DRAIN_DESIRED_UNREADABLE_LOGGED: AtomicBool = AtomicBool::new(false);

fn note_drain_desired_unreadable(instance: &str, error: &impl std::fmt::Debug) {
    if DRAIN_DESIRED_UNREADABLE_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "forensics.lifecycle event=drain-desired-unreadable instance={instance} error={error:?}"
    );
}

/// Unified drain signal: the process signal latch, the durable journal
/// marker, or a store-read-through lifecycle desired state of `draining`.
/// Unreadable inputs are not drain orders: a store or journal blip must not
/// tear down supervision, while the latch and the latched marker still do.
fn should_drain(latch: bool, journal: &Journal, lifecycle: Option<&ActiveLifecycle>) -> bool {
    if latch {
        return true;
    }
    if journal_marker_latched(journal) {
        return true;
    }
    let Some(lifecycle) = lifecycle else {
        return false;
    };
    match lifecycle.service.desired_fresh(&lifecycle.instance) {
        Ok(fresh) => fresh.desired == "draining",
        Err(error) => {
            note_drain_desired_unreadable(&lifecycle.instance, &error);
            false
        }
    }
}

/// Run once on the drain edge before `drain_children`: latch the journal
/// marker when this controller is newly draining, and record the observed
/// `draining` projection exactly once. Both writes are best-effort around
/// the exit they precede: a failed write is forensic, never a reason to
/// keep supervising through a drain — and never a reason to skip the
/// `drain_children` exit that follows.
fn drain_edge(journal: &mut Journal, lifecycle: Option<&ActiveLifecycle>) {
    let journal_active = match journal.materialized_state() {
        Ok(state) => state.drain_active,
        Err(error) => {
            eprintln!("forensics.lifecycle event=drain-edge-unreadable error={error:?}");
            false
        }
    };
    let fresh = lifecycle.as_ref().and_then(|lifecycle| {
        lifecycle
            .service
            .desired_fresh(&lifecycle.instance)
            .map_err(|error| {
                note_drain_desired_unreadable(&lifecycle.instance, &error);
            })
            .ok()
    });
    if !journal_active {
        // The lifecycle version ties the marker to the drain request; a
        // latch-only drain with no ledger row still latches at version 1 so
        // slot readers observe one marker shape.
        let version = fresh.as_ref().map(|state| state.version).unwrap_or(1);
        match journal.set_drain(version) {
            Ok(changed) => eprintln!(
                "forensics.lifecycle event=drain-edge version={version} marker_changed={changed}"
            ),
            Err(error) => eprintln!(
                "forensics.lifecycle event=drain-edge-unlatched version={version} error={error:?}"
            ),
        }
    }
    if let (Some(lifecycle), Some(fresh)) = (lifecycle, fresh) {
        let observation_needed = if fresh.observed != "draining" {
            true
        } else {
            match lifecycle_observation_needed(lifecycle, &fresh) {
                Ok(needed) => needed,
                Err(error) => {
                    eprintln!(
                        "forensics.lifecycle event=drain-observation-pending-unreadable instance={} error={error}",
                        lifecycle.instance
                    );
                    false
                }
            }
        };
        if observation_needed
            && let Err(error) = lifecycle.store.record_lifecycle_observed(
                &lifecycle.instance,
                "draining",
                fresh.version,
            )
        {
            // A version conflict cannot surface here: it converges inside the
            // store call. Only IO and missing-row failures land here.
            eprintln!(
                "forensics.lifecycle event=drain-observed-unrecorded instance={} error={error}",
                lifecycle.instance
            );
        }
    }
}

fn lifecycle_observation_needed(
    lifecycle: &ActiveLifecycle,
    fresh: &velnor_control::lifecycle::LifecycleState,
) -> anyhow::Result<bool> {
    if fresh.observed != fresh.desired {
        return Ok(true);
    }
    lifecycle
        .store
        .lifecycle_operation_pending(&lifecycle.instance, &fresh.desired, fresh.version)
        .map_err(|error| anyhow::anyhow!("read pending lifecycle observation: {error}"))
}

/// Reconcile lifecycle desired state into the local admission boundary before
/// any permit or waiter decision. The durable store is the source of truth;
/// the journal marker is the crash-safe actuator consumed by slot processes.
/// Unknown desired states fail closed instead of being accepted as inert
/// metadata.
fn reconcile_lifecycle_admission(
    journal: &mut Journal,
    lifecycle: Option<&ActiveLifecycle>,
) -> anyhow::Result<()> {
    let Some(lifecycle) = lifecycle else {
        return Ok(());
    };
    // Decode the complete journal before touching either durable marker. A
    // malformed admission/drain value is a fail-closed state, not something
    // a later `ready` observation may silently delete.
    let state = journal
        .materialized_state()
        .map_err(|error| anyhow::anyhow!("read lifecycle admission state: {error}"))?;
    let fresh = lifecycle
        .service
        .desired_fresh(&lifecycle.instance)
        .map_err(|error| anyhow::anyhow!("read lifecycle desired state: {error}"))?;
    let observation_needed = lifecycle_observation_needed(lifecycle, &fresh)?;
    match fresh.desired.as_str() {
        "ready" => {
            journal
                .clear_admission_blocked_if(
                    state.admission_blocked.then_some(state.admission_version),
                )
                .map_err(|error| anyhow::anyhow!("clear lifecycle admission fence: {error}"))?;
            if observation_needed
                && let Err(error) = lifecycle.store.record_lifecycle_observed(
                    &lifecycle.instance,
                    "ready",
                    fresh.version,
                )
            {
                eprintln!(
                    "forensics.lifecycle event=ready-observed-unrecorded instance={} error={error}",
                    lifecycle.instance
                );
            }
        }
        "cordoned" => {
            journal
                .set_admission_blocked(fresh.version)
                .map_err(|error| anyhow::anyhow!("set lifecycle admission fence: {error}"))?;
            if observation_needed
                && let Err(error) = lifecycle.store.record_lifecycle_observed(
                    &lifecycle.instance,
                    "cordoned",
                    fresh.version,
                )
            {
                eprintln!(
                    "forensics.lifecycle event=cordon-observed-unrecorded instance={} error={error}",
                    lifecycle.instance
                );
            }
        }
        "draining" => {
            // `should_drain` normally catches this at the loop edge. This
            // second check closes the race where a Drain arrives after that
            // edge but before this cycle's permit reconciliation.
            if !state.drain_active {
                journal
                    .set_drain(fresh.version)
                    .map_err(|error| anyhow::anyhow!("latch lifecycle drain: {error}"))?;
            }
        }
        other => {
            anyhow::bail!(
                "unsupported durable lifecycle desired state {other:?}; admission remains fenced"
            );
        }
    }
    Ok(())
}

/// Stop controller-owned slots, waiters, and stale job workers when the daemon
/// receives SIGTERM. Active in-flight job workers remain alive for completion.
async fn drain_children(
    journal: &Journal,
    slots: &mut SlotChildren,
    jobs: &mut HashMap<String, Child>,
) -> anyhow::Result<()> {
    for child in slots.values() {
        request_child_shutdown(&child.child)?;
    }

    // Ready-slot broker waiters and real job workers share the `jobs` map.
    // Waiters have no durable job yet and must exit during a daemon drain;
    // active workers must survive so an upgrade cannot lose in-flight work.
    let active_jobs = journal.materialized_state()?.jobs;
    let active_job_ids: HashSet<String> = active_jobs
        .iter()
        .filter(|job| job.phase.occupies_slot())
        .map(|job| job.job_id.0.clone())
        .collect();
    let active_slot_ids: HashSet<String> = active_jobs
        .iter()
        .filter(|job| job.phase.occupies_slot())
        .map(|job| job.slot_id.0.clone())
        .collect();
    for (job_id, child) in jobs.iter() {
        if is_drainable_job(job_id, &active_job_ids, &active_slot_ids) {
            request_child_shutdown(child)?;
        }
    }
    let mut deadline = Instant::now() + CONTROLLER_CHILD_DRAIN_TIMEOUT;
    let mut escalated = false;
    loop {
        reap_draining(slots, "slot")?;
        reap_draining_jobs(jobs, &active_job_ids, &active_slot_ids)?;
        if slots.is_empty()
            && jobs
                .keys()
                .all(|job_id| !is_drainable_job(job_id, &active_job_ids, &active_slot_ids))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            if escalated {
                anyhow::bail!(
                    "controller child drain timed out after SIGKILL; {} handles retained",
                    slots.len()
                        + jobs
                            .keys()
                            .filter(|job_id| {
                                is_drainable_job(job_id, &active_job_ids, &active_slot_ids)
                            })
                            .count()
                );
            }
            kill_draining(slots, "slot")?;
            kill_draining_jobs(jobs, &active_job_ids, &active_slot_ids)?;
            eprintln!("controller child drain escalated to SIGKILL");
            escalated = true;
            deadline = Instant::now() + CONTROLLER_CHILD_DRAIN_TIMEOUT;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn is_drainable_job(
    job_id: &str,
    active_job_ids: &HashSet<String>,
    active_slot_ids: &HashSet<String>,
) -> bool {
    // A waiter process keeps its `wait-<slot>` key after it promotes itself to
    // a real job. Preserve that handle when its slot has durable in-flight
    // work; killing every `wait-*` key was the drain-time duplicate/lost-job
    // race.
    if let Some(slot_id) = job_id.strip_prefix("wait-") {
        return !active_slot_ids.contains(slot_id);
    }
    !active_job_ids.contains(job_id)
}

fn request_child_shutdown(child: &Child) -> anyhow::Result<()> {
    if child.id() == 0 {
        return Ok(());
    }

    #[cfg(unix)]
    {
        // SAFETY: the PID comes from the live Child handle owned by this
        // controller. SIGTERM lets the child exit through its normal signal
        // path; SIGKILL remains systemd's final timeout action.
        let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = child;
        anyhow::bail!("graceful controller-child shutdown requires a Unix target")
    }
}

#[allow(clippy::too_many_arguments)]
async fn reconcile_once(
    args: &ControllerArgs,
    journal: &mut Journal,
    server: &HealthServer,
    slots: &mut SlotChildren,
    jobs: &mut HashMap<String, Child>,
    heartbeats: &mut HashMap<String, (u32, u64)>,
    startup_deadlines: &mut HashMap<String, Instant>,
    last_registration_reconcile: &mut Instant,
    last_outbox_reconcile: &mut Instant,
    pacing: &mut GithubPacing,
    metrics: &MetricsPublisher,
    lifecycle: Option<&ActiveLifecycle>,
) -> anyhow::Result<LocalCycle> {
    let remote_deadline = tokio::time::Instant::now() + CONTROLLER_REMOTE_BUDGET;
    let total = args.desired_ready;
    let mut effects = Vec::new();
    reap(slots);
    reap(jobs);
    // Ingest a surviving slot's heartbeat before deciding whether its permit
    // needs repair. On controller restart the child handle is gone, so the
    // heartbeat is the only fresh local proof that prevents a double spawn.
    let mut verified_heartbeats =
        ingest_slot_heartbeats(args, journal, total as usize, slots, heartbeats)?;
    reconcile_lifecycle_admission(journal, lifecycle)?;
    let state = journal.materialized_state()?;
    // The loop top exits on drain, so this closes the race where another
    // process latches the marker after this cycle's check. No new permits
    // while draining; the reducer rejects them too.
    let permits_gated = state.drain_active || state.admission_blocked;
    for index in 1..=total {
        if permits_gated {
            continue;
        }
        let id = slot_id(&args.scope, index as usize);
        let slot = state.slots.iter().find(|slot| slot.slot_id == id);
        let generation = slot
            .map(|slot| slot.generation)
            .unwrap_or(Generation::INITIAL);
        let fenced = slot.is_some_and(|slot| slot.phase == SlotPhase2::Fenced);
        let admission_blocked = slot_has_admission_block(&state, &id, generation);
        let heartbeat_fresh = slot_heartbeat_is_fresh_from_cache(
            args,
            &id,
            generation,
            slots,
            &mut verified_heartbeats,
        );
        let acting = slot_is_acting(&args.state_dir, &state, jobs, &id, generation)?;
        // A leftover deadline from before/during a job must not fire the
        // instant the journal row is gone. The waiter is still the broker
        // actor; fencing the supervisor while that waiter lives splits
        // GitHub's view from the journal and then `child_owns_slot` blocks
        // the only generation-recovery path.
        if heartbeat_fresh || acting {
            startup_deadlines.remove(&id.0);
        } else if args.spawn_slots
            && !fenced
            && stale_slot_deadline_reached(
                args,
                slot,
                &id,
                startup_deadlines,
                slots,
                Instant::now(),
            )
        {
            fence_stale_slot_actor(args, journal, slots, jobs, &id, generation).await?;
            startup_deadlines.remove(&id.0);
            continue;
        }
        if fenced && args.spawn_slots {
            // Proof: `fenced` is `slot.is_some_and(...)`, so `slot` is `Some`
            // in this arm.
            #[allow(clippy::expect_used, reason = "fenced implies slot is Some")]
            terminate_fenced_slot_actor(args, slots, jobs, &state, &id, slot.expect("fenced slot"))
                .await?;
        }
        let fenced_generation =
            if let Some(fenced_slot) = slot.filter(|slot| slot.phase == SlotPhase2::Fenced) {
                let exec = load_exec_config(&args.state_dir)?;
                let slot_dir =
                    recovery_slot_config_dir(&args.state_dir, &exec, &state, &fenced_slot.slot_id)?;
                fenced_slot_recovery_generation(
                    Some(fenced_slot),
                    &args.state_dir,
                    &slot_dir,
                    &state,
                    jobs,
                )?
            } else {
                None
            };
        let generation = fenced_generation.unwrap_or(generation);
        let process_alive = heartbeat_fresh;
        if fenced && fenced_generation.is_none() {
            continue;
        }
        if fenced_generation.is_none()
            && (admission_blocked
                || child_owns_slot(&args.state_dir, &state, jobs, &id, generation)?
                || !permit_needs_reconciliation(slot, generation, args.spawn_slots, process_alive))
        {
            continue;
        }
        effects.extend(
            journal
                .apply(Event::PermitReserved {
                    slot_id: id,
                    generation,
                })?
                .commands,
        );
    }
    for command in effects {
        execute_effect(
            args,
            journal,
            slots,
            startup_deadlines,
            &mut *pacing,
            remote_deadline,
            command,
        )
        .await?;
    }

    metrics.update(slots, jobs, 1);

    observe_github_and_routing(args, journal, pacing, remote_deadline).await?;

    if last_registration_reconcile.elapsed() >= REGISTRATION_RECONCILE_INTERVAL
        && pacing.rest_requests_allowed(tokio::time::Instant::now())
    {
        *last_registration_reconcile = Instant::now();
        let reconciliation = run_bounded_remote_reconciliation(
            reconcile_remote_registrations(args, journal, jobs, pacing),
            remaining_remote_budget(remote_deadline),
        )
        .await;
        if let Err(error) = reconciliation {
            eprintln!("remote registration reconciliation failed closed: {error:#}");
            publish_fail_closed_health(args, journal, server)?;
            return Ok(LocalCycle::finished());
        }
    }

    let execution = crate::execution::load_execution_file(&args.state_dir, None)?;
    let executor = prove::observe_executor(&args.state_dir, execution.backend());
    let snapshot = journal.materialized_state()?;
    let snapshot_generations: HashMap<&str, Generation> = snapshot
        .slots
        .iter()
        .map(|slot| (slot.slot_id.0.as_str(), slot.generation))
        .collect();
    let now = tokio::time::Instant::now();
    let mut proof_events = Vec::new();
    for index in 1..=total {
        let id = slot_id(&args.scope, index as usize);
        let generation = snapshot_generations
            .get(id.0.as_str())
            .copied()
            .unwrap_or(Generation::INITIAL);
        if executor {
            proof_events.push(Event::ExecutorProven {
                slot_id: id.clone(),
                generation,
            });
        }
        if slot_heartbeat_is_fresh_from_cache(
            args,
            &id,
            generation,
            slots,
            &mut verified_heartbeats,
        ) {
            proof_events.push(Event::SessionLive {
                slot_id: id,
                generation,
            });
        }
    }
    let mut proof_effects = Vec::new();
    for outcome in journal.apply_observation_batch(proof_events)? {
        proof_effects.extend(outcome.commands);
    }

    let state = journal.materialized_state()?;
    let state_slots: HashMap<&str, &SlotRecord> = state
        .slots
        .iter()
        .map(|slot| (slot.slot_id.0.as_str(), slot))
        .collect();
    let mut registration_events = Vec::new();
    for index in 1..=total {
        let id = slot_id(&args.scope, index as usize);
        let Some(slot) = state_slots.get(id.0.as_str()).copied() else {
            continue;
        };
        let generation = snapshot_generations
            .get(id.0.as_str())
            .copied()
            .unwrap_or(Generation::INITIAL);
        if slot.ready_proof().is_ok() && !slot.registered && pacing.registration_due(&id.0, now) {
            registration_events.push(Event::RegistrationIntended {
                slot_id: id,
                generation,
            });
        }
    }
    // The journal rechecks each intent against current admission state while
    // holding its write transaction, so a concurrent state change still
    // fails closed.
    for outcome in journal.apply_observation_batch(registration_events)? {
        proof_effects.extend(outcome.commands);
    }
    let mut registrations = Vec::new();
    for command in proof_effects {
        match command {
            SideEffect::RegisterRunner {
                slot_id,
                generation,
            } => registrations.push((slot_id, generation)),
            command => {
                execute_effect(
                    args,
                    journal,
                    slots,
                    startup_deadlines,
                    &mut *pacing,
                    remote_deadline,
                    command,
                )
                .await?
            }
        }
    }
    let _newly_ready_slots =
        register_runners(args, journal, pacing, registrations, remote_deadline).await?;

    spawn_provisional_recovery_waiters(args, journal, jobs)?;
    spawn_ready_waiters(args, journal, jobs)?;
    reap(jobs);
    let outbox_reconcile_due = last_outbox_reconcile.elapsed() >= OUTBOX_RECONCILIATION_INTERVAL;
    reclaim_orphaned_jobs_with_children(
        args,
        journal,
        jobs,
        remote_deadline,
        outbox_reconcile_due,
        crate::docker::client::host_call,
    )
    .await?;
    if outbox_reconcile_due {
        reconcile_orphaned_outboxes(args, journal, jobs)?;
        *last_outbox_reconcile = Instant::now();
    }

    for row in journal.pending_outbox()? {
        // A row whose durable budget is spent reaches its bounded terminal
        // state here even if no replay was ever attempted, so an expired
        // deadline alone is enough to stop it blocking admission forever.
        if abandon_if_budget_spent(args, journal, &row)? {
            continue;
        }
        preserve_outbox(
            args,
            journal,
            &row.job_id,
            row.generation,
            &row.payload_sha256,
        )?;
    }

    reap(slots);
    reap(jobs);
    metrics.update(slots, jobs, 1);
    let mut health = journal.materialized_state()?.health();
    health.execution_backend = execution.backend();
    server.publish(&health)?;
    Ok(LocalCycle::finished())
}

/// Remote registration state is advisory. Keep a slow or wedged GitHub API
/// from preventing the controller from completing its local supervision cycle.
fn remaining_remote_budget(deadline: tokio::time::Instant) -> Duration {
    deadline.saturating_duration_since(tokio::time::Instant::now())
}

fn publish_fail_closed_health(
    args: &ControllerArgs,
    journal: &Journal,
    server: &HealthServer,
) -> anyhow::Result<()> {
    let mut health = journal.materialized_state()?.health();
    health.actual_ready_slots = 0;
    health.capacity_permits = 0;
    health.state = FleetHealthState::NotReady;
    server.publish(&health)?;
    std::fs::write(args.state_dir.join("advertised-capacity"), "0")?;
    Ok(())
}

async fn run_bounded_remote_reconciliation<F>(operation: F, timeout: Duration) -> anyhow::Result<()>
where
    F: Future<Output = anyhow::Result<()>>,
{
    match tokio::time::timeout(timeout, operation).await {
        Ok(result) => result,
        Err(_) => {
            eprintln!(
                "registration reconciliation timed out after {}s; keeping local state and retrying later",
                timeout.as_secs()
            );
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_effect(
    args: &ControllerArgs,
    journal: &mut Journal,
    slots: &mut SlotChildren,
    startup_deadlines: &mut HashMap<String, Instant>,
    pacing: &mut GithubPacing,
    remote_deadline: tokio::time::Instant,
    command: SideEffect,
) -> anyhow::Result<()> {
    match command {
        SideEffect::SpawnSlot {
            slot_id,
            generation,
        } => maybe_spawn_slot(
            args,
            journal,
            slots,
            startup_deadlines,
            &slot_id,
            generation,
        ),
        SideEffect::RegisterRunner {
            slot_id,
            generation,
        } => register_runner(args, journal, pacing, slot_id, generation, remote_deadline).await,
        SideEffect::AdvertiseCapacity { permits } => {
            std::fs::write(
                args.state_dir.join("advertised-capacity"),
                permits.to_string(),
            )?;
            Ok(())
        }
        SideEffect::SendCompletion {
            job_id,
            generation,
            payload_sha256,
        } => preserve_outbox(args, journal, &job_id, generation, &payload_sha256),
        SideEffect::Cleanup {
            isolation_id,
            generation,
        } => cleanup::remove_owned(&args.state_dir, &isolation_id, generation.0),
        SideEffect::DeleteOutbox { job_id, generation } => {
            cleanup::remove_outbox(&args.state_dir, &job_id.0, generation.0)
        }
        SideEffect::FenceSlot { .. } => Ok(()),
    }
}

async fn register_runner(
    args: &ControllerArgs,
    journal: &mut Journal,
    pacing: &mut GithubPacing,
    slot_id: SlotId,
    generation: Generation,
    remote_deadline: tokio::time::Instant,
) -> anyhow::Result<()> {
    register_runners(
        args,
        journal,
        pacing,
        vec![(slot_id, generation)],
        remote_deadline,
    )
    .await?;
    Ok(())
}

/// Configure independent, already-proven slots concurrently, then commit the
/// resulting journal events in slot order. Network work never mutates the
/// journal; routing, executor, session, and permit proofs remain prerequisites
/// for every request.
async fn register_runners(
    args: &ControllerArgs,
    journal: &mut Journal,
    pacing: &mut GithubPacing,
    registrations: Vec<(SlotId, Generation)>,
    remote_deadline: tokio::time::Instant,
) -> anyhow::Result<HashSet<(SlotId, Generation)>> {
    if registrations.is_empty() {
        return Ok(HashSet::new());
    }
    super::scheduler::production_scheduler().ensure_current()?;
    let exec = match load_exec_config(&args.state_dir) {
        Ok(exec) => exec,
        Err(error) => {
            eprintln!("JIT registration skipped: cannot load daemon execution config: {error:#}");
            return Ok(HashSet::new());
        }
    };

    let already_ready = journal
        .materialized_state()?
        .slots
        .into_iter()
        .filter(|slot| slot.phase == SlotPhase2::Ready)
        .map(|slot| (slot.slot_id, slot.generation))
        .collect::<HashSet<_>>();
    let mut newly_ready = HashSet::new();

    let config_base = exec
        .config_dir
        .clone()
        .unwrap_or_else(|| args.state_dir.clone());
    let slot_count = exec.slots;
    use futures_util::stream::{self, StreamExt as _};
    let concurrency = registrations.len().clamp(1, JIT_REGISTRATION_CONCURRENCY);
    let mut outcomes = stream::iter(registrations)
        .map(|(slot_id, generation)| {
            let exec = exec.clone();
            let config_base = config_base.clone();
            async move {
                let index = slot_index_from_id(&slot_id);
                let timeout = remaining_remote_budget(remote_deadline);
                let jit_timeout = timeout.saturating_sub(JIT_ORPHAN_CLEANUP_BUDGET);
                let result = if jit_timeout.is_zero() {
                    Err(anyhow::anyhow!(
                        "JIT registration skipped: controller remote budget exhausted"
                    ))
                } else {
                    match tokio::time::timeout(
                        jit_timeout,
                        crate::runner::jit_configure_one_slot(
                            &exec,
                            &config_base,
                            index,
                            slot_count,
                        ),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => {
                            let cleanup_timeout = remaining_remote_budget(remote_deadline);
                            if !cleanup_timeout.is_zero() {
                                match tokio::time::timeout(
                                    cleanup_timeout,
                                    crate::runner::cleanup_orphaned_jit_one_slot(
                                        &exec,
                                        &config_base,
                                        index,
                                        slot_count,
                                    ),
                                )
                                .await
                                {
                                    Ok(Ok(())) => {}
                                    Ok(Err(error)) => eprintln!(
                                        "JIT timeout orphan cleanup for slot-{index} failed: {error:#}"
                                    ),
                                    Err(_) => eprintln!(
                                        "JIT timeout orphan cleanup for slot-{index} timed out"
                                    ),
                                }
                            }
                            Err(anyhow::anyhow!(
                                "JIT registration timed out before controller remote budget expired"
                            ))
                        }
                    }
                };
                (slot_id, generation, result)
            }
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
    outcomes.sort_by_key(|(slot_id, _, _)| slot_id.0.clone());

    for (slot_id, generation, result) in outcomes {
        if let Err(error) = result {
            // Per-slot backoff always. Quota 403/429 also sets rest_hold_until
            // so other unregistered slots do not keep calling generate-jitconfig
            // against an exhausted PAT. Permission 403 with remaining>0 does not.
            let now = tokio::time::Instant::now();
            pacing.record_registration_error(&slot_id.0, now, &error);
            eprintln!(
                "Warning: JIT register {} failed (slot stays unregistered; backing off {}s): {error:#}",
                slot_id.0,
                pacing
                    .registration_retry
                    .get(&slot_id.0)
                    .map_or(0, |(deadline, _)| deadline.duration_since(now).as_secs())
            );
            continue;
        }
        pacing.record_registration_success(&slot_id.0);
        let registered = journal.apply(Event::Registered {
            slot_id: slot_id.clone(),
            generation,
        })?;
        if registered.rejected {
            continue;
        }
        let ready = journal.apply(Event::ReadyAttempt {
            slot_id: slot_id.clone(),
            generation,
        })?;
        if !ready.rejected {
            for nested in ready.commands {
                if let SideEffect::AdvertiseCapacity { permits } = nested {
                    std::fs::write(
                        args.state_dir.join("advertised-capacity"),
                        permits.to_string(),
                    )?;
                }
            }
            if !already_ready.contains(&(slot_id.clone(), generation))
                && journal.materialized_state()?.slots.iter().any(|slot| {
                    slot.slot_id == slot_id
                        && slot.generation == generation
                        && slot.phase == SlotPhase2::Ready
                })
            {
                newly_ready.insert((slot_id, generation));
            }
        }
    }
    Ok(newly_ready)
}

/// Reconcile the durable local registration claim against GitHub. A JIT
/// runner can disappear remotely while its local runner.json and journal stay
/// intact (manual cleanup, expiry, or a crashed registration flow). Trusting
/// only the local `registered` bit then permanently suppresses fresh JIT
/// configuration and leaves every slot dead after restart.
async fn reconcile_remote_registrations(
    args: &ControllerArgs,
    journal: &mut Journal,
    jobs: &mut HashMap<String, Child>,
    pacing: &mut GithubPacing,
) -> anyhow::Result<()> {
    // Reconciliation is the proof that permits and remote registration still
    // agree. Without executable config or a PAT that proof cannot be made for
    // a fleet that still holds registrations: fail closed so reconcile_once
    // publishes zero capacity and NotReady instead of silently preserving an
    // unverified registration. A fleet with no registered slots has no remote
    // state to verify, so reconciliation is vacuous and the local cycle —
    // session/executor proofs, orphan recovery, and JIT registration attempts
    // — must still proceed.
    let any_registered = journal
        .materialized_state()?
        .slots
        .iter()
        .any(|slot| slot.registered);
    let exec = match load_exec_config(&args.state_dir) {
        Ok(exec) => exec,
        Err(error) => {
            if any_registered {
                return Err(error)
                    .context("registration reconciliation requires daemon execution config");
            }
            eprintln!(
                "registration reconciliation skipped: cannot load daemon execution config: {error:#}"
            );
            return Ok(());
        }
    };
    let url = exec.url.as_deref().filter(|url| !url.trim().is_empty());
    let pat = exec.pat.as_deref().filter(|pat| !pat.trim().is_empty());
    let (Some(url), Some(pat)) = (url, pat) else {
        if any_registered {
            anyhow::bail!(
                "registration reconciliation requires GitHub URL and PAT while slots remain registered"
            );
        }
        eprintln!("registration reconciliation skipped: GitHub URL or PAT unavailable");
        return Ok(());
    };
    let scope = GitHubScope::parse(url)?;
    let client = RegistrationClient::new()?;
    let state = journal.materialized_state()?;
    let config_base = exec
        .config_dir
        .clone()
        .unwrap_or_else(|| args.state_dir.clone());
    let slot_count = exec.slots;
    let mut lost = Vec::new();
    let mut registered = Vec::new();
    for slot in state.slots.iter().filter(|slot| slot.registered) {
        let index = slot_index_from_id(&slot.slot_id);
        let slot_dir = crate::runner::daemon_slot_config_dir(&config_base, index, slot_count);
        let local = match load_local_runner_config(&slot_dir) {
            Ok(local) => local,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "cannot load local runner config for registered slot {} at {}",
                        slot.slot_id.0,
                        slot_dir.display()
                    )
                });
            }
        };
        let local_id = local.as_ref().and_then(|stored| stored.settings.agent_id);
        let Some(local_id) = local_id else {
            // A registered slot without its durable numeric identity cannot
            // be reconciled safely. Name matching can select another live
            // runner, so release the stale local claim and let JIT rebuild it.
            lost.push((slot.slot_id.clone(), slot.generation));
            continue;
        };
        registered.push((slot.slot_id.clone(), slot.generation, local_id));
    }

    if !registered.is_empty() {
        let remote_ids = match client.list_runners(&scope, pat).await {
            Ok(runners) => remote_runner_ids(runners.into_iter().map(|runner| runner.id)),
            Err(error) => {
                if let Some(quota) = crate::protocol::github_api_quota_status(&error) {
                    pacing.hold_rest_until(
                        tokio::time::Instant::now(),
                        quota.reset_epoch_or_retry_after(epoch_now()),
                    );
                }
                eprintln!("registration reconciliation listing failed: {error:#}");
                None
            }
        };
        if let Some(remote_ids) = remote_ids {
            for (slot_id, generation, local_id) in registered {
                if !remote_ids.contains(&local_id) {
                    // Registration loss invalidates the broker worker even while it
                    // owns a job. The durable event releases admission first; the
                    // worker remains journal-owned until normal teardown reconciles it.
                    lost.push((slot_id, generation));
                }
            }
        } else {
            eprintln!(
                "registration reconciliation skipped: runner listing did not provide numeric identities"
            );
        }
    }

    for (slot_id, generation) in lost {
        let state = journal.materialized_state()?;
        let outcome = journal.apply(Event::RegistrationLost {
            slot_id: slot_id.clone(),
            generation,
        })?;
        if outcome.rejected {
            continue;
        }
        for key in job_child_keys_for_slot(jobs, &state, &slot_id) {
            if let Some(child) = jobs.get(&key) {
                request_child_shutdown(child)?;
            }
        }
        eprintln!(
            "registration lost for {}; local claim cleared for fresh JIT recovery",
            slot_id.0
        );
    }
    Ok(())
}

/// Return a complete remote identity set, or no evidence at all when the
/// listing contains an entry without an id. Partial identity evidence must
/// never make reconciliation delete a valid local registration.
fn remote_runner_ids(runners: impl IntoIterator<Item = Option<i64>>) -> Option<HashSet<i64>> {
    runners.into_iter().collect()
}

fn load_local_runner_config(slot_dir: &Path) -> anyhow::Result<Option<config::StoredRunnerConfig>> {
    let runner_config = slot_dir.join("runner.json");
    match std::fs::symlink_metadata(&runner_config) {
        Ok(_) => config::load(slot_dir).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if has_dangling_symlink_component(&runner_config)? {
                anyhow::bail!("runner config path contains a dangling symlink")
            }
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn has_dangling_symlink_component(path: &Path) -> anyhow::Result<bool> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        let link = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata.file_type().is_symlink(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if link
            && matches!(
                std::fs::metadata(&current),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            )
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn job_child_keys_for_slot(
    jobs: &HashMap<String, Child>,
    state: &velnor_control::journal::FleetState,
    slot_id: &SlotId,
) -> Vec<String> {
    let mut keys = state
        .jobs
        .iter()
        .filter(|job| job.slot_id == *slot_id)
        .filter(|job| jobs.contains_key(&job.job_id.0))
        .map(|job| job.job_id.0.clone())
        .collect::<Vec<_>>();
    let waiter_key = format!("wait-{}", slot_id.0);
    if jobs.contains_key(&waiter_key) {
        keys.push(waiter_key);
    }
    keys
}

async fn observe_github_and_routing(
    args: &ControllerArgs,
    journal: &mut Journal,
    pacing: &mut GithubPacing,
    remote_deadline: tokio::time::Instant,
) -> anyhow::Result<()> {
    let mut reachable = false;
    let mut dependency_observed = false;
    if let Ok(exec) = load_exec_config(&args.state_dir) {
        let group = exec
            .pool_name
            .clone()
            .or_else(|| exec.name.clone())
            .unwrap_or_default();
        let trust = prove::runtime_trust_scope(&exec.trust_scope);
        // Repo-scoped fleets derive policy from URL + labels + trust (the
        // immutable daemon config). Refresh the on-disk snapshot when that
        // config changes; otherwise a first-boot `untrusted` file freezes
        // `routing_valid=false` after the operator raises VELNOR_TRUST_SCOPE.
        // Explicit `--routing-policy-file` is the operator override. Org
        // fleets load the generated `<org>-desired-policy.json` allowlist
        // every cycle and replace `routing-policy.json`. Never snapshot live
        // group membership: a truncated GitHub group would become the desired
        // baseline and hide drift.
        let repo_policy = exec.url.as_deref().and_then(|url| {
            prove::policy_from_github_url(url, group.clone(), exec.labels.clone(), trust.clone())
        });
        let policy = if let Some(path) = exec.routing_policy_file.as_deref() {
            let policy = prove::read_policy_file(path)?;
            prove::write_policy(&args.state_dir, &policy)?;
            Some(policy)
        } else if let Some(policy) = repo_policy {
            prove::write_policy_if_changed(&args.state_dir, &policy)?;
            Some(policy)
        } else if let Some(url) = exec.url.as_deref() {
            if let Ok(scope) = crate::protocol::GitHubScope::parse(url) {
                if let Some(org) = scope.org_login() {
                    let generated =
                        prove::org_desired_policy(org, exec.labels.clone(), trust.clone());
                    if let Some(policy) = &generated {
                        prove::write_policy(&args.state_dir, policy)?;
                    }
                    generated
                } else {
                    prove::read_policy(&args.state_dir)
                }
            } else {
                prove::read_policy(&args.state_dir)
            }
        } else {
            prove::read_policy(&args.state_dir)
        };
        if policy.is_none() {
            note_unresolved_desired_policy(args, exec.url.as_deref());
        }
        if let (Some(url), Some(token)) = (exec.url.as_deref(), exec.pat.as_deref()) {
            let now = tokio::time::Instant::now();
            if pacing.probe_due(now) {
                let probe = match tokio::time::timeout(
                    remaining_remote_budget(remote_deadline),
                    prove::probe_github(prove::GitHubProbeRequest {
                        url,
                        token,
                        policy: policy.as_ref(),
                        configured_group: (!group.is_empty()).then_some(group.as_str()),
                        pool_id: exec.pool_id,
                        configured_labels: &exec.labels,
                        configured_trust: &exec.trust_scope,
                    }),
                )
                .await
                {
                    Ok(probe) => probe,
                    Err(_) => {
                        eprintln!("GitHub probe timed out before controller remote budget expired");
                        prove::GitHubProbe {
                            diagnostic: Some(
                                "GitHub routing probe timed out before controller remote budget expired"
                                    .to_owned(),
                            ),
                            ..prove::GitHubProbe::default()
                        }
                    }
                };
                if probe.rate_limited {
                    let reset = probe
                        .rate_limit_reset_epoch
                        .map_or_else(|| "unknown".to_owned(), |epoch| epoch.to_string());
                    eprintln!(
                        "Warning: GitHub rate limit hit on probe (remaining={:?}, reset epoch {reset}); \
                         holding fleet REST traffic until the window resets",
                        probe.rate_limit_remaining
                    );
                    pacing.record_probe(
                        now,
                        true,
                        probe.rate_limit_remaining,
                        probe.rate_limit_reset_epoch,
                    );
                } else if probe.reachable {
                    pacing.record_probe(
                        now,
                        false,
                        probe.rate_limit_remaining,
                        probe.rate_limit_reset_epoch,
                    );
                } else {
                    pacing.record_probe_unreachable(now);
                }
                reachable = probe.reachable;
                dependency_observed = true;
                match (probe.diagnostic.as_deref(), probe.evidence) {
                    (Some(diagnostic), _) => {
                        eprintln!("GitHub routing probe failed closed: {diagnostic}");
                        prove::invalidate_routing_evidence(&args.state_dir)?;
                    }
                    (None, Some(evidence)) => {
                        prove::write_evidence(&args.state_dir, &evidence)?;
                    }
                    (None, None) => {}
                }
            }
            // Probe skipped for pacing: keep the last observed dependency
            // state rather than stamping a false observation.
        }
    }
    if dependency_observed || !probe_configured(args) {
        journal.apply(Event::Dependency {
            github_reachable: reachable,
        })?;
    }
    let _ = prove::reconcile_from_dir(&args.state_dir)?;
    let routing = prove::observe_routing(&args.state_dir);
    journal.apply(Event::Routing {
        valid: routing.valid,
        group_valid: routing.group_valid,
    })?;
    Ok(())
}

/// Fail-closed must never mean silent. A fleet that cannot name its desired
/// routing policy registers no runner, so the only symptoms are idle slot
/// processes, queued GitHub jobs, and a degraded health file with no
/// explanation. Repeat the diagnosis at the probe cadence, never per cycle.
fn note_unresolved_desired_policy(args: &ControllerArgs, url: Option<&str>) {
    let now = epoch_now();
    if !unresolved_policy_note_due(UNRESOLVED_POLICY_LOGGED_AT.load(Ordering::Relaxed), now) {
        return;
    }
    UNRESOLVED_POLICY_LOGGED_AT.store(now, Ordering::Relaxed);
    let note = unresolved_desired_policy_note(url);
    eprintln!("{note}");
    crate::sd_notify::status(&note);
    daemon_forensic_log(&args.state_dir, &note);
}

/// `last == 0` is "never said it out loud yet": the first observation always
/// speaks, later ones wait out the interval.
fn unresolved_policy_note_due(last: u64, now: u64) -> bool {
    last == 0 || now.saturating_sub(last) >= UNRESOLVED_POLICY_LOG_INTERVAL.as_secs()
}

/// The operator-facing diagnosis: which policy source is missing and which
/// existing knob fixes it.
fn unresolved_desired_policy_note(url: Option<&str>) -> String {
    let detail = match url.and_then(|url| GitHubScope::parse(url).ok()) {
        Some(scope) => match scope.org_login().map(str::to_owned) {
            Some(org) => {
                let searched = prove::org_desired_policy_search_dirs()
                    .iter()
                    .map(|dir| dir.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" or ");
                format!("no generated allowlist {org}-desired-policy.json in {searched}")
            }
            None => format!(
                "no complete routing-policy.json on disk and repo scope {} does not \
                 yield one (group, labels, and trust_scope must all be set)",
                scope.original_url
            ),
        },
        None => "daemon URL is not a parseable GitHub scope".to_owned(),
    };
    format!(
        "GitHub routing fail-closed: desired routing policy unresolved ({detail}); \
         no slot can register and every job for this fleet queues. Install the \
         generated allowlist or point --routing-policy-file at an explicit policy."
    )
}

/// False when no live probe can run (no exec config, URL, or token): the old
/// behavior of stamping `github_reachable: false` every cycle is preserved.
fn probe_configured(args: &ControllerArgs) -> bool {
    let Ok(exec) = load_exec_config(&args.state_dir) else {
        return false;
    };
    exec.url.is_some() && exec.pat.is_some()
}

/// GitHub session waiters for Ready slots. Do not intend an acquisition:
/// REST queued ids are not broker job ids, and Ready must stay Ready until
/// the run-service acquire path owns the broker GUID.
fn spawn_ready_waiters(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
) -> anyhow::Result<()> {
    spawn_ready_waiters_with(args, journal, jobs, maybe_spawn_job)
}

fn spawn_ready_waiters_with(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
    mut spawn: impl FnMut(
        &ControllerArgs,
        &Journal,
        &mut HashMap<String, Child>,
        &str,
        u64,
        Option<&SlotId>,
        bool,
    ) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let Ok(exec) = load_exec_config(&args.state_dir) else {
        return Ok(());
    };
    let state = journal.materialized_state()?;
    if state.admission_blocked || state.drain_active {
        stop_idle_waiters(jobs, &state)?;
        return Ok(());
    }
    for slot in &state.slots {
        if slot.phase != SlotPhase2::Ready {
            continue;
        }
        if state
            .jobs
            .iter()
            .any(|job| job.slot_id == slot.slot_id && job.phase.occupies_slot())
        {
            continue;
        }
        // Journal Ready is not physical idleness. A live waiter, a persisted
        // waiter owner, or an in-flight lease whose containers are still
        // tearing down must keep this slot unspawnable.
        if child_owns_slot(
            &args.state_dir,
            &state,
            jobs,
            &slot.slot_id,
            slot.generation,
        )? {
            continue;
        }
        let Ok(slot_dir) = recovery_slot_config_dir(&args.state_dir, &exec, &state, &slot.slot_id)
        else {
            continue;
        };
        if crate::runner::recorded_in_flight_job_exists(&slot_dir)? {
            continue;
        }
        let Some(stored) = load_local_runner_config(&slot_dir)? else {
            continue;
        };
        if !ready_waiter_config_matches(&exec, &args.scope, &slot.slot_id, &stored) {
            continue;
        }
        let waiter_id = format!("wait-{}", slot.slot_id.0);
        if jobs.contains_key(&waiter_id) {
            continue;
        }
        let waiter_liveness =
            cleanup::owned_pid_liveness(&args.state_dir, &waiter_id, slot.generation.0)?;
        // The exact persisted JIT identity and the absence of an in-flight
        // lease establish that this Ready slot needs a waiter. A missing owner
        // marker after restart is therefore recoverable once live owners have
        // already been excluded above.
        if !waiter_marker_allows_spawn(waiter_liveness) {
            continue;
        }
        spawn(
            args,
            journal,
            jobs,
            &waiter_id,
            slot.generation.0,
            Some(&slot.slot_id),
            false,
        )?;
    }
    Ok(())
}

fn waiter_marker_allows_spawn(liveness: cleanup::OwnedPidLiveness) -> bool {
    match liveness {
        cleanup::OwnedPidLiveness::Dead | cleanup::OwnedPidLiveness::Absent => true,
        cleanup::OwnedPidLiveness::Live | cleanup::OwnedPidLiveness::UnpublishedIntentLive => false,
    }
}

/// A persisted Ready row may start a waiter after controller restart only
/// when its slot-local JIT config still names the exact current registration.
/// The durable daemon exec config plus runner.json carry this identity; missing
/// or drifted fields fail closed instead of borrowing mutable in-memory state.
fn ready_waiter_config_matches(
    exec: &crate::args::DaemonArgs,
    controller_scope: &str,
    slot: &SlotId,
    stored: &config::StoredRunnerConfig,
) -> bool {
    let Some(url) = exec.url.as_deref() else {
        return false;
    };
    let Some(slot_index) = slot
        .0
        .rsplit_once('-')
        .and_then(|(_, index)| index.parse::<usize>().ok())
    else {
        return false;
    };
    if slot_index == 0 || slot_index > exec.slots || slot_id(controller_scope, slot_index) != *slot
    {
        return false;
    }

    let Some(requested_scope) = jit_scope_identity(url) else {
        return false;
    };
    if jit_scope_identity(&stored.settings.github_url).as_deref() != Some(&requested_scope) {
        return false;
    }

    let requested_labels = crate::runner::normalize_labels(
        exec.labels.clone(),
        exec.target_mvp_labels,
        exec.target_mvp_arm_label,
    );
    let requested_labels: std::collections::BTreeSet<&str> =
        requested_labels.iter().map(String::as_str).collect();
    let stored_labels: std::collections::BTreeSet<&str> =
        stored.settings.labels.iter().map(String::as_str).collect();
    if stored_labels != requested_labels {
        return false;
    }

    let runner_group_matches = match exec.pool_name.as_deref().filter(|name| !name.is_empty()) {
        Some(_) if exec.pool_id_pre_resolved => {
            stored.settings.pool_id == Some(exec.pool_id.unwrap_or(1))
        }
        Some(_) if exec.dry_run_registration && exec.pool_id.is_some() => {
            stored.settings.pool_id == Some(exec.pool_id.unwrap_or(1))
        }
        Some(name) => stored
            .settings
            .pool_name
            .as_deref()
            .is_some_and(|stored| stored.eq_ignore_ascii_case(name)),
        None => stored.settings.pool_id == Some(exec.pool_id.unwrap_or(1)),
    };
    if !runner_group_matches {
        return false;
    }

    let host = crate::runner::github_runner_host_slug();
    let instance = exec.name.as_deref().unwrap_or("local");
    let slot_zero = slot_index - 1;
    let current_agent = crate::runner::compose_github_runner_name(&host, instance, slot_zero);
    let agent_name_matches = stored.settings.agent_name == current_agent
        || stored
            .settings
            .agent_name
            .rsplit_once("-next-")
            .and_then(|(_, suffix)| suffix.split_once('-'))
            .and_then(|(pid, cycle)| Some((pid.parse::<u32>().ok()?, cycle.parse::<u64>().ok()?)))
            .is_some_and(|(pid, cycle)| {
                crate::runner::compose_github_runner_successor_name(
                    &host, instance, slot_zero, pid, cycle,
                ) == stored.settings.agent_name
            });
    if !agent_name_matches {
        return false;
    }

    let requires_trusted_scope = stored_labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case(crate::runner::TRUST_GATED_RUNNER_LABEL));
    if requires_trusted_scope
        && !crate::runner::github_trust_scope_allows_host_docker(&exec.trust_scope)
    {
        return false;
    }

    stored.settings.agent_id.is_some_and(|id| id > 0)
        && stored.settings.use_v2_flow
        && stored
            .settings
            .server_url_v2
            .as_deref()
            .is_some_and(|url| !url.is_empty())
        && stored.settings.ephemeral
        && stored.settings.disable_update
        && stored.credentials.is_some()
}

fn jit_scope_identity(url: &str) -> Option<String> {
    let scope = GitHubScope::parse(url).ok()?;
    let mut url = url::Url::parse(&scope.original_url).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Some(url.to_string().to_ascii_lowercase())
}

/// A provisional Assigned row cannot start its normal Ready waiter, yet the
/// slot worker is the only place that previously ran the renewjob oracle.
/// Launch one marked recovery-only waiter only when the row carries the plan,
/// exact permit lease, and remaining probe budget needed to make progress.
/// Planless, exhausted, and pre-migration rows stay durably fenced; repeatedly
/// spawning a helper cannot resolve them safely.
fn spawn_provisional_recovery_waiters(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
) -> anyhow::Result<()> {
    spawn_provisional_recovery_waiters_with(args, journal, jobs, maybe_spawn_job)
}

fn spawn_provisional_recovery_waiters_with(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
    mut spawn: impl FnMut(
        &ControllerArgs,
        &Journal,
        &mut HashMap<String, Child>,
        &str,
        u64,
        Option<&SlotId>,
        bool,
    ) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let state = journal.materialized_state()?;
    if state.drain_active || state.admission_blocked {
        return Ok(());
    }
    let now = epoch_now();
    let eligible_rows: Vec<_> = state
        .jobs
        .iter()
        .filter(|job| {
            job.provisional
                && job.phase == JobPhase2::Assigned
                && !job.plan_id.is_empty()
                && !job.probe_budget_exhausted(now)
                && job.permit_lease.is_some()
        })
        .collect();
    if eligible_rows.is_empty() {
        return Ok(());
    }

    // A recovery child resolves every eligible provisional row when it
    // starts. Precompute row ownership by slot once and start at most one
    // child, so N rows cause one fleet probe pass rather than N passes.
    let mut slot_owners: HashMap<(String, u64), (usize, bool)> = HashMap::new();
    for job in state.jobs.iter().filter(|job| job.phase.occupies_slot()) {
        let owners = slot_owners
            .entry((job.slot_id.0.clone(), job.generation.0))
            .or_default();
        if job.provisional {
            owners.0 += 1;
        } else {
            owners.1 = true;
        }
    }
    let assigned_slots: HashSet<(String, u64)> = state
        .slots
        .iter()
        .filter(|slot| slot.phase == SlotPhase2::Assigned)
        .map(|slot| (slot.slot_id.0.clone(), slot.generation.0))
        .collect();

    for row in &eligible_rows {
        let waiter_id = format!("wait-{}", row.slot_id.0);
        if jobs.contains_key(&row.job_id.0) || jobs.contains_key(&waiter_id) {
            // Any owned child may itself be resolving this fleet's pending
            // acquisitions. Do not overlap its one-pass probe.
            return Ok(());
        }
        let worker_liveness =
            cleanup::owned_pid_liveness(&args.state_dir, &row.job_id.0, row.generation.0)?;
        let waiter_liveness =
            cleanup::owned_pid_liveness(&args.state_dir, &waiter_id, row.generation.0)?;
        if !matches!(
            worker_liveness,
            cleanup::OwnedPidLiveness::Absent | cleanup::OwnedPidLiveness::Dead
        ) || waiter_liveness != cleanup::OwnedPidLiveness::Dead
        {
            // The child probes every eligible row. One live, unpublished, or
            // owner-unknown row prevents this fleet-wide pass from starting.
            return Ok(());
        }
    }

    let candidate = eligible_rows.into_iter().find(|row| {
        let key = (row.slot_id.0.clone(), row.generation.0);
        let exactly_one_provisional =
            slot_owners
                .get(&key)
                .is_some_and(|(provisional_count, has_other_owner)| {
                    *provisional_count == 1 && !*has_other_owner
                });
        let assigned_slot = assigned_slots.contains(&key);
        exactly_one_provisional && assigned_slot
    });
    let Some(row) = candidate else {
        return Ok(());
    };

    // The recovery role validates this exact slot against the journal before
    // it begins the single fleet-wide provisional probe pass.
    let waiter_id = format!("wait-{}", row.slot_id.0);
    spawn(
        args,
        journal,
        jobs,
        &waiter_id,
        row.generation.0,
        Some(&row.slot_id),
        true,
    )?;
    Ok(())
}

/// Stop only broker waiters that have not promoted into durable job work.
/// Waiter processes retain their `wait-<slot>` key while executing a job, so
/// the slot's active journal row is the authority for preserving that child.
fn stop_idle_waiters(
    jobs: &mut HashMap<String, Child>,
    state: &velnor_control::journal::FleetState,
) -> anyhow::Result<()> {
    let active_slots: HashSet<&SlotId> = state
        .jobs
        .iter()
        .filter(|job| job.phase.occupies_slot())
        .map(|job| &job.slot_id)
        .collect();
    for (job_id, child) in jobs.iter() {
        let Some(slot_id) = job_id.strip_prefix("wait-") else {
            continue;
        };
        if !active_slots.iter().any(|active| active.0 == slot_id) {
            request_child_shutdown(child)?;
        }
    }
    Ok(())
}

/// Force-remove a provably dead worker's containers before its slot permit
/// and marker can be released. A Docker error keeps the marker and permit held
/// so the controller can retry cleanup on its next cycle.
fn teardown_orphaned_job_containers(
    job_id: &str,
    docker_backend: bool,
    docker: impl FnMut(&[String]) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    if !docker_backend {
        return Ok(());
    }
    // Docker ownership labels use the exact job-container identity emitted by
    // the GitHub adapter (`velnor-job-<job_id>`), not the Run Service's raw
    // job id. Keep this conversion at the orphan-teardown boundary so every
    // controller recovery path uses the same key as normal execution.
    let job_container = crate::github_adapter::job_container_name_for_id(job_id);
    crate::docker_lease::force_remove_job_owned_containers(&job_container, docker)
        .with_context(|| format!("remove containers owned by orphaned job {job_id}"))?;
    Ok(())
}

/// Return slots occupied by job workers that died without a terminal
/// completion (daemon drain mid-run, OOM-kill, reboot). Without this the
/// slot stays `Assigned` forever and advertised capacity never recovers.
async fn reclaim_orphaned_jobs(
    args: &ControllerArgs,
    journal: &mut Journal,
    remote_deadline: tokio::time::Instant,
    scan_persisted_markers: bool,
    docker: impl FnMut(&[String]) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    reclaim_orphaned_jobs_with_children(
        args,
        journal,
        &HashMap::new(),
        remote_deadline,
        scan_persisted_markers,
        docker,
    )
    .await
}

async fn reclaim_orphaned_jobs_with_children(
    args: &ControllerArgs,
    journal: &mut Journal,
    owned_children: &HashMap<String, Child>,
    remote_deadline: tokio::time::Instant,
    scan_persisted_markers: bool,
    docker: impl FnMut(&[String]) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    reclaim_orphaned_jobs_with_marker_cleanup(
        args,
        journal,
        owned_children,
        remote_deadline,
        scan_persisted_markers,
        docker,
        |slot_dir, record| {
            crate::runner::cleanup_recorded_in_flight_job_for_record_with(
                slot_dir,
                record,
                |record| {
                    let generation = record.generation().ok_or_else(|| {
                        anyhow::anyhow!(
                            "in-flight marker for job {} has no recorded generation",
                            record.job_id()
                        )
                    })?;
                    cleanup::remove_outbox(&args.state_dir, record.job_id(), generation)
                },
            )
        },
    )
    .await
}

async fn reclaim_orphaned_jobs_with_marker_cleanup(
    args: &ControllerArgs,
    journal: &mut Journal,
    owned_children: &HashMap<String, Child>,
    remote_deadline: tokio::time::Instant,
    scan_persisted_markers: bool,
    mut docker: impl FnMut(&[String]) -> anyhow::Result<String>,
    mut cleanup_marker: impl FnMut(&Path, &crate::runner::InFlightJobRecord) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let state = journal.materialized_state()?;
    let mut orphan_jobs = Vec::new();
    for job in state
        .jobs
        .iter()
        .filter(|job| job.phase.occupies_slot() && !job.provisional)
    {
        // Controller-spawned slot waiters keep their waiter identity while
        // running broker jobs. Until the journal records any other owner role,
        // that waiter marker is the required death proof. Child handles remain
        // stronger local proof than persisted markers.
        let waiter_id = format!("wait-{}", job.slot_id.0);
        if owned_children.contains_key(&job.job_id.0) || owned_children.contains_key(&waiter_id) {
            continue;
        }
        let worker_liveness =
            cleanup::owned_pid_liveness(&args.state_dir, &job.job_id.0, job.generation.0)?;
        let waiter_liveness =
            cleanup::owned_pid_liveness(&args.state_dir, &waiter_id, job.generation.0)?;
        if matches!(
            worker_liveness,
            cleanup::OwnedPidLiveness::Live | cleanup::OwnedPidLiveness::UnpublishedIntentLive
        ) || matches!(
            waiter_liveness,
            cleanup::OwnedPidLiveness::Live | cleanup::OwnedPidLiveness::UnpublishedIntentLive
        ) {
            continue;
        }
        if !waiter_owned_job_markers_prove_death(worker_liveness, waiter_liveness) {
            eprintln!(
                "Warning: orphan recovery for job {} deferred; worker or waiter ownership remains unknown",
                job.job_id.0
            );
            continue;
        }
        orphan_jobs.push(job.clone());
    }

    if orphan_jobs.is_empty() && !scan_persisted_markers {
        return Ok(());
    }

    let exec = match load_exec_config(&args.state_dir) {
        Ok(exec) => Some(exec),
        Err(error)
            if orphan_jobs.is_empty()
                || orphan_jobs.iter().all(|job| {
                    job.phase == JobPhase2::Completing
                        && state.outbox.iter().any(|row| {
                            row.job_id == job.job_id
                                && row.generation == job.generation
                                && row.intended
                                && !row.remote_acked
                        })
                }) =>
        {
            // A markerless pending completion has no persisted Run Service URL
            // from which the controller can safely replay the remote call.
            // Preserve the durable outbox barrier and retry after the worker
            // or execution config returns; never discard it or invent a path.
            eprintln!("orphan recovery deferred: cannot load daemon execution config: {error:#}");
            None
        }
        Err(error) => {
            return Err(error).context("load daemon execution config for orphan recovery")
        }
    };
    let Some(exec) = exec else {
        return Ok(());
    };

    // Container teardown runs only on the docker backend. Missing or
    // unparsable selection is never treated as docker, which also keeps
    // backend-less unit fixtures hermetic.
    let backend = crate::execution::load_execution_file(&args.state_dir, None)
        .ok()
        .map(|file| file.backend());
    let docker_backend =
        velnor_model::ExecutionBackendKind::permits_host_docker_maintenance(backend);
    let mut teardown =
        |job_id: &str| teardown_orphaned_job_containers(job_id, docker_backend, &mut docker);

    for job in orphan_jobs {
        // One Completing row must not abort the fleet cycle. Log and move on
        // so other slots still reclaim; the next tick retries this job.
        if let Err(error) = recover_one_orphaned_job(
            args,
            journal,
            &exec,
            &state,
            &job,
            remote_deadline,
            &mut teardown,
        )
        .await
        {
            eprintln!(
                "Warning: orphan recovery for job {} failed this cycle: {error:#}",
                job.job_id.0
            );
        }
    }

    if !scan_persisted_markers {
        return Ok(());
    }

    // A prior RemoteAcked event removes the journal job before container,
    // storage, and permit cleanup finish. Scan every exact persisted slot
    // path so a crash in that gap remains retryable without a job row.
    let current = journal.materialized_state()?;
    for slot in &state.slots {
        let slot_dir = recovery_slot_config_dir(&args.state_dir, &exec, &state, &slot.slot_id)?;
        let Some(marker_record) = crate::runner::recorded_in_flight_job_record(&slot_dir)? else {
            continue;
        };
        let marker_job_id = marker_record.job_id().to_owned();
        let marker_generation = marker_record.generation().ok_or_else(|| {
            anyhow::anyhow!(
                "legacy in-flight marker for job {} has no recorded generation; retaining it for operator recovery",
                marker_job_id
            )
        })?;
        let marker_generation = Generation(marker_generation);
        if let Some(job) = current
            .jobs
            .iter()
            .find(|job| job.job_id.0 == marker_job_id)
        {
            if job.slot_id != slot.slot_id || job.generation != marker_generation {
                return Err(anyhow::anyhow!(
                    "in-flight marker job {} generation {} does not match journal slot {} job generation {}",
                    marker_job_id,
                    marker_generation.0,
                    slot.slot_id.0,
                    job.generation.0
                ));
            }
            continue;
        }
        let waiter_id = format!("wait-{}", slot.slot_id.0);
        if owned_children.contains_key(&marker_job_id) || owned_children.contains_key(&waiter_id) {
            // A Child handle is authoritative even if the worker has not
            // published its PID marker yet. RemoteAcked ends remote ownership,
            // not the process that still needs to finish local cleanup.
            continue;
        }
        let remote_acked =
            journal.has_remote_terminal_ack(&JobId(marker_job_id.clone()), marker_generation)?;
        let locally_abandoned = journal
            .unresolvable_completions()?
            .iter()
            .any(|completion| {
                completion.job_id.0 == marker_job_id && completion.generation == marker_generation
            });
        // RemoteAcked proves terminal completion, not that the waiter
        // finished local cleanup. Require its marker to prove death; a
        // job-keyed marker is optional but must be dead if present.
        let worker_liveness =
            cleanup::owned_pid_liveness(&args.state_dir, &marker_job_id, marker_generation.0)?;
        let waiter_liveness =
            cleanup::owned_pid_liveness(&args.state_dir, &waiter_id, marker_generation.0)?;
        if matches!(
            worker_liveness,
            cleanup::OwnedPidLiveness::Live | cleanup::OwnedPidLiveness::UnpublishedIntentLive
        ) || matches!(
            waiter_liveness,
            cleanup::OwnedPidLiveness::Live | cleanup::OwnedPidLiveness::UnpublishedIntentLive
        ) {
            // The marker can legitimately exist while an owner retries local
            // cleanup after its remote acknowledgement. Defer all mutation.
            continue;
        }
        if !waiter_owned_job_markers_prove_death(worker_liveness, waiter_liveness) {
            return Err(anyhow::anyhow!(
                "marker-only in-flight job {} has unknown worker or waiter ownership",
                marker_job_id
            ));
        }
        // Owner death must be followed by container cleanup before the permit
        // or marker can be released.
        teardown(&marker_job_id)?;
        if remote_acked || locally_abandoned {
            // Local payload loss and bounded completion abandonment are also
            // durable terminal outcomes. They do not authorize a remote send,
            // but they do prove this marker's job no longer owns a slot.
            cleanup_marker(&slot_dir, &marker_record)?;
        } else {
            let Some(stored) = load_local_runner_config(&slot_dir)? else {
                return Err(anyhow::anyhow!(
                    "runner credentials missing while recovering marker-only in-flight job {}",
                    marker_job_id
                ));
            };
            let Some(cleaned) = defer_remote_recovery_on_timeout(
                remaining_remote_budget(remote_deadline),
                crate::runner::complete_recorded_in_flight_job_after_journal_acceptance_for_record(
                    &slot_dir,
                    &stored,
                    &marker_record,
                ),
                "complete marker-only in-flight job after journal acceptance gap",
            )
            .await?
            else {
                continue;
            };
            if !cleaned {
                return Err(anyhow::anyhow!(
                    "marker-only in-flight job {} disappeared during recovery",
                    marker_job_id
                ));
            }
        }
        if crate::runner::recorded_in_flight_job_exists(&slot_dir)? {
            return Err(anyhow::anyhow!(
                "orphaned in-flight marker remained for job {}",
                marker_job_id
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn recover_one_orphaned_job(
    args: &ControllerArgs,
    journal: &mut Journal,
    exec: &crate::args::DaemonArgs,
    state: &velnor_control::journal::FleetState,
    job: &velnor_control::journal::JobRecord,
    remote_deadline: tokio::time::Instant,
    teardown: &mut impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let slot_dir = recovery_slot_config_dir(&args.state_dir, exec, state, &job.slot_id)?;
    let marker_record = crate::runner::recorded_in_flight_job_record(&slot_dir)?;
    let marker_job_id = marker_record
        .as_ref()
        .map(|record| record.job_id().to_owned());
    let mut tore_down = false;
    let pending_completion = job.phase == JobPhase2::Completing
        && state.outbox.iter().any(|row| {
            row.job_id == job.job_id
                && row.generation == job.generation
                && row.intended
                && !row.remote_acked
        });
    if let Some(record) = marker_record.as_ref() {
        if record.job_id() != job.job_id.0 {
            anyhow::bail!(
                "in-flight marker job {} does not match orphan job {}",
                record.job_id(),
                job.job_id.0
            );
        }
        if record.generation() != Some(job.generation.0) {
            anyhow::bail!(
                "in-flight marker for orphan job {} has generation {:?}, expected {}",
                record.job_id(),
                record.generation(),
                job.generation.0
            );
        }
    }
    if job.phase == JobPhase2::Completing
        && !pending_completion
        && job.terminal_conclusion.is_none()
    {
        // Missing payload is a retryable gap, not a controller failure.
        // Returning Err here used to abort the whole reconcile loop and
        // leave every Completing slot stuck until the next process start.
        eprintln!(
            "Warning: completing job {} has no exact durable completion payload; retrying next cycle",
            job.job_id.0
        );
        return Ok(());
    }
    if let Some(stored) = load_local_runner_config(&slot_dir)? {
        if let Some(expected_marker) = marker_record.as_ref() {
            // The ownership filter above proved both the worker and waiter
            // are dead. Tear down owned containers before any terminal
            // replay can release the permit and marker.
            teardown(&job.job_id.0)?;
            tore_down = true;
            let cleanup = if pending_completion {
                match defer_remote_recovery_on_timeout(
                    remaining_remote_budget(remote_deadline),
                    crate::runner::replay_recorded_completion_for_record(
                        &slot_dir,
                        &stored,
                        &args.state_dir,
                        expected_marker,
                    ),
                    "replay recorded completion during orphan recovery",
                )
                .await
                {
                    Ok(cleanup) => cleanup,
                    Err(error) => {
                        eprintln!(
                            "Warning: completion replay for job {} failed: {error:#}",
                            job.job_id.0
                        );
                        if let Some(row) =
                            journal
                                .materialized_state()?
                                .outbox
                                .into_iter()
                                .find(|row| {
                                    row.job_id == job.job_id && row.generation == job.generation
                                })
                        {
                            // A spent budget abandons the row and restores the
                            // slot to Ready. Containers were torn down before
                            // replay, while the marker still blocked reuse.
                            abandon_if_budget_spent(args, journal, &row)?;
                        }
                        return Ok(());
                    }
                }
            } else if let Some(conclusion) = job.terminal_conclusion.as_deref() {
                match defer_remote_recovery_on_timeout(
                    remaining_remote_budget(remote_deadline),
                    crate::runner::complete_recorded_in_flight_job_with_terminal_conclusion_for_record(
                        &slot_dir,
                        &stored,
                        conclusion,
                        expected_marker,
                    ),
                    "complete recorded terminal conclusion during orphan recovery",
                )
                .await
                {
                    Ok(cleanup) => cleanup,
                    Err(error) => {
                        eprintln!(
                            "Warning: terminal conclusion replay for job {} failed: {error:#}",
                            job.job_id.0
                        );
                        return Ok(());
                    }
                }
            } else {
                match defer_remote_recovery_on_timeout(
                    remaining_remote_budget(remote_deadline),
                    crate::runner::complete_recorded_in_flight_job_for_record(
                        &slot_dir,
                        &stored,
                        expected_marker,
                    ),
                    "complete recorded in-flight job during orphan recovery",
                )
                .await
                {
                    Ok(cleanup) => cleanup,
                    Err(error) => {
                        eprintln!(
                            "Warning: in-flight completion for job {} failed: {error:#}",
                            job.job_id.0
                        );
                        return Ok(());
                    }
                }
            };
            let Some(cleanup) = cleanup else {
                return Ok(());
            };
            if cleanup {
                eprintln!(
                    "Recovered stale in-flight job {} before restoring slot {}",
                    job.job_id.0, job.slot_id.0
                );
            }
        }
    } else if marker_job_id.is_some() {
        if job.phase == JobPhase2::Completing {
            eprintln!(
                "Warning: runner credentials missing while recovering completing job {}; retrying next cycle",
                job.job_id.0
            );
            return Ok(());
        }
        anyhow::bail!(
            "runner credentials missing while recovering in-flight job {}",
            job.job_id.0
        );
    }
    if crate::runner::recorded_in_flight_job_exists(&slot_dir)? && pending_completion {
        eprintln!(
            "Warning: completing job {} retained its in-flight marker after recovery; retrying next cycle",
            job.job_id.0
        );
        return Ok(());
    }
    let current = journal.materialized_state()?;
    if current.jobs.iter().all(|row| row.job_id != job.job_id) {
        // Marker-backed terminal paths already removed containers while the
        // marker still blocked reuse. Markerless recovery reaches this branch
        // only after the journal itself removed the row.
        if !tore_down {
            teardown(&job.job_id.0)?;
        }
        return Ok(());
    }
    if pending_completion {
        eprintln!(
            "Warning: completing job {} has no recoverable terminal acknowledgement; retrying next cycle",
            job.job_id.0
        );
        return Ok(());
    }
    // The role-specific owner markers prove the worker is dead, so any
    // container still carrying this job's label is a leak. Remove them
    // before the slot returns to Ready; after JobWorkerLost no path
    // would ever touch them again.
    if !tore_down {
        teardown(&job.job_id.0)?;
    }
    let lost = journal.apply(Event::JobWorkerLost {
        job_id: job.job_id.clone(),
        generation: job.generation,
    })?;
    if !lost.rejected {
        eprintln!(
            "Warning: job {} worker lost on {}; slot restored to Ready",
            job.job_id.0, job.slot_id.0
        );
    }
    Ok(())
}

/// Remote orphan recovery must not consume the controller's whole cycle. A
/// timeout preserves the durable marker and lets the next cycle retry it.
async fn defer_remote_recovery_on_timeout<F, T>(
    timeout: Duration,
    operation: F,
    description: &'static str,
) -> anyhow::Result<Option<T>>
where
    F: Future<Output = anyhow::Result<T>>,
{
    if timeout.is_zero() {
        eprintln!("{description} deferred; remote recovery budget is exhausted");
        return Ok(None);
    }
    match tokio::time::timeout(timeout, operation).await {
        Ok(result) => result.map(Some).context(description),
        Err(_) => {
            eprintln!("{description} timed out; preserving durable recovery state");
            Ok(None)
        }
    }
}

/// Reconcile files left by a crash between durable outbox publication and
/// `CompletionIntended`. Live child handles and ownership markers keep the
/// writer's file from being deleted during that tiny window. The worker may
/// still be registered as `wait-{slot}` while completing its acquired job, so
/// a matching active journal row maps the outbox job id back to that waiter.
/// Unknown ownership keeps the payload until a later reconciliation can prove
/// that every possible writer is dead.
fn reconcile_orphaned_outboxes(
    args: &ControllerArgs,
    journal: &Journal,
    owned_children: &HashMap<String, Child>,
) -> anyhow::Result<()> {
    let state = journal.materialized_state()?;
    let abandoned = journal
        .unresolvable_completions()?
        .into_iter()
        .map(|completion| (completion.job_id.0, completion.generation.0))
        .collect::<HashSet<_>>();
    cleanup::reconcile_outbox_entries(&args.state_dir, |name| {
        let (job_id, generation, temporary) = parse_outbox_entry_name(name)?;
        if abandoned.contains(&(job_id.clone(), generation)) {
            return Ok(false);
        }
        let row = state
            .outbox
            .iter()
            .find(|row| row.job_id.0 == job_id && row.generation.0 == generation);
        let keep = if temporary {
            outbox_owner_requires_retention(
                &args.state_dir,
                &job_id,
                generation,
                &state,
                owned_children,
            )
        } else {
            row.is_some_and(|row| row.intended && !row.remote_acked && !row.abandoned)
                || row.is_none()
                    && outbox_owner_requires_retention(
                        &args.state_dir,
                        &job_id,
                        generation,
                        &state,
                        owned_children,
                    )
        };
        Ok(keep)
    })
}

fn parse_outbox_name(name: &str) -> anyhow::Result<(String, u64)> {
    let (job_id, generation) = name
        .rsplit_once('.')
        .ok_or_else(|| anyhow::anyhow!("completion outbox filename has no generation: {name}"))?;
    cleanup::assert_safe_id(job_id)?;
    let generation = generation.parse::<u64>().map_err(|error| {
        anyhow::anyhow!("completion outbox filename has invalid generation {generation}: {error}")
    })?;
    Ok((job_id.to_owned(), generation))
}

fn parse_outbox_entry_name(name: &str) -> anyhow::Result<(String, u64, bool)> {
    if let Some(stem) = name.strip_prefix("..") {
        let (stem, nonce) = stem.rsplit_once(".tmp-").ok_or_else(|| {
            anyhow::anyhow!("completion outbox temporary filename is malformed: {name}")
        })?;
        let (pid, serial) = nonce.split_once('-').ok_or_else(|| {
            anyhow::anyhow!("completion outbox temporary filename is malformed: {name}")
        })?;
        pid.parse::<u32>().map_err(|error| {
            anyhow::anyhow!("completion outbox temporary filename has invalid pid: {error}")
        })?;
        serial.parse::<u64>().map_err(|error| {
            anyhow::anyhow!("completion outbox temporary filename has invalid serial: {error}")
        })?;
        let (job_id, generation) = parse_outbox_name(stem)?;
        return Ok((job_id, generation, true));
    }
    let (job_id, generation) = parse_outbox_name(name)?;
    Ok((job_id, generation, false))
}

fn outbox_owner_requires_retention(
    state_dir: &Path,
    job_id: &str,
    generation: u64,
    state: &velnor_control::journal::FleetState,
    owned_children: &HashMap<String, Child>,
) -> bool {
    if owned_children.contains_key(job_id) {
        return true;
    }

    let active_job = state.jobs.iter().find(|job| {
        job.job_id.0 == job_id && job.generation.0 == generation && job.phase.occupies_slot()
    });
    let waiter_id = active_job.map(|job| format!("wait-{}", job.slot_id.0));
    if waiter_id
        .as_ref()
        .is_some_and(|waiter_id| owned_children.contains_key(waiter_id))
    {
        return true;
    }

    let worker_liveness = match cleanup::owned_pid_liveness(state_dir, job_id, generation) {
        Ok(owner_state) => owner_state,
        Err(error) => {
            eprintln!(
                "Warning: orphan outbox for job {job_id} generation {generation} retained; ownership state for {job_id} is unknown: {error:#}"
            );
            return true;
        }
    };
    let Some(waiter_id) = waiter_id else {
        return worker_liveness != cleanup::OwnedPidLiveness::Dead;
    };
    let waiter_liveness = match cleanup::owned_pid_liveness(state_dir, &waiter_id, generation) {
        Ok(owner_state) => owner_state,
        Err(error) => {
            eprintln!(
                "Warning: orphan outbox for job {job_id} generation {generation} retained; ownership state for {waiter_id} is unknown: {error:#}"
            );
            return true;
        }
    };
    if !waiter_owned_job_markers_prove_death(worker_liveness, waiter_liveness) {
        eprintln!(
            "Warning: orphan outbox for job {job_id} generation {generation} retained; worker or waiter ownership is unknown"
        );
        return true;
    }
    false
}

/// Controller-spawned slot workers keep their durable identity as
/// `wait-{slot}` while `run_daemon_slot` executes broker jobs. Require that
/// canonical marker to be dead; a job-keyed marker is optional but, if it
/// exists, must also be dead. Absent or unreadable waiter evidence cannot
/// authorize cleanup.
fn waiter_owned_job_markers_prove_death(
    worker: cleanup::OwnedPidLiveness,
    waiter: cleanup::OwnedPidLiveness,
) -> bool {
    waiter == cleanup::OwnedPidLiveness::Dead
        && matches!(
            worker,
            cleanup::OwnedPidLiveness::Absent | cleanup::OwnedPidLiveness::Dead
        )
}

fn recovery_slot_config_dir(
    default_base: &Path,
    exec: &crate::args::DaemonArgs,
    state: &velnor_control::journal::FleetState,
    slot_id: &SlotId,
) -> anyhow::Result<PathBuf> {
    let slot_index = slot_id
        .0
        .rsplit('-')
        .next()
        .ok_or_else(|| anyhow::anyhow!("slot id {} has no numeric suffix", slot_id.0))?
        .parse::<usize>()
        .map_err(|error| {
            anyhow::anyhow!("slot id {} has invalid numeric suffix: {error}", slot_id.0)
        })?;
    if slot_index == 0 {
        return Err(anyhow::anyhow!("slot id {} has zero index", slot_id.0));
    }
    if exec.slots == 0 {
        return Err(anyhow::anyhow!(
            "daemon execution config has zero slots during orphan recovery"
        ));
    }
    if slot_index > exec.slots {
        return Err(anyhow::anyhow!(
            "slot {} is outside daemon execution layout 1..={}",
            slot_id.0,
            exec.slots
        ));
    }
    if !state.slots.iter().any(|slot| slot.slot_id == *slot_id) {
        return Err(anyhow::anyhow!(
            "orphan job references slot {} absent from durable state",
            slot_id.0
        ));
    }
    let config_base = exec
        .config_dir
        .clone()
        .unwrap_or_else(|| default_base.to_path_buf());
    let slot_dir = crate::runner::daemon_slot_config_dir(&config_base, slot_index, exec.slots);
    let alternate = if exec.slots == 1 {
        config_base.join("slots").join(format!("slot-{slot_index}"))
    } else {
        config_base.clone()
    };
    if alternate != slot_dir && recovery_path_has_state(&alternate)? {
        return Err(anyhow::anyhow!(
            "daemon slot {} has state in both incompatible config layouts; refusing recovery",
            slot_id.0
        ));
    }
    Ok(slot_dir)
}

fn recovery_path_has_state(path: &Path) -> anyhow::Result<bool> {
    for name in ["runner.json", "in-flight-job.json"] {
        match std::fs::symlink_metadata(path.join(name)) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

/// Move a completion whose durable attempt or time budget is spent into its
/// bounded terminal state. Returns whether the row was abandoned.
///
/// The journal is the authority: it refuses the terminal state while any
/// budget remains, so this can only ever ratify what durable state already
/// proves.
fn abandon_if_budget_spent(
    args: &ControllerArgs,
    journal: &mut Journal,
    row: &velnor_control::journal::OutboxRecord,
) -> anyhow::Result<bool> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    if !row.budget_exhausted(now) {
        return Ok(false);
    }
    let reason = if row.permanent {
        "the run service permanently refused this completion payload"
    } else if row.attempts >= velnor_control::journal::MAX_COMPLETION_ATTEMPTS {
        "the durable completion send budget is spent"
    } else {
        "the completion resolution deadline passed"
    };
    complete::abandon_unresolvable_completion(
        journal,
        &args.state_dir,
        &row.job_id,
        row.generation,
        reason,
    )
}

/// Keep a durable completion payload. Never replace it with the checksum
/// and never stamp `CompletionSendStarted` without an actual send.
fn preserve_outbox(
    args: &ControllerArgs,
    journal: &mut Journal,
    job_id: &JobId,
    generation: Generation,
    payload_sha256: &str,
) -> anyhow::Result<()> {
    if outbox_payload_is_missing(&args.state_dir, &job_id.0, generation.0)? {
        return record_payload_loss(
            args,
            journal,
            job_id,
            generation,
            payload_sha256,
            "completion outbox payload is missing",
        );
    }
    let bytes = match cleanup::read_outbox(&args.state_dir, &job_id.0, generation.0) {
        Ok(bytes) => bytes,
        Err(_error) if outbox_payload_is_missing(&args.state_dir, &job_id.0, generation.0)? => {
            return record_payload_loss(
                args,
                journal,
                job_id,
                generation,
                payload_sha256,
                "completion outbox payload disappeared before it could be read",
            );
        }
        Err(error) => return Err(error),
    };
    let actual = velnor_control::journal::payload_checksum(&bytes);
    if actual != payload_sha256 {
        return record_payload_loss(
            args,
            journal,
            job_id,
            generation,
            payload_sha256,
            "completion outbox checksum mismatch",
        );
    }
    Ok(())
}

/// Check whether only the exact payload path is absent. Symlinks, non-directories,
/// and permission errors stay on the hard-error path in `preserve_outbox`.
fn outbox_payload_is_missing(
    state_dir: &Path,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<bool> {
    cleanup::assert_safe_id(job_id)?;
    let parent = state_dir.join("outbox");
    match std::fs::symlink_metadata(&parent) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Ok(false);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error.into()),
    }
    let path = cleanup::outbox_path(state_dir, job_id, generation);
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

/// Commit local payload loss before removing the exact outbox path. The
/// reducer validates owner, generation, and checksum and emits only local
/// cleanup/capacity effects; no remote acknowledgement is possible here.
fn record_payload_loss(
    args: &ControllerArgs,
    journal: &mut Journal,
    job_id: &JobId,
    generation: Generation,
    payload_sha256: &str,
    reason: &str,
) -> anyhow::Result<()> {
    let outcome =
        journal.record_completion_payload_loss(job_id, generation, payload_sha256, reason)?;
    if outcome.rejected {
        anyhow::bail!(
            "completion payload loss rejected for job {} generation {}",
            job_id.0,
            generation.0
        );
    }

    let mut deleted = false;
    for command in outcome.commands {
        match command {
            SideEffect::DeleteOutbox {
                job_id: command_job_id,
                generation: command_generation,
            } if command_job_id == *job_id && command_generation == generation => {
                cleanup::remove_outbox(args.state_dir.as_path(), &job_id.0, generation.0)
                    .context("remove lost completion outbox")?;
                deleted = true;
            }
            SideEffect::AdvertiseCapacity { permits } => {
                std::fs::write(
                    args.state_dir.join("advertised-capacity"),
                    permits.to_string(),
                )?;
            }
            other => {
                anyhow::bail!("payload-loss reducer emitted unexpected side effect: {other:?}");
            }
        }
    }
    if !deleted {
        anyhow::bail!(
            "payload-loss reducer omitted outbox deletion for job {} generation {}",
            job_id.0,
            generation.0
        );
    }
    eprintln!(
        "Error: completion payload for job {} generation {} was lost locally; recorded terminal local failure",
        job_id.0, generation.0
    );
    Ok(())
}

fn slot_index_from_id(slot_id: &SlotId) -> usize {
    slot_id
        .0
        .rsplit('-')
        .next()
        .and_then(|part| part.parse::<usize>().ok())
        .unwrap_or(1)
}

fn fenced_slot_recovery_generation(
    slot: Option<&SlotRecord>,
    state_dir: &Path,
    slot_config_dir: &Path,
    state: &velnor_control::journal::FleetState,
    jobs: &HashMap<String, Child>,
) -> anyhow::Result<Option<Generation>> {
    let Some(slot) = slot.filter(|slot| slot.phase == SlotPhase2::Fenced) else {
        return Ok(None);
    };
    // RemoteAcked removes the journal row immediately, while the physical
    // in-flight marker remains until storage and permit cleanup complete. Keep
    // the old generation so marker replay checks its exact ack and owner PIDs.
    if crate::runner::recorded_in_flight_job_exists(slot_config_dir)?
        || slot_has_admission_block(state, &slot.slot_id, slot.generation)
        || child_owns_slot(state_dir, state, jobs, &slot.slot_id, slot.generation)?
    {
        return Ok(None);
    }
    Ok(Some(slot.generation.next()))
}

fn slot_has_admission_block(
    state: &velnor_control::journal::FleetState,
    slot_id: &SlotId,
    generation: Generation,
) -> bool {
    state.admission_blocked
        || state.drain_active
        || state
            .jobs
            .iter()
            .any(|job| job.slot_id == *slot_id && job.phase.occupies_slot())
        || state
            .outbox
            .iter()
            .any(|row| row.is_pending() && row.slot_id == *slot_id && row.generation == generation)
}

fn child_owns_slot(
    state_dir: &Path,
    state: &velnor_control::journal::FleetState,
    jobs: &HashMap<String, Child>,
    slot_id: &SlotId,
    generation: Generation,
) -> anyhow::Result<bool> {
    let waiter_id = format!("wait-{}", slot_id.0);
    if jobs.contains_key(&waiter_id) {
        return Ok(true);
    }
    if state
        .jobs
        .iter()
        .any(|job| job.slot_id == *slot_id && jobs.contains_key(&job.job_id.0))
    {
        return Ok(true);
    }
    // Persisted ownership survives controller restart while the process lives.
    if matches!(
        cleanup::owned_pid_liveness(state_dir, &waiter_id, generation.0)?,
        cleanup::OwnedPidLiveness::Live | cleanup::OwnedPidLiveness::UnpublishedIntentLive
    ) {
        return Ok(true);
    }
    for job in state
        .jobs
        .iter()
        .filter(|job| job.slot_id == *slot_id && job.generation == generation)
    {
        if matches!(
            cleanup::owned_pid_liveness(state_dir, &job.job_id.0, generation.0)?,
            cleanup::OwnedPidLiveness::Live | cleanup::OwnedPidLiveness::UnpublishedIntentLive
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Journal work or a live waiter/worker means the slot is still acting.
/// Supervisor-heartbeat stale must not fence through that window: the waiter
/// is what GitHub talks to, and leaving it up after `SlotStale` deadlocks
/// generation recovery.
fn slot_is_acting(
    state_dir: &Path,
    state: &velnor_control::journal::FleetState,
    jobs: &HashMap<String, Child>,
    slot_id: &SlotId,
    generation: Generation,
) -> anyhow::Result<bool> {
    Ok(slot_has_admission_block(state, slot_id, generation)
        || child_owns_slot(state_dir, state, jobs, slot_id, generation)?)
}

async fn reap_supervised_child<C: ProcessHandle>(
    children: &mut HashMap<String, C>,
    key: &str,
    label: &str,
) -> anyhow::Result<()> {
    if !children.contains_key(key) {
        return Ok(());
    }
    // Proof: the `contains_key` guard above holds with no `await` between
    // it and this lookup (shutdown is synchronous), so the entry is `Some`.
    #[allow(clippy::expect_used, reason = "contains_key just proved presence")]
    request_child_shutdown(children.get(key).expect("child still present").process())?;
    let mut deadline = Instant::now() + FENCED_SLOT_TERMINATION_TIMEOUT;
    let mut escalated = false;
    loop {
        // Proof: `children` is exclusively borrowed, so no other task can
        // remove the entry; the loop's only `remove` returns immediately,
        // so the entry is present on every iteration.
        #[allow(clippy::expect_used, reason = "child retained until reap")]
        if children
            .get_mut(key)
            .expect("child retained until reap")
            .process_mut()
            .try_wait()?
            .is_some()
        {
            children.remove(key);
            return Ok(());
        }
        if Instant::now() >= deadline {
            if escalated {
                anyhow::bail!("{label} failed to reap after SIGKILL; handle retained");
            }
            // Proof: same as above — exclusive borrow plus remove-then-return
            // means the entry is present on every loop iteration.
            #[allow(clippy::expect_used, reason = "child retained until escalation")]
            children
                .get_mut(key)
                .expect("child retained until escalation")
                .process_mut()
                .kill()
                .map_err(|error| {
                    anyhow::anyhow!("{label} SIGKILL escalation failed; handle retained: {error}")
                })?;
            eprintln!("{label} shutdown escalated to SIGKILL");
            escalated = true;
            deadline = Instant::now() + FENCED_SLOT_TERMINATION_TIMEOUT;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn terminate_fenced_slot_actor(
    args: &ControllerArgs,
    slots: &mut SlotChildren,
    jobs: &mut HashMap<String, Child>,
    state: &velnor_control::journal::FleetState,
    slot_id: &SlotId,
    slot: &SlotRecord,
) -> anyhow::Result<()> {
    reap_supervised_child(slots, &slot_id.0, &format!("fenced slot {:?}", slot_id)).await?;
    for key in job_child_keys_for_slot(jobs, state, slot_id) {
        reap_supervised_child(
            jobs,
            &key,
            &format!("fenced slot {:?} child {key}", slot_id),
        )
        .await?;
    }

    let Some(pid) = slot.pid else {
        return Ok(());
    };
    let Some(process) = cleanup::pin_slot_process(pid, &args.state_dir, slot_id, slot.generation)
        .context("cannot prove fenced slot PID identity")?
    else {
        return Ok(());
    };
    if !process
        .signal(libc::SIGTERM)
        .context("cannot signal fenced slot process with SIGTERM")?
    {
        return Ok(());
    }
    let deadline = Instant::now() + FENCED_SLOT_TERMINATION_TIMEOUT;
    while Instant::now() < deadline {
        if !process
            .is_alive()
            .context("cannot recheck pinned fenced slot process")?
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // The same pidfd remains pinned from initial identity validation through
    // escalation, so PID reuse can never redirect SIGKILL to another actor.
    if process
        .is_alive()
        .context("cannot recheck pinned fenced slot process before escalation")?
    {
        process
            .signal(libc::SIGKILL)
            .context("cannot signal fenced slot process with SIGKILL")?;
    }
    Ok(())
}

fn stale_slot_deadline_reached(
    _args: &ControllerArgs,
    slot: Option<&SlotRecord>,
    id: &SlotId,
    deadlines: &mut HashMap<String, Instant>,
    slots: &mut SlotChildren,
    now: Instant,
) -> bool {
    let Some(slot) = slot else {
        return false;
    };
    if prove::slot_heartbeat_is_fresh_with_child(
        &_args.state_dir,
        id,
        slot.generation,
        SLOT_HEARTBEAT_MAX_AGE,
        owned_slot_child(slots, id),
    ) {
        return false;
    }
    let deadline = deadlines
        .entry(id.0.clone())
        .or_insert(now + SLOT_HEARTBEAT_MAX_AGE);
    *deadline <= now
}

fn owned_slot_child<'a>(
    slots: &'a mut SlotChildren,
    slot_id: &SlotId,
) -> Option<(&'a mut Child, Generation)> {
    let slot = slots.get_mut(&slot_id.0)?;
    let generation = slot.generation;
    Some((&mut slot.child, generation))
}

fn slot_heartbeat_is_fresh_from_cache(
    args: &ControllerArgs,
    id: &SlotId,
    generation: Generation,
    slots: &mut SlotChildren,
    verified: &mut HashMap<String, prove::VerifiedSlotHeartbeat>,
) -> bool {
    if let Some(evidence) = verified.get(&id.0).copied()
        && prove::cached_slot_heartbeat_is_fresh_with_child(
            &args.state_dir,
            id,
            &evidence,
            generation,
            Instant::now(),
            owned_slot_child(slots, id),
        )
        .is_some()
    {
        return true;
    }

    let current = prove::read_verified_slot_heartbeat_with_child(
        &args.state_dir,
        id,
        generation,
        SLOT_HEARTBEAT_MAX_AGE,
        owned_slot_child(slots, id),
    );
    if let Some(current) = current {
        verified.insert(id.0.clone(), current);
        true
    } else {
        verified.remove(&id.0);
        false
    }
}

async fn fence_stale_slot_actor(
    args: &ControllerArgs,
    journal: &mut Journal,
    slots: &mut SlotChildren,
    jobs: &mut HashMap<String, Child>,
    id: &SlotId,
    generation: Generation,
) -> anyhow::Result<()> {
    let outcome = journal.apply(Event::SlotStale {
        slot_id: id.clone(),
        generation,
    })?;
    if outcome.rejected {
        return Ok(());
    }
    let state = journal.materialized_state()?;
    let slot = state
        .slots
        .iter()
        .find(|slot| {
            slot.slot_id == *id && slot.generation == generation && slot.phase == SlotPhase2::Fenced
        })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("stale slot {id:?} was not fenced"))?;
    terminate_fenced_slot_actor(args, slots, jobs, &state, id, &slot).await
}

fn permit_needs_reconciliation(
    slot: Option<&SlotRecord>,
    generation: Generation,
    spawn_slots: bool,
    process_alive: bool,
) -> bool {
    let permit_matches = slot.is_some_and(|slot| slot.generation == generation && slot.permit_held);
    !permit_matches || (spawn_slots && !process_alive)
}

/// Read per-slot liveness files and serialize their durable journal effects in
/// this controller process. Slot processes must not contend on the shared
/// SQLite writer just to report liveness.
fn ingest_slot_heartbeats(
    args: &ControllerArgs,
    journal: &mut Journal,
    total: usize,
    slots: &mut SlotChildren,
    seen: &mut HashMap<String, (u32, u64)>,
) -> anyhow::Result<HashMap<String, prove::VerifiedSlotHeartbeat>> {
    let state = journal.materialized_state()?;
    let mut pending = Vec::new();
    let mut verified = HashMap::new();
    for index in 1..=total {
        let id = slot_id(&args.scope, index);
        let Some(slot) = state.slots.iter().find(|slot| slot.slot_id == id) else {
            continue;
        };
        let Some(heartbeat) = prove::read_verified_slot_heartbeat_with_child(
            &args.state_dir,
            &id,
            slot.generation,
            SLOT_HEARTBEAT_MAX_AGE,
            owned_slot_child(slots, &id),
        ) else {
            continue;
        };
        verified.insert(id.0.clone(), heartbeat);
        if seen.get(&id.0).is_some_and(|(pid, sequence)| {
            *pid == heartbeat.pid() && *sequence >= heartbeat.sequence()
        }) {
            continue;
        }
        pending.push((id, heartbeat));
    }
    let outcomes = journal.apply_observation_batch(pending.iter().map(|(id, heartbeat)| {
        Event::SlotHeartbeat {
            slot_id: id.clone(),
            generation: heartbeat.generation(),
            pid: heartbeat.pid(),
        }
    }))?;
    for ((id, heartbeat), outcome) in pending.into_iter().zip(outcomes) {
        if !outcome.rejected {
            seen.insert(id.0, (heartbeat.pid(), heartbeat.sequence()));
        }
    }
    Ok(verified)
}

fn maybe_spawn_slot(
    args: &ControllerArgs,
    journal: &Journal,
    children: &mut SlotChildren,
    startup_deadlines: &mut HashMap<String, Instant>,
    slot_id: &SlotId,
    generation: Generation,
) -> anyhow::Result<()> {
    if !args.spawn_slots {
        return Ok(());
    }
    if children.contains_key(&slot_id.0) {
        return Ok(());
    }
    let state = journal.materialized_state()?;
    if let Some(slot) = state.slots.iter().find(|slot| slot.slot_id == *slot_id)
        && let Some(pid) = slot.pid
        && prove::slot_process_liveness(pid, &args.state_dir, slot_id, generation)?
    {
        return Ok(());
    }
    let exe = crate::service::node_service_executable()?;
    let index = slot_index_from_id(slot_id);
    let child = Command::new(exe)
        .arg("slot")
        .arg("--state-dir")
        .arg(&args.state_dir)
        .arg("--scope")
        .arg(&args.scope)
        .arg("--slot-index")
        .arg(index.to_string())
        .arg("--generation")
        .arg(generation.0.to_string())
        .spawn()?;
    children.insert(slot_id.0.clone(), SlotChild { generation, child });
    startup_deadlines.insert(slot_id.0.clone(), Instant::now() + SLOT_HEARTBEAT_MAX_AGE);
    Ok(())
}

fn maybe_spawn_job(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
    job_id: &str,
    generation: u64,
    slot_id: Option<&SlotId>,
    recovery_only: bool,
) -> anyhow::Result<()> {
    let exe = crate::service::node_service_executable()?;
    maybe_spawn_job_with_executable(
        args,
        journal,
        jobs,
        job_id,
        generation,
        slot_id,
        recovery_only,
        &exe,
    )
}

fn maybe_spawn_job_with_executable(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
    job_id: &str,
    generation: u64,
    slot_id: Option<&SlotId>,
    recovery_only: bool,
    executable: &Path,
) -> anyhow::Result<()> {
    let generation = Generation(generation);
    let slot_id = slot_id
        .cloned()
        .or_else(|| {
            journal.materialized_state().ok().and_then(|state| {
                state
                    .jobs
                    .into_iter()
                    .find(|job| job.job_id.0 == job_id && job.generation == generation)
                    .map(|job| job.slot_id)
            })
        })
        .ok_or_else(|| {
            anyhow::anyhow!("cannot spawn worker {job_id} without generation-owned slot identity")
        })?;
    let key = job_id.to_owned();
    if jobs.contains_key(&key) {
        return Ok(());
    }
    let slot_index = slot_index_from_id(&slot_id);
    // Hold the marker lock through the OS spawn. Otherwise a second
    // controller can see a still-unpublished token before the first child is
    // visible in the process table, rotate it, and make both children unable
    // to publish ownership. The child blocks on the same lock until spawn
    // returns, then publishes its PID before opening journal or job state.
    let publish = |launch_token: &str| {
        let mut command = Command::new(executable);
        command
            .arg("job")
            .arg("--state-dir")
            .arg(&args.state_dir)
            .arg("--job-id")
            .arg(job_id)
            .arg("--generation")
            .arg(generation.0.to_string())
            .arg("--slot-index")
            .arg(slot_index.to_string())
            .arg("--slot-id")
            .arg(&slot_id.0)
            .arg("--scope")
            .arg(&args.scope)
            .arg("--launch-token")
            .arg(launch_token);
        if recovery_only {
            command.arg("--recovery-only");
        }
        // Leave the durable intent in place if spawn fails. A caller must
        // not infer that an interrupted platform spawn created no child.
        command.spawn().map_err(anyhow::Error::from)
    };
    // Recovery is a respawn path, so its marker must remain explicitly Dead
    // until the same lock publishes the new intent and creates the child.
    // Normal first-time waiter launch still permits an absent marker.
    let child = if recovery_only {
        cleanup::with_existing_dead_owned_pid_intent(
            &args.state_dir,
            job_id,
            generation.0,
            publish,
        )?
    } else {
        cleanup::with_dead_owned_pid_intent(&args.state_dir, job_id, generation.0, publish)?
    };
    let Some(child) = child else {
        return Ok(());
    };
    jobs.insert(job_id.to_owned(), child);
    Ok(())
}

fn reap<C: ProcessHandle>(children: &mut HashMap<String, C>) {
    let mut dead = Vec::new();
    for (id, child) in children.iter_mut() {
        match child.process_mut().try_wait() {
            Ok(Some(_)) => dead.push(id.clone()),
            Ok(None) => {}
            Err(error) => {
                eprintln!("process-reap error for child {id}; handle retained for retry: {error}");
            }
        }
    }
    for id in dead {
        children.remove(&id);
    }
}

fn reap_draining<C: ProcessHandle>(
    children: &mut HashMap<String, C>,
    kind: &str,
) -> anyhow::Result<()> {
    let mut dead = Vec::new();
    for (id, child) in children.iter_mut() {
        if child
            .process_mut()
            .try_wait()
            .map_err(|error| {
                anyhow::anyhow!(
                    "process-reap error while draining {kind} child {id}; handle retained: {error}"
                )
            })?
            .is_some()
        {
            dead.push(id.clone());
        }
    }
    for id in dead {
        children.remove(&id);
    }
    Ok(())
}

fn kill_draining<C: ProcessHandle>(
    children: &mut HashMap<String, C>,
    kind: &str,
) -> anyhow::Result<()> {
    for (id, child) in children.iter_mut() {
        child.process_mut().kill().map_err(|error| {
            anyhow::anyhow!(
                "process-reap escalation failed for {kind} child {id}; handle retained: {error}"
            )
        })?;
    }
    Ok(())
}

fn reap_draining_jobs(
    jobs: &mut HashMap<String, Child>,
    active_job_ids: &HashSet<String>,
    active_slot_ids: &HashSet<String>,
) -> anyhow::Result<()> {
    let mut dead = Vec::new();
    for (job_id, child) in jobs.iter_mut() {
        if !is_drainable_job(job_id, active_job_ids, active_slot_ids) {
            continue;
        }
        if child
            .try_wait()
            .map_err(|error| {
                anyhow::anyhow!(
                    "process-reap error while draining job child {job_id}; handle retained: {error}"
                )
            })?
            .is_some()
        {
            dead.push(job_id.clone());
        }
    }
    for job_id in dead {
        jobs.remove(&job_id);
    }
    Ok(())
}

fn kill_draining_jobs(
    jobs: &mut HashMap<String, Child>,
    active_job_ids: &HashSet<String>,
    active_slot_ids: &HashSet<String>,
) -> anyhow::Result<()> {
    for (job_id, child) in jobs
        .iter_mut()
        .filter(|(job_id, _)| is_drainable_job(job_id, active_job_ids, active_slot_ids))
    {
        child.kill().map_err(|error| {
            anyhow::anyhow!(
                "process-reap escalation failed for job child {job_id}; handle retained: {error}"
            )
        })?;
    }
    Ok(())
}

/// Daemon production path: spawn one OS process per configured slot instead
/// of a shared-process `JoinSet`.
///
/// `lifecycle` carries the operational-store handle plus the explicit
/// lifecycle ledger slug. That slug is the hostname-derived daemon slug and
/// differs from `scope` (the slot-id prefix); the daemon maps both at its
/// call site. `None` drains on the signal latch plus journal marker only.
pub async fn supervise_from_daemon(
    state_dir: PathBuf,
    scope: String,
    desired_ready: u32,
    once: bool,
    lifecycle: Option<ControllerLifecycle>,
) -> anyhow::Result<()> {
    run(ControllerArgs {
        state_dir,
        scope,
        desired_ready,
        once,
        spawn_slots: true,
        lifecycle,
    })
    .await
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
    use crate::args::DaemonArgs;
    use crate::node::exec::write_exec_config;
    use serde_json::json;
    use velnor_control::journal::FleetState;
    #[cfg(feature = "test-support")]
    use wiremock::matchers::{method, path};
    #[cfg(feature = "test-support")]
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn outbox_entry_parser_reserves_dot_prefixed_names_for_temporaries() {
        assert!(parse_outbox_entry_name(".job.7").is_err());
        assert!(parse_outbox_entry_name("..tmp-user.7").is_err());
        assert_eq!(
            parse_outbox_entry_name("..job.0.tmp-user.7.tmp-123-0").unwrap(),
            ("job.0.tmp-user".to_owned(), 7, true)
        );
        assert!(parse_outbox_entry_name("..malformed").is_err());
    }

    const TEST_RUN_SERVICE_URL: &str = "https://run.example/run";

    fn test_native_permit_lease(
        runner_request_id: &str,
    ) -> velnor_control::journal::NativePermitLease {
        velnor_control::journal::NativePermitLease {
            holder: crate::permit_guard::native_permit_holder("velnor", runner_request_id),
            ledger_path: "/tmp/test-permit-ledger.db".to_owned(),
            generation: 1,
        }
    }

    fn owned_job_events(
        job_id: &JobId,
        slot_id: &SlotId,
        generation: Generation,
        runner_request_id: &str,
        permit_lease: Option<velnor_control::journal::NativePermitLease>,
    ) -> Vec<Event> {
        let provisional_job_id = JobId(runner_request_id.to_owned());
        let intent = match permit_lease.as_ref() {
            Some(permit_lease) => Event::JobAcquisitionIntendedWithPermit {
                slot_id: slot_id.clone(),
                job_id: provisional_job_id.clone(),
                generation,
                message_id: format!("message-{runner_request_id}"),
                runner_request_id: runner_request_id.to_owned(),
                run_service_url: TEST_RUN_SERVICE_URL.to_owned(),
                intended_unix: 1_000,
                permit_lease: permit_lease.clone(),
            },
            None => Event::JobAcquisitionIntended {
                slot_id: slot_id.clone(),
                job_id: provisional_job_id.clone(),
                generation,
                message_id: format!("message-{runner_request_id}"),
                runner_request_id: Some(runner_request_id.to_owned()),
                run_service_url: TEST_RUN_SERVICE_URL.to_owned(),
                intended_unix: 1_000,
            },
        };
        vec![
            intent,
            Event::JobAcquisitionResolved {
                provisional_job_id,
                acquired_job_id: job_id.clone(),
                plan_id: "plan-1".to_owned(),
                generation,
                runner_request_id: Some(runner_request_id.to_owned()),
                permit_lease,
            },
            Event::JobOwned {
                job_id: job_id.clone(),
                slot_id: slot_id.clone(),
                attempt: 1,
                generation,
                worker: format!("worker-{}", job_id.0),
                accepted_unix: 1,
            },
        ]
    }

    fn with_journal_write_gate(
        path: &Path,
        mutation: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<()>,
    ) -> rusqlite::Result<()> {
        let mut connection = rusqlite::Connection::open(path)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute("INSERT INTO journal_write_gate (id) VALUES (1)", [])?;
        mutation(&transaction)?;
        transaction.execute("DELETE FROM journal_write_gate WHERE id = 1", [])?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(unix)]
    struct TokenProcess(Child);

    #[cfg(unix)]
    impl Drop for TokenProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(unix)]
    fn spawn_tokenized_owned_process(
        state_dir: &Path,
        isolation_id: &str,
        generation: Generation,
    ) -> TokenProcess {
        TokenProcess(spawn_tokenized_owned_child(
            state_dir,
            isolation_id,
            generation,
        ))
    }

    #[cfg(unix)]
    fn spawn_tokenized_owned_child(
        state_dir: &Path,
        isolation_id: &str,
        generation: Generation,
    ) -> Child {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let ready_path = state_dir.join(format!(".owned-process-ready-{token}"));
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("trap 'exit 0' TERM INT; printf ready > \"$2\"; while :; do sleep 1; done")
            .arg("velnor-controller-fixture")
            .arg(format!("--launch-token={token}"))
            .arg(&ready_path)
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ready_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !ready_path.exists() {
            panic!("token-bearing fixture process failed to start");
        }
        std::fs::remove_file(&ready_path).unwrap();
        cleanup::write_owned_pid_with_launch_token(
            state_dir,
            isolation_id,
            generation.0,
            child.id(),
            &token,
        )
        .unwrap();
        child
    }

    #[cfg(unix)]
    fn write_dead_published_owned_process(
        state_dir: &Path,
        isolation_id: &str,
        generation: Generation,
    ) {
        use std::io::Write as _;
        use std::os::unix::process::CommandExt;

        let token = uuid::Uuid::new_v4().simple().to_string();
        let ready_path = state_dir.join(format!(".dead-owned-ready-{token}"));
        let mut child = TokenProcess(
            Command::new("/bin/sh")
                .arg0(format!("--launch-token={token}"))
                .args([
                    "-c",
                    "set -eu; printf ready > \"$2\"; IFS= read -r release; [ \"$release\" = release ]",
                    "velnor-controller-dead-fixture",
                ])
                .arg(format!("--launch-token={token}"))
                .arg(&ready_path)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ready_path.exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "published-owner fixture exited before readiness"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "published-owner fixture did not reach readiness"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(child.0.try_wait().unwrap().is_none());

        cleanup::write_owned_pid_with_launch_token(
            state_dir,
            isolation_id,
            generation.0,
            child.0.id(),
            &token,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(cleanup::owned_path(state_dir, isolation_id, generation.0)).unwrap(),
            )
            .unwrap()["pid"]
                .as_u64(),
            Some(child.0.id().into()),
            "the child PID must be published before it is released"
        );
        let mut stdin = child.0.stdin.take().expect("child release pipe exists");
        stdin.write_all(b"release\n").unwrap();
        drop(stdin);
        assert!(child.0.wait().unwrap().success());
        assert_eq!(
            cleanup::owned_pid_liveness(state_dir, isolation_id, generation.0).unwrap(),
            cleanup::OwnedPidLiveness::Dead,
            "the fixture must retain a published marker for an exited process"
        );
    }

    fn write_in_flight_job_marker(
        slot_dir: &Path,
        job_id: &str,
        runner_request_id: &str,
        generation: Generation,
    ) -> PathBuf {
        write_in_flight_job_marker_with_url(
            slot_dir,
            job_id,
            runner_request_id,
            generation,
            TEST_RUN_SERVICE_URL,
        )
    }

    fn write_in_flight_job_marker_with_url(
        slot_dir: &Path,
        job_id: &str,
        runner_request_id: &str,
        generation: Generation,
        run_service_url: &str,
    ) -> PathBuf {
        write_in_flight_job_marker_with_url_and_permit(
            slot_dir,
            job_id,
            runner_request_id,
            generation,
            run_service_url,
            Some(test_native_permit_lease(runner_request_id)),
        )
    }

    fn write_in_flight_job_marker_with_url_and_permit(
        slot_dir: &Path,
        job_id: &str,
        runner_request_id: &str,
        generation: Generation,
        run_service_url: &str,
        permit_lease: Option<velnor_control::journal::NativePermitLease>,
    ) -> PathBuf {
        std::fs::create_dir_all(slot_dir).unwrap();
        let marker = crate::runner::test_in_flight_job_path(slot_dir);
        std::fs::write(
            &marker,
            serde_json::to_vec(&json!({
                "plan_id": "plan-1",
                "job_id": job_id,
                "run_service_url": run_service_url,
                "billing_owner_id": null,
                "runner_request_id": runner_request_id,
                "generation": generation.0,
                "permit_holder": permit_lease.as_ref().map_or("", |lease| &lease.holder),
                "permit_ledger": permit_lease.as_ref().map_or("", |lease| &lease.ledger_path),
                "permit_generation": permit_lease.as_ref().map(|lease| lease.generation)
            }))
            .unwrap(),
        )
        .unwrap();
        marker
    }

    fn prime_owned_job(
        journal: &mut Journal,
        job_id: &JobId,
        slot_id: &SlotId,
        generation: Generation,
        runner_request_id: &str,
        permit_lease: Option<velnor_control::journal::NativePermitLease>,
    ) {
        for event in owned_job_events(job_id, slot_id, generation, runner_request_id, permit_lease)
        {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        assert!(
            !journal
                .apply(Event::JobStarted {
                    job_id: job_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
    }

    fn prime_pending_completion(journal: &mut Journal) -> (JobId, Generation, String) {
        let slot_id = SlotId("velnor-1".to_owned());
        let job_id = JobId("job-1".to_owned());
        let runner_request_id = "request-1";
        let generation = Generation::INITIAL;
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ExecutorProven {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::SessionLive {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::RegistrationIntended {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::Registered {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ReadyAttempt {
                slot_id: slot_id.clone(),
                generation,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        prime_owned_job(
            journal,
            &job_id,
            &slot_id,
            generation,
            runner_request_id,
            None,
        );
        for event in [Event::JobTerminalResult {
            job_id: job_id.clone(),
            generation,
            conclusion: "success".to_owned(),
        }] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let payload_sha256 = velnor_control::journal::payload_checksum(b"payload");
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job_id.clone(),
                    generation,
                    payload_sha256: payload_sha256.clone(),
                })
                .unwrap()
                .rejected
        );
        (job_id, generation, payload_sha256)
    }

    fn prime_completion_publish_window(journal: &mut Journal) -> (JobId, Generation) {
        let slot_id = SlotId("velnor-1".to_owned());
        let job_id = JobId("job-1".to_owned());
        let generation = Generation::INITIAL;
        prime_owned_job(journal, &job_id, &slot_id, generation, "request-1", None);
        assert!(
            !journal
                .apply(Event::JobTerminalResult {
                    job_id: job_id.clone(),
                    generation,
                    conclusion: "success".to_owned(),
                })
                .unwrap()
                .rejected
        );
        (job_id, generation)
    }

    #[cfg(unix)]
    #[test]
    fn orphan_outbox_reconciliation_keeps_payload_for_live_slot_waiter() {
        let dir = metrics_test_dir("outbox-live-slot-waiter");
        let mut journal = ready_slot_journal(&dir);
        let (job_id, generation) = prime_completion_publish_window(&mut journal);
        cleanup::write_outbox(&dir, &job_id.0, generation.0, b"completion payload").unwrap();

        // A ready waiter remains registered under `wait-{slot}` after the
        // journal adopts the acquired job id. The completion writer publishes
        // these bytes before CompletionIntended, so the journal has no outbox
        // row yet while the waiter still owns the job.
        cleanup::write_owned_pid(&dir, &job_id.0, generation.0, 99_999_999).unwrap();
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, &job_id.0, generation.0).unwrap(),
            cleanup::OwnedPidLiveness::Dead
        );
        let child = spawn_tokenized_owned_child(&dir, "wait-velnor-1", generation);
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, "wait-velnor-1", generation.0).unwrap(),
            cleanup::OwnedPidLiveness::Live
        );
        let mut jobs = HashMap::from([("wait-velnor-1".to_owned(), child)]);

        reconcile_orphaned_outboxes(&controller_test_args(dir.clone()), &journal, &jobs).unwrap();

        assert_eq!(
            cleanup::read_outbox(&dir, &job_id.0, generation.0).unwrap(),
            b"completion payload"
        );
        let mut child = jobs.remove("wait-velnor-1").unwrap();
        let _ = child.kill();
        let _ = child.wait();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn orphan_outbox_reconciliation_keeps_payload_when_active_owner_is_unknown() {
        let dir = metrics_test_dir("outbox-unknown-active-owner");
        let mut journal = ready_slot_journal(&dir);
        let (job_id, generation) = prime_completion_publish_window(&mut journal);
        cleanup::write_outbox(&dir, &job_id.0, generation.0, b"completion payload").unwrap();

        // Both ownership markers are absent. Because this active journal row
        // can still own the completion write, absence alone cannot authorize
        // deleting a payload before CompletionIntended is recorded.
        reconcile_orphaned_outboxes(
            &controller_test_args(dir.clone()),
            &journal,
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(
            cleanup::read_outbox(&dir, &job_id.0, generation.0).unwrap(),
            b"completion payload"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn orphan_outbox_reconciliation_keeps_payload_when_one_owner_is_absent() {
        let dir = metrics_test_dir("outbox-dead-worker-absent-waiter");
        let mut journal = ready_slot_journal(&dir);
        let (job_id, generation) = prime_completion_publish_window(&mut journal);
        cleanup::write_outbox(&dir, &job_id.0, generation.0, b"completion payload").unwrap();
        cleanup::write_owned_pid(&dir, &job_id.0, generation.0, 99_999_999).unwrap();
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, &job_id.0, generation.0).unwrap(),
            cleanup::OwnedPidLiveness::Dead
        );
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, "wait-velnor-1", generation.0).unwrap(),
            cleanup::OwnedPidLiveness::Absent
        );

        reconcile_orphaned_outboxes(
            &controller_test_args(dir.clone()),
            &journal,
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(
            cleanup::read_outbox(&dir, &job_id.0, generation.0).unwrap(),
            b"completion payload"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn orphan_outbox_reconciliation_removes_payload_after_waiter_death_without_job_marker() {
        let dir = metrics_test_dir("outbox-dead-waiter-no-job-marker");
        let mut journal = ready_slot_journal(&dir);
        let (job_id, generation) = prime_completion_publish_window(&mut journal);
        let payload_path = cleanup::outbox_path(&dir, &job_id.0, generation.0);
        cleanup::write_outbox(&dir, &job_id.0, generation.0, b"completion payload").unwrap();
        cleanup::write_owned_pid(&dir, "wait-velnor-1", generation.0, 99_999_999).unwrap();
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, &job_id.0, generation.0).unwrap(),
            cleanup::OwnedPidLiveness::Absent
        );
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, "wait-velnor-1", generation.0).unwrap(),
            cleanup::OwnedPidLiveness::Dead
        );

        reconcile_orphaned_outboxes(
            &controller_test_args(dir.clone()),
            &journal,
            &HashMap::new(),
        )
        .unwrap();

        assert!(std::fs::symlink_metadata(&payload_path).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn orphan_outbox_reconciliation_removes_payload_for_abandoned_row() {
        let dir = metrics_test_dir("outbox-abandoned-row");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (job_id, generation, payload_sha256) = prime_pending_completion(&mut journal);
        let payload_path = cleanup::outbox_path(&dir, &job_id.0, generation.0);
        cleanup::write_outbox(&dir, &job_id.0, generation.0, b"payload").unwrap();

        let abandoned = journal
            .apply(Event::CompletionPayloadLost {
                job_id: job_id.clone(),
                generation,
                payload_sha256,
                reason: "simulated crash before outbox deletion".to_owned(),
            })
            .unwrap();
        assert!(!abandoned.rejected);
        assert!(journal
            .unresolvable_completions()
            .unwrap()
            .iter()
            .any(|completion| completion.job_id == job_id && completion.generation == generation));

        // The reducer committed abandonment, then the process crashed before
        // its DeleteOutbox side effect. Reconciliation must finish that work.
        reconcile_orphaned_outboxes(
            &controller_test_args(dir.clone()),
            &journal,
            &HashMap::new(),
        )
        .unwrap();

        assert!(std::fs::symlink_metadata(&payload_path).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn waiter_owned_job_requires_dead_waiter_and_dead_existing_job_marker() {
        use cleanup::OwnedPidLiveness::{Absent, Dead, Live, UnpublishedIntentLive};

        assert!(waiter_owned_job_markers_prove_death(Absent, Dead));
        assert!(waiter_owned_job_markers_prove_death(Dead, Dead));
        assert!(!waiter_owned_job_markers_prove_death(Dead, Absent));
        assert!(!waiter_owned_job_markers_prove_death(Absent, Absent));
        assert!(!waiter_owned_job_markers_prove_death(Dead, Live));
        assert!(!waiter_owned_job_markers_prove_death(Live, Dead));
        assert!(!waiter_owned_job_markers_prove_death(
            UnpublishedIntentLive,
            Dead
        ));
    }

    #[test]
    fn ready_waiter_respawn_accepts_absent_or_dead_owner_marker_after_identity_proof() {
        use cleanup::OwnedPidLiveness::{Absent, Dead, Live, UnpublishedIntentLive};

        assert!(waiter_marker_allows_spawn(Dead));
        assert!(waiter_marker_allows_spawn(Absent));
        assert!(!waiter_marker_allows_spawn(Live));
        assert!(!waiter_marker_allows_spawn(UnpublishedIntentLive));
    }

    fn prime_provisional_assignment(journal: &mut Journal) -> (JobId, SlotId, Generation) {
        let slot_id = SlotId("velnor-1".to_owned());
        let request_id = JobId("request-1".to_owned());
        let acquired_id = JobId("job-1".to_owned());
        let generation = Generation::INITIAL;
        let permit_lease = test_native_permit_lease(&request_id.0);
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ExecutorProven {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::SessionLive {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::RegistrationIntended {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::Registered {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ReadyAttempt {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::JobAcquisitionIntendedWithPermit {
                slot_id: slot_id.clone(),
                job_id: request_id.clone(),
                generation,
                message_id: "broker-message-1".into(),
                runner_request_id: request_id.0.clone(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: epoch_now(),
                permit_lease: permit_lease.clone(),
            },
            Event::JobAcquisitionResolved {
                provisional_job_id: request_id,
                acquired_job_id: acquired_id.clone(),
                plan_id: "plan-1".to_owned(),
                generation,
                runner_request_id: Some("request-1".to_owned()),
                permit_lease: Some(permit_lease),
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        (acquired_id, slot_id, generation)
    }

    fn prime_second_provisional_assignment(journal: &mut Journal) {
        let slot_id = SlotId("velnor-2".to_owned());
        let request_id = JobId("request-2".to_owned());
        let acquired_id = JobId("job-2".to_owned());
        let generation = Generation::INITIAL;
        let permit_lease = test_native_permit_lease(&request_id.0);
        for event in [
            Event::DesiredCapacity { ready: 2 },
            Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ExecutorProven {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::SessionLive {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::RegistrationIntended {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::Registered {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ReadyAttempt {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::JobAcquisitionIntendedWithPermit {
                slot_id: slot_id.clone(),
                job_id: request_id.clone(),
                generation,
                message_id: "broker-message-2".into(),
                runner_request_id: request_id.0.clone(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: epoch_now(),
                permit_lease: permit_lease.clone(),
            },
            Event::JobAcquisitionResolved {
                provisional_job_id: request_id,
                acquired_job_id: acquired_id,
                plan_id: "plan-2".to_owned(),
                generation,
                runner_request_id: Some("request-2".to_owned()),
                permit_lease: Some(permit_lease),
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
    }

    fn controller_test_args(state_dir: PathBuf) -> ControllerArgs {
        ControllerArgs {
            state_dir,
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        }
    }

    #[test]
    fn malformed_provisional_waiter_pid_fails_closed_without_spawning() {
        let dir = metrics_test_dir("provisional-malformed-pid");
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (_, slot_id, generation) = prime_provisional_assignment(&mut journal);
        let waiter_id = format!("wait-{}", slot_id.0);
        std::fs::write(
            cleanup::owned_path(&dir, &waiter_id, generation.0),
            b"not a process record",
        )
        .unwrap();
        let args = controller_test_args(dir.clone());
        let mut jobs = HashMap::new();

        let result = spawn_provisional_recovery_waiters_with(
            &args,
            &journal,
            &mut jobs,
            |_, _, _, _, _, _, _| panic!("unreadable PID evidence must prevent spawn"),
        );

        assert!(result.is_err());
        assert!(jobs.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn provisional_recovery_does_not_spawn_when_waiter_marker_is_absent() {
        let dir = metrics_test_dir("provisional-absent-waiter");
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (job_id, slot_id, generation) = prime_provisional_assignment(&mut journal);
        let waiter_id = format!("wait-{}", slot_id.0);
        let args = controller_test_args(dir.clone());
        let mut jobs = HashMap::new();
        let mut spawn_calls = 0;

        spawn_provisional_recovery_waiters_with(
            &args,
            &journal,
            &mut jobs,
            |_, _, _, _, _, _, _| {
                spawn_calls += 1;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(spawn_calls, 0, "absence is not proof of owner death");
        assert!(jobs.is_empty());
        assert!(!cleanup::owned_path(&dir, &waiter_id, generation.0).exists());
        assert!(!cleanup::owned_path(&dir, &job_id.0, generation.0).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn provisional_recovery_starts_one_fleet_pass_only_after_all_waiters_are_dead() {
        let dir = metrics_test_dir("provisional-single-fleet-pass");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (_, first_slot, generation) = prime_provisional_assignment(&mut journal);
        prime_second_provisional_assignment(&mut journal);
        let first_waiter = format!("wait-{}", first_slot.0);
        let second_waiter = "wait-velnor-2";
        cleanup::write_owned_pid(&dir, &first_waiter, generation.0, 99_999_999).unwrap();
        let args = controller_test_args(dir.clone());
        let mut jobs = HashMap::new();
        let mut spawn_calls = 0;
        let mut spawn = |_: &ControllerArgs,
                         _: &Journal,
                         _: &mut HashMap<String, Child>,
                         job_id: &str,
                         _: u64,
                         slot_id: Option<&SlotId>,
                         recovery_only: bool| {
            spawn_calls += 1;
            assert!(job_id == "wait-velnor-1" || job_id == "wait-velnor-2");
            assert_eq!(slot_id.map(|slot| slot.0.as_str()), Some(&job_id[5..]));
            assert!(recovery_only);
            Ok(())
        };

        spawn_provisional_recovery_waiters_with(&args, &journal, &mut jobs, &mut spawn).unwrap();
        assert_eq!(
            spawn_calls, 0,
            "an absent marker is unknown fleet ownership"
        );

        cleanup::write_owned_pid(&dir, second_waiter, generation.0, 99_999_999).unwrap();
        spawn_provisional_recovery_waiters_with(&args, &journal, &mut jobs, &mut spawn).unwrap();
        assert_eq!(spawn_calls, 1, "one child performs the fleet probe pass");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unknown_provisional_job_owner_marker_blocks_dead_waiter_respawn() {
        let dir = metrics_test_dir("provisional-unknown-job-owner");
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (job_id, slot_id, generation) = prime_provisional_assignment(&mut journal);
        let waiter_id = format!("wait-{}", slot_id.0);
        cleanup::write_owned_pid(&dir, &waiter_id, generation.0, 99_999_999).unwrap();
        std::fs::write(
            cleanup::owned_path(&dir, &job_id.0, generation.0),
            b"not a process record",
        )
        .unwrap();
        let args = controller_test_args(dir.clone());
        let mut jobs = HashMap::new();

        let result = spawn_provisional_recovery_waiters_with(
            &args,
            &journal,
            &mut jobs,
            |_, _, _, _, _, _, _| panic!("unknown job-owner evidence must prevent spawn"),
        );

        assert!(result.is_err());
        assert!(jobs.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn pre_handshake_decimal_pid_marker_fails_closed_after_upgrade() {
        let dir = metrics_test_dir("legacy-decimal-pid");
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (_, slot_id, generation) = prime_provisional_assignment(&mut journal);
        let waiter_id = format!("wait-{}", slot_id.0);
        std::fs::write(
            cleanup::owned_path(&dir, &waiter_id, generation.0),
            b"12345\n",
        )
        .unwrap();
        let args = controller_test_args(dir.clone());
        let mut jobs = HashMap::new();

        let result = spawn_provisional_recovery_waiters_with(
            &args,
            &journal,
            &mut jobs,
            |_, _, _, _, _, _, _| panic!("legacy PID text cannot prove the old owner is dead"),
        );

        assert!(result.is_err());
        assert!(jobs.is_empty());
        assert_eq!(
            std::fs::read(cleanup::owned_path(&dir, &waiter_id, generation.0)).unwrap(),
            b"12345\n",
            "the new reader must retain an unprovable pre-handshake marker"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn planless_pre_migration_provisional_row_stays_fenced_without_probe_child() {
        let dir = metrics_test_dir("provisional-legacy-fenced");
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let slot_id = SlotId("velnor-1".to_owned());
        let request_id = JobId("legacy-request".to_owned());
        let generation = Generation::INITIAL;
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ExecutorProven {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::SessionLive {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::RegistrationIntended {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::Registered {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ReadyAttempt {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::JobAcquisitionIntended {
                slot_id,
                job_id: request_id.clone(),
                generation,
                message_id: "legacy-message".to_owned(),
                runner_request_id: Some(request_id.0.clone()),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: epoch_now(),
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let args = controller_test_args(dir.clone());
        let mut jobs = HashMap::new();

        spawn_provisional_recovery_waiters_with(
            &args,
            &journal,
            &mut jobs,
            |_, _, _, _, _, _, _| panic!("legacy planless row has no safe probe authority"),
        )
        .unwrap();

        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert!(state.jobs[0].provisional);
        assert!(state.jobs[0].plan_id.is_empty());
        assert!(state.jobs[0].permit_lease.is_none());
        assert!(jobs.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn failed_provisional_spawn_keeps_intent_and_retry_rotates_token() {
        use std::os::unix::fs::PermissionsExt;

        let dir = metrics_test_dir("provisional-spawn-retry");
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (_, slot_id, generation) = prime_provisional_assignment(&mut journal);
        cleanup::write_owned_pid(
            &dir,
            &format!("wait-{}", slot_id.0),
            generation.0,
            99_999_999,
        )
        .unwrap();
        let args = controller_test_args(dir.clone());
        let mut jobs = HashMap::new();
        let missing_executable = dir.join("missing-node-service");

        let first = spawn_provisional_recovery_waiters_with(
            &args,
            &journal,
            &mut jobs,
            |args, journal, jobs, job_id, generation, slot_id, recovery_only| {
                maybe_spawn_job_with_executable(
                    args,
                    journal,
                    jobs,
                    job_id,
                    generation,
                    slot_id,
                    recovery_only,
                    &missing_executable,
                )
            },
        );

        assert!(first.is_err());
        assert!(jobs.is_empty());
        let waiter_id = format!("wait-{}", slot_id.0);
        let marker_path = cleanup::owned_path(&dir, &waiter_id, generation.0);
        let first_marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
        let first_token = first_marker["launch_token"]
            .as_str()
            .expect("failed spawn preserves its exact launch intent")
            .to_owned();
        assert_eq!(first_marker["pid"].as_u64(), Some(0));

        let executable = dir.join("retry-node-service.sh");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
set -eu
[ "$#" -eq 16 ]
[ "$1" = job ]
[ "$2" = --state-dir ]
[ -d "$3" ]
[ "$4" = --job-id ]
[ "$5" = wait-velnor-1 ]
[ "$6" = --generation ]
[ "$7" = 1 ]
[ "$8" = --slot-index ]
[ "$9" = 1 ]
[ "${10}" = --slot-id ]
[ "${11}" = velnor-1 ]
[ "${12}" = --scope ]
[ "${13}" = velnor ]
[ "${14}" = --launch-token ]
[ -n "${15}" ]
[ "${16}" = --recovery-only ]
printf '%s\n' "${15}" > "$3/spawned-token.txt"
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();

        spawn_provisional_recovery_waiters_with(
            &args,
            &journal,
            &mut jobs,
            |args, journal, jobs, job_id, generation, slot_id, recovery_only| {
                maybe_spawn_job_with_executable(
                    args,
                    journal,
                    jobs,
                    job_id,
                    generation,
                    slot_id,
                    recovery_only,
                    &executable,
                )
            },
        )
        .unwrap();

        let child = jobs
            .get_mut(&waiter_id)
            .expect("retry starts after the old intent is proven orphaned");
        assert!(child.wait().unwrap().success());
        let second_token = std::fs::read_to_string(dir.join("spawned-token.txt")).unwrap();
        let second_marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
        assert_ne!(first_token, second_token.trim());
        assert_eq!(
            second_marker["launch_token"].as_str(),
            Some(second_token.trim())
        );
        assert_eq!(second_marker["pid"].as_u64(), Some(0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn restart_spawns_only_a_token_marked_recovery_waiter_for_provisional_assignment() {
        use std::os::unix::fs::PermissionsExt;

        let dir = metrics_test_dir("provisional-recovery-waiter");
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (job_id, slot_id, generation) = prime_provisional_assignment(&mut journal);
        let waiter_id = format!("wait-{}", slot_id.0);
        cleanup::write_owned_pid(&dir, &job_id.0, generation.0, 99_999_999).unwrap();
        let args = controller_test_args(dir.clone());
        let executable = dir.join("fake-node-service.sh");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
set -eu
[ "$#" -eq 16 ]
[ "$1" = job ]
[ "$2" = --state-dir ]
[ -d "$3" ]
[ "$4" = --job-id ]
[ "$5" = wait-velnor-1 ]
[ "$6" = --generation ]
[ "$7" = 1 ]
[ "$8" = --slot-index ]
[ "$9" = 1 ]
[ "${10}" = --slot-id ]
[ "${11}" = velnor-1 ]
[ "${12}" = --scope ]
[ "${13}" = velnor ]
[ "${14}" = --launch-token ]
[ -n "${15}" ]
[ "${16}" = --recovery-only ]
grep -F "\"launch_token\":\"${15}\"" "$3/owned/${5}.${7}" >/dev/null
printf '%s\n' "${15}" > "$3/recovery-child.txt"
printf 'ready\n' > "$3/recovery-child-ready.txt"
while [ ! -f "$3/recovery-child-release.txt" ]; do sleep 1; done
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let mut jobs = HashMap::new();
        let waiter_marker = cleanup::owned_path(&dir, &waiter_id, generation.0);
        let mut spawn_fake = |args: &ControllerArgs,
                              journal: &Journal,
                              jobs: &mut HashMap<String, Child>,
                              job_id: &str,
                              generation: u64,
                              slot_id: Option<&SlotId>,
                              recovery_only: bool| {
            maybe_spawn_job_with_executable(
                args,
                journal,
                jobs,
                job_id,
                generation,
                slot_id,
                recovery_only,
                &executable,
            )
        };

        // Absence after restart is unknown ownership, so even a usable fake
        // node service must not be launched.
        spawn_provisional_recovery_waiters_with(&args, &journal, &mut jobs, &mut spawn_fake)
            .unwrap();
        assert!(
            jobs.is_empty(),
            "an absent waiter marker cannot authorize respawn"
        );
        assert!(!waiter_marker.exists());

        // A malformed waiter marker is unknown evidence and fails closed.
        std::fs::write(&waiter_marker, b"not a process record").unwrap();
        assert!(spawn_provisional_recovery_waiters_with(
            &args,
            &journal,
            &mut jobs,
            &mut spawn_fake,
        )
        .is_err());
        assert!(jobs.is_empty());
        std::fs::remove_file(&waiter_marker).unwrap();

        // Simulate marker loss after the preflight proves Dead but before the
        // OS spawn. The strict spawn path rechecks under the ownership lock.
        cleanup::write_owned_pid(&dir, &waiter_id, generation.0, 99_999_999).unwrap();
        let mut remove_marker_then_spawn =
            |args: &ControllerArgs,
             journal: &Journal,
             jobs: &mut HashMap<String, Child>,
             job_id: &str,
             generation: u64,
             slot_id: Option<&SlotId>,
             recovery_only: bool| {
                std::fs::remove_file(&waiter_marker)?;
                maybe_spawn_job_with_executable(
                    args,
                    journal,
                    jobs,
                    job_id,
                    generation,
                    slot_id,
                    recovery_only,
                    &executable,
                )
            };
        spawn_provisional_recovery_waiters_with(
            &args,
            &journal,
            &mut jobs,
            &mut remove_marker_then_spawn,
        )
        .unwrap();
        assert!(
            jobs.is_empty(),
            "marker loss after preflight must still block spawn"
        );
        assert!(!waiter_marker.exists());

        // A present, live job-owner marker also blocks waiter recovery even
        // when the waiter marker itself is explicitly dead.
        cleanup::write_owned_pid(&dir, &waiter_id, generation.0, 99_999_999).unwrap();
        std::fs::remove_file(cleanup::owned_path(&dir, &job_id.0, generation.0)).unwrap();
        let mut live_job_owner = spawn_tokenized_owned_process(&dir, &job_id.0, generation);
        spawn_provisional_recovery_waiters_with(&args, &journal, &mut jobs, &mut spawn_fake)
            .unwrap();
        assert!(jobs.is_empty(), "a live job owner must prevent respawn");
        live_job_owner.0.kill().unwrap();
        let _ = live_job_owner.0.wait().unwrap();
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, &job_id.0, generation.0).unwrap(),
            cleanup::OwnedPidLiveness::Dead
        );

        spawn_provisional_recovery_waiters_with(&args, &journal, &mut jobs, &mut spawn_fake)
            .unwrap();

        let child = jobs.get_mut(&waiter_id).expect("recovery waiter spawned");
        let ready_path = dir.join("recovery-child-ready.txt");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ready_path.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "recovery child exited before the readiness handshake"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "recovery child did not complete the readiness handshake"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(child.try_wait().unwrap().is_none());
        assert_eq!(
            cleanup::replace_dead_owned_pid_with_intent(&dir, &waiter_id, generation.0).unwrap(),
            None,
            "an unresolved intent with a live token-bearing child blocks duplicate spawn"
        );
        std::fs::write(dir.join("recovery-child-release.txt"), "release").unwrap();
        let status = child.wait().unwrap();
        assert!(status.success());
        let args_token = std::fs::read_to_string(dir.join("recovery-child.txt")).unwrap();
        let marker: serde_json::Value = serde_json::from_slice(
            &std::fs::read(cleanup::owned_path(&dir, &waiter_id, generation.0)).unwrap(),
        )
        .unwrap();
        assert_eq!(marker["launch_token"].as_str(), Some(args_token.trim()));
        assert_eq!(marker["pid"].as_u64(), Some(0));
        assert_eq!(
            cleanup::replace_dead_owned_pid_with_intent(&dir, &waiter_id, generation.0)
                .unwrap()
                .is_some(),
            true,
            "a restarted controller can replace a token intent after its child exits"
        );
        assert_eq!(job_id.0, "job-1");
        assert_eq!(
            journal.materialized_state().unwrap().jobs[0].job_id,
            job_id,
            "the recovery child must not rewrite the provisional row before its probe"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_completion_payload_is_recorded_and_releases_exact_slot() {
        let dir = metrics_test_dir("missing-completion-payload");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (job_id, generation, checksum) = prime_pending_completion(&mut journal);
        let args = controller_test_args(dir.clone());

        preserve_outbox(&args, &mut journal, &job_id, generation, &checksum).unwrap();

        let state = journal.materialized_state().unwrap();
        assert!(state.jobs.is_empty());
        assert!(state.outbox.is_empty());
        assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
        assert!(!journal
            .has_remote_terminal_ack(&job_id, generation)
            .unwrap());
        assert_eq!(
            journal.unresolvable_completions().unwrap()[0].reason,
            "completion outbox payload is missing"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("advertised-capacity")).unwrap(),
            "1"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn checksum_mismatch_is_recorded_and_corrupt_payload_is_deleted() {
        let dir = metrics_test_dir("checksum-completion-payload");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (job_id, generation, checksum) = prime_pending_completion(&mut journal);
        cleanup::write_outbox(&dir, &job_id.0, generation.0, b"corrupt").unwrap();
        let args = controller_test_args(dir.clone());

        preserve_outbox(&args, &mut journal, &job_id, generation, &checksum).unwrap();

        assert!(!cleanup::outbox_path(&dir, &job_id.0, generation.0).exists());
        assert!(journal.pending_outbox().unwrap().is_empty());
        assert_eq!(
            journal.unresolvable_completions().unwrap()[0].reason,
            "completion outbox checksum mismatch"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_completion_payload_stays_a_hard_error() {
        let dir = metrics_test_dir("symlink-completion-payload");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (job_id, generation, checksum) = prime_pending_completion(&mut journal);
        let target = dir.join("target");
        std::fs::write(&target, b"payload").unwrap();
        std::fs::create_dir_all(dir.join("outbox")).unwrap();
        std::os::unix::fs::symlink(&target, cleanup::outbox_path(&dir, &job_id.0, generation.0))
            .unwrap();
        let args = controller_test_args(dir.clone());

        assert!(preserve_outbox(&args, &mut journal, &job_id, generation, &checksum).is_err());
        assert!(journal.pending_outbox().unwrap()[0].is_pending());
        assert!(journal.unresolvable_completions().unwrap().is_empty());
        assert!(cleanup::outbox_path(&dir, &job_id.0, generation.0).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn dummy_exec(url: &str) -> DaemonArgs {
        serde_json::from_value(json!({
            "url": url,
            "name": "velnor",
            "labels": ["velnor"],
            "target_mvp_labels": false,
            "target_mvp_arm_label": false,
            "replace": false,
            "dry_run_registration": false,
            "slots": 1,
            "once": false,
            "complete_noop": false,
            "execute_scripts": false,
            "dry_run_jobs": false,
            "docker_image": "img",
                                    "trust_scope": "trusted",
            "emergency_reserve_bytes": 0,
            "job_peak_bytes": 0,
            "node_action_image": "img",
            "skip_preflight": false,
            "require_docker_socket": false
        }))
        .unwrap()
    }

    fn ready_runner_config(exec: &DaemonArgs, slot_index: usize) -> config::StoredRunnerConfig {
        config::StoredRunnerConfig {
            settings: config::RunnerSettings {
                github_url: exec.url.clone().unwrap_or_default(),
                server_url: None,
                server_url_v2: Some("https://run.example/v2".to_owned()),
                pool_id: Some(exec.pool_id.unwrap_or(1)),
                pool_name: exec.pool_name.clone(),
                agent_id: Some(71),
                agent_name: crate::runner::compose_github_runner_name(
                    &crate::runner::github_runner_host_slug(),
                    exec.name.as_deref().unwrap_or("local"),
                    slot_index - 1,
                ),
                labels: crate::runner::normalize_labels(
                    exec.labels.clone(),
                    exec.target_mvp_labels,
                    exec.target_mvp_arm_label,
                ),
                use_v2_flow: true,
                ephemeral: true,
                disable_update: true,
            },
            credentials: Some(config::StoredCredentials {
                scheme: config::CredentialScheme::OAuthAccessToken,
                data: json!({"token": "test-token"}),
            }),
        }
    }

    fn write_ready_runner_config(
        state_dir: &Path,
        exec: &DaemonArgs,
        slot_index: usize,
        slot_count: usize,
    ) -> PathBuf {
        let slot_dir = crate::runner::daemon_slot_config_dir(state_dir, slot_index, slot_count);
        config::save(&slot_dir, &ready_runner_config(exec, slot_index)).unwrap();
        slot_dir
    }

    fn metrics_test_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-controller-metrics-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();
        dir
    }

    #[test]
    fn metrics_counts_waiters_from_wait_keys_once() {
        let job_ids = [
            "job-1".to_owned(),
            "wait-velnor-1".to_owned(),
            "job-2".to_owned(),
        ];
        let (job_processes, waiter_processes) = job_process_counts(job_ids.iter());
        assert_eq!((job_processes, waiter_processes), (2, 1));

        let dir = metrics_test_dir("active-jobs");
        publish_controller_metrics(
            &dir,
            1,
            2,
            job_processes,
            waiter_processes,
            7,
            ReconcileTelemetry::default(),
        )
        .unwrap();

        let metrics: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("controller-metrics.json")).unwrap())
                .unwrap();
        assert_eq!(metrics["job_processes"], json!(2));
        assert_eq!(metrics["waiter_processes"], json!(1));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn metrics_publisher_recovers_from_a_poisoned_lock() {
        let dir = metrics_test_dir("poison-recovery");
        let mut publisher = MetricsPublisher::start(&dir);
        // Poison the shared lock the way any panicking holder would. Poison
        // is sticky, so one poisoning exercises every lock site below.
        let shared = publisher.state.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = shared.lock().unwrap();
            panic!("poison the metrics lock");
        }));
        assert!(publisher.state.is_poisoned());
        // Each of these recovered its guard via `into_inner` instead of
        // crashing the controller on the poisoned lock.
        publisher.update(&HashMap::new(), &HashMap::new(), 5);
        publisher.stop_and_publish().await.unwrap();
        assert!(dir.join("controller-metrics.json").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn metrics_publish_cpu_uses_explicit_controller_aggregate() {
        let dir = metrics_test_dir("controller-cpu");
        let mut checksum = 0_u64;
        for value in 0..2_000_000_u64 {
            checksum = checksum.wrapping_add(value.rotate_left(7));
        }
        std::hint::black_box(checksum);
        publish_controller_metrics(&dir, 1, 0, 0, 0, 1, ReconcileTelemetry::default()).unwrap();
        let metrics: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("controller-metrics.json")).unwrap())
                .unwrap();
        let controller_cpu = metrics["cpu"]["controller"]["user_us"]
            .as_u64()
            .unwrap()
            .saturating_add(metrics["cpu"]["controller"]["system_us"].as_u64().unwrap());
        assert!(
            controller_cpu > 0,
            "aggregate controller CPU must be measured"
        );
        assert_eq!(
            metrics["cpu"]["phases"]["child_supervision"],
            json!({
                "user_us": 0,
                "system_us": 0
            })
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn orphan_recovery_timeout_is_deferred_without_error() {
        let result = defer_remote_recovery_on_timeout(
            Duration::from_millis(1),
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok::<_, anyhow::Error>(true)
            },
            "test orphan recovery",
        )
        .await
        .unwrap();
        assert_eq!(result, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn drain_preserves_active_jobs_without_waiting_for_them() {
        let dir = metrics_test_dir("drain-active-job");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ExecutorProven {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::SessionLive {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::RegistrationIntended {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::Registered {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ReadyAttempt {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        prime_owned_job(
            &mut journal,
            &JobId("job-1".to_owned()),
            &SlotId("velnor-1".to_owned()),
            Generation::INITIAL,
            "request-1",
            None,
        );

        let child = || Command::new("sleep").arg("5").spawn().unwrap();
        let mut slots = HashMap::from([(
            String::from("velnor-1"),
            SlotChild {
                generation: Generation::INITIAL,
                child: child(),
            },
        )]);
        let mut jobs = HashMap::from([
            (String::from("job-1"), child()),
            (String::from("wait-velnor-1"), child()),
            (String::from("stale-job"), child()),
        ]);
        let state = journal.materialized_state().unwrap();
        assert!(child_owns_slot(
            &dir,
            &state,
            &jobs,
            &SlotId("velnor-1".to_owned()),
            Generation::INITIAL,
        )
        .unwrap());
        assert_eq!(
            job_child_keys_for_slot(&jobs, &state, &SlotId("velnor-1".to_owned())),
            vec!["job-1".to_owned(), "wait-velnor-1".to_owned()]
        );

        let started = Instant::now();
        drain_children(&journal, &mut slots, &mut jobs)
            .await
            .unwrap();
        let active_preserved = jobs.get_mut("job-1").unwrap().try_wait().unwrap().is_none();
        let waiter_preserved = jobs
            .get_mut("wait-velnor-1")
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none();
        let active_handles_remain =
            jobs.len() == 2 && jobs.contains_key("job-1") && jobs.contains_key("wait-velnor-1");
        for child in jobs.values_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }

        assert!(started.elapsed() < CONTROLLER_CHILD_DRAIN_TIMEOUT);
        assert!(slots.is_empty());
        assert!(active_preserved);
        assert!(waiter_preserved);
        assert!(active_handles_remain);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn drain_test_lifecycle(
        dir: &std::path::Path,
        slug: &str,
        desired: velnor_control::ports::MutationKind,
    ) -> ActiveLifecycle {
        use velnor_control::ports::{MutationPort, MutationRequest};
        let store = Arc::new(Store::open(dir.join("state.db")).unwrap());
        let service = LifecycleService::with_store_for_instance(Arc::clone(&store), slug).unwrap();
        service
            .mutate(MutationRequest {
                kind: desired,
                target: slug.to_owned(),
                reason: "test".to_owned(),
                idempotency_key: format!("drain-test-{slug}"),
                expected_version: None,
                scale_to: None,
            })
            .unwrap();
        ActiveLifecycle {
            service,
            store,
            instance: slug.to_owned(),
        }
    }

    #[test]
    fn should_drain_combines_latch_journal_and_fresh_desired() {
        use velnor_control::ports::MutationKind;
        let dir = metrics_test_dir("should-drain");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();

        assert!(!should_drain(false, &journal, None));
        assert!(should_drain(true, &journal, None));

        journal.set_drain(2).unwrap();
        assert!(should_drain(false, &journal, None));

        let draining = drain_test_lifecycle(&dir, "draining", MutationKind::Drain);
        let ready = drain_test_lifecycle(&dir, "ready", MutationKind::Uncordon);
        let fresh_journal = Journal::open(dir.join("other.db")).unwrap();
        assert!(should_drain(false, &fresh_journal, Some(&draining)));
        assert!(!should_drain(false, &fresh_journal, Some(&ready)));

        // An unreadable ledger row is not a drain order.
        let store = Arc::new(Store::open(dir.join("state.db")).unwrap());
        let missing = ActiveLifecycle {
            service: LifecycleService::with_store_for_instance(Arc::clone(&store), "ghost")
                .unwrap(),
            store,
            instance: "ghost".to_owned(),
        };
        assert!(!should_drain(false, &fresh_journal, Some(&missing)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lifecycle_cordon_and_resume_reconcile_the_durable_admission_fence() {
        use velnor_control::ports::{MutationKind, MutationPort, MutationRequest};

        let dir = metrics_test_dir("lifecycle-admission");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        prime_registered_slot(&mut journal);
        assert!(
            !journal
                .apply(Event::ReadyAttempt {
                    slot_id: SlotId("velnor-1".to_owned()),
                    generation: Generation::INITIAL,
                })
                .unwrap()
                .rejected
        );
        let lifecycle = drain_test_lifecycle(&dir, "primary", MutationKind::Cordon);

        reconcile_lifecycle_admission(&mut journal, Some(&lifecycle)).unwrap();
        let fenced = journal.materialized_state().unwrap();
        assert!(fenced.admission_blocked);
        assert_eq!(fenced.advertised_capacity(), 0);
        let observed = lifecycle
            .store
            .lifecycle_instance("primary")
            .unwrap()
            .unwrap();
        assert_eq!(observed.desired_state, "cordoned");
        assert_eq!(observed.observed_state, "cordoned");

        // A repeated Cordon is a new accepted operation even though the
        // projection is already cordoned. Reconciliation must observe it so
        // the operation cannot remain accepted forever.
        let repeated = lifecycle
            .service
            .mutate(MutationRequest {
                kind: MutationKind::Cordon,
                target: "primary".to_owned(),
                reason: "repeat cordon test".to_owned(),
                idempotency_key: "repeat-cordon-admission-test".to_owned(),
                expected_version: Some(observed.resource_version),
                scale_to: None,
            })
            .unwrap();
        reconcile_lifecycle_admission(&mut journal, Some(&lifecycle)).unwrap();
        let repeated_phase: String = rusqlite::Connection::open(dir.join("state.db"))
            .unwrap()
            .query_row(
                "SELECT phase FROM lifecycle_operations WHERE operation_id = ?1",
                [repeated.operation_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repeated_phase, "completed");

        let fresh = lifecycle.service.desired_fresh("primary").unwrap();
        lifecycle
            .service
            .mutate(MutationRequest {
                kind: MutationKind::Resume,
                target: "primary".to_owned(),
                reason: "resume test".to_owned(),
                idempotency_key: "resume-admission-test".to_owned(),
                expected_version: Some(fresh.version),
                scale_to: None,
            })
            .unwrap();
        reconcile_lifecycle_admission(&mut journal, Some(&lifecycle)).unwrap();
        let resumed = journal.materialized_state().unwrap();
        assert!(!resumed.admission_blocked);
        assert_eq!(resumed.advertised_capacity(), 1);
        let observed = lifecycle
            .store
            .lifecycle_instance("primary")
            .unwrap()
            .unwrap();
        assert_eq!(observed.desired_state, "ready");
        assert_eq!(observed.observed_state, "ready");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lifecycle_reconcile_does_not_clear_a_corrupt_admission_marker() {
        use velnor_control::ports::MutationKind;

        let dir = metrics_test_dir("lifecycle-admission-corrupt");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        journal.set_admission_blocked(4).unwrap();
        let lifecycle = drain_test_lifecycle(&dir, "primary", MutationKind::Uncordon);
        with_journal_write_gate(&dir.join("journal.db"), |transaction| {
            transaction.execute(
                "UPDATE meta SET value = 'corrupt' WHERE key = 'admission'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let error = reconcile_lifecycle_admission(&mut journal, Some(&lifecycle))
            .expect_err("corrupt admission marker must fail closed");
        assert!(error.to_string().contains("read lifecycle admission state"));
        let marker: String = rusqlite::Connection::open(dir.join("journal.db"))
            .unwrap()
            .query_row(
                "SELECT value FROM meta WHERE key = 'admission'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker, "corrupt");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn drain_preserves_waiter_handle_when_its_slot_owns_active_work() {
        let active_jobs = HashSet::from(["job-1".to_owned()]);
        let active_slots = HashSet::from(["velnor-1".to_owned()]);
        assert!(!is_drainable_job(
            "wait-velnor-1",
            &active_jobs,
            &active_slots
        ));
        assert!(is_drainable_job(
            "wait-velnor-2",
            &active_jobs,
            &active_slots
        ));
        assert!(!is_drainable_job("job-1", &active_jobs, &active_slots));
        assert!(is_drainable_job("job-2", &active_jobs, &active_slots));
    }

    #[test]
    fn drain_edge_latches_marker_and_records_observed_once() {
        use velnor_control::ports::MutationKind;
        let dir = metrics_test_dir("drain-edge");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let lifecycle = drain_test_lifecycle(&dir, "primary", MutationKind::Drain);

        drain_edge(&mut journal, Some(&lifecycle));
        let state = journal.materialized_state().unwrap();
        assert!(state.drain_active);
        assert_eq!(state.drain_version, 2);
        let row = lifecycle
            .store
            .lifecycle_instance("primary")
            .unwrap()
            .unwrap();
        assert_eq!(row.desired_state, "draining");
        assert_eq!(row.observed_state, "draining");
        assert_eq!(row.resource_version, 3);

        // Edge-triggered: a second pass writes neither the marker nor the
        // observed projection again.
        drain_edge(&mut journal, Some(&lifecycle));
        let row = lifecycle
            .store
            .lifecycle_instance("primary")
            .unwrap()
            .unwrap();
        assert_eq!(row.resource_version, 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn drain_edge_preserves_an_existing_marker_and_needs_no_ledger() {
        let dir = metrics_test_dir("drain-edge-static");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        journal.set_drain(9).unwrap();
        drain_edge(&mut journal, None);
        let state = journal.materialized_state().unwrap();
        assert!(state.drain_active);
        assert_eq!(state.drain_version, 9);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn drain_edge_write_failure_still_drains_children() {
        let dir = metrics_test_dir("drain-edge-write-fails");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        // Fault injection: fail every `meta` write while reads keep
        // working (`set_drain` writes `meta`; `materialized_state` and
        // the `drain_children` job scan only read it).
        rusqlite::Connection::open(dir.join("journal.db"))
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER drain_edge_test_block_meta_writes BEFORE INSERT ON meta
                 BEGIN SELECT RAISE(ABORT, 'test write failure'); END;",
            )
            .unwrap();
        assert!(journal.set_drain(2).is_err());

        // The edge logs forensics and returns instead of propagating, so
        // the caller still runs `drain_children` next — the same order as
        // the loop top. No marker was latched: the write failed.
        drain_edge(&mut journal, None);
        assert!(!journal.materialized_state().unwrap().drain_active);

        let child = || Command::new("sleep").arg("30").spawn().unwrap();
        let mut slots = HashMap::from([(
            String::from("velnor-1"),
            SlotChild {
                generation: Generation::INITIAL,
                child: child(),
            },
        )]);
        let mut jobs = HashMap::new();
        drain_children(&journal, &mut slots, &mut jobs)
            .await
            .unwrap();
        assert!(slots.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn drain_edge_tolerates_an_unreadable_journal() {
        let dir = metrics_test_dir("drain-edge-unreadable");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        rusqlite::Connection::open(dir.join("journal.db"))
            .unwrap()
            .execute_batch("DROP TABLE meta")
            .unwrap();
        assert!(journal.materialized_state().is_err());
        // Both the read and the latch write fail: forensics, then return.
        drain_edge(&mut journal, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn reconcile_skips_new_permits_while_journal_drain_is_latched() {
        // Flag on for this process. Never unset: sibling tests create no
        // drain markers, so the leaked flag cannot change their outcome.
        unsafe { std::env::set_var("VELNOR_JOURNAL_DRAIN", "1") };
        let dir = metrics_test_dir("reconcile-drain-gate");
        std::fs::write(
            dir.join("execution.toml"),
            "[execution]\nbackend = \"docker\"\n",
        )
        .unwrap();
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        journal.apply(Event::DesiredCapacity { ready: 1 }).unwrap();
        journal.set_drain(2).unwrap();
        let server = HealthServer::bind(&dir).unwrap();
        let mut metrics = MetricsPublisher::start(&dir);
        let mut pacing = GithubPacing::default();
        let mut slots = HashMap::new();
        let mut jobs = HashMap::new();
        let mut heartbeats = HashMap::new();
        let mut startup_deadlines = HashMap::new();
        // Skip the remote and outbox scans: this cycle only proves the
        // permit loop observes the latched marker.
        let mut last_registration_reconcile = Instant::now();
        let mut last_outbox_reconcile = Instant::now();

        reconcile_once(
            &args,
            &mut journal,
            &server,
            &mut slots,
            &mut jobs,
            &mut heartbeats,
            &mut startup_deadlines,
            &mut last_registration_reconcile,
            &mut last_outbox_reconcile,
            &mut pacing,
            &metrics,
            None,
        )
        .await
        .unwrap();
        metrics.stop_and_publish().await.unwrap();

        let state = journal.materialized_state().unwrap();
        assert!(state.drain_active);
        assert!(
            state.slots.iter().all(|slot| !slot.permit_held),
            "no permit may be reserved while draining: {:?}",
            state.slots
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn active_lifecycle_bind_rejects_a_noncanonical_slug() {
        let dir = metrics_test_dir("drain-bind");
        let store = Arc::new(Store::open(dir.join("state.db")).unwrap());
        assert!(ActiveLifecycle::bind(&ControllerLifecycle {
            store: Arc::clone(&store),
            instance: "primary".to_owned(),
        })
        .is_some());
        assert!(ActiveLifecycle::bind(&ControllerLifecycle {
            store,
            instance: "not a slug!!".to_owned(),
        })
        .is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn metrics_publisher_synchronously_writes_the_final_snapshot() {
        let dir = metrics_test_dir("final-snapshot");
        let mut publisher = MetricsPublisher::start(&dir);
        publisher.reconcile_completed(&HashMap::new(), &HashMap::new(), 42);

        publisher.stop_and_publish().await.unwrap();

        let metrics: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("controller-metrics.json")).unwrap())
                .unwrap();
        assert_eq!(metrics["reconcile"]["last_wall_duration_ms"], json!(42));
        assert_eq!(metrics["reconcile"]["completed_cycles"], json!(1));
        assert_eq!(metrics["reconcile"]["wall_duration_ms_total"], json!(42));
        assert!(metrics["cpu"]["controller_at_reconcile_boundary"]["user_us"].is_number());
        assert_eq!(
            metrics["sequence"].as_u64().unwrap(),
            publisher.state.lock().unwrap().sequence
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    fn reserved_slot() -> SlotRecord {
        SlotRecord {
            slot_id: SlotId("velnor-1".to_owned()),
            generation: Generation::INITIAL,
            phase: SlotPhase2::Provisioning,
            permit_held: true,
            routing_valid: false,
            session_live: false,
            executor_proven: false,
            registered: false,
            pid: None,
            heartbeat_unix: 0,
        }
    }

    #[test]
    fn orphan_recovery_uses_and_validates_persisted_slot_layout() {
        let dir = metrics_test_dir("orphan-layout");
        let mut state = FleetState::default();
        state.slots.push(reserved_slot());
        let mut exec = dummy_exec("https://github.com/tailrocks/fixture");
        exec.slots = 2;

        assert_eq!(
            recovery_slot_config_dir(&dir, &exec, &state, &SlotId("velnor-1".to_owned())).unwrap(),
            dir.join("slots").join("slot-1")
        );

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("runner.json"), b"peer single-slot state").unwrap();
        let error = recovery_slot_config_dir(&dir, &exec, &state, &SlotId("velnor-1".to_owned()))
            .unwrap_err();
        assert!(error.to_string().contains("incompatible config layouts"));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn orphan_recovery_rejects_zero_slot_execution_config() {
        let dir = metrics_test_dir("orphan-zero-slots");
        let mut state = FleetState::default();
        state.slots.push(reserved_slot());
        let mut exec = dummy_exec("https://github.com/tailrocks/fixture");
        exec.slots = 0;

        let error = recovery_slot_config_dir(&dir, &exec, &state, &SlotId("velnor-1".to_owned()))
            .unwrap_err();
        assert!(error.to_string().contains("zero slots"));
    }

    #[test]
    fn stable_live_slot_suppresses_duplicate_permit() {
        let slot = reserved_slot();

        assert!(!permit_needs_reconciliation(
            Some(&slot),
            Generation::INITIAL,
            false,
            true,
        ));
    }

    #[test]
    fn missing_or_dead_slot_reissues_permit_for_respawn() {
        let slot = reserved_slot();

        assert!(permit_needs_reconciliation(
            Some(&slot),
            Generation::INITIAL,
            true,
            false,
        ));
        assert!(permit_needs_reconciliation(
            None,
            Generation::INITIAL,
            true,
            false,
        ));
    }

    #[test]
    fn fenced_slot_reconciliation_advances_generation() {
        let dir = metrics_test_dir("fenced-recovery");
        let mut slot = reserved_slot();
        let state = FleetState::default();
        let children = HashMap::new();
        assert_eq!(
            fenced_slot_recovery_generation(Some(&slot), &dir, &dir, &state, &children).unwrap(),
            None
        );

        slot.phase = SlotPhase2::Fenced;
        assert_eq!(
            fenced_slot_recovery_generation(Some(&slot), &dir, &dir, &state, &children).unwrap(),
            Some(Generation(slot.generation.0 + 1))
        );
        std::fs::write(
            crate::runner::test_in_flight_job_path(&dir),
            serde_json::to_vec(&json!({
                "plan_id": "plan-1",
                "job_id": "job-1",
                "run_service_url": "https://run.example/run",
                "billing_owner_id": null,
                "runner_request_id": "request-1",
                "generation": slot.generation.0,
                "permit_holder": "native/request-1",
                "permit_ledger": "/unused/permit-ledger.db",
                "permit_generation": 1
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            fenced_slot_recovery_generation(Some(&slot), &dir, &dir, &state, &children).unwrap(),
            None,
            "the old marker fences generation rotation until terminal cleanup removes it"
        );
        std::fs::remove_file(crate::runner::test_in_flight_job_path(&dir)).unwrap();
        let mut legacy_marker = serde_json::json!({
            "plan_id": "plan-legacy",
            "job_id": "job-legacy",
            "run_service_url": "https://run.example/run",
            "billing_owner_id": null,
            "runner_request_id": "request-legacy",
            "permit_holder": "native/request-legacy",
            "permit_ledger": "/unused/permit-ledger.db",
            "permit_generation": 1
        });
        legacy_marker.as_object_mut().unwrap().remove("generation");
        std::fs::write(
            crate::runner::test_in_flight_job_path(&dir),
            serde_json::to_vec(&legacy_marker).unwrap(),
        )
        .unwrap();
        assert_eq!(
            fenced_slot_recovery_generation(Some(&slot), &dir, &dir, &state, &children).unwrap(),
            None,
            "even a legacy marker with unknown generation must fence advancement"
        );
        std::fs::remove_file(crate::runner::test_in_flight_job_path(&dir)).unwrap();
        assert_eq!(
            fenced_slot_recovery_generation(Some(&slot), &dir, &dir, &state, &children).unwrap(),
            Some(Generation(slot.generation.0 + 1))
        );
        assert_eq!(
            fenced_slot_recovery_generation(None, &dir, &dir, &state, &children).unwrap(),
            None
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_live_waiter_is_an_acting_slot_and_blocks_fenced_recovery() {
        let dir = metrics_test_dir("live-waiter-in-memory");
        let mut slot = reserved_slot();
        slot.phase = SlotPhase2::Ready;
        let state = FleetState::default();
        let waiter = Command::new("sleep").arg("5").spawn().unwrap();
        let mut jobs = HashMap::from([(String::from("wait-velnor-1"), waiter)]);

        assert!(slot_is_acting(&dir, &state, &jobs, &slot.slot_id, slot.generation).unwrap());
        slot.phase = SlotPhase2::Fenced;
        assert_eq!(
            fenced_slot_recovery_generation(Some(&slot), &dir, &dir, &state, &jobs).unwrap(),
            None,
            "a live waiter must not be skipped: it is why generation recovery deadlocks"
        );

        let _ = jobs.get_mut("wait-velnor-1").unwrap().kill();
        let _ = jobs.get_mut("wait-velnor-1").unwrap().wait();
        jobs.remove("wait-velnor-1");
        assert!(!slot_is_acting(&dir, &state, &jobs, &slot.slot_id, slot.generation).unwrap());
        assert_eq!(
            fenced_slot_recovery_generation(Some(&slot), &dir, &dir, &state, &jobs).unwrap(),
            Some(Generation(slot.generation.0 + 1))
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn persisted_live_waiter_pid_blocks_acting_slot_after_controller_restart() {
        let dir = metrics_test_dir("persisted-waiter-restart");
        let mut slot = reserved_slot();
        let state = FleetState::default();
        let jobs = HashMap::<String, Child>::new();
        let mut waiter = spawn_tokenized_owned_process(&dir, "wait-velnor-1", slot.generation);

        assert!(
            slot_is_acting(&dir, &state, &jobs, &slot.slot_id, slot.generation).unwrap(),
            "persisted waiter pid must count as acting with an empty jobs map"
        );
        slot.phase = SlotPhase2::Fenced;
        assert_eq!(
            fenced_slot_recovery_generation(Some(&slot), &dir, &dir, &state, &jobs).unwrap(),
            None,
            "fencing must stay blocked while the persisted waiter pid is live"
        );

        waiter.0.kill().unwrap();
        let _ = waiter.0.wait();
        assert!(
            !slot_is_acting(&dir, &state, &jobs, &slot.slot_id, slot.generation).unwrap(),
            "dead persisted waiter must no longer block acting"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fencing_terminates_the_waiter_so_generation_can_advance() {
        let dir = metrics_test_dir("fence-waiter");
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: true,
            lifecycle: None,
        };
        let mut slot = reserved_slot();
        slot.phase = SlotPhase2::Fenced;
        let state = FleetState::default();
        let mut slots = HashMap::new();
        let mut jobs = HashMap::from([(
            String::from("wait-velnor-1"),
            Command::new("sleep").arg("30").spawn().unwrap(),
        )]);
        let waiter_pid = jobs.get("wait-velnor-1").unwrap().id();

        terminate_fenced_slot_actor(&args, &mut slots, &mut jobs, &state, &slot.slot_id, &slot)
            .await
            .unwrap();

        assert!(
            !jobs.contains_key("wait-velnor-1"),
            "fenced recovery must reap the waiter that would block the next generation"
        );
        assert!(
            !prove::pid_is_alive(waiter_pid),
            "the waiter process itself must be gone, not just dropped from the map"
        );
        assert_eq!(
            fenced_slot_recovery_generation(Some(&slot), &dir, &dir, &state, &jobs).unwrap(),
            Some(Generation(slot.generation.0 + 1))
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fencing_leaves_unrelated_pid_untouched() {
        let dir = metrics_test_dir("fence-unrelated-pid");
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: true,
            lifecycle: None,
        };
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let mut slot = reserved_slot();
        slot.phase = SlotPhase2::Fenced;
        slot.pid = Some(child.id());
        let state = FleetState::default();
        let mut slots = HashMap::new();
        let mut jobs = HashMap::new();

        terminate_fenced_slot_actor(&args, &mut slots, &mut jobs, &state, &slot.slot_id, &slot)
            .await
            .unwrap();

        assert!(
            prove::pid_is_alive(child.id()),
            "a process without the exact slot argv must not receive the fence signal"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pending_outbox_blocks_only_its_slot_and_generation() {
        let mut state = FleetState::default();
        state.outbox.push(velnor_control::journal::OutboxRecord {
            job_id: JobId("job-1".into()),
            slot_id: SlotId("velnor-1".into()),
            generation: Generation::INITIAL,
            payload_sha256: "checksum".into(),
            intended: true,
            send_started: false,
            remote_acked: false,
            created_unix: 0,
            attempts: 0,
            deadline_unix: 0,
            permanent: false,
            abandoned: false,
        });

        assert!(slot_has_admission_block(
            &state,
            &SlotId("velnor-1".into()),
            Generation::INITIAL,
        ));
        assert!(!slot_has_admission_block(
            &state,
            &SlotId("velnor-2".into()),
            Generation::INITIAL,
        ));
        assert!(!slot_has_admission_block(
            &state,
            &SlotId("velnor-1".into()),
            Generation(2),
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_waiter_marker_is_not_hidden_by_stale_job_marker() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-orphan-reclaim-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();

        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ExecutorProven {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::SessionLive {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::RegistrationIntended {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::Registered {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ReadyAttempt {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        prime_owned_job(
            &mut journal,
            &JobId("job-1".to_owned()),
            &SlotId("velnor-1".to_owned()),
            Generation::INITIAL,
            "request-1",
            None,
        );
        write_dead_published_owned_process(&dir, "job-1", Generation::INITIAL);
        let mut live_waiter =
            spawn_tokenized_owned_process(&dir, "wait-velnor-1", Generation::INITIAL);

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            |args| panic!("docker must not be invoked on this recovery path: {args:?}"),
        )
        .await
        .unwrap();

        let state = journal.load_state().unwrap();
        let job = state
            .jobs
            .iter()
            .find(|job| job.job_id == JobId("job-1".to_owned()))
            .unwrap();
        assert_eq!(job.phase, JobPhase2::Running);

        let _ = live_waiter.0.kill();
        let _ = live_waiter.0.wait();
        std::fs::remove_dir_all(dir).ok();
    }

    /// Journal with one Running job whose worker and waiter pids are both
    /// dead, plus an exec config whose two-slot layout maps `velnor-1` to
    /// `slots/slot-1`. With `backend`, the state dir also selects that
    /// execution backend; without it no `execution.toml` exists.
    #[cfg(unix)]
    fn stale_running_job_fixture(label: &str, backend: Option<&str>) -> (PathBuf, Journal) {
        let dir = std::env::temp_dir().join(format!(
            "velnor-orphan-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();
        if let Some(backend) = backend {
            std::fs::write(
                dir.join("execution.toml"),
                format!("[execution]\nbackend = \"{backend}\"\n"),
            )
            .unwrap();
        }
        write_exec_config(&dir, &dummy_exec("https://github.com/tailrocks/fixture"), 2).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let slot_id = SlotId("velnor-1".to_owned());
        let job_id = JobId("job-1".to_owned());
        let generation = Generation::INITIAL;
        let runner_request_id = "request-1";
        let permit_lease = test_native_permit_lease(runner_request_id);
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ExecutorProven {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::SessionLive {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::RegistrationIntended {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::Registered {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ReadyAttempt {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        prime_owned_job(
            &mut journal,
            &job_id,
            &slot_id,
            generation,
            runner_request_id,
            Some(permit_lease),
        );
        write_dead_published_owned_process(&dir, "job-1", generation);
        write_dead_published_owned_process(&dir, "wait-velnor-1", generation);
        (dir, journal)
    }

    fn remote_ack_stale_running_job(journal: &mut Journal) {
        let job_id = JobId("job-1".to_owned());
        let generation = Generation::INITIAL;
        for event in [
            Event::JobTerminalResult {
                job_id: job_id.clone(),
                generation,
                conclusion: "success".to_owned(),
            },
            Event::CompletionIntended {
                job_id: job_id.clone(),
                generation,
                payload_sha256: velnor_control::journal::payload_checksum(b"payload"),
            },
            Event::CompletionSendStarted {
                job_id: job_id.clone(),
                generation,
            },
            Event::RemoteAcked { job_id, generation },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
    }

    fn write_remote_acked_in_flight_marker(slot_dir: &Path) -> PathBuf {
        write_in_flight_job_marker(slot_dir, "job-1", "request-1", Generation::INITIAL)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_acked_marker_still_defers_to_live_worker_or_waiter() {
        for live_owner in ["job-1", "wait-velnor-1"] {
            let (dir, mut journal) = stale_running_job_fixture("acked-live-owner", Some("docker"));
            remote_ack_stale_running_job(&mut journal);
            let slot_dir = dir.join("slots").join("slot-1");
            let marker = write_remote_acked_in_flight_marker(&slot_dir);
            std::fs::remove_file(cleanup::owned_path(&dir, live_owner, Generation::INITIAL.0))
                .unwrap();
            let mut live_process =
                spawn_tokenized_owned_process(&dir, live_owner, Generation::INITIAL);

            reclaim_orphaned_jobs(
                &controller_test_args(dir.clone()),
                &mut journal,
                tokio::time::Instant::now() + Duration::from_secs(15),
                true,
                |docker_args| panic!("live owner must defer teardown: {docker_args:?}"),
            )
            .await
            .unwrap();

            assert!(marker.exists(), "live {live_owner} must keep the marker");
            let _ = live_process.0.kill();
            let _ = live_process.0.wait();
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_acked_marker_requires_worker_death_evidence() {
        let (dir, mut journal) = stale_running_job_fixture("acked-no-worker-pid", Some("docker"));
        remote_ack_stale_running_job(&mut journal);
        let slot_dir = dir.join("slots").join("slot-1");
        let marker = write_remote_acked_in_flight_marker(&slot_dir);
        std::fs::remove_file(cleanup::owned_path(&dir, "job-1", Generation::INITIAL.0)).unwrap();
        std::fs::remove_file(cleanup::owned_path(
            &dir,
            "wait-velnor-1",
            Generation::INITIAL.0,
        ))
        .unwrap();

        let error = reclaim_orphaned_jobs(
            &controller_test_args(dir.clone()),
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
            |docker_args| panic!("missing worker proof must defer teardown: {docker_args:?}"),
        )
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("has unknown worker or waiter ownership"),
            "{error:#}"
        );
        assert!(marker.exists(), "missing owner evidence retains the marker");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_remote_acked_marker_without_generation_fails_closed() {
        let (dir, mut journal) =
            stale_running_job_fixture("acked-legacy-generation", Some("docker"));
        remote_ack_stale_running_job(&mut journal);
        let slot_dir = dir.join("slots").join("slot-1");
        let marker = write_remote_acked_in_flight_marker(&slot_dir);
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        record.as_object_mut().unwrap().remove("generation");
        std::fs::write(&marker, serde_json::to_vec(&record).unwrap()).unwrap();

        let error = reclaim_orphaned_jobs(
            &controller_test_args(dir.clone()),
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
            |docker_args| panic!("unknown marker generation must defer teardown: {docker_args:?}"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("has no recorded generation"));
        assert!(marker.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_acked_marker_uses_its_recorded_generation_after_slot_rotation() {
        let (dir, mut journal) = stale_running_job_fixture("acked-old-generation", Some("docker"));
        remote_ack_stale_running_job(&mut journal);
        let slot_id = SlotId("velnor-1".to_owned());
        assert!(
            !journal
                .apply(Event::PermitReserved {
                    slot_id,
                    generation: Generation::INITIAL.next(),
                })
                .unwrap()
                .rejected
        );
        let slot_dir = dir.join("slots").join("slot-1");
        let marker = write_remote_acked_in_flight_marker(&slot_dir);
        cleanup::write_outbox(&dir, "job-1", Generation::INITIAL.0, b"old-generation").unwrap();
        cleanup::write_outbox(
            &dir,
            "job-1",
            Generation::INITIAL.next().0,
            b"new-generation",
        )
        .unwrap();
        let teardown_marker = marker.clone();
        let cleanup_marker_path = marker.clone();
        let cleanup_state_dir = dir.clone();

        reclaim_orphaned_jobs_with_marker_cleanup(
            &controller_test_args(dir.clone()),
            &mut journal,
            &HashMap::new(),
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
            move |_docker_args| {
                assert!(
                    teardown_marker.exists(),
                    "teardown must precede marker cleanup"
                );
                Ok(String::new())
            },
            move |slot_dir, record| {
                assert_eq!(record.job_id(), "job-1");
                assert_eq!(record.generation(), Some(Generation::INITIAL.0));
                assert!(cleanup_marker_path.exists());
                cleanup::remove_outbox(
                    &cleanup_state_dir,
                    record.job_id(),
                    record.generation().unwrap(),
                )?;
                std::fs::remove_file(crate::runner::test_in_flight_job_path(slot_dir))?;
                Ok(true)
            },
        )
        .await
        .unwrap();

        assert!(!marker.exists());
        assert!(
            !cleanup::outbox_path(&dir, "job-1", Generation::INITIAL.0).exists(),
            "marker cleanup removes only the outbox for its recorded generation"
        );
        assert_eq!(
            std::fs::read(cleanup::outbox_path(
                &dir,
                "job-1",
                Generation::INITIAL.next().0
            ))
            .unwrap(),
            b"new-generation",
            "a newer generation's outbox must be untouched"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn marker_only_cleanup_rejects_same_job_generation_with_replaced_permit_identity() {
        let (dir, mut journal) = stale_running_job_fixture("acked-replaced-marker", Some("docker"));
        remote_ack_stale_running_job(&mut journal);
        let slot_dir = dir.join("slots").join("slot-1");
        let marker = write_remote_acked_in_flight_marker(&slot_dir);
        let marker_for_teardown = marker.clone();
        let marker_for_assertion = marker.clone();

        let error = reclaim_orphaned_jobs_with_marker_cleanup(
            &controller_test_args(dir.clone()),
            &mut journal,
            &HashMap::new(),
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
            move |_docker_args| {
                let mut record: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&marker_for_teardown)?)?;
                let object = record.as_object_mut().unwrap();
                object.insert("runner_request_id".into(), json!("replacement-request"));
                object.insert("permit_holder".into(), json!("native/replacement-request"));
                object.insert("permit_generation".into(), json!(2));
                std::fs::write(&marker_for_teardown, serde_json::to_vec(&record)?)?;
                Ok(String::new())
            },
            |slot_dir, record| {
                assert_eq!(record.job_id(), "job-1");
                assert_eq!(record.generation(), Some(Generation::INITIAL.0));
                crate::runner::cleanup_recorded_in_flight_job_for_record(slot_dir, record)
            },
        )
        .await
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("marker changed before terminal cleanup"));
        let replacement: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker_for_assertion).unwrap()).unwrap();
        assert_eq!(replacement["runner_request_id"], "replacement-request");
        assert_eq!(replacement["permit_generation"], 2);
        assert!(
            marker_for_assertion.exists(),
            "replacement marker must remain for its owner"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_acked_dead_owner_teardown_precedes_marker_cleanup() {
        let (dir, mut journal) = stale_running_job_fixture("acked-dead-owner", Some("docker"));
        remote_ack_stale_running_job(&mut journal);
        let slot_dir = dir.join("slots").join("slot-1");
        let marker = write_remote_acked_in_flight_marker(&slot_dir);
        let teardown_marker = marker.clone();
        let cleanup_marker_path = marker.clone();
        let docker_calls = Arc::new(Mutex::new(0usize));
        let recorded_docker_calls = docker_calls.clone();

        reclaim_orphaned_jobs_with_marker_cleanup(
            &controller_test_args(dir.clone()),
            &mut journal,
            &HashMap::new(),
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
            move |_docker_args| {
                *recorded_docker_calls.lock().unwrap() += 1;
                assert!(teardown_marker.exists(), "marker cleared before teardown");
                Ok(String::new())
            },
            move |slot_dir, record| {
                assert_eq!(record.job_id(), "job-1");
                assert_eq!(record.generation(), Some(Generation::INITIAL.0));
                assert!(
                    cleanup_marker_path.exists(),
                    "marker absent before local cleanup"
                );
                std::fs::remove_file(crate::runner::test_in_flight_job_path(slot_dir))?;
                Ok(true)
            },
        )
        .await
        .unwrap();

        assert!(
            *docker_calls.lock().unwrap() > 0,
            "dead owner containers must be inspected"
        );
        assert!(!marker.exists(), "successful cleanup removes the marker");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn locally_abandoned_completion_cleans_marker_without_remote_replay() {
        let (dir, mut journal) = stale_running_job_fixture("payload-lost-marker", Some("docker"));
        let job_id = JobId("job-1".to_owned());
        let generation = Generation::INITIAL;
        let checksum = velnor_control::journal::payload_checksum(b"payload");
        for event in [
            Event::JobTerminalResult {
                job_id: job_id.clone(),
                generation,
                conclusion: "success".to_owned(),
            },
            Event::CompletionIntended {
                job_id: job_id.clone(),
                generation,
                payload_sha256: checksum.clone(),
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        preserve_outbox(
            &controller_test_args(dir.clone()),
            &mut journal,
            &job_id,
            generation,
            &checksum,
        )
        .unwrap();
        assert!(journal.materialized_state().unwrap().jobs.is_empty());
        assert!(!journal
            .has_remote_terminal_ack(&job_id, generation)
            .unwrap());
        assert_eq!(
            journal.unresolvable_completions().unwrap()[0].job_id,
            job_id,
            "payload-loss evidence must be the exact durable terminal proof"
        );

        let slot_dir = dir.join("slots").join("slot-1");
        let marker = write_remote_acked_in_flight_marker(&slot_dir);
        let marker_for_teardown = marker.clone();
        let marker_for_cleanup = marker.clone();
        let cleanup_called = Arc::new(AtomicBool::new(false));
        let cleanup_called_by_callback = cleanup_called.clone();
        reclaim_orphaned_jobs_with_marker_cleanup(
            &controller_test_args(dir.clone()),
            &mut journal,
            &HashMap::new(),
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
            move |_docker_args| {
                assert!(marker_for_teardown.exists());
                Ok(String::new())
            },
            move |slot_dir, record| {
                assert_eq!(record.job_id(), "job-1");
                assert_eq!(record.generation(), Some(generation.0));
                assert!(marker_for_cleanup.exists(), "teardown must precede cleanup");
                cleanup_called_by_callback.store(true, Ordering::Relaxed);
                std::fs::remove_file(crate::runner::test_in_flight_job_path(slot_dir))?;
                Ok(true)
            },
        )
        .await
        .unwrap();

        assert!(cleanup_called.load(Ordering::Relaxed));
        assert!(!marker.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_recovery_force_removes_containers_before_restoring_slot() {
        let (dir, mut journal) = stale_running_job_fixture("teardown", Some("docker"));
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let job_container = crate::github_adapter::job_container_name_for_id("job-1");
        let listing = format!(
            "job-cid\t{job_container}\t{job_container}\trunning\n\
             guest-cid\tguest-sidecar\t{job_container}\texited\n"
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            move |docker_args: &[String]| {
                recorded.lock().unwrap().push(docker_args.to_vec());
                if docker_args.first().is_some_and(|command| command == "ps") {
                    return Ok(listing.clone());
                }
                Ok(String::new())
            },
        )
        .await
        .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(
            calls[0],
            crate::docker_lease::list_owned_containers_state_args(&job_container)
        );
        assert_eq!(
            calls[1],
            crate::docker_lease::force_remove_container_args(&[
                "guest-cid".to_string(),
                "job-cid".to_string()
            ])
        );
        assert_eq!(calls.len(), 2);

        let state = journal.load_state().unwrap();
        assert!(
            state
                .jobs
                .iter()
                .all(|job| job.job_id != JobId("job-1".to_owned())),
            "JobWorkerLost must remove the job row"
        );
        let slot = state
            .slots
            .iter()
            .find(|slot| slot.slot_id == SlotId("velnor-1".to_owned()))
            .unwrap();
        assert_eq!(slot.phase, SlotPhase2::Ready);

        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_running_job_recovers_with_dead_waiter_and_absent_job_marker() {
        let (dir, journal) = stale_running_job_fixture("waiter-owner-no-job-marker", None);
        let job_id = JobId("job-1".to_owned());
        let generation = Generation::INITIAL;
        std::fs::remove_file(cleanup::owned_path(&dir, &job_id.0, generation.0)).unwrap();
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, &job_id.0, generation.0).unwrap(),
            cleanup::OwnedPidLiveness::Absent
        );
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, "wait-velnor-1", generation.0).unwrap(),
            cleanup::OwnedPidLiveness::Dead
        );
        // Reopen the durable journal as a fresh controller process would.
        drop(journal);
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();

        reclaim_orphaned_jobs(
            &controller_test_args(dir.clone()),
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            |docker_args| {
                panic!("docker must not be invoked without docker backend: {docker_args:?}")
            },
        )
        .await
        .unwrap();

        let state = journal.materialized_state().unwrap();
        assert!(state.jobs.iter().all(|job| job.job_id != job_id));
        assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_recovery_without_docker_backend_restores_slot_without_docker() {
        let (dir, mut journal) = stale_running_job_fixture("no-backend", None);
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            |docker_args| {
                panic!("docker must not be invoked without a docker backend: {docker_args:?}")
            },
        )
        .await
        .unwrap();

        let state = journal.load_state().unwrap();
        assert!(
            state
                .jobs
                .iter()
                .all(|job| job.job_id != JobId("job-1".to_owned())),
            "JobWorkerLost must remove the job row"
        );
        let slot = state
            .slots
            .iter()
            .find(|slot| slot.slot_id == SlotId("velnor-1".to_owned()))
            .unwrap();
        assert_eq!(slot.phase, SlotPhase2::Ready);

        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_recovery_on_microvm_backend_restores_slot_without_docker() {
        let (dir, mut journal) = stale_running_job_fixture("microvm", Some("microvm"));
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            |docker_args| {
                panic!("docker must not be invoked on the microvm backend: {docker_args:?}")
            },
        )
        .await
        .unwrap();

        let state = journal.load_state().unwrap();
        assert!(
            state
                .jobs
                .iter()
                .all(|job| job.job_id != JobId("job-1".to_owned())),
            "JobWorkerLost must remove the job row"
        );
        let slot = state
            .slots
            .iter()
            .find(|slot| slot.slot_id == SlotId("velnor-1".to_owned()))
            .unwrap();
        assert_eq!(slot.phase, SlotPhase2::Ready);

        std::fs::remove_dir_all(dir).ok();
    }

    /// A marker+creds recovery whose completion is remotely accepted removes
    /// the job row, so the slot returns to Ready through the row-gone branch.
    /// The dead worker's containers must still be torn down on that path.
    #[cfg(feature = "test-support")]
    #[cfg(unix)]
    #[tokio::test]
    async fn stale_recovery_marker_success_tears_down_containers_before_ready() {
        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/jobs/1/completejob"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let run_service_url = format!("{}/jobs/1", server.uri());
        let (dir, mut journal) = stale_running_job_fixture("marker-success", Some("docker"));
        // Marker cleanup releases the durable storage reservation through
        // the process-wide sink, which the daemon installs at startup.
        crate::ops::init_at("test-instance".to_owned(), Some(&dir.join("state.db"))).unwrap();
        let slot_dir = dir.join("slots").join("slot-1");
        write_in_flight_job_marker_with_url(
            &slot_dir,
            "job-1",
            "request-1",
            Generation::INITIAL,
            &run_service_url,
        );
        config::save(
            &slot_dir,
            &config::StoredRunnerConfig {
                settings: config::RunnerSettings {
                    github_url: "https://github.com/tailrocks/fixture".to_owned(),
                    server_url: None,
                    server_url_v2: None,
                    pool_id: Some(7),
                    pool_name: Some("velnor".to_owned()),
                    agent_id: Some(7),
                    agent_name: "slot-1".to_owned(),
                    labels: vec!["velnor".to_owned()],
                    use_v2_flow: true,
                    ephemeral: true,
                    disable_update: true,
                },
                credentials: Some(config::StoredCredentials {
                    scheme: config::CredentialScheme::OAuthAccessToken,
                    data: json!({ "token": "token" }),
                }),
            },
        )
        .unwrap();

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let job_container = crate::github_adapter::job_container_name_for_id("job-1");
        let listing = format!(
            "job-cid\t{job_container}\t{job_container}\trunning\n\
             guest-cid\tguest-sidecar\t{job_container}\texited\n"
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            move |docker_args: &[String]| {
                recorded.lock().unwrap().push(docker_args.to_vec());
                if docker_args.first().is_some_and(|command| command == "ps") {
                    return Ok(listing.clone());
                }
                Ok(String::new())
            },
        )
        .await
        .unwrap();

        {
            let calls = calls.lock().unwrap();
            assert_eq!(
                calls[0],
                crate::docker_lease::list_owned_containers_state_args(&job_container)
            );
            assert_eq!(
                calls[1],
                crate::docker_lease::force_remove_container_args(&[
                    "guest-cid".to_string(),
                    "job-cid".to_string()
                ])
            );
            assert_eq!(calls.len(), 2);
        }
        server.verify().await;

        let state = journal.load_state().unwrap();
        assert!(
            state
                .jobs
                .iter()
                .all(|job| job.job_id != JobId("job-1".to_owned())),
            "the accepted completion must remove the job row"
        );
        let slot = state
            .slots
            .iter()
            .find(|slot| slot.slot_id == SlotId("velnor-1".to_owned()))
            .unwrap();
        assert_eq!(slot.phase, SlotPhase2::Ready);

        std::fs::remove_dir_all(dir).ok();
    }

    /// A replay failure with a spent durable budget abandons the row and
    /// restores the slot to Ready. The dead worker's containers must be torn
    /// down on that abandon path, not stranded behind Ready. Replay fails on
    /// the missing outbox payload before any network, so no mock is needed.
    #[cfg(unix)]
    #[tokio::test]
    async fn stale_recovery_budget_spent_abandon_tears_down_containers() {
        let (dir, mut journal) = stale_running_job_fixture("budget-spent", Some("docker"));
        let job_id = JobId("job-1".to_owned());
        let generation = Generation::INITIAL;
        assert!(
            !journal
                .apply(Event::JobTerminalResult {
                    job_id: job_id.clone(),
                    generation,
                    conclusion: "success".to_owned(),
                })
                .unwrap()
                .rejected
        );
        let payload_sha256 = velnor_control::journal::payload_checksum(b"payload");
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job_id.clone(),
                    generation,
                    payload_sha256,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::CompletionSendStarted {
                    job_id: job_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
        for _ in 0..velnor_control::journal::MAX_COMPLETION_ATTEMPTS {
            assert!(
                !journal
                    .apply(Event::CompletionAttemptFailed {
                        job_id: job_id.clone(),
                        generation,
                        permanent: false,
                    })
                    .unwrap()
                    .rejected
            );
        }
        let slot_dir = dir.join("slots").join("slot-1");
        write_in_flight_job_marker_with_url(
            &slot_dir,
            "job-1",
            "request-1",
            generation,
            "https://example.invalid/run-service",
        );
        config::save(
            &slot_dir,
            &config::StoredRunnerConfig {
                settings: config::RunnerSettings {
                    github_url: "https://github.com/tailrocks/fixture".to_owned(),
                    server_url: None,
                    server_url_v2: None,
                    pool_id: Some(7),
                    pool_name: Some("velnor".to_owned()),
                    agent_id: Some(7),
                    agent_name: "slot-1".to_owned(),
                    labels: vec!["velnor".to_owned()],
                    use_v2_flow: true,
                    ephemeral: true,
                    disable_update: true,
                },
                credentials: None,
            },
        )
        .unwrap();

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let job_container = crate::github_adapter::job_container_name_for_id("job-1");
        let listing = format!(
            "job-cid\t{job_container}\t{job_container}\trunning\n\
             guest-cid\tguest-sidecar\t{job_container}\texited\n"
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            move |docker_args: &[String]| {
                recorded.lock().unwrap().push(docker_args.to_vec());
                if docker_args.first().is_some_and(|command| command == "ps") {
                    return Ok(listing.clone());
                }
                Ok(String::new())
            },
        )
        .await
        .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(
            calls[0],
            crate::docker_lease::list_owned_containers_state_args(&job_container)
        );
        assert_eq!(
            calls[1],
            crate::docker_lease::force_remove_container_args(&[
                "guest-cid".to_string(),
                "job-cid".to_string()
            ])
        );
        assert_eq!(calls.len(), 2);

        let state = journal.load_state().unwrap();
        assert!(
            state
                .jobs
                .iter()
                .all(|job| job.job_id != JobId("job-1".to_owned())),
            "the abandonment must remove the job row"
        );
        assert!(
            journal.pending_outbox().unwrap().is_empty(),
            "the abandonment must clear the pending outbox row"
        );
        let slot = state
            .slots
            .iter()
            .find(|slot| slot.slot_id == SlotId("velnor-1".to_owned()))
            .unwrap();
        assert_eq!(slot.phase, SlotPhase2::Ready);

        std::fs::remove_dir_all(dir).ok();
    }

    /// Journal with one `Ready` slot and no assigned job, plus an exec config
    /// whose two-slot layout maps `velnor-1` to `slots/slot-1`.
    fn ready_slot_journal(dir: &Path) -> Journal {
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ExecutorProven {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::SessionLive {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::RegistrationIntended {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::Registered {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ReadyAttempt {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        journal
    }

    fn marker_only_recovery_fixture(label: &str) -> (PathBuf, Journal, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "velnor-marker-only-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();
        let journal = ready_slot_journal(&dir);
        write_exec_config(&dir, &dummy_exec("https://github.com/tailrocks/fixture"), 2).unwrap();
        let slot_dir = dir.join("slots").join("slot-1");
        write_in_flight_job_marker(&slot_dir, "job-9", "request-9", Generation::INITIAL);
        (dir, journal, slot_dir)
    }

    #[test]
    fn ready_waiters_are_not_spawned_while_an_in_flight_lease_exists() {
        let (dir, journal, _slot_dir) = marker_only_recovery_fixture("ready-waiter-in-flight");
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: true,
            lifecycle: None,
        };
        let mut jobs = HashMap::new();
        spawn_ready_waiters(&args, &journal, &mut jobs).unwrap();
        assert!(
            jobs.is_empty(),
            "an in-flight lease is physical occupancy; Ready is not enough to spawn"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn persisted_ready_slot_with_absent_waiter_marker_respawns_from_exact_config() {
        let dir = metrics_test_dir("ready-waiter-absent-marker");
        let journal = ready_slot_journal(&dir);
        let exec = dummy_exec("https://github.com/tailrocks/fixture");
        write_exec_config(&dir, &exec, 2).unwrap();
        write_ready_runner_config(&dir, &exec, 1, 2);
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: true,
            lifecycle: None,
        };
        let mut jobs = HashMap::new();
        let mut spawn_calls = 0;

        spawn_ready_waiters_with(
            &args,
            &journal,
            &mut jobs,
            |_, _, _, job_id, generation, slot_id, recovery_only| {
                spawn_calls += 1;
                assert_eq!(job_id, "wait-velnor-1");
                assert_eq!(generation, Generation::INITIAL.0);
                assert_eq!(slot_id, Some(&SlotId("velnor-1".to_owned())));
                assert!(!recovery_only);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(spawn_calls, 1, "a proven Ready registration needs a waiter");
        assert!(jobs.is_empty(), "the injected spawn callback owns no child");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn persisted_ready_slot_with_drifted_config_does_not_respawn() {
        let dir = metrics_test_dir("ready-waiter-drifted-config");
        let journal = ready_slot_journal(&dir);
        let exec = dummy_exec("https://github.com/tailrocks/fixture");
        write_exec_config(&dir, &exec, 2).unwrap();
        let slot_dir = crate::runner::daemon_slot_config_dir(&dir, 1, 2);
        let mut stored = ready_runner_config(&exec, 1);
        stored.settings.agent_name.push_str("-stale");
        config::save(&slot_dir, &stored).unwrap();
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: true,
            lifecycle: None,
        };
        let mut jobs = HashMap::new();

        spawn_ready_waiters_with(&args, &journal, &mut jobs, |_, _, _, _, _, _, _| {
            panic!("drifted JIT identity must prevent waiter respawn")
        })
        .unwrap();

        assert!(jobs.is_empty());
        assert!(
            !cleanup::owned_path(&dir, "wait-velnor-1", Generation::INITIAL.0).exists(),
            "failed identity proof must not publish a new waiter owner"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn ready_waiters_are_not_spawned_while_a_persisted_waiter_pid_lives() {
        let dir = metrics_test_dir("ready-waiter-live-pid");
        let journal = ready_slot_journal(&dir);
        let exec = dummy_exec("https://github.com/tailrocks/fixture");
        write_exec_config(&dir, &exec, 1).unwrap();
        write_ready_runner_config(&dir, &exec, 1, 1);
        let _waiter = spawn_tokenized_owned_process(&dir, "wait-velnor-1", Generation::INITIAL);
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: true,
            lifecycle: None,
        };
        let mut jobs = HashMap::new();
        spawn_ready_waiters(&args, &journal, &mut jobs).unwrap();
        assert!(
            jobs.is_empty(),
            "a live waiter pid after controller restart must keep the slot unspawnable"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn ready_waiters_are_not_spawned_while_a_persisted_waiter_intent_lives() {
        let dir = metrics_test_dir("ready-waiter-live-intent");
        let journal = ready_slot_journal(&dir);
        let exec = dummy_exec("https://github.com/tailrocks/fixture");
        write_exec_config(&dir, &exec, 1).unwrap();
        write_ready_runner_config(&dir, &exec, 1, 1);
        cleanup::initialize_owned_directory(&dir).unwrap();
        let mut waiter = cleanup::with_dead_owned_pid_intent(
            &dir,
            "wait-velnor-1",
            Generation::INITIAL.0,
            |launch_token| {
                let launch_argument = format!("--launch-token={launch_token}");
                Command::new("/bin/sh")
                    .args([
                        "-c",
                        "trap 'exit 0' TERM INT; while :; do sleep 1; done",
                        "velnor-controller-waiter-fixture",
                        &launch_argument,
                    ])
                    .spawn()
                    .map_err(anyhow::Error::from)
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, "wait-velnor-1", Generation::INITIAL.0).unwrap(),
            cleanup::OwnedPidLiveness::UnpublishedIntentLive
        );

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: true,
            lifecycle: None,
        };
        let mut jobs = HashMap::new();
        spawn_ready_waiters(&args, &journal, &mut jobs).unwrap();
        assert!(
            jobs.is_empty(),
            "a live process carrying the durable launch token must block duplicate waiter spawn"
        );
        let _ = waiter.kill();
        let _ = waiter.wait();
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn marker_only_in_flight_job_defers_to_live_waiter_pid() {
        let (dir, mut journal, slot_dir) = marker_only_recovery_fixture("live-waiter");
        // Crash window: the waiter persisted the in-flight marker before
        // journal admission, so ownership is still keyed `wait-{slot}` and no
        // `job-9` worker marker exists yet. Recovery must defer to the live
        // waiter instead of hard-erroring on the missing worker marker.
        let _waiter = spawn_tokenized_owned_process(&dir, "wait-velnor-1", Generation::INITIAL);

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
            |args| panic!("docker must not be invoked on this recovery path: {args:?}"),
        )
        .await
        .unwrap();

        assert!(
            crate::runner::test_in_flight_job_path(&slot_dir).exists(),
            "live waiter must keep its in-flight marker for the next tick"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn marker_only_in_flight_job_reclaims_with_dead_waiter_and_absent_job_marker() {
        let (dir, mut journal, _slot_dir) = marker_only_recovery_fixture("dead-waiter");
        write_dead_published_owned_process(&dir, "wait-velnor-1", Generation::INITIAL);
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, "job-9", Generation::INITIAL.0).unwrap(),
            cleanup::OwnedPidLiveness::Absent
        );
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, "wait-velnor-1", Generation::INITIAL.0).unwrap(),
            cleanup::OwnedPidLiveness::Dead
        );

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let error = reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
            |args| panic!("docker must not be invoked on this recovery path: {args:?}"),
        )
        .await
        .unwrap_err();
        // The production worker remains keyed by its waiter id, so the dead
        // waiter marker alone proves owner death when no job-id marker exists.
        // Recovery advances past the ownership gate and stops at credentials.
        assert!(
            error.to_string().contains(
                "runner credentials missing while recovering marker-only in-flight job job-9"
            ),
            "{error:#}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn marker_only_in_flight_job_without_any_ownership_marker_errors() {
        let (dir, mut journal, _slot_dir) = marker_only_recovery_fixture("no-owner");

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let error = reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
            |args| panic!("docker must not be invoked on this recovery path: {args:?}"),
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("has unknown worker or waiter ownership"),
            "{error:#}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completing_job_without_payload_does_not_abort_orphan_reclaim() {
        let dir = metrics_test_dir("completing-no-payload-reclaim");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let generation = Generation::INITIAL;
        let completing_slot = SlotId("velnor-1".to_owned());
        let running_slot = SlotId("velnor-2".to_owned());
        let completing_job = JobId("job-completing".to_owned());
        let running_job = JobId("job-running".to_owned());
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 2 },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        for slot_id in [completing_slot.clone(), running_slot.clone()] {
            for event in [
                Event::PermitReserved {
                    slot_id: slot_id.clone(),
                    generation,
                },
                Event::ExecutorProven {
                    slot_id: slot_id.clone(),
                    generation,
                },
                Event::SessionLive {
                    slot_id: slot_id.clone(),
                    generation,
                },
                Event::RegistrationIntended {
                    slot_id: slot_id.clone(),
                    generation,
                },
                Event::Registered {
                    slot_id: slot_id.clone(),
                    generation,
                },
                Event::ReadyAttempt {
                    slot_id: slot_id.clone(),
                    generation,
                },
            ] {
                assert!(!journal.apply(event).unwrap().rejected);
            }
        }
        for (slot_id, job_id, runner_request_id) in [
            (
                completing_slot.clone(),
                completing_job.clone(),
                "request-completing",
            ),
            (running_slot.clone(), running_job.clone(), "request-running"),
        ] {
            for event in owned_job_events(&job_id, &slot_id, generation, runner_request_id, None)
                .into_iter()
                .chain([Event::JobStarted {
                    job_id: job_id.clone(),
                    generation,
                }])
            {
                assert!(!journal.apply(event).unwrap().rejected);
            }
        }
        cleanup::write_outbox(&dir, &completing_job.0, generation.0, b"payload").unwrap();
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: completing_job.clone(),
                    generation,
                    payload_sha256: velnor_control::journal::payload_checksum(b"payload"),
                })
                .unwrap()
                .rejected
        );
        drop(journal);
        // Exercise payload loss after the durable intent. Preserve the
        // event-sourced outbox row; deleting that row would corrupt the
        // journal projection instead of modeling a missing payload file.
        std::fs::remove_file(cleanup::outbox_path(&dir, &completing_job.0, generation.0)).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        write_exec_config(&dir, &dummy_exec("https://github.com/tailrocks/fixture"), 2).unwrap();
        let slot_dir = dir.join("slots").join("slot-1");
        write_in_flight_job_marker_with_url_and_permit(
            &slot_dir,
            &completing_job.0,
            "request-completing",
            generation,
            "https://example.invalid/run-service",
            None,
        );
        for isolation in [
            completing_job.0.as_str(),
            running_job.0.as_str(),
            "wait-velnor-1",
            "wait-velnor-2",
        ] {
            write_dead_published_owned_process(&dir, isolation, generation);
        }

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 2,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            |args| panic!("docker must not be invoked on this recovery path: {args:?}"),
        )
        .await
        .expect("missing completion payload must not abort the controller cycle");

        let state = journal.materialized_state().unwrap();
        let completing = state
            .jobs
            .iter()
            .find(|job| job.job_id == completing_job)
            .expect("completing job is retried, not discarded");
        assert_eq!(completing.phase, JobPhase2::Completing);
        assert!(completing.terminal_conclusion.is_none());
        assert!(
            state.jobs.iter().all(|job| job.job_id != running_job),
            "the other slot must still reclaim after the completing job is skipped: {:?}",
            state.jobs
        );
        assert_eq!(
            state
                .slots
                .iter()
                .find(|slot| slot.slot_id == running_slot)
                .map(|slot| slot.phase),
            Some(SlotPhase2::Ready)
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completing_job_with_pending_outbox_does_not_reconstruct_after_replay_failure() {
        let dir = metrics_test_dir("completing-pending-outbox-no-reconstruct");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let generation = Generation::INITIAL;
        let completing_slot = SlotId("velnor-1".to_owned());
        let running_slot = SlotId("velnor-2".to_owned());
        let completing_job = JobId("job-completing".to_owned());
        let running_job = JobId("job-running".to_owned());
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 2 },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        for slot_id in [completing_slot.clone(), running_slot.clone()] {
            for event in [
                Event::PermitReserved {
                    slot_id: slot_id.clone(),
                    generation,
                },
                Event::ExecutorProven {
                    slot_id: slot_id.clone(),
                    generation,
                },
                Event::SessionLive {
                    slot_id: slot_id.clone(),
                    generation,
                },
                Event::RegistrationIntended {
                    slot_id: slot_id.clone(),
                    generation,
                },
                Event::Registered {
                    slot_id: slot_id.clone(),
                    generation,
                },
                Event::ReadyAttempt {
                    slot_id: slot_id.clone(),
                    generation,
                },
            ] {
                assert!(!journal.apply(event).unwrap().rejected);
            }
        }
        for (slot_id, job_id, runner_request_id) in [
            (
                completing_slot.clone(),
                completing_job.clone(),
                "request-completing",
            ),
            (running_slot.clone(), running_job.clone(), "request-running"),
        ] {
            for event in owned_job_events(&job_id, &slot_id, generation, runner_request_id, None)
                .into_iter()
                .chain([Event::JobStarted {
                    job_id: job_id.clone(),
                    generation,
                }])
            {
                assert!(!journal.apply(event).unwrap().rejected);
            }
        }
        assert!(
            !journal
                .apply(Event::JobTerminalResult {
                    job_id: completing_job.clone(),
                    generation,
                    conclusion: "success".to_owned(),
                })
                .unwrap()
                .rejected
        );
        let payload = b"payload";
        let payload_sha256 = velnor_control::journal::payload_checksum(payload);
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: completing_job.clone(),
                    generation,
                    payload_sha256: payload_sha256.clone(),
                })
                .unwrap()
                .rejected
        );
        cleanup::write_outbox(&dir, &completing_job.0, generation.0, payload).unwrap();
        write_exec_config(&dir, &dummy_exec("https://github.com/tailrocks/fixture"), 2).unwrap();
        let slot_dir = dir.join("slots").join("slot-1");
        std::fs::create_dir_all(&slot_dir).unwrap();
        config::save(
            &slot_dir,
            &config::StoredRunnerConfig {
                settings: config::RunnerSettings {
                    github_url: "https://github.com/tailrocks/fixture".to_owned(),
                    server_url: None,
                    server_url_v2: None,
                    pool_id: Some(7),
                    pool_name: Some("velnor".to_owned()),
                    agent_id: Some(7),
                    agent_name: "slot-1".to_owned(),
                    labels: vec!["velnor".to_owned()],
                    use_v2_flow: true,
                    ephemeral: true,
                    disable_update: true,
                },
                credentials: None,
            },
        )
        .unwrap();
        write_in_flight_job_marker_with_url_and_permit(
            &slot_dir,
            &completing_job.0,
            "request-completing",
            generation,
            "https://example.invalid/run-service",
            None,
        );
        for isolation in [
            completing_job.0.as_str(),
            running_job.0.as_str(),
            "wait-velnor-1",
            "wait-velnor-2",
        ] {
            write_dead_published_owned_process(&dir, isolation, generation);
        }

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 2,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            |args| panic!("docker must not be invoked on this recovery path: {args:?}"),
        )
        .await
        .expect("replay failure must not abort the controller cycle");

        let state = journal.materialized_state().unwrap();
        let completing = state
            .jobs
            .iter()
            .find(|job| job.job_id == completing_job)
            .expect("completing job is retried, not reconstructed");
        assert_eq!(completing.phase, JobPhase2::Completing);
        assert_eq!(completing.terminal_conclusion.as_deref(), Some("success"));
        let outbox = state
            .outbox
            .iter()
            .find(|row| row.job_id == completing_job && row.generation == generation)
            .expect("pending outbox row is kept");
        assert!(outbox.is_pending());
        assert_eq!(outbox.payload_sha256, payload_sha256);
        assert_eq!(
            cleanup::read_outbox(&dir, &completing_job.0, generation.0).unwrap(),
            payload
        );
        assert!(
            crate::runner::test_in_flight_job_path(&slot_dir).exists(),
            "replay failure must not reconstruct or clear the in-flight marker"
        );
        assert!(
            state.jobs.iter().all(|job| job.job_id != running_job),
            "the other slot must still reclaim after replay failure: {:?}",
            state.jobs
        );
        assert_eq!(
            state
                .slots
                .iter()
                .find(|slot| slot.slot_id == running_slot)
                .map(|slot| slot.phase),
            Some(SlotPhase2::Ready)
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn missing_remote_registration_clears_local_claim() {
        let env_guard = crate::test_support::github_test_env().await;
        env_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runners/7"))
            .respond_with(ResponseTemplate::new(404))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runners"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 0,
                "runners": []
            })))
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!(
            "velnor-registration-reconcile-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("{}/tailrocks", server.uri());
        write_exec_config(&dir, &dummy_exec(&url), 1).unwrap();
        config::save(
            &crate::runner::daemon_slot_config_dir(&dir, 1, 1),
            &config::StoredRunnerConfig {
                settings: config::RunnerSettings {
                    github_url: url.clone(),
                    server_url: None,
                    server_url_v2: None,
                    pool_id: Some(7),
                    pool_name: Some("velnor".to_owned()),
                    agent_id: Some(7),
                    agent_name: "slot-1".to_owned(),
                    labels: vec!["velnor".to_owned()],
                    use_v2_flow: true,
                    ephemeral: true,
                    disable_update: true,
                },
                credentials: None,
            },
        )
        .unwrap();
        // SAFETY: the test holds the process-wide GITHUB_TOKEN environment lock.
        unsafe { std::env::set_var("GITHUB_TOKEN", "ghs_test") };

        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ExecutorProven {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::SessionLive {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::RegistrationIntended {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::Registered {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ReadyAttempt {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        prime_owned_job(
            &mut journal,
            &JobId("job-1".to_owned()),
            &SlotId("velnor-1".to_owned()),
            Generation::INITIAL,
            "request-1",
            Some(test_native_permit_lease("request-1")),
        );

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        reconcile_remote_registrations(
            &args,
            &mut journal,
            &mut HashMap::new(),
            &mut GithubPacing::default(),
        )
        .await
        .unwrap();
        let slot = journal
            .load_state()
            .unwrap()
            .slots
            .into_iter()
            .find(|slot| slot.slot_id == SlotId("velnor-1".to_owned()))
            .unwrap();
        assert!(!slot.registered);
        assert!(!slot.permit_held);
        assert!(!slot.session_live);
        assert_eq!(slot.phase, SlotPhase2::Fenced);
        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, JobId("job-1".to_owned()));
        // SAFETY: the test holds the process-wide GITHUB_TOKEN environment lock.
        unsafe { std::env::remove_var("GITHUB_TOKEN") };
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(feature = "test-support")]
    async fn reconcile_with_runner_config_fixture(
        prepare_runner_config: impl FnOnce(&Path),
    ) -> anyhow::Result<SlotRecord> {
        let env_guard = crate::test_support::github_test_env().await;
        env_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runners"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 0,
                "runners": []
            })))
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!(
            "velnor-registration-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("{}/tailrocks", server.uri());
        write_exec_config(&dir, &dummy_exec(&url), 1).unwrap();
        prepare_runner_config(&dir);
        // SAFETY: the test holds the process-wide GITHUB_TOKEN environment lock.
        unsafe { std::env::set_var("GITHUB_TOKEN", "ghs_test") };

        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ExecutorProven {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::SessionLive {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::RegistrationIntended {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::Registered {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let reconciliation = reconcile_remote_registrations(
            &args,
            &mut journal,
            &mut HashMap::new(),
            &mut GithubPacing::default(),
        )
        .await;

        let slot = journal
            .load_state()
            .unwrap()
            .slots
            .into_iter()
            .find(|slot| slot.slot_id == SlotId("velnor-1".to_owned()))
            .unwrap();
        // SAFETY: the test holds the process-wide GITHUB_TOKEN environment lock.
        unsafe { std::env::remove_var("GITHUB_TOKEN") };
        std::fs::remove_dir_all(dir).ok();
        reconciliation.map(|()| slot)
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn missing_runner_config_allows_registration_recovery() {
        let slot = reconcile_with_runner_config_fixture(|_| {}).await.unwrap();

        assert!(!slot.registered);
        assert!(!slot.permit_held);
        assert!(!slot.session_live);
        assert_eq!(slot.phase, SlotPhase2::Provisioning);
    }

    /// A slot that holds a remote registration is what makes reconciliation
    /// fail closed: without it there is no remote state to verify and the
    /// check is vacuous.
    fn prime_registered_slot(journal: &mut Journal) {
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::ExecutorProven {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::SessionLive {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::RegistrationIntended {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::Registered {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
    }

    #[tokio::test]
    async fn missing_execution_config_fails_registration_reconciliation_closed() {
        let dir = metrics_test_dir("missing-exec-config");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        prime_registered_slot(&mut journal);
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };

        let error = reconcile_remote_registrations(
            &args,
            &mut journal,
            &mut HashMap::new(),
            &mut GithubPacing::default(),
        )
        .await
        .expect_err("missing execution config must fail closed");
        assert!(error
            .to_string()
            .contains("requires daemon execution config"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn missing_execution_config_skips_reconciliation_without_registered_slots() {
        let dir = metrics_test_dir("missing-exec-config-vacuous");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };

        reconcile_remote_registrations(
            &args,
            &mut journal,
            &mut HashMap::new(),
            &mut GithubPacing::default(),
        )
        .await
        .expect("reconciliation is vacuous while no slot holds a registration");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn missing_pat_fails_registration_reconciliation_closed() {
        let _env_guard = crate::test_support::github_test_env().await;
        let previous_token = std::env::var_os("GITHUB_TOKEN");
        let dir = metrics_test_dir("missing-pat");
        write_exec_config(&dir, &dummy_exec("https://github.com/tailrocks/velnor"), 1).unwrap();
        // SAFETY: the process-wide token lock serializes this test's env access.
        unsafe { std::env::remove_var("GITHUB_TOKEN") };
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        prime_registered_slot(&mut journal);
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };

        let error = reconcile_remote_registrations(
            &args,
            &mut journal,
            &mut HashMap::new(),
            &mut GithubPacing::default(),
        )
        .await
        .expect_err("missing PAT must fail closed");
        assert!(error.to_string().contains("requires GitHub URL and PAT"));
        // SAFETY: the process-wide token lock serializes this test's env access.
        match previous_token {
            Some(token) => unsafe { std::env::set_var("GITHUB_TOKEN", token) },
            None => unsafe { std::env::remove_var("GITHUB_TOKEN") },
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn corrupt_runner_config_does_not_clear_local_claim() {
        let error = reconcile_with_runner_config_fixture(|dir| {
            std::fs::write(dir.join("runner.json"), b"{not-json").unwrap();
        })
        .await
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("cannot load local runner config"));
    }

    #[cfg(unix)]
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn unreadable_runner_config_does_not_clear_local_claim() {
        let error = reconcile_with_runner_config_fixture(|dir| {
            std::fs::create_dir(dir.join("runner.json")).unwrap();
        })
        .await
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("cannot load local runner config"));
    }

    #[cfg(unix)]
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn broken_runner_config_symlink_does_not_allow_recovery() {
        let error = reconcile_with_runner_config_fixture(|dir| {
            std::os::unix::fs::symlink("missing-runner.json", dir.join("runner.json")).unwrap();
        })
        .await
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("cannot load local runner config"));
    }

    #[cfg(unix)]
    #[test]
    fn dangling_parent_symlink_is_not_treated_as_missing_config() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-dangling-parent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("slot");
        std::os::unix::fs::symlink("missing-slot", &link).unwrap();

        assert!(has_dangling_symlink_component(&link.join("runner.json")).unwrap());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn stalled_remote_reconciliation_does_not_block_local_cycle() {
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            run_bounded_remote_reconciliation(
                std::future::pending::<anyhow::Result<()>>(),
                Duration::from_millis(1),
            ),
        )
        .await;

        assert!(matches!(result, Ok(Ok(()))));
    }

    #[test]
    fn remote_runner_ids_require_complete_identity_evidence() {
        let ids = remote_runner_ids([Some(7), Some(8)]);
        assert_eq!(ids, Some(HashSet::from([7, 8])));
        assert!(remote_runner_ids([Some(7), None]).is_none());
    }

    #[cfg(feature = "test-support")]
    async fn reconciliation_lookup_error_pacing(
        status: u16,
        headers: &[(&'static str, String)],
    ) -> GithubPacing {
        let env_guard = crate::test_support::github_test_env().await;
        env_guard.set_native();
        let server = MockServer::start().await;
        let response = headers
            .iter()
            .fold(ResponseTemplate::new(status), |response, (name, value)| {
                response.insert_header(*name, value.clone())
            });
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runners"))
            .respond_with(response)
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!(
            "velnor-registration-error-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("{}/tailrocks", server.uri());
        write_exec_config(&dir, &dummy_exec(&url), 1).unwrap();
        let slot_dir = crate::runner::daemon_slot_config_dir(&dir, 1, 1);
        crate::config::save(
            &slot_dir,
            &crate::config::StoredRunnerConfig {
                settings: crate::config::RunnerSettings {
                    github_url: url.clone(),
                    server_url: None,
                    server_url_v2: None,
                    pool_id: Some(7),
                    pool_name: Some("velnor".to_owned()),
                    agent_id: Some(7),
                    agent_name: "velnor-1".to_owned(),
                    labels: vec!["velnor".to_owned()],
                    use_v2_flow: false,
                    ephemeral: true,
                    disable_update: true,
                },
                credentials: None,
            },
        )
        .unwrap();
        // SAFETY: the test holds the process-wide GITHUB_TOKEN environment lock.
        unsafe { std::env::set_var("GITHUB_TOKEN", "ghs_test") };
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::Registered {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".into(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let mut pacing = GithubPacing::default();
        reconcile_remote_registrations(&args, &mut journal, &mut HashMap::new(), &mut pacing)
            .await
            .unwrap();
        // SAFETY: the test holds the process-wide GITHUB_TOKEN environment lock.
        unsafe { std::env::remove_var("GITHUB_TOKEN") };
        std::fs::remove_dir_all(dir).ok();
        pacing
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn reconciliation_quota_errors_hold_fleet_until_absolute_deadline() {
        let reset = epoch_now() + 3600;
        let pacing = reconciliation_lookup_error_pacing(
            403,
            &[
                ("x-ratelimit-remaining", "0".to_owned()),
                ("x-ratelimit-reset", reset.to_string()),
            ],
        )
        .await;
        assert!(!pacing.registration_due("unregistered", tokio::time::Instant::now()));

        let pacing =
            reconciliation_lookup_error_pacing(429, &[("retry-after", "30".to_owned())]).await;
        assert!(!pacing.registration_due("unregistered", tokio::time::Instant::now()));
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn reconciliation_permission_error_does_not_hold_fleet() {
        let pacing = reconciliation_lookup_error_pacing(
            403,
            &[
                ("x-ratelimit-remaining", "4200".to_owned()),
                ("x-ratelimit-reset", (epoch_now() + 3600).to_string()),
            ],
        )
        .await;
        assert!(pacing.registration_due("unregistered", tokio::time::Instant::now()));
    }

    #[test]
    fn remote_budget_leaves_controller_watchdog_margin() {
        assert!(CONTROLLER_REMOTE_BUDGET < Duration::from_secs(30));
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn org_url_probe_bootstraps_policy_from_generated_allowlist() {
        let env_guard = crate::test_support::github_test_env().await;
        env_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runners"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "runners": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runner-groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 1,
                "runner_groups": [{"id": 7, "name": "velnor", "default": false}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v3/orgs/tailrocks/actions/runner-groups/7/repositories",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 1,
                "repositories": [{"full_name": "tailrocks/velnor"}]
            })))
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!(
            "velnor-ctrl-org-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let policy_dir = dir.join("fleet-policy");
        std::fs::create_dir_all(&policy_dir).unwrap();
        std::fs::write(
            policy_dir.join("tailrocks-desired-policy.json"),
            serde_json::to_vec(&json!({
                "organization": "tailrocks",
                "group_name": "velnor",
                "selected_repositories": ["tailrocks/velnor", "tailrocks/velnor-apt"]
            }))
            .unwrap(),
        )
        .unwrap();
        // SAFETY: the test holds the process-wide environment lock.
        unsafe { std::env::set_var("VELNOR_FLEET_POLICY_OUT_DIR", policy_dir.as_os_str()) };
        let url = format!("{}/tailrocks", server.uri());
        write_exec_config(&dir, &dummy_exec(&url), 1).unwrap();
        // Stale live-membership snapshot must not win over generated JSON.
        std::fs::write(
            dir.join(prove::ROUTING_POLICY_FILE),
            serde_json::to_vec_pretty(&json!({
                "group": "velnor",
                "selected_repositories": ["tailrocks/velnor"],
                "labels": ["velnor"],
                "trust_scope": "trusted"
            }))
            .unwrap(),
        )
        .unwrap();
        // SAFETY: the test holds the process-wide GITHUB_TOKEN environment lock.
        unsafe { std::env::set_var("GITHUB_TOKEN", "ghs_test") };
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        journal.apply(Event::ControlLive).unwrap();
        journal.apply(Event::JournalWritable).unwrap();
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".into(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        journal
            .apply(Event::DesiredCapacity {
                ready: args.desired_ready,
            })
            .unwrap();
        let mut pacing = GithubPacing::default();
        observe_github_and_routing(
            &args,
            &mut journal,
            &mut pacing,
            tokio::time::Instant::now() + CONTROLLER_REMOTE_BUDGET,
        )
        .await
        .unwrap();
        let state = journal.load_state().unwrap();
        assert!(state.github_reachable, "{state:?}");
        let evidence: crate::node::prove::RoutingFields =
            serde_json::from_slice(&std::fs::read(dir.join(prove::ROUTING_EVIDENCE_FILE)).unwrap())
                .unwrap();
        assert_eq!(evidence.group, "velnor");
        assert_eq!(evidence.selected_repositories, vec!["tailrocks/velnor"]);
        let policy: crate::node::prove::RoutingFields =
            serde_json::from_slice(&std::fs::read(dir.join(prove::ROUTING_POLICY_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            policy.selected_repositories,
            vec!["tailrocks/velnor", "tailrocks/velnor-apt"],
            "generated allowlist must replace a stale live-membership snapshot"
        );
        assert!(
            !state.routing_valid,
            "drift against generated allowlist must fail closed: {state:?}"
        );
        // SAFETY: the test holds the process-wide environment lock.
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
            std::env::remove_var("VELNOR_FLEET_POLICY_OUT_DIR");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    /// First-boot `write_policy_if_absent` froze `trust_scope=untrusted` on
    /// disk. Raising `VELNOR_TRUST_SCOPE=trusted` updated evidence but not
    /// policy, so `routing_valid` stayed false and slots refused jobs.
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn repo_scoped_derived_policy_refreshes_when_trust_scope_changes() {
        let env_guard = crate::test_support::github_test_env().await;
        env_guard.set_native();
        // SAFETY: the test holds the process-wide GITHUB_TOKEN environment lock.
        unsafe { std::env::remove_var("GITHUB_TOKEN") };

        let dir = std::env::temp_dir().join(format!(
            "velnor-ctrl-repo-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write_exec_config(&dir, &dummy_exec("https://github.com/acme/repo"), 1).unwrap();
        std::fs::write(
            dir.join(prove::ROUTING_POLICY_FILE),
            serde_json::to_vec_pretty(&json!({
                "group": "velnor",
                "selected_repositories": ["acme/repo"],
                "labels": ["velnor"],
                "trust_scope": "untrusted"
            }))
            .unwrap(),
        )
        .unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        journal.apply(Event::ControlLive).unwrap();
        journal.apply(Event::JournalWritable).unwrap();
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".into(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        journal
            .apply(Event::DesiredCapacity {
                ready: args.desired_ready,
            })
            .unwrap();
        let mut pacing = GithubPacing::default();
        observe_github_and_routing(
            &args,
            &mut journal,
            &mut pacing,
            tokio::time::Instant::now() + CONTROLLER_REMOTE_BUDGET,
        )
        .await
        .unwrap();
        let policy = prove::read_policy(&dir).expect("derived policy must be on disk");
        assert_eq!(
            policy.trust_scope, "trusted",
            "repo-scoped policy must track daemon trust_scope, not freeze the first-boot snapshot: {policy:?}"
        );
        assert_eq!(policy.selected_repositories, vec!["acme/repo"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unresolved_org_policy_names_every_searched_source() {
        let note = unresolved_desired_policy_note(Some("https://github.com/tailrocks"));
        assert!(note.contains("fail-closed"), "{note}");
        assert!(
            note.contains("tailrocks-desired-policy.json"),
            "the missing artifact must be named: {note}"
        );
        for dir in prove::org_desired_policy_search_dirs() {
            assert!(
                note.contains(&dir.display().to_string()),
                "every searched directory must be named: {note}"
            );
        }
        assert!(
            note.contains("--routing-policy-file"),
            "the operator fix must be named: {note}"
        );
    }

    #[test]
    fn unresolved_policy_without_org_scope_says_why() {
        let note = unresolved_desired_policy_note(Some("https://github.com/tailrocks/velnor"));
        assert!(
            note.contains("https://github.com/tailrocks/velnor"),
            "the scope that yields no policy must be named: {note}"
        );
        assert!(
            note.contains("routing-policy.json"),
            "the repo-scoped source must be named: {note}"
        );
        let unparseable = unresolved_desired_policy_note(None);
        assert!(
            unparseable.contains("not a parseable GitHub scope"),
            "{unparseable}"
        );
    }

    #[test]
    fn unresolved_policy_diagnosis_repeats_on_cadence_not_per_cycle() {
        assert!(
            unresolved_policy_note_due(0, 1_000),
            "first observation speaks"
        );
        assert!(!unresolved_policy_note_due(1_000, 1_059));
        assert!(unresolved_policy_note_due(1_000, 1_060));
        assert!(!unresolved_policy_note_due(1_060, 1_060 + 59));
    }

    #[test]
    fn pacing_holds_probe_to_cadence_after_success() {
        let mut pacing = GithubPacing::default();
        let now = tokio::time::Instant::now();
        assert!(pacing.probe_due(now));
        pacing.record_probe(now, false, Some(4999), Some(epoch_now() + 3600));
        assert!(!pacing.probe_due(now + Duration::from_secs(59)));
        assert!(pacing.probe_due(now + Duration::from_secs(60)));
    }

    #[test]
    fn pacing_probe_backoff_grows_on_unreachable() {
        let mut pacing = GithubPacing::default();
        let now = tokio::time::Instant::now();
        pacing.record_probe_unreachable(now);
        assert!(!pacing.probe_due(now + Duration::from_secs(59)));
        pacing.record_probe_unreachable(now + Duration::from_secs(60));
        assert!(!pacing.probe_due(now + Duration::from_secs(60 + 119)));
        assert!(pacing.probe_due(now + Duration::from_secs(60 + 120)));
        // Third failure is 240s, not linear 180s.
        pacing.record_probe_unreachable(now + Duration::from_secs(180));
        assert!(!pacing.probe_due(now + Duration::from_secs(180 + 239)));
        assert!(pacing.probe_due(now + Duration::from_secs(180 + 240)));
    }

    #[test]
    fn pacing_rate_limited_probe_holds_until_reset() {
        let mut pacing = GithubPacing::default();
        let now = tokio::time::Instant::now();
        let reset = epoch_now() + 3500;
        pacing.record_probe(now, true, Some(0), Some(reset));
        // No retry before the reset window, even after the normal 60s floor.
        assert!(!pacing.probe_due(now + GITHUB_PROBE_MAX_BACKOFF));
        assert!(pacing.probe_due(now + Duration::from_secs(3700)));
    }

    #[test]
    fn pacing_rate_limit_blocks_registration_reconciliation_until_reset() {
        let mut pacing = GithubPacing::default();
        let now = tokio::time::Instant::now();
        pacing.record_probe(now, true, Some(0), Some(epoch_now() + 3500));
        assert!(!pacing.rest_requests_allowed(now + GITHUB_PROBE_MAX_BACKOFF));
        assert!(pacing.rest_requests_allowed(now + Duration::from_secs(3700)));
    }

    #[test]
    fn pacing_low_remaining_reserves_headroom_until_reset() {
        let mut pacing = GithubPacing::default();
        let now = tokio::time::Instant::now();
        pacing.record_probe(now, false, Some(40), Some(epoch_now() + 120));
        assert!(!pacing.probe_due(now + Duration::from_secs(119)));
        assert!(pacing.probe_due(now + Duration::from_secs(140)));
    }

    #[test]
    fn pacing_rest_hold_does_not_shorten_on_decreasing_deadline() {
        let mut pacing = GithubPacing::default();
        let now = tokio::time::Instant::now();
        pacing.hold_rest_until(now, Some(epoch_now() + 3600));
        let first_deadline = pacing.rest_hold_until.unwrap();
        pacing.hold_rest_until(now, Some(epoch_now() + 120));

        assert_eq!(pacing.rest_hold_until, Some(first_deadline));
        assert_eq!(pacing.next_probe, first_deadline);
        assert!(!pacing.registration_due("velnor-1", first_deadline - Duration::from_secs(1)));
    }

    #[test]
    fn pacing_low_remaining_update_does_not_shorten_rest_hold() {
        let mut pacing = GithubPacing::default();
        let now = tokio::time::Instant::now();
        pacing.record_probe(now, false, Some(40), Some(epoch_now() + 3600));
        let first_deadline = pacing.rest_hold_until.unwrap();
        pacing.record_probe(
            now + Duration::from_secs(1),
            false,
            Some(40),
            Some(epoch_now() + 120),
        );

        assert_eq!(pacing.rest_hold_until, Some(first_deadline));
        assert_eq!(pacing.next_probe, first_deadline);
        assert!(!pacing.registration_due("velnor-1", first_deadline - Duration::from_secs(1)));
    }

    #[test]
    fn pacing_registration_retries_at_most_once_per_window() {
        let mut pacing = GithubPacing::default();
        let now = tokio::time::Instant::now();
        assert!(pacing.registration_due("velnor-1", now));
        pacing.record_registration_failure("velnor-1", now, None);
        assert!(!pacing.registration_due("velnor-1", now + Duration::from_secs(4)));
        assert!(pacing.registration_due("velnor-1", now + Duration::from_secs(5)));

        // A rate-limit hint (x-ratelimit-reset) holds the slot until reset.
        pacing.record_registration_failure("velnor-2", now, Some(Duration::from_secs(3500)));
        assert!(!pacing.registration_due("velnor-2", now + Duration::from_secs(600)));
        assert!(pacing.registration_due("velnor-2", now + Duration::from_secs(3500)));

        // Success clears the backoff entirely.
        pacing.record_registration_success("velnor-1");
        assert!(pacing.registration_due("velnor-1", now));
    }

    #[test]
    fn pacing_quota_holds_all_registrations_until_reset() {
        let mut pacing = GithubPacing::default();
        let now = tokio::time::Instant::now();
        let reset = epoch_now() + 3500;
        assert!(pacing.registration_due("proven-unregistered", now));
        pacing.record_probe(now, true, Some(0), Some(reset));
        assert!(
            !pacing.registration_due("proven-unregistered", now + Duration::from_secs(600)),
            "quota 403 must not let proven slots keep issuing JIT"
        );
        assert!(pacing.registration_due("proven-unregistered", now + Duration::from_secs(3700)));
        pacing.record_probe(
            now + Duration::from_secs(3700),
            false,
            Some(4999),
            Some(epoch_now() + 7200),
        );
        assert!(pacing.registration_due("proven-unregistered", now + Duration::from_secs(3700)));
    }

    #[test]
    fn pacing_jit_quota_holds_all_registrations_until_reset() {
        let mut pacing = GithubPacing::default();
        let now = tokio::time::Instant::now();
        let reset = epoch_now() + 3500;
        let quota = anyhow::Error::from(crate::protocol::GitHubApiError {
            status: 403,
            action: "JIT runner config request".into(),
            body: "API rate limit exceeded".into(),
            retry_after_seconds: None,
            rate_limit_reset_epoch: Some(reset),
            remaining: Some(0),
            category: None,
        });
        assert!(pacing.registration_due("velnor-1", now));
        assert!(pacing.registration_due("velnor-2", now));
        pacing.record_registration_error("velnor-1", now, &quota);
        assert!(
            !pacing.registration_due("velnor-1", now + Duration::from_secs(600)),
            "failing slot stays backed off"
        );
        assert!(
            !pacing.registration_due("velnor-2", now + Duration::from_secs(600)),
            "quota 403/429 must hold ALL unregistered slots via rest_hold_until"
        );
        assert!(pacing.registration_due("velnor-2", now + Duration::from_secs(3700)));

        let throttled = anyhow::Error::from(crate::protocol::GitHubApiError {
            status: 429,
            action: "JIT runner config request".into(),
            body: "too many requests".into(),
            retry_after_seconds: Some(30),
            rate_limit_reset_epoch: None,
            remaining: None,
            category: None,
        });
        let mut pacing = GithubPacing::default();
        pacing.record_registration_error("velnor-1", now, &throttled);
        assert!(
            !pacing.registration_due("velnor-2", now + Duration::from_secs(59)),
            "429 Retry-After must fleet-hold (floor is the 60s probe interval)"
        );
        assert!(pacing.registration_due("velnor-2", now + Duration::from_secs(76)));
    }

    #[test]
    fn pacing_permission_403_does_not_hold_other_slots() {
        let mut pacing = GithubPacing::default();
        let now = tokio::time::Instant::now();
        let permission = anyhow::Error::from(crate::protocol::GitHubApiError {
            status: 403,
            action: "JIT runner config request".into(),
            body: "Resource not accessible by integration".into(),
            retry_after_seconds: None,
            rate_limit_reset_epoch: Some(epoch_now() + 3500),
            remaining: Some(4200),
            category: None,
        });
        pacing.record_registration_error("velnor-1", now, &permission);
        assert!(
            !pacing.registration_due("velnor-1", now + Duration::from_secs(4)),
            "failing slot still backs off"
        );
        assert!(
            pacing.registration_due("velnor-2", now),
            "permission 403 with remaining>0 must not fleet-hold"
        );
    }

    /// Regression oracle for the August 2026 quota-exhaustion class: a 403
    /// with `x-ratelimit-remaining: 0` must park the fleet (visible degraded
    /// health) and issue at most ONE probe per rate-limit window instead of
    /// one per 2s reconcile tick.
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn rate_limited_probe_parks_instead_of_retrying_per_tick() {
        let env_guard = crate::test_support::github_test_env().await;
        env_guard.set_native();
        let server = MockServer::start().await;
        let reset_epoch = epoch_now() + 3600;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runners"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("x-ratelimit-remaining", "0")
                    .insert_header("x-ratelimit-reset", &reset_epoch.to_string()),
            )
            .expect(1)
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!(
            "velnor-ctrl-rl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("{}/tailrocks", server.uri());
        write_exec_config(&dir, &dummy_exec(&url), 1).unwrap();
        let fields = prove::RoutingFields {
            group: "velnor".into(),
            selected_repositories: vec!["tailrocks/velnor".into()],
            labels: vec!["velnor".into()],
            trust_scope: "trusted".into(),
        };
        prove::write_routing_document(&dir, fields.clone(), fields).unwrap();
        // SAFETY: the test holds the process-wide GITHUB_TOKEN environment lock.
        unsafe { std::env::set_var("GITHUB_TOKEN", "ghs_test") };
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        journal.apply(Event::ControlLive).unwrap();
        journal.apply(Event::JournalWritable).unwrap();
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".into(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        journal
            .apply(Event::DesiredCapacity {
                ready: args.desired_ready,
            })
            .unwrap();
        let mut pacing = GithubPacing::default();

        // First tick: the 403 is observed, health degrades honestly.
        observe_github_and_routing(
            &args,
            &mut journal,
            &mut pacing,
            tokio::time::Instant::now() + CONTROLLER_REMOTE_BUDGET,
        )
        .await
        .unwrap();
        let state = journal.load_state().unwrap();
        assert!(!state.github_reachable, "{state:?}");
        assert!(!dir.join(prove::ROUTING_FILE).exists());
        assert!(!dir.join(prove::ROUTING_EVIDENCE_FILE).exists());
        assert_eq!(
            prove::observe_routing(&dir),
            prove::RoutingObservation::invalid()
        );
        assert!(!state.routing_valid, "{state:?}");
        assert!(!state.runner_group_valid, "{state:?}");
        assert_eq!(
            state.health().state,
            velnor_model::FleetHealthState::Degraded
        );

        // Simulate a burst of reconcile ticks inside the reset window: the
        // pacer must not send another request (Mock::expect(1) enforces it).
        for _ in 0..10 {
            observe_github_and_routing(
                &args,
                &mut journal,
                &mut pacing,
                tokio::time::Instant::now() + CONTROLLER_REMOTE_BUDGET,
            )
            .await
            .unwrap();
        }
        server.verify().await;

        // SAFETY: the test holds the process-wide GITHUB_TOKEN environment lock.
        unsafe { std::env::remove_var("GITHUB_TOKEN") };
        std::fs::remove_dir_all(dir).ok();
    }
}

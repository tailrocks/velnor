//! Per-scope controller: desired state, permits, slot/job child processes.
//!
//! Restarting this process must not stop existing slot or job workers: children
//! are spawned without kill-on-drop, and packaged units must not use
//! `PartOf=controller`. Every journal side effect is executed here.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Args;
use serde_json::json;
use velnor_control::journal::{Event, Journal, SideEffect, SlotRecord};
use velnor_model::{ActorPhase, FleetHealthState, Generation, JobId, SlotId};

use crate::config;
use crate::protocol::{GitHubScope, RegistrationClient};

use super::cleanup;
use super::complete;
use super::exec::load_exec_config;
use super::health::HealthServer;
use super::prove;
use super::slot::{heartbeat_path, slot_id, SlotHeartbeat};
use super::watchdog::{feed_after_cycle, LocalCycle};

/// Bound live JIT requests during startup/recovery without making the GitHub
/// API a burst target. This matches the bounded configure path.
const JIT_REGISTRATION_CONCURRENCY: usize = 4;
const REGISTRATION_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
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
const CONTROLLER_CHILD_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// A slot must publish fresh, generation-bound progress within this startup
/// bound before it can prove session liveness or readiness.
const SLOT_HEARTBEAT_MAX_AGE: Duration = Duration::from_secs(10);
const SUPERVISION_METRICS_INTERVAL: Duration = Duration::from_secs(1);

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

/// Fleet-wide REST pacing for the shared PAT, owned by the controller.
/// Read-only probes and JIT registration retries draw from the same budget:
/// when GitHub reports exhaustion (403/429 with rate-limit headers), every
/// paced call holds until the reset epoch, so a quota storm cannot feed on
/// its own retries. Health degrades visibly (`github_reachable: false`)
/// instead of silently burning budget.
#[derive(Debug)]
struct GithubPacing {
    next_probe: tokio::time::Instant,
    probe_failures: u32,
    /// Fleet-wide hold on JIT registration while the shared PAT is exhausted.
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

    /// Record a probe outcome and schedule the next probe. Rate-limited
    /// probes hold the whole fleet until the reported reset epoch.
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

    /// Fleet-wide REST hold until the GitHub reset epoch. Same duration
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
}

#[derive(Clone, Copy, Debug)]
struct MetricsSnapshot {
    slot_processes: usize,
    job_processes: usize,
    waiter_processes: usize,
    reconcile_duration_ms: u64,
}

impl Default for MetricsSnapshot {
    fn default() -> Self {
        Self {
            slot_processes: 0,
            job_processes: 0,
            waiter_processes: 0,
            reconcile_duration_ms: 1,
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
        slots: &HashMap<String, Child>,
        jobs: &HashMap<String, Child>,
        reconcile_duration_ms: u64,
    ) {
        let (job_processes, waiter_processes) = job_process_counts(jobs.keys());
        let mut state = self
            .state
            .lock()
            .expect("metrics snapshot mutex is not poisoned");
        state.snapshot.slot_processes = slots.len();
        state.snapshot.job_processes = job_processes;
        state.snapshot.waiter_processes = waiter_processes;
        state.snapshot.reconcile_duration_ms = reconcile_duration_ms.max(1);
    }

    async fn stop_and_publish(&mut self) -> anyhow::Result<()> {
        self.state
            .lock()
            .expect("metrics snapshot mutex is not poisoned")
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
    let mut state = state
        .lock()
        .expect("metrics snapshot mutex is not poisoned");
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
        current.reconcile_duration_ms,
    )?;
    Ok(true)
}

pub async fn run(args: ControllerArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.state_dir)?;
    let mut journal = Journal::open(args.state_dir.join("journal.db"))?;
    let server = HealthServer::bind(&args.state_dir)?;
    journal.apply(Event::ControlLive)?;
    journal.apply(Event::JournalWritable)?;
    journal.apply(Event::DesiredCapacity {
        ready: args.desired_ready,
    })?;
    let mut slots: HashMap<String, Child> = HashMap::new();
    let mut jobs: HashMap<String, Child> = HashMap::new();
    let mut heartbeats: HashMap<String, (u32, u64)> = HashMap::new();
    let mut startup_deadlines: HashMap<String, Instant> = HashMap::new();
    let mut last_registration_reconcile = Instant::now() - REGISTRATION_RECONCILE_INTERVAL;
    let mut last_outbox_reconcile = Instant::now() - OUTBOX_RECONCILIATION_INTERVAL;
    let mut pacing = GithubPacing::default();
    let mut ready_announced = false;
    let mut last_reconcile_duration_ms = 1;
    publish_controller_metrics(&args.state_dir, 0, 0, 0, 0, 1)?;
    let mut metrics = MetricsPublisher::start(&args.state_dir);
    loop {
        if crate::runner::draining() {
            drain_children(&journal, &mut slots, &mut jobs).await?;
            metrics.update(&slots, &jobs, last_reconcile_duration_ms);
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
        )
        .await?;
        let reconcile_duration_ms = cycle_started.elapsed().as_millis().max(1) as u64;
        last_reconcile_duration_ms = reconcile_duration_ms;
        metrics.update(&slots, &jobs, reconcile_duration_ms);
        let _ = feed_after_cycle(cycle, !ready_announced);
        ready_announced = true;
        if args.once {
            // Leave children running: a controller restart (or --once exit)
            // must not stop slot or job processes. Reap completed children
            // through the same ownership path used by the normal loop.
            reap(&mut slots);
            reap(&mut jobs);
            metrics.update(&slots, &jobs, reconcile_duration_ms);
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
    reconcile_p95_ms: u64,
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
        "reconcile_duration_ms": { "p95": reconcile_p95_ms },
        "journal": { "transactions": sequence.saturating_add(1), "wal_bytes": wal_bytes },
        "cpu": {
            "controller": { "user_us": user_us, "system_us": system_us },
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

/// Stop controller-owned slots, waiters, and stale job workers when the daemon
/// receives SIGTERM. Active in-flight job workers remain alive for completion.
async fn drain_children(
    journal: &Journal,
    slots: &mut HashMap<String, Child>,
    jobs: &mut HashMap<String, Child>,
) -> anyhow::Result<()> {
    for child in slots.values() {
        request_child_shutdown(child)?;
    }

    // Ready-slot broker waiters and real job workers share the `jobs` map.
    // Waiters have no durable job yet and must exit during a daemon drain;
    // active workers must survive so an upgrade cannot lose in-flight work.
    let active_job_ids: HashSet<String> = journal
        .materialized_state()?
        .jobs
        .into_iter()
        .filter(|job| {
            matches!(
                job.phase,
                ActorPhase::Assigned
                    | ActorPhase::Starting
                    | ActorPhase::Running
                    | ActorPhase::Completing
            )
        })
        .map(|job| job.job_id.0)
        .collect();
    for (job_id, child) in jobs.iter() {
        if is_drainable_job(job_id, &active_job_ids) {
            request_child_shutdown(child)?;
        }
    }
    let mut deadline = Instant::now() + CONTROLLER_CHILD_DRAIN_TIMEOUT;
    let mut escalated = false;
    loop {
        reap_draining(slots, "slot")?;
        reap_draining_jobs(jobs, &active_job_ids)?;
        if slots.is_empty()
            && jobs
                .keys()
                .all(|job_id| !is_drainable_job(job_id, &active_job_ids))
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
                            .filter(|job_id| is_drainable_job(job_id, &active_job_ids))
                            .count()
                );
            }
            kill_draining(slots, "slot")?;
            kill_draining_jobs(jobs, &active_job_ids)?;
            eprintln!("controller child drain escalated to SIGKILL");
            escalated = true;
            deadline = Instant::now() + CONTROLLER_CHILD_DRAIN_TIMEOUT;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn is_drainable_job(job_id: &str, active_job_ids: &HashSet<String>) -> bool {
    job_id.starts_with("wait-") || !active_job_ids.contains(job_id)
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
    slots: &mut HashMap<String, Child>,
    jobs: &mut HashMap<String, Child>,
    heartbeats: &mut HashMap<String, (u32, u64)>,
    startup_deadlines: &mut HashMap<String, Instant>,
    last_registration_reconcile: &mut Instant,
    last_outbox_reconcile: &mut Instant,
    pacing: &mut GithubPacing,
    metrics: &MetricsPublisher,
) -> anyhow::Result<LocalCycle> {
    let remote_deadline = tokio::time::Instant::now() + CONTROLLER_REMOTE_BUDGET;
    let total = args.desired_ready;
    let mut effects = Vec::new();
    reap(slots);
    reap(jobs);
    // Ingest a surviving slot's heartbeat before deciding whether its permit
    // needs repair. On controller restart the child handle is gone, so the
    // heartbeat is the only fresh local proof that prevents a double spawn.
    ingest_slot_heartbeats(args, journal, total as usize, heartbeats)?;
    let state = journal.materialized_state()?;
    for index in 1..=total {
        let id = slot_id(&args.scope, index as usize);
        let slot = state.slots.iter().find(|slot| slot.slot_id == id);
        let generation = slot
            .map(|slot| slot.generation)
            .unwrap_or(Generation::INITIAL);
        let fenced = slot.is_some_and(|slot| slot.phase == ActorPhase::Fenced);
        let admission_blocked = slot_has_admission_block(&state, &id, generation);
        let heartbeat_fresh = prove::slot_heartbeat_is_fresh(
            &args.state_dir,
            &id,
            generation,
            SLOT_HEARTBEAT_MAX_AGE,
        );
        if heartbeat_fresh {
            startup_deadlines.remove(&id.0);
        } else if args.spawn_slots
            && !fenced
            && !admission_blocked
            && stale_slot_deadline_reached(args, slot, &id, startup_deadlines, Instant::now())
        {
            fence_stale_slot_actor(args, journal, slots, &id, generation).await?;
            startup_deadlines.remove(&id.0);
            continue;
        }
        if fenced && args.spawn_slots {
            terminate_fenced_slot_actor(args, slots, &id, slot.expect("fenced slot")).await?;
        }
        let fenced_generation = fenced_slot_recovery_generation(slot, &state, jobs);
        let generation = fenced_generation.unwrap_or(generation);
        let process_alive = heartbeat_fresh;
        if fenced && fenced_generation.is_none() {
            continue;
        }
        if fenced_generation.is_none()
            && (admission_blocked
                || child_owns_slot(&state, jobs, &id)
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
            jobs,
            startup_deadlines,
            &mut *pacing,
            remote_deadline,
            command,
        )
        .await?;
    }

    metrics.update(slots, jobs, 1);

    observe_github_and_routing(args, journal, pacing, remote_deadline).await?;

    if last_registration_reconcile.elapsed() >= REGISTRATION_RECONCILE_INTERVAL {
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

    let mut proof_effects = Vec::new();
    let execution = crate::execution::load_execution_file(&args.state_dir, None)?;
    let executor = prove::observe_executor(&args.state_dir, execution.backend());
    let snapshot = journal.materialized_state()?;
    let now = tokio::time::Instant::now();
    for index in 1..=total {
        let id = slot_id(&args.scope, index as usize);
        let generation = snapshot
            .slots
            .iter()
            .find(|slot| slot.slot_id == id)
            .map(|slot| slot.generation)
            .unwrap_or(Generation::INITIAL);
        if executor {
            proof_effects.extend(
                journal
                    .apply(Event::ExecutorProven {
                        slot_id: id.clone(),
                        generation,
                    })?
                    .commands,
            );
        }
        if prove::slot_heartbeat_is_fresh(&args.state_dir, &id, generation, SLOT_HEARTBEAT_MAX_AGE)
        {
            proof_effects.extend(
                journal
                    .apply(Event::SessionLive {
                        slot_id: id.clone(),
                        generation,
                    })?
                    .commands,
            );
        }
        let state = journal.materialized_state()?;
        if let Some(slot) = state.slots.iter().find(|slot| slot.slot_id == id)
            && slot.ready_proof().is_ok()
            && !slot.registered
            && pacing.registration_due(&id.0, now)
        {
            proof_effects.extend(
                journal
                    .apply(Event::RegistrationIntended {
                        slot_id: id,
                        generation,
                    })?
                    .commands,
            );
        }
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
                    jobs,
                    startup_deadlines,
                    &mut *pacing,
                    remote_deadline,
                    command,
                )
                .await?
            }
        }
    }
    register_runners(args, journal, pacing, registrations, remote_deadline).await?;

    spawn_ready_waiters(args, journal, jobs)?;
    reap(jobs);
    let outbox_reconcile_due = last_outbox_reconcile.elapsed() >= OUTBOX_RECONCILIATION_INTERVAL;
    reclaim_orphaned_jobs(args, journal, remote_deadline, outbox_reconcile_due).await?;
    if outbox_reconcile_due {
        reconcile_orphaned_outboxes(args, journal)?;
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
    slots: &mut HashMap<String, Child>,
    jobs: &mut HashMap<String, Child>,
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
        SideEffect::StartJob { job_id, generation } => {
            maybe_spawn_job(args, journal, jobs, &job_id.0, generation.0, None)
        }
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
    .await
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
) -> anyhow::Result<()> {
    if registrations.is_empty() {
        return Ok(());
    }
    super::scheduler::production_scheduler().ensure_current()?;
    let exec = match load_exec_config(&args.state_dir) {
        Ok(exec) => exec,
        Err(error) => {
            eprintln!("JIT registration skipped: cannot load daemon execution config: {error:#}");
            return Ok(());
        }
    };

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
            slot_id,
            generation,
        })?;
        for nested in ready.commands {
            if let SideEffect::AdvertiseCapacity { permits } = nested {
                std::fs::write(
                    args.state_dir.join("advertised-capacity"),
                    permits.to_string(),
                )?;
            }
        }
    }
    Ok(())
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
        let remote = match client.get_runner(&scope, pat, local_id).await {
            Ok(remote) => remote,
            Err(error) => {
                if let Some(quota) = crate::protocol::github_api_quota_status(&error) {
                    pacing.hold_rest_until(
                        tokio::time::Instant::now(),
                        quota.reset_epoch_or_retry_after(epoch_now()),
                    );
                }
                eprintln!(
                    "registration reconciliation lookup failed for runner id {local_id}: {error:#}"
                );
                continue;
            }
        };
        if remote.is_none() {
            // Registration loss invalidates the broker worker even while it
            // owns a job. The durable event releases admission first; the
            // worker remains journal-owned until normal teardown reconciles it.
            lost.push((slot.slot_id.clone(), slot.generation));
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
        // Repo-scoped fleets derive policy from the URL (operator override
        // on disk wins). Org-scoped fleets load the generated
        // `<org>-desired-policy.json` allowlist every cycle and replace
        // `routing-policy.json`. Never snapshot live group membership: a
        // truncated GitHub group would become the desired baseline and hide
        // drift.
        let repo_policy = exec.url.as_deref().and_then(|url| {
            prove::policy_from_github_url(url, group.clone(), exec.labels.clone(), trust.clone())
        });
        let policy = if let Some(path) = exec.routing_policy_file.as_deref() {
            let policy = prove::read_policy_file(path)?;
            prove::write_policy(&args.state_dir, &policy)?;
            Some(policy)
        } else if let Some(policy) = repo_policy {
            prove::write_policy_if_absent(&args.state_dir, &policy)?;
            Some(policy)
        } else if let Some(url) = exec.url.as_deref() {
            if let Ok(scope) = crate::protocol::GitHubScope::parse(url) {
                if let Some(org) = scope.org_login() {
                    let generated = prove::org_policy_from_generated(
                        org,
                        exec.labels.clone(),
                        trust.clone(),
                        &prove::generated_policy_dir(),
                    );
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

/// False when no live probe can run (no exec config, URL, or token): the old
/// behavior of stamping `github_reachable: false` every cycle is preserved.
fn probe_configured(args: &ControllerArgs) -> bool {
    let Ok(exec) = load_exec_config(&args.state_dir) else {
        return false;
    };
    exec.url.is_some() && exec.pat.is_some()
}

/// GitHub session waiters for Ready slots. Do not apply Assigned: REST
/// queued ids are not broker job ids, and Ready must stay Ready until
/// `accept_job` on the broker GUID.
fn spawn_ready_waiters(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
) -> anyhow::Result<()> {
    if load_exec_config(&args.state_dir).is_err() {
        return Ok(());
    }
    let state = journal.materialized_state()?;
    for slot in &state.slots {
        if slot.phase != ActorPhase::Ready {
            continue;
        }
        if state.jobs.iter().any(|job| {
            job.slot_id == slot.slot_id
                && matches!(
                    job.phase,
                    ActorPhase::Assigned
                        | ActorPhase::Starting
                        | ActorPhase::Running
                        | ActorPhase::Completing
                )
        }) {
            continue;
        }
        let waiter_id = format!("wait-{}", slot.slot_id.0);
        if jobs.contains_key(&waiter_id) {
            continue;
        }
        maybe_spawn_job(
            args,
            journal,
            jobs,
            &waiter_id,
            slot.generation.0,
            Some(&slot.slot_id),
        )?;
    }
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
) -> anyhow::Result<()> {
    let state = journal.materialized_state()?;
    let orphan_jobs: Vec<_> = state
        .jobs
        .iter()
        .filter(|job| {
            matches!(
                job.phase,
                ActorPhase::Assigned
                    | ActorPhase::Starting
                    | ActorPhase::Running
                    | ActorPhase::Completing
            )
        })
        // A ready-slot waiter is spawned before GitHub assigns a job, so its
        // durable ownership marker is keyed by the waiter identity rather
        // than the later journal job id. Check both markers independently:
        // a stale job marker must not suppress a live waiter marker.
        .filter(|job| {
            let waiter_id = format!("wait-{}", job.slot_id.0);
            let job_worker_live =
                cleanup::read_owned_pid(&args.state_dir, &job.job_id.0, job.generation.0)
                    .is_some_and(prove::pid_is_alive);
            let waiter_live =
                cleanup::read_owned_pid(&args.state_dir, &waiter_id, job.generation.0)
                    .is_some_and(prove::pid_is_alive);
            !(job_worker_live || waiter_live)
        })
        .cloned()
        .collect();

    if orphan_jobs.is_empty() && !scan_persisted_markers {
        return Ok(());
    }

    let exec = match load_exec_config(&args.state_dir) {
        Ok(exec) => Some(exec),
        Err(error)
            if orphan_jobs.is_empty()
                || orphan_jobs.iter().all(|job| {
                    job.phase == ActorPhase::Completing
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

    for job in orphan_jobs {
        let slot_dir = recovery_slot_config_dir(&args.state_dir, &exec, &state, &job.slot_id)?;
        let marker_job_id = crate::runner::recorded_in_flight_job_id(&slot_dir)?;
        let pending_completion = job.phase == ActorPhase::Completing
            && state.outbox.iter().any(|row| {
                row.job_id == job.job_id
                    && row.generation == job.generation
                    && row.intended
                    && !row.remote_acked
            });
        if let Some(marker_job_id) = marker_job_id.as_deref()
            && marker_job_id != job.job_id.0
        {
            return Err(anyhow::anyhow!(
                "in-flight marker job {} does not match orphan job {}",
                marker_job_id,
                job.job_id.0
            ));
        }
        if job.phase == ActorPhase::Completing
            && !pending_completion
            // A job whose terminal result is durable but whose payload is not
            // is the crash window between the two writes, not a lost payload.
            // Its real conclusion is recorded, so recovery re-drives the
            // completion instead of failing the whole reconciliation.
            && job.terminal_conclusion.is_none()
        {
            return Err(anyhow::anyhow!(
                "completing job {} has no exact durable completion payload",
                job.job_id.0
            ));
        }
        if let Some(stored) = load_local_runner_config(&slot_dir)? {
            if marker_job_id.is_some() {
                let cleanup = if pending_completion {
                    match defer_remote_recovery_on_timeout(
                        remaining_remote_budget(remote_deadline),
                        crate::runner::replay_recorded_completion(
                            &slot_dir,
                            &stored,
                            &args.state_dir,
                        ),
                        "replay recorded completion during orphan recovery",
                    )
                    .await
                    {
                        Ok(cleanup) => cleanup,
                        Err(error) => {
                            // A completion that cannot be delivered is not a
                            // controller failure. The completion path already
                            // charged this attempt to the row's durable
                            // budget; propagating instead would re-run the
                            // same doomed replay on every cycle and never let
                            // the row terminate.
                            eprintln!(
                                "Warning: completion replay for job {} failed: {error:#}",
                                job.job_id.0
                            );
                            if let Some(row) = journal
                                .materialized_state()?
                                .outbox
                                .into_iter()
                                .find(|row| {
                                    row.job_id == job.job_id && row.generation == job.generation
                                })
                            {
                                abandon_if_budget_spent(args, journal, &row)?;
                            }
                            continue;
                        }
                    }
                } else {
                    let recovery = if let Some(conclusion) = job.terminal_conclusion.as_deref() {
                        defer_remote_recovery_on_timeout(
                            remaining_remote_budget(remote_deadline),
                            crate::runner::complete_recorded_in_flight_job_with_terminal_conclusion(
                                &slot_dir, &stored, conclusion,
                            ),
                            "complete recorded terminal conclusion during orphan recovery",
                        )
                        .await?
                    } else {
                        defer_remote_recovery_on_timeout(
                            remaining_remote_budget(remote_deadline),
                            crate::runner::complete_recorded_in_flight_job(&slot_dir, &stored),
                            "complete recorded in-flight job during orphan recovery",
                        )
                        .await?
                    };
                    recovery
                };
                let Some(cleanup) = cleanup else {
                    continue;
                };
                if cleanup {
                    eprintln!(
                        "Recovered stale in-flight job {} before restoring slot {}",
                        job.job_id.0, job.slot_id.0
                    );
                }
            }
        } else if marker_job_id.is_some() {
            return Err(anyhow::anyhow!(
                "runner credentials missing while recovering in-flight job {}",
                job.job_id.0
            ));
        }
        if crate::runner::recorded_in_flight_job_exists(&slot_dir)? && pending_completion {
            return Err(anyhow::anyhow!(
                "completing job {} retained its in-flight marker after recovery",
                job.job_id.0
            ));
        }
        let current = journal.materialized_state()?;
        if current.jobs.iter().all(|row| row.job_id != job.job_id) {
            // complete_recorded_in_flight_job already committed the terminal
            // acknowledgement and removed this job from the journal.
            continue;
        }
        if pending_completion {
            return Err(anyhow::anyhow!(
                "completing job {} has no recoverable terminal acknowledgement",
                job.job_id.0
            ));
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
    }

    if !scan_persisted_markers {
        return Ok(());
    }

    // A prior RemoteAcked event removes the journal job before local storage
    // release and marker deletion. Scan every exact persisted slot path so a
    // crash in that gap remains retryable even though no job row remains.
    let current = journal.materialized_state()?;
    for slot in &state.slots {
        let slot_dir = recovery_slot_config_dir(&args.state_dir, &exec, &state, &slot.slot_id)?;
        let Some(marker_job_id) = crate::runner::recorded_in_flight_job_id(&slot_dir)? else {
            continue;
        };
        if let Some(job) = current
            .jobs
            .iter()
            .find(|job| job.job_id.0 == marker_job_id)
        {
            if job.slot_id != slot.slot_id || job.generation != slot.generation {
                return Err(anyhow::anyhow!(
                    "in-flight marker job {} does not match journal slot {} generation {}",
                    marker_job_id,
                    slot.slot_id.0,
                    slot.generation.0
                ));
            }
            continue;
        }
        let remote_acked =
            journal.has_remote_terminal_ack(&JobId(marker_job_id.clone()), slot.generation)?;
        if !remote_acked {
            // A ready-slot waiter persists the in-flight marker before journal
            // admission while its ownership pid is still keyed `wait-{slot}`;
            // the `write_owned_pid(job_id)` marker appears only when the job
            // worker spawns. Consult both markers independently: a live owner
            // defers recovery, and only a genuinely dead worker and waiter
            // make the marker safe to reclaim.
            let waiter_id = format!("wait-{}", slot.slot_id.0);
            let worker_pid =
                cleanup::read_owned_pid(&args.state_dir, &marker_job_id, slot.generation.0);
            let waiter_pid =
                cleanup::read_owned_pid(&args.state_dir, &waiter_id, slot.generation.0);
            if worker_pid.is_none() && waiter_pid.is_none() {
                return Err(anyhow::anyhow!(
                    "marker-only in-flight job {} has no valid worker or waiter ownership pid",
                    marker_job_id
                ));
            }
            if worker_pid.is_some_and(prove::pid_is_alive)
                || waiter_pid.is_some_and(prove::pid_is_alive)
            {
                // The marker can legitimately exist between persistence and
                // journal admission. Never terminalize a live owner in that
                // window; the next reconciliation tick will retry.
                continue;
            }
            let Some(stored) = load_local_runner_config(&slot_dir)? else {
                return Err(anyhow::anyhow!(
                    "runner credentials missing while recovering marker-only in-flight job {}",
                    marker_job_id
                ));
            };
            let Some(cleaned) = defer_remote_recovery_on_timeout(
                remaining_remote_budget(remote_deadline),
                crate::runner::complete_recorded_in_flight_job_after_journal_acceptance(
                    &slot_dir, &stored,
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
        cleanup::remove_outbox(&args.state_dir, &marker_job_id, slot.generation.0)?;
        crate::runner::cleanup_recorded_in_flight_job(&slot_dir)?;
        if crate::runner::recorded_in_flight_job_exists(&slot_dir)? {
            return Err(anyhow::anyhow!(
                "orphaned in-flight marker remained for job {}",
                marker_job_id
            ));
        }
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
/// `CompletionIntended`. A live ownership marker keeps the writer's file from
/// being deleted during that tiny window; a dead or absent owner makes the
/// file safe to remove because no journal intent can authorize a send.
fn reconcile_orphaned_outboxes(args: &ControllerArgs, journal: &Journal) -> anyhow::Result<()> {
    let outbox_dir = args.state_dir.join("outbox");
    let metadata = match std::fs::symlink_metadata(&outbox_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(anyhow::anyhow!(
                "completion outbox directory must not be a symlink: {}",
                outbox_dir.display()
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(anyhow::anyhow!(
                "completion outbox path is not a directory: {}",
                outbox_dir.display()
            ));
        }
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let _ = metadata;
    let state = journal.materialized_state()?;
    for entry in std::fs::read_dir(&outbox_dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("completion outbox filename is not UTF-8"))?;
        let file_metadata = std::fs::symlink_metadata(&path)?;
        #[cfg(unix)]
        if cleanup::outbox_quarantine_pid(name)?.is_some() {
            if file_metadata.file_type().is_symlink() || !file_metadata.is_dir() {
                return Err(anyhow::anyhow!(
                    "completion outbox quarantine is not a directory: {}",
                    path.display()
                ));
            }
            if !cleanup::remove_stale_outbox_quarantine(&args.state_dir, name)? {
                continue;
            }
            continue;
        }
        if file_metadata.file_type().is_symlink() || !file_metadata.is_file() {
            return Err(anyhow::anyhow!(
                "completion outbox entry is not a regular file: {}",
                path.display()
            ));
        }
        let (job_id, generation, temporary) = parse_outbox_entry_name(name)?;
        let row = state
            .outbox
            .iter()
            .find(|row| row.job_id.0 == job_id && row.generation.0 == generation);
        let keep = if temporary {
            outbox_owner_is_live(&args.state_dir, &job_id, generation)?
        } else {
            row.is_some_and(|row| row.intended && !row.remote_acked)
                || row.is_none() && outbox_owner_is_live(&args.state_dir, &job_id, generation)?
        };
        if keep {
            continue;
        }
        if temporary {
            std::fs::remove_file(&path)?;
            #[cfg(unix)]
            std::fs::OpenOptions::new()
                .read(true)
                .open(&outbox_dir)?
                .sync_all()?;
        } else {
            cleanup::remove_outbox(&args.state_dir, &job_id, generation)?;
        }
    }
    Ok(())
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

fn outbox_owner_is_live(state_dir: &Path, job_id: &str, generation: u64) -> anyhow::Result<bool> {
    let path = cleanup::owned_path(state_dir, job_id, generation);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(anyhow::anyhow!(
                "ownership marker must not be a symlink: {}",
                path.display()
            ));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(anyhow::anyhow!(
                "ownership marker is not a regular file: {}",
                path.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    let pid = cleanup::read_owned_pid(state_dir, job_id, generation)
        .ok_or_else(|| anyhow::anyhow!("ownership marker has no valid pid: {}", path.display()))?;
    Ok(prove::pid_is_alive(pid))
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
    state: &velnor_control::journal::FleetState,
    jobs: &HashMap<String, Child>,
) -> Option<Generation> {
    let slot = slot.filter(|slot| slot.phase == ActorPhase::Fenced)?;
    if slot_has_admission_block(state, &slot.slot_id, slot.generation)
        || child_owns_slot(state, jobs, &slot.slot_id)
    {
        return None;
    }
    Some(slot.generation.next())
}

fn slot_has_admission_block(
    state: &velnor_control::journal::FleetState,
    slot_id: &SlotId,
    generation: Generation,
) -> bool {
    state.jobs.iter().any(|job| {
        job.slot_id == *slot_id
            && matches!(
                job.phase,
                ActorPhase::Assigned
                    | ActorPhase::Starting
                    | ActorPhase::Running
                    | ActorPhase::Completing
            )
    }) || state
        .outbox
        .iter()
        .any(|row| row.is_pending() && row.slot_id == *slot_id && row.generation == generation)
}

fn child_owns_slot(
    state: &velnor_control::journal::FleetState,
    jobs: &HashMap<String, Child>,
    slot_id: &SlotId,
) -> bool {
    let waiter_id = format!("wait-{}", slot_id.0);
    jobs.contains_key(&waiter_id)
        || state
            .jobs
            .iter()
            .any(|job| job.slot_id == *slot_id && jobs.contains_key(&job.job_id.0))
}

async fn terminate_fenced_slot_actor(
    args: &ControllerArgs,
    slots: &mut HashMap<String, Child>,
    slot_id: &SlotId,
    slot: &SlotRecord,
) -> anyhow::Result<()> {
    if slots.contains_key(&slot_id.0) {
        request_child_shutdown(slots.get(&slot_id.0).expect("child still present"))?;
        let mut deadline = Instant::now() + FENCED_SLOT_TERMINATION_TIMEOUT;
        let mut escalated = false;
        loop {
            if slots
                .get_mut(&slot_id.0)
                .expect("child retained until reap")
                .try_wait()?
                .is_some()
            {
                slots.remove(&slot_id.0);
                return Ok(());
            }
            if Instant::now() >= deadline {
                if escalated {
                    anyhow::bail!(
                        "fenced slot {:?} child failed to reap after SIGKILL; handle retained",
                        slot_id
                    );
                }
                slots
                    .get_mut(&slot_id.0)
                    .expect("child retained until escalation")
                    .kill()
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "fenced slot {:?} SIGKILL escalation failed; handle retained: {error}",
                            slot_id
                        )
                    })?;
                eprintln!("fenced slot {:?} shutdown escalated to SIGKILL", slot_id);
                escalated = true;
                deadline = Instant::now() + FENCED_SLOT_TERMINATION_TIMEOUT;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    let Some(pid) = slot.pid else {
        return Ok(());
    };
    if !prove::slot_process_is_alive(pid, &args.state_dir, slot_id, slot.generation) {
        return Ok(());
    }
    send_pid_signal(pid, libc::SIGTERM)?;
    let deadline = Instant::now() + FENCED_SLOT_TERMINATION_TIMEOUT;
    while Instant::now() < deadline {
        if !prove::slot_process_is_alive(pid, &args.state_dir, slot_id, slot.generation) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Re-prove the command line immediately before escalation. If the PID was
    // reused, leave the unrelated process untouched and still rotate the
    // durable generation below.
    if prove::slot_process_is_alive(pid, &args.state_dir, slot_id, slot.generation) {
        send_pid_signal(pid, libc::SIGKILL)?;
    }
    Ok(())
}

#[cfg(unix)]
fn send_pid_signal(pid: u32, signal: libc::c_int) -> anyhow::Result<()> {
    if pid == 0 {
        return Ok(());
    }
    // SAFETY: callers prove the PID belongs to the fenced Velnor slot actor
    // immediately before signaling it; SIGTERM is followed by a re-proof
    // before SIGKILL.
    let result = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn send_pid_signal(_pid: u32, _signal: i32) -> anyhow::Result<()> {
    anyhow::bail!("fenced slot recovery requires Unix process signaling")
}

fn stale_slot_deadline_reached(
    _args: &ControllerArgs,
    slot: Option<&SlotRecord>,
    id: &SlotId,
    deadlines: &mut HashMap<String, Instant>,
    now: Instant,
) -> bool {
    let Some(slot) = slot else {
        return false;
    };
    if prove::slot_heartbeat_is_fresh(
        &_args.state_dir,
        id,
        slot.generation,
        SLOT_HEARTBEAT_MAX_AGE,
    ) {
        return false;
    }
    let deadline = deadlines
        .entry(id.0.clone())
        .or_insert(now + SLOT_HEARTBEAT_MAX_AGE);
    *deadline <= now
}

async fn fence_stale_slot_actor(
    args: &ControllerArgs,
    journal: &mut Journal,
    slots: &mut HashMap<String, Child>,
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
    let slot = journal
        .materialized_state()?
        .slots
        .into_iter()
        .find(|slot| {
            slot.slot_id == *id && slot.generation == generation && slot.phase == ActorPhase::Fenced
        })
        .ok_or_else(|| anyhow::anyhow!("stale slot {id:?} was not fenced"))?;
    terminate_fenced_slot_actor(args, slots, id, &slot).await
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
    seen: &mut HashMap<String, (u32, u64)>,
) -> anyhow::Result<()> {
    let state = journal.materialized_state()?;
    let mut pending = Vec::new();
    for index in 1..=total {
        let path = heartbeat_path(&args.state_dir, index);
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let id = slot_id(&args.scope, index);
        let Some(slot) = state.slots.iter().find(|slot| slot.slot_id == id) else {
            continue;
        };
        let Ok(heartbeat) = serde_json::from_slice::<SlotHeartbeat>(&bytes) else {
            continue;
        };
        if slot.generation.0 != heartbeat.generation
            || !prove::slot_heartbeat_is_fresh(
                &args.state_dir,
                &id,
                slot.generation,
                SLOT_HEARTBEAT_MAX_AGE,
            )
            || seen.get(&id.0).is_some_and(|(pid, sequence)| {
                *pid == heartbeat.pid && *sequence >= heartbeat.sequence
            })
        {
            continue;
        }
        pending.push((id, heartbeat));
    }
    let outcomes =
        journal.apply_many(pending.iter().map(|(id, heartbeat)| Event::SlotHeartbeat {
            slot_id: id.clone(),
            generation: Generation(heartbeat.generation),
            pid: heartbeat.pid,
        }))?;
    for ((id, heartbeat), outcome) in pending.into_iter().zip(outcomes) {
        if !outcome.rejected {
            seen.insert(id.0, (heartbeat.pid, heartbeat.sequence));
        }
    }
    Ok(())
}

fn maybe_spawn_slot(
    args: &ControllerArgs,
    journal: &Journal,
    children: &mut HashMap<String, Child>,
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
    if let Ok(state) = journal.materialized_state()
        && let Some(slot) = state.slots.iter().find(|slot| slot.slot_id == *slot_id)
        && slot.pid.is_some_and(|pid| {
            prove::slot_process_is_alive(pid, &args.state_dir, slot_id, generation)
        })
    {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
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
    children.insert(slot_id.0.clone(), child);
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
    if cleanup::read_owned_pid(&args.state_dir, job_id, generation.0)
        .is_some_and(prove::pid_is_alive)
    {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let slot_index = slot_index_from_id(&slot_id);
    let child = Command::new(exe)
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
        .spawn()?;
    if let Err(error) = cleanup::write_owned_pid(&args.state_dir, job_id, generation.0, child.id())
    {
        let mut child = child;
        let kill_result = child.kill();
        let wait_result = child.wait();
        let cleanup_result = cleanup::remove_owned(&args.state_dir, job_id, generation.0);
        return Err(error.context(format!(
            "failed to publish ownership marker for job {job_id}; child cleanup: kill={kill_result:?}, wait={wait_result:?}, marker={cleanup_result:?}"
        )));
    }
    jobs.insert(job_id.to_owned(), child);
    Ok(())
}

fn reap(children: &mut HashMap<String, Child>) {
    let mut dead = Vec::new();
    for (id, child) in children.iter_mut() {
        match child.try_wait() {
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

fn reap_draining(children: &mut HashMap<String, Child>, kind: &str) -> anyhow::Result<()> {
    let mut dead = Vec::new();
    for (id, child) in children.iter_mut() {
        if child
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

fn kill_draining(children: &mut HashMap<String, Child>, kind: &str) -> anyhow::Result<()> {
    for (id, child) in children.iter_mut() {
        child.kill().map_err(|error| {
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
) -> anyhow::Result<()> {
    let mut dead = Vec::new();
    for (job_id, child) in jobs.iter_mut() {
        if !is_drainable_job(job_id, active_job_ids) {
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
) -> anyhow::Result<()> {
    for (job_id, child) in jobs
        .iter_mut()
        .filter(|(job_id, _)| is_drainable_job(job_id, active_job_ids))
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
pub async fn supervise_from_daemon(
    state_dir: PathBuf,
    scope: String,
    desired_ready: u32,
    once: bool,
) -> anyhow::Result<()> {
    run(ControllerArgs {
        state_dir,
        scope,
        desired_ready,
        once,
        spawn_slots: true,
    })
    .await
}

#[cfg(test)]
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

    fn prime_pending_completion(journal: &mut Journal) -> (JobId, Generation, String) {
        let slot_id = SlotId("velnor-1".to_owned());
        let job_id = JobId("job-1".to_owned());
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
            Event::Assigned {
                slot_id: slot_id.clone(),
                job_id: job_id.clone(),
                generation,
            },
            Event::JobOwned {
                job_id: job_id.clone(),
                slot_id,
                attempt: 1,
                generation,
                worker: "worker-1".to_owned(),
                accepted_unix: 1,
            },
            Event::JobStarted {
                job_id: job_id.clone(),
                generation,
            },
            Event::JobTerminalResult {
                job_id: job_id.clone(),
                generation,
                conclusion: "success".to_owned(),
            },
        ] {
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

    fn controller_test_args(state_dir: PathBuf) -> ControllerArgs {
        ControllerArgs {
            state_dir,
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
        }
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
        assert_eq!(state.slots[0].phase, ActorPhase::Ready);
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
            "job_cpus": "",
            "job_memory": "",
            "trust_scope": "trusted",
            "emergency_reserve_bytes": 0,
            "job_peak_bytes": 0,
            "node_action_image": "img",
            "skip_preflight": false,
            "require_docker_socket": false
        }))
        .unwrap()
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
        publish_controller_metrics(&dir, 1, 2, job_processes, waiter_processes, 7).unwrap();

        let metrics: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("controller-metrics.json")).unwrap())
                .unwrap();
        assert_eq!(metrics["job_processes"], json!(2));
        assert_eq!(metrics["waiter_processes"], json!(1));

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
        publish_controller_metrics(&dir, 1, 0, 0, 0, 1).unwrap();
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
            Event::Assigned {
                slot_id: SlotId("velnor-1".to_owned()),
                job_id: JobId("job-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::JobOwned {
                job_id: JobId("job-1".to_owned()),
                slot_id: SlotId("velnor-1".to_owned()),
                attempt: 1,
                generation: Generation::INITIAL,
                worker: "worker-1".to_owned(),
                accepted_unix: 1_234,
            },
            Event::JobStarted {
                job_id: JobId("job-1".to_owned()),
                generation: Generation::INITIAL,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }

        let child = || Command::new("sleep").arg("5").spawn().unwrap();
        let mut slots = HashMap::from([(String::from("velnor-1"), child())]);
        let mut jobs = HashMap::from([
            (String::from("job-1"), child()),
            (String::from("wait-velnor-1"), child()),
            (String::from("stale-job"), child()),
        ]);
        let state = journal.materialized_state().unwrap();
        assert!(child_owns_slot(
            &state,
            &jobs,
            &SlotId("velnor-1".to_owned())
        ));
        assert_eq!(
            job_child_keys_for_slot(&jobs, &state, &SlotId("velnor-1".to_owned())),
            vec!["job-1".to_owned(), "wait-velnor-1".to_owned()]
        );

        let started = Instant::now();
        drain_children(&journal, &mut slots, &mut jobs)
            .await
            .unwrap();
        let active_preserved = jobs.get_mut("job-1").unwrap().try_wait().unwrap().is_none();
        let only_active_handle_remains = jobs.len() == 1 && jobs.contains_key("job-1");
        for child in jobs.values_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }

        assert!(started.elapsed() < CONTROLLER_CHILD_DRAIN_TIMEOUT);
        assert!(slots.is_empty());
        assert!(active_preserved);
        assert!(only_active_handle_remains);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn metrics_publisher_synchronously_writes_the_final_snapshot() {
        let dir = metrics_test_dir("final-snapshot");
        let mut publisher = MetricsPublisher::start(&dir);
        publisher.update(&HashMap::new(), &HashMap::new(), 42);

        publisher.stop_and_publish().await.unwrap();

        let metrics: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("controller-metrics.json")).unwrap())
                .unwrap();
        assert_eq!(metrics["reconcile_duration_ms"]["p95"], json!(42));
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
            phase: ActorPhase::Provisioning,
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
        let mut slot = reserved_slot();
        let state = FleetState::default();
        let children = HashMap::new();
        assert_eq!(
            fenced_slot_recovery_generation(Some(&slot), &state, &children),
            None
        );

        slot.phase = ActorPhase::Fenced;
        assert_eq!(
            fenced_slot_recovery_generation(Some(&slot), &state, &children),
            Some(Generation(slot.generation.0 + 1))
        );
        assert_eq!(
            fenced_slot_recovery_generation(None, &state, &children),
            None
        );
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

        let mut stale_worker = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let stale_pid = stale_worker.id();
        assert!(stale_worker.wait().unwrap().success());
        assert!(!prove::pid_is_alive(stale_pid));

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
            Event::Assigned {
                slot_id: SlotId("velnor-1".to_owned()),
                job_id: JobId("job-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::JobOwned {
                job_id: JobId("job-1".to_owned()),
                slot_id: SlotId("velnor-1".to_owned()),
                attempt: 1,
                generation: Generation::INITIAL,
                worker: "worker-1".to_owned(),
                accepted_unix: 1_234,
            },
            Event::JobStarted {
                job_id: JobId("job-1".to_owned()),
                generation: Generation::INITIAL,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        cleanup::write_owned_pid(&dir, "job-1", Generation::INITIAL.0, stale_pid).unwrap();
        cleanup::write_owned_pid(
            &dir,
            "wait-velnor-1",
            Generation::INITIAL.0,
            std::process::id(),
        )
        .unwrap();

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
        };
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
        )
        .await
        .unwrap();

        let state = journal.load_state().unwrap();
        let job = state
            .jobs
            .iter()
            .find(|job| job.job_id == JobId("job-1".to_owned()))
            .unwrap();
        assert_eq!(job.phase, ActorPhase::Running);

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
        let journal = ready_slot_journal(&dir);
        write_exec_config(&dir, &dummy_exec("https://github.com/tailrocks/fixture"), 2).unwrap();
        let slot_dir = dir.join("slots").join("slot-1");
        std::fs::create_dir_all(&slot_dir).unwrap();
        std::fs::write(
            slot_dir.join("in-flight-job.json"),
            serde_json::to_vec(&json!({
                "plan_id": "plan-1",
                "job_id": "job-9",
                "run_service_url": "https://example.invalid/run-service",
                "billing_owner_id": null
            }))
            .unwrap(),
        )
        .unwrap();
        (dir, journal, slot_dir)
    }

    fn stale_pid() -> u32 {
        let mut stale_worker = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = stale_worker.id();
        assert!(stale_worker.wait().unwrap().success());
        assert!(!prove::pid_is_alive(pid));
        pid
    }

    #[tokio::test]
    async fn marker_only_in_flight_job_defers_to_live_waiter_pid() {
        let (dir, mut journal, slot_dir) = marker_only_recovery_fixture("live-waiter");
        // Crash window: the waiter persisted the in-flight marker before
        // journal admission, so ownership is still keyed `wait-{slot}` and no
        // `job-9` worker marker exists yet. Recovery must defer to the live
        // waiter instead of hard-erroring on the missing worker marker.
        cleanup::write_owned_pid(
            &dir,
            "wait-velnor-1",
            Generation::INITIAL.0,
            std::process::id(),
        )
        .unwrap();

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
        };
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
        )
        .await
        .unwrap();

        assert!(
            slot_dir.join("in-flight-job.json").exists(),
            "live waiter must keep its in-flight marker for the next tick"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn marker_only_in_flight_job_reclaims_once_worker_and_waiter_are_dead() {
        let (dir, mut journal, _slot_dir) = marker_only_recovery_fixture("dead-waiter");
        cleanup::write_owned_pid(&dir, "wait-velnor-1", Generation::INITIAL.0, stale_pid())
            .unwrap();

        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
        };
        let error = reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
        )
        .await
        .unwrap_err();
        // Both ownership markers are dead, so recovery advances past the
        // ownership gate and stops only at the missing runner credentials.
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
        };
        let error = reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("has no valid worker or waiter ownership pid"),
            "{error:#}"
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
            Event::Assigned {
                slot_id: SlotId("velnor-1".to_owned()),
                job_id: JobId("job-1".to_owned()),
                generation: Generation::INITIAL,
            },
            Event::JobOwned {
                job_id: JobId("job-1".to_owned()),
                slot_id: SlotId("velnor-1".to_owned()),
                attempt: 1,
                generation: Generation::INITIAL,
                worker: "worker-1".to_owned(),
                accepted_unix: 1_234,
            },
            Event::JobStarted {
                job_id: JobId("job-1".to_owned()),
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
        assert_eq!(slot.phase, ActorPhase::Fenced);
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
        assert_eq!(slot.phase, ActorPhase::Provisioning);
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
            .and(path("/api/v3/orgs/tailrocks/actions/runners/7"))
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

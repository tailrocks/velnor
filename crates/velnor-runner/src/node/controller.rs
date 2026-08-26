//! Per-scope controller: desired state, permits, slot/job child processes.
//!
//! Restarting this process must not stop existing slot or job workers: children
//! are spawned without kill-on-drop, and packaged units must not use
//! `PartOf=controller`. Every journal side effect is executed here.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use clap::Args;
use serde::Serialize;
use velnor_control::journal::{Event, Journal, JournalStats, SideEffect};
use velnor_model::{ActorPhase, ExecutionBackendKind, Generation, JobId, SlotId};

use crate::config;
use crate::protocol::{
    classify_broker_poll_error, classify_registration_error, GitHubApiError, GitHubScope,
    ListedRunner, RegistrationClient, RegistrationErrorClass,
};
use tokio::sync::{mpsc, Mutex, Notify};

use super::cleanup;
use super::exec::load_exec_config;
use super::health::HealthServer;
use super::prove;
use super::recovery::{RecoveryCoordinator, RecoverySignal, RecoveryState};
use super::slot::{heartbeat_path, slot_id, SlotHeartbeat};
use super::watchdog::{feed_after_cycle, LocalCycle};

/// Bound live JIT requests during startup/recovery without making the GitHub
/// API a burst target. This matches the bounded configure path.
const JIT_REGISTRATION_CONCURRENCY: usize = 4;
const REGISTRATION_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
/// Full reconciliation is a safety watchdog, not the broker polling clock.
/// Broker waiters own the short assignment latency path; running the whole
/// proof/filesystem/journal pass every two seconds multiplied idle CPU across
/// every scope and slot. Keep the watchdog bounded while preserving eventual
/// slot/process recovery and registration reconciliation.
const FULL_RECONCILE_INTERVAL: Duration = Duration::from_secs(10);
const HEARTBEAT_JOURNAL_INTERVAL: Duration = Duration::from_secs(30);
const HIGH_CPU_ALERT_PERCENT: f64 = 5.0;
const ALERT_SUSTAINED_CYCLES: u32 = 3;

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
const BROKER_OPEN_RETRY_MAX_BACKOFF: Duration = Duration::from_secs(600);

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

pub(crate) fn epoch_now() -> u64 {
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
        if let (Some(remaining), Some(reset_epoch)) = (remaining, reset_epoch) {
            if remaining < RATE_LIMIT_HEADROOM_REMAINING {
                // Nearly exhausted: keep the remaining budget for
                // DELETE traffic until the window resets. Do not spend it
                // on new JIT registrations.
                if let Some(hold) = until_epoch_with_jitter(reset_epoch, salt) {
                    let hold = hold.max(GITHUB_PROBE_MIN_INTERVAL);
                    self.next_probe = now + hold;
                    self.rest_hold_until = Some(now + hold);
                    return;
                }
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
        self.next_probe = now + hold;
        self.rest_hold_until = Some(now + hold);
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
    /// Extra fully reserved slots so `M` survives one replace.
    #[arg(long, default_value_t = 1)]
    pub surge: u32,
    #[arg(long)]
    pub once: bool,
    /// Spawn slot OS processes (production and isolation tests).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub spawn_slots: bool,
}

#[derive(Debug, Default, Serialize)]
struct ControllerMetrics {
    schema_version: u32,
    sequence: u64,
    published_unix: u64,
    reconcile_cycles: u64,
    reconcile_duration_ms: DurationQuantiles,
    reconcile_overlap_count: u64,
    events_per_second: f64,
    durable_events_per_second: f64,
    jobs: u32,
    idle_slots: u32,
    slot_processes: usize,
    child_processes: usize,
    /// Process-role counts make zero-job waiter regressions visible.
    daemon_processes: u32,
    controller_processes: u32,
    waiter_processes: u32,
    job_processes: u32,
    journal: JournalStats,
    broker: crate::runner::BrokerMetricsSnapshot,
    jit: JitMetrics,
    cpu: CpuAttribution,
    alerts: Vec<ControllerAlert>,
    #[serde(skip)]
    durations_ms: VecDeque<u64>,
    #[serde(skip)]
    last_event_total: u64,
    #[serde(skip)]
    last_durable_event_total: u64,
    high_cpu_streak: u32,
    repeated_noop_streak: u32,
    churn_streak: u32,
    #[serde(skip)]
    last_metrics_at: Option<Instant>,
}

#[derive(Debug, Clone, Serialize)]
struct ControllerAlert {
    code: &'static str,
    severity: &'static str,
    message: &'static str,
}

#[derive(Debug, Default, Serialize)]
struct DurationQuantiles {
    p50: u64,
    p95: u64,
    p99: u64,
}

#[derive(Debug, Default)]
struct ExecutionObservationCache {
    source: Option<PathBuf>,
    modified: Option<SystemTime>,
    backend: Option<ExecutionBackendKind>,
}

impl ExecutionObservationCache {
    fn load(
        &mut self,
        config_dir: &std::path::Path,
    ) -> Result<ExecutionBackendKind, velnor_model::ExecutionConfigError> {
        let primary = config_dir.join(velnor_model::ExecutionFile::FILE_NAME);
        let source = if primary.is_file() {
            primary
        } else {
            std::path::Path::new("/etc/velnor").join(velnor_model::ExecutionFile::FILE_NAME)
        };
        let modified = std::fs::metadata(&source)
            .and_then(|metadata| metadata.modified())
            .ok();
        if self.source.as_ref() == Some(&source) && self.modified == modified {
            if let Some(backend) = self.backend {
                return Ok(backend);
            }
        }
        let backend = crate::execution::load_execution_file(config_dir, None)?.backend();
        self.source = Some(source);
        self.modified = modified;
        self.backend = Some(backend);
        Ok(backend)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct CpuPhase {
    user_us: u64,
    system_us: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct CpuAttribution {
    journal: CpuPhase,
    filesystem: CpuPhase,
    github: CpuPhase,
    broker: CpuPhase,
    child_supervision: CpuPhase,
}

#[derive(Clone, Copy, Default)]
struct ControllerCpuTime {
    user_us: u64,
    system_us: u64,
}

fn controller_cpu_time() -> ControllerCpuTime {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `getrusage` initializes the structure on success.
        let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if result == 0 {
            // SAFETY: success above initialized `usage`.
            let usage = unsafe { usage.assume_init() };
            return ControllerCpuTime {
                user_us: u64::try_from(usage.ru_utime.tv_sec)
                    .unwrap_or_default()
                    .saturating_mul(1_000_000)
                    .saturating_add(u64::try_from(usage.ru_utime.tv_usec).unwrap_or_default()),
                system_us: u64::try_from(usage.ru_stime.tv_sec)
                    .unwrap_or_default()
                    .saturating_mul(1_000_000)
                    .saturating_add(u64::try_from(usage.ru_stime.tv_usec).unwrap_or_default()),
            };
        }
    }
    ControllerCpuTime::default()
}

fn account_cpu(phase: &mut CpuPhase, before: ControllerCpuTime) {
    let after = controller_cpu_time();
    phase.user_us = phase
        .user_us
        .saturating_add(after.user_us.saturating_sub(before.user_us));
    phase.system_us = phase
        .system_us
        .saturating_add(after.system_us.saturating_sub(before.system_us));
}

#[derive(Debug, Clone, Default, Serialize)]
struct JitMetrics {
    create_attempts: u64,
    create_successes: u64,
    create_failures: u64,
    create_latency_ms: u64,
    delete_attempts: u64,
    delete_successes: u64,
    delete_failures: u64,
    delete_latency_ms: u64,
    delete_error_statuses: std::collections::BTreeMap<u16, u64>,
}

impl ControllerMetrics {
    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        elapsed: Duration,
        health: &velnor_model::HealthDocument,
        slots: usize,
        jobs: usize,
        journal: &JournalStats,
        broker: &crate::runner::BrokerMetricsSnapshot,
        jit: &JitMetrics,
        cpu: &CpuAttribution,
    ) {
        let previous_cpu = self.cpu.clone();
        let previous_jit = self.jit.clone();
        let previous_journal = self.journal.clone();
        self.schema_version = 1;
        self.sequence = self.sequence.saturating_add(1);
        self.published_unix = epoch_now();
        self.reconcile_cycles = self.reconcile_cycles.saturating_add(1);
        // The controller loop is single-owner and never re-enters itself;
        // retain the explicit invariant in telemetry rather than guessing
        // from elapsed time.
        self.reconcile_overlap_count = 0;
        self.durations_ms
            .push_back(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
        while self.durations_ms.len() > 128 {
            self.durations_ms.pop_front();
        }
        let mut sorted = self.durations_ms.iter().copied().collect::<Vec<_>>();
        sorted.sort_unstable();
        self.reconcile_duration_ms = DurationQuantiles {
            p50: quantile(&sorted, 50),
            p95: quantile(&sorted, 95),
            p99: quantile(&sorted, 99),
        };
        self.jobs = health.jobs;
        self.idle_slots = health.idle_slots;
        self.slot_processes = slots;
        self.child_processes = jobs;
        self.daemon_processes = 1;
        self.controller_processes = 1;
        self.waiter_processes = 0;
        self.job_processes = u32::try_from(jobs).unwrap_or(u32::MAX);
        self.journal = journal.clone();
        self.broker = broker.clone();
        self.jit = jit.clone();
        self.cpu = cpu.clone();
        self.cpu.broker = CpuPhase {
            user_us: broker.cpu_user_us,
            system_us: broker.cpu_system_us,
        };
        let event_total = journal
            .events
            .values()
            .map(|event| event.accepted + event.rejected)
            .sum::<u64>();
        let durable_event_total = journal
            .events
            .values()
            .map(|event| event.durable)
            .sum::<u64>();
        let no_op_total = journal
            .events
            .values()
            .map(|event| event.no_op)
            .sum::<u64>();
        let previous_no_op_total = previous_journal
            .events
            .values()
            .map(|event| event.no_op)
            .sum::<u64>();
        let jit_churn = self
            .jit
            .create_attempts
            .saturating_sub(previous_jit.create_attempts)
            + self
                .jit
                .delete_attempts
                .saturating_sub(previous_jit.delete_attempts);
        let cpu_us = self
            .cpu
            .journal
            .user_us
            .saturating_add(self.cpu.journal.system_us)
            .saturating_add(self.cpu.filesystem.user_us)
            .saturating_add(self.cpu.filesystem.system_us)
            .saturating_add(self.cpu.github.user_us)
            .saturating_add(self.cpu.github.system_us)
            .saturating_add(self.cpu.broker.user_us)
            .saturating_add(self.cpu.broker.system_us)
            .saturating_add(self.cpu.child_supervision.user_us)
            .saturating_add(self.cpu.child_supervision.system_us);
        let previous_cpu_us = previous_cpu
            .journal
            .user_us
            .saturating_add(previous_cpu.journal.system_us)
            .saturating_add(previous_cpu.filesystem.user_us)
            .saturating_add(previous_cpu.filesystem.system_us)
            .saturating_add(previous_cpu.github.user_us)
            .saturating_add(previous_cpu.github.system_us)
            .saturating_add(previous_cpu.broker.user_us)
            .saturating_add(previous_cpu.broker.system_us)
            .saturating_add(previous_cpu.child_supervision.user_us)
            .saturating_add(previous_cpu.child_supervision.system_us);
        let interval = self
            .last_metrics_at
            .map(|at| at.elapsed().as_secs_f64())
            .unwrap_or_default();
        let cpu_percent = if interval > 0.0 {
            cpu_us.saturating_sub(previous_cpu_us) as f64 / (interval * 1_000_000.0) * 100.0
        } else {
            0.0
        };
        self.high_cpu_streak = if jobs == 0 && cpu_percent > HIGH_CPU_ALERT_PERCENT {
            self.high_cpu_streak.saturating_add(1)
        } else {
            0
        };
        self.repeated_noop_streak = if no_op_total > previous_no_op_total
            && durable_event_total == self.last_durable_event_total
        {
            self.repeated_noop_streak.saturating_add(1)
        } else {
            0
        };
        self.churn_streak = if jit_churn > 0 {
            self.churn_streak.saturating_add(1)
        } else {
            0
        };
        self.alerts.clear();
        if self.high_cpu_streak >= ALERT_SUSTAINED_CYCLES {
            self.alerts.push(ControllerAlert {
                code: "idle_high_cpu",
                severity: "critical",
                message: "zero-job controller CPU exceeds the idle budget",
            });
        }
        if self.repeated_noop_streak >= ALERT_SUSTAINED_CYCLES {
            self.alerts.push(ControllerAlert {
                code: "repeated_noop_events",
                severity: "warning",
                message: "identical observations are being accepted without durable change",
            });
        }
        if self.churn_streak >= ALERT_SUSTAINED_CYCLES {
            self.alerts.push(ControllerAlert {
                code: "registration_jit_churn",
                severity: "critical",
                message: "registration or JIT mutations are recurring across idle cycles",
            });
        }
        if let Some(previous) = self.last_metrics_at {
            let elapsed = previous.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                self.events_per_second =
                    event_total.saturating_sub(self.last_event_total) as f64 / elapsed;
                self.durable_events_per_second =
                    durable_event_total.saturating_sub(self.last_durable_event_total) as f64
                        / elapsed;
            }
        }
        self.last_event_total = event_total;
        self.last_durable_event_total = durable_event_total;
        self.last_metrics_at = Some(Instant::now());
    }

    fn publish(&self, state_dir: &std::path::Path) -> anyhow::Result<()> {
        let path = state_dir.join("controller-metrics.json");
        let temporary = state_dir.join(".controller-metrics.json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temporary, path)?;
        Ok(())
    }
}

fn quantile(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * percentile).div_ceil(100).max(1);
    let index = (rank - 1).min(sorted.len() - 1);
    sorted[index]
}

pub async fn run(args: ControllerArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.state_dir)?;
    let mut journal = Journal::open(args.state_dir.join("journal.db"))?;
    let server = HealthServer::bind(&args.state_dir)?;
    journal.apply(Event::ControlLive)?;
    journal.apply(Event::JournalWritable)?;
    journal.apply(Event::DesiredCapacity {
        ready: args.desired_ready,
        surge: args.surge,
    })?;
    let mut slots: HashMap<String, Child> = HashMap::new();
    let mut jobs: HashMap<String, Child> = HashMap::new();
    let mut job_generations: HashMap<String, u64> = HashMap::new();
    let mut heartbeats: HashMap<String, (u32, u64)> = HashMap::new();
    let mut last_registration_reconcile = Instant::now() - REGISTRATION_RECONCILE_INTERVAL;
    let mut pacing = GithubPacing::default();
    let mut metrics = ControllerMetrics::default();
    let mut jit_metrics = JitMetrics::default();
    let mut cpu = CpuAttribution::default();
    let mut execution_cache = ExecutionObservationCache::default();
    let (assignment_tx, mut assignment_rx) = mpsc::channel(32);
    let recovery = Arc::new(Mutex::new(RecoveryCoordinator::default()));
    let broker_metrics = Arc::new(crate::runner::BrokerMetrics::default());
    let reconcile_notify = Arc::new(Notify::new());
    let mut manager_task = tokio::spawn(run_scope_broker_manager(
        args.clone(),
        assignment_tx.clone(),
        recovery.clone(),
        reconcile_notify.clone(),
        broker_metrics.clone(),
    ));
    let mut ready_announced = false;
    loop {
        if manager_task.is_finished() {
            match manager_task.await {
                Ok(()) => eprintln!("scope broker manager exited; restarting"),
                Err(error) => eprintln!("scope broker manager stopped unexpectedly: {error}"),
            }
            recovery.lock().await.observe(
                RecoverySignal::Error(crate::protocol::BrokerPollErrorClass::Transport),
                Duration::from_secs(epoch_now()),
            );
            manager_task = tokio::spawn(run_scope_broker_manager(
                args.clone(),
                assignment_tx.clone(),
                recovery.clone(),
                reconcile_notify.clone(),
                broker_metrics.clone(),
            ));
        }
        if crate::runner::draining() {
            let _ = manager_task.await;
            drain_children(&args.state_dir, &mut slots, &mut jobs, &mut job_generations).await?;
            return Ok(());
        }
        let started = Instant::now();
        let supervision_cpu = controller_cpu_time();
        recover_pending_handoffs(&args, &journal, &mut jobs, &mut job_generations)?;
        drain_broker_assignments(
            &args,
            &journal,
            &mut jobs,
            &mut job_generations,
            &mut assignment_rx,
        )?;
        account_cpu(&mut cpu.child_supervision, supervision_cpu);
        let (cycle, mut health) = reconcile_once(
            &args,
            &mut journal,
            &server,
            &mut slots,
            &mut jobs,
            &mut job_generations,
            &mut heartbeats,
            &mut last_registration_reconcile,
            &mut pacing,
            &recovery,
            &mut jit_metrics,
            &mut cpu,
            &mut execution_cache,
        )
        .await?;
        let recovery = recovery.lock().await;
        health.resource_safe = crate::runner::free_space_bytes(&args.state_dir)
            .is_some_and(|free| free >= crate::runner::DISK_MIN_FREE_BYTES);
        health.recovery_state = match recovery.state() {
            RecoveryState::Healthy => velnor_model::RecoveryHealthState::Healthy,
            RecoveryState::MissingSession => velnor_model::RecoveryHealthState::MissingSession,
            RecoveryState::Backoff => velnor_model::RecoveryHealthState::Backoff,
            RecoveryState::Quarantined => velnor_model::RecoveryHealthState::Quarantined,
        };
        health.recovery_retry_streak = recovery.retry_streak();
        health.recovery_budget_used = recovery.retry_budget_used();
        health.recovery_retry_at_seconds = recovery.retry_at().as_secs();
        health.recovery_quarantine_until_seconds = recovery
            .quarantine_until()
            .map(|deadline| deadline.as_secs());
        health.recovery_affected_slots =
            u32::from(recovery.state() != RecoveryState::Healthy && health.actual_ready_slots > 0)
                * health.actual_ready_slots;
        health = health.with_derived_state();
        server.publish(&health)?;
        drain_broker_assignments(
            &args,
            &journal,
            &mut jobs,
            &mut job_generations,
            &mut assignment_rx,
        )?;
        let delete_metrics = crate::runner::jit_delete_metrics_snapshot();
        jit_metrics.delete_attempts = delete_metrics.attempts;
        jit_metrics.delete_successes = delete_metrics.successes;
        jit_metrics.delete_failures = delete_metrics.failures;
        jit_metrics.delete_latency_ms = delete_metrics.latency_ms;
        jit_metrics.delete_error_statuses = delete_metrics.error_statuses;
        metrics.record(
            started.elapsed(),
            &health,
            slots.len(),
            jobs.len(),
            &journal.telemetry_stats(),
            &broker_metrics.snapshot(),
            &jit_metrics,
            &cpu,
        );
        if let Err(error) = metrics.publish(&args.state_dir) {
            eprintln!("Warning: controller metrics publication failed: {error:#}");
        }
        let _ = feed_after_cycle(cycle, !ready_announced);
        ready_announced = true;
        if args.once {
            // Leave children running: a controller restart (or --once exit)
            // must not stop slot or job processes.
            for (_id, child) in slots.drain().chain(jobs.drain()) {
                std::mem::forget(child);
            }
            return Ok(());
        }
        tokio::select! {
            _ = reconcile_notify.notified() => {},
            _ = tokio::time::sleep(FULL_RECONCILE_INTERVAL) => {},
        }
    }
}

/// One scope-owned authority for all broker sessions. Session records remain
/// per slot for protocol isolation, but creation, retry budget, assignment
/// delivery, and drain are coordinated by this single state machine.
struct ScopeBrokerManager {
    sessions: HashMap<String, crate::runner::ScopeBrokerSession>,
    open_retries: HashMap<String, (tokio::time::Instant, u32)>,
}

impl ScopeBrokerManager {
    fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            open_retries: HashMap::new(),
        }
    }

    async fn run(
        &mut self,
        args: ControllerArgs,
        assignments: mpsc::Sender<crate::runner::BrokerAssignment>,
        recovery: Arc<Mutex<RecoveryCoordinator>>,
        reconcile_notify: Arc<Notify>,
        broker_metrics: Arc<crate::runner::BrokerMetrics>,
    ) {
        loop {
            if crate::runner::draining() {
                for (_, session) in self.sessions.drain() {
                    let _ = tokio::time::timeout(Duration::from_secs(5), session.close()).await;
                }
                return;
            }
            let journal_path = args.state_dir.join("journal.db");
            let Ok(journal) = Journal::open(journal_path) else {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            };
            let slots = match journal.materialized_state() {
                Ok(state) => state.slots,
                Err(error) => {
                    eprintln!("scope broker manager state read failed: {error:#}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            if let Err(error) = ensure_broker_sessions(
                &args,
                slots,
                &mut self.sessions,
                &mut self.open_retries,
                recovery.clone(),
            )
            .await
            {
                eprintln!("scope broker manager reconcile failed: {error:#}");
            }
            if !recovery.lock().await.due(Duration::from_secs(epoch_now())) {
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            let signals = crate::runner::BrokerManagerSignals {
                recovery: recovery.clone(),
                reconcile_notify: reconcile_notify.clone(),
                broker_metrics: broker_metrics.clone(),
            };
            let stopped = Arc::new(Mutex::new(Vec::<String>::new()));
            let failed = Arc::new(Mutex::new(Vec::<(String, Duration)>::new()));
            use futures_util::stream::{self, StreamExt as _};
            stream::iter(self.sessions.values_mut())
                .for_each_concurrent(16, |session| {
                    let state_dir = args.state_dir.clone();
                    let assignments = assignments.clone();
                    let signals = signals.clone();
                    let stopped = stopped.clone();
                    let failed = failed.clone();
                    async move {
                        match session.poll(&state_dir, &assignments, &signals).await {
                            Ok(crate::runner::ScopeBrokerPoll::Stopped) => {
                                stopped.lock().await.push(session.slot_id.clone());
                            }
                            Ok(crate::runner::ScopeBrokerPoll::Idle) => {}
                            Err(error) => {
                            let class = error
                                .downcast_ref::<GitHubApiError>()
                                .map_or(crate::protocol::BrokerPollErrorClass::Transport, |api| {
                                    classify_broker_poll_error(api.status)
                                });
                            let now = Duration::from_secs(epoch_now());
                            let (action, wait) = {
                                let mut coordinator = signals.recovery.lock().await;
                                let action = coordinator.observe(RecoverySignal::Error(class), now);
                                let wait = coordinator.retry_at().saturating_sub(now);
                                (action, wait)
                            };
                            session.backoff(wait);
                            failed.lock().await.push((session.slot_id.clone(), wait));
                            eprintln!(
                                "broker session {} generation {} failed class={class:?} action={action:?}; retrying in {}s: {error:#}",
                                session.slot_id, session.generation.0, wait.as_secs()
                            );
                            }
                        }
                    }
                })
                .await;
            for id in stopped.lock().await.drain(..) {
                if let Some(session) = self.sessions.remove(&id) {
                    let _ = tokio::time::timeout(Duration::from_secs(5), session.close()).await;
                }
            }
            for (id, wait) in failed.lock().await.drain(..) {
                if let Some(session) = self.sessions.remove(&id) {
                    let _ = tokio::time::timeout(Duration::from_secs(5), session.close()).await;
                }
                let delay = wait.max(Duration::from_secs(1));
                self.open_retries
                    .entry(id)
                    .and_modify(|(deadline, streak)| {
                        *streak = streak.saturating_add(1);
                        *deadline = tokio::time::Instant::now() + delay;
                    })
                    .or_insert((tokio::time::Instant::now() + delay, 1));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

async fn run_scope_broker_manager(
    args: ControllerArgs,
    assignments: mpsc::Sender<crate::runner::BrokerAssignment>,
    recovery: Arc<Mutex<RecoveryCoordinator>>,
    reconcile_notify: Arc<Notify>,
    broker_metrics: Arc<crate::runner::BrokerMetrics>,
) {
    ScopeBrokerManager::new()
        .run(
            args,
            assignments,
            recovery,
            reconcile_notify,
            broker_metrics,
        )
        .await;
}

async fn ensure_broker_sessions(
    args: &ControllerArgs,
    slots: Vec<velnor_control::journal::SlotRecord>,
    managers: &mut HashMap<String, crate::runner::ScopeBrokerSession>,
    open_retries: &mut HashMap<String, (tokio::time::Instant, u32)>,
    recovery: Arc<Mutex<RecoveryCoordinator>>,
) -> anyhow::Result<()> {
    let daemon = match load_exec_config(&args.state_dir) {
        Ok(daemon) => daemon,
        Err(_) => return Ok(()),
    };
    let base = daemon
        .config_dir
        .clone()
        .unwrap_or_else(|| args.state_dir.clone());
    let total = args.desired_ready.saturating_add(args.surge).max(1) as usize;
    let mut desired = HashMap::new();
    for index in 1..=total {
        let id = slot_id(&args.scope, index);
        let Some(slot) = slots.iter().find(|slot| slot.slot_id == id) else {
            continue;
        };
        if slot.phase != ActorPhase::Ready {
            continue;
        }
        desired.insert(id.0.clone(), (slot.generation, index));
    }
    let stale = managers
        .keys()
        .filter(|id| {
            desired
                .get(*id)
                .is_none_or(|(generation, _)| managers[*id].generation != *generation)
        })
        .cloned()
        .collect::<Vec<_>>();
    for id in stale {
        open_retries.remove(&id);
        if let Some(session) = managers.remove(&id) {
            let _ = tokio::time::timeout(Duration::from_secs(5), session.close()).await;
        }
    }
    let recovery_due = {
        let coordinator = recovery.lock().await;
        coordinator.due(Duration::from_secs(epoch_now()))
    };
    if !recovery_due {
        return Ok(());
    }
    let desired_len = desired.len();
    for (id, (generation, index)) in desired {
        if managers.contains_key(&id) {
            continue;
        }
        let run_args = crate::runner::daemon_slot_run_args(&daemon, &base, index, total)?;
        if has_pending_handoff(&args.state_dir, &id, generation)? {
            continue;
        }
        if let Some((deadline, _)) = open_retries.get(&id) {
            if tokio::time::Instant::now() < *deadline {
                continue;
            }
        }
        match crate::runner::ScopeBrokerSession::open(
            &run_args,
            &args.state_dir,
            id.clone(),
            generation,
            index,
        )
        .await
        {
            Ok(session) => {
                open_retries.remove(&id);
                managers.insert(id, session);
            }
            Err(error) => {
                let class = error
                    .downcast_ref::<GitHubApiError>()
                    .map_or(crate::protocol::BrokerPollErrorClass::Transport, |api| {
                        classify_broker_poll_error(api.status)
                    });
                let now = Duration::from_secs(epoch_now());
                let mut coordinator = recovery.lock().await;
                coordinator.observe(RecoverySignal::Error(class), now);
                let streak = open_retries
                    .get(&id)
                    .map_or(1, |(_, streak)| streak.saturating_add(1));
                let delay = broker_open_retry_delay(streak);
                open_retries.insert(id.clone(), (tokio::time::Instant::now() + delay, streak));
                eprintln!("broker session open failed class={class:?}: {error:#}");
            }
        }
    }
    if managers.len() == desired_len && open_retries.is_empty() {
        recovery
            .lock()
            .await
            .recovered(Duration::from_secs(epoch_now()));
    }
    Ok(())
}

fn broker_open_retry_delay(streak: u32) -> Duration {
    let shift = streak.saturating_sub(1).min(10);
    (Duration::from_secs(1) * (1u32 << shift)).min(BROKER_OPEN_RETRY_MAX_BACKOFF)
}

fn drain_broker_assignments(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
    job_generations: &mut HashMap<String, u64>,
    receiver: &mut mpsc::Receiver<crate::runner::BrokerAssignment>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(args.state_dir.join("handoffs"))?;
    while let Ok(assignment) = receiver.try_recv() {
        let state = journal.materialized_state()?;
        let valid = state.slots.iter().any(|slot| {
            slot.slot_id.0 == assignment.handoff.slot_id
                && slot.generation == assignment.handoff.generation
                && slot.phase == ActorPhase::Ready
                && !state.jobs.iter().any(|job| {
                    job.slot_id == slot.slot_id
                        && matches!(
                            job.phase,
                            ActorPhase::Assigned
                                | ActorPhase::Starting
                                | ActorPhase::Running
                                | ActorPhase::Completing
                        )
                })
        });
        if !valid {
            crate::node::handoff::write_completion(
                &assignment.done_path,
                &assignment.handoff.nonce,
                assignment.handoff.generation,
                crate::node::handoff::CompletionStatus::Stale,
            )?;
            continue;
        }
        crate::node::handoff::write_atomic(&assignment.handoff_path, &assignment.handoff)?;
        let exe = std::env::current_exe()?;
        let key = assignment.handoff.nonce.clone();
        if jobs.contains_key(&key) {
            let _ = std::fs::remove_file(&assignment.handoff_path);
            let _ = crate::node::handoff::write_completion(
                &assignment.done_path,
                &assignment.handoff.nonce,
                assignment.handoff.generation,
                crate::node::handoff::CompletionStatus::Duplicate,
            );
            continue;
        }
        let child = match Command::new(exe)
            .arg("job")
            .arg("--state-dir")
            .arg(&args.state_dir)
            .arg("--job-id")
            .arg(&key)
            .arg("--generation")
            .arg(assignment.handoff.generation.0.to_string())
            .arg("--slot-index")
            .arg(assignment.handoff.slot_index.to_string())
            .arg("--scope")
            .arg(&args.scope)
            .arg("--handoff")
            .arg(&assignment.handoff_path)
            .arg("--done")
            .arg(&assignment.done_path)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = std::fs::remove_file(&assignment.handoff_path);
                let _ = crate::node::handoff::write_completion(
                    &assignment.done_path,
                    &assignment.handoff.nonce,
                    assignment.handoff.generation,
                    crate::node::handoff::CompletionStatus::SpawnFailed,
                );
                eprintln!(
                    "broker assignment {} worker spawn failed; manager released: {error}",
                    assignment.handoff.nonce
                );
                continue;
            }
        };
        job_generations.insert(key.clone(), assignment.handoff.generation.0);
        jobs.insert(key, child);
    }
    Ok(())
}

fn spawn_handoff_worker(
    args: &ControllerArgs,
    jobs: &HashMap<String, Child>,
    handoff: &crate::node::handoff::AssignmentHandoff,
    handoff_path: &std::path::Path,
    done_path: &std::path::Path,
) -> anyhow::Result<Option<Child>> {
    if jobs.contains_key(&handoff.nonce) {
        return Ok(None);
    }
    let child = Command::new(std::env::current_exe()?)
        .arg("job")
        .arg("--state-dir")
        .arg(&args.state_dir)
        .arg("--job-id")
        .arg(&handoff.nonce)
        .arg("--generation")
        .arg(handoff.generation.0.to_string())
        .arg("--slot-index")
        .arg(handoff.slot_index.to_string())
        .arg("--scope")
        .arg(&args.scope)
        .arg("--handoff")
        .arg(handoff_path)
        .arg("--done")
        .arg(done_path)
        .spawn()?;
    Ok(Some(child))
}

fn has_pending_handoff(
    state_dir: &std::path::Path,
    slot_id: &str,
    generation: Generation,
) -> anyhow::Result<bool> {
    let dir = state_dir.join("handoffs");
    if !dir.is_dir() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Ok(handoff) = crate::node::handoff::read(&path) {
            if handoff.slot_id == slot_id && handoff.generation == generation {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Reclaim assignment envelopes left by a controller restart. The envelope
/// is the authority until the worker consumes it; a new broker manager must
/// not create a competing session for that slot in the meantime.
fn recover_pending_handoffs(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
    job_generations: &mut HashMap<String, u64>,
) -> anyhow::Result<()> {
    let dir = args.state_dir.join("handoffs");
    if !dir.is_dir() {
        return Ok(());
    }
    let state = journal.materialized_state()?;
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let handoff = match crate::node::handoff::read(&path) {
            Ok(handoff) => handoff,
            Err(error) => {
                eprintln!(
                    "discarding invalid pending handoff {}: {error:#}",
                    path.display()
                );
                let _ = std::fs::remove_file(&path);
                continue;
            }
        };
        let valid = state.slots.iter().any(|slot| {
            slot.slot_id.0 == handoff.slot_id
                && slot.generation == handoff.generation
                && !state.jobs.iter().any(|job| {
                    job.slot_id == slot.slot_id
                        && matches!(
                            job.phase,
                            ActorPhase::Assigned
                                | ActorPhase::Starting
                                | ActorPhase::Running
                                | ActorPhase::Completing
                        )
                })
                && matches!(
                    slot.phase,
                    ActorPhase::Ready
                        | ActorPhase::Assigned
                        | ActorPhase::Starting
                        | ActorPhase::Running
                        | ActorPhase::Completing
                )
        });
        let done = crate::node::handoff::completion_path(&args.state_dir, &handoff.nonce);
        if !valid {
            let _ = std::fs::remove_file(&path);
            let _ = crate::node::handoff::write_completion(
                &done,
                &handoff.nonce,
                handoff.generation,
                crate::node::handoff::CompletionStatus::Stale,
            );
            continue;
        }
        if jobs.contains_key(&handoff.nonce) {
            continue;
        }
        match spawn_handoff_worker(args, jobs, &handoff, &path, &done) {
            Ok(Some(child)) => {
                job_generations.insert(handoff.nonce.clone(), handoff.generation.0);
                jobs.insert(handoff.nonce.clone(), child);
            }
            Ok(None) => {}
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                let _ = crate::node::handoff::write_completion(
                    &done,
                    &handoff.nonce,
                    handoff.generation,
                    crate::node::handoff::CompletionStatus::SpawnFailed,
                );
                eprintln!(
                    "pending broker assignment {} recovery spawn failed: {error:#}",
                    handoff.nonce
                );
            }
        }
    }
    Ok(())
}

/// Stop idle controller-owned slot processes when the daemon receives SIGTERM.
/// Job workers are deliberately left alone so systemd's stop timeout remains
/// the outer bound for an in-flight job rather than turning an upgrade into a
/// lost job. The daemon's drain flag lives in the supervisor process, so this
/// explicit handoff is the process boundary that makes graceful drain real.
async fn drain_children(
    state_dir: &std::path::Path,
    slots: &mut HashMap<String, Child>,
    jobs: &mut HashMap<String, Child>,
    job_generations: &mut HashMap<String, u64>,
) -> anyhow::Result<()> {
    for child in slots.values() {
        request_child_shutdown(child)?;
    }

    loop {
        reap(slots);
        reap_jobs(state_dir, jobs, job_generations);
        if slots.is_empty() && jobs.is_empty() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
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
    job_generations: &mut HashMap<String, u64>,
    heartbeats: &mut HashMap<String, (u32, u64)>,
    last_registration_reconcile: &mut Instant,
    pacing: &mut GithubPacing,
    recovery: &Arc<Mutex<RecoveryCoordinator>>,
    jit_metrics: &mut JitMetrics,
    cpu: &mut CpuAttribution,
    execution_cache: &mut ExecutionObservationCache,
) -> anyhow::Result<(LocalCycle, velnor_model::HealthDocument)> {
    let total = args.desired_ready.saturating_add(args.surge).max(1);
    let journal_cpu = controller_cpu_time();
    let existing = journal.materialized_state()?;
    let permit_events = (1..=total).map(|index| {
        let id = slot_id(&args.scope, index as usize);
        let generation = existing
            .slots
            .iter()
            .find(|slot| slot.slot_id == id)
            .map_or(Generation::INITIAL, |slot| slot.generation);
        Event::PermitReserved {
            slot_id: id,
            generation,
            surge: index > args.desired_ready,
        }
    });
    let effects = journal
        .apply_many(permit_events)?
        .into_iter()
        .flat_map(|outcome| outcome.commands)
        .collect::<Vec<_>>();
    for command in effects {
        execute_effect(args, journal, slots, &mut *pacing, command).await?;
    }
    account_cpu(&mut cpu.journal, journal_cpu);
    // Slot process recovery is supervision, not a durable state transition.
    // Keep checking the child boundary without re-journaling PermitReserved
    // (a dead child may leave the slot in Provisioning with the same permit).
    let filesystem_cpu = controller_cpu_time();
    for index in 1..=total {
        let id = slot_id(&args.scope, index as usize);
        let generation = existing
            .slots
            .iter()
            .find(|slot| slot.slot_id == id)
            .map_or(Generation::INITIAL, |slot| slot.generation);
        maybe_spawn_slot(args, journal, slots, &id, generation)?;
    }

    ingest_slot_heartbeats(args, journal, total as usize, heartbeats)?;
    account_cpu(&mut cpu.filesystem, filesystem_cpu);

    let github_cpu = controller_cpu_time();
    observe_github_and_routing(args, journal, pacing).await?;
    account_cpu(&mut cpu.github, github_cpu);

    if last_registration_reconcile.elapsed() >= REGISTRATION_RECONCILE_INTERVAL {
        *last_registration_reconcile = Instant::now();
        let github_cpu = controller_cpu_time();
        reconcile_remote_registrations(args, journal, jobs, recovery).await?;
        account_cpu(&mut cpu.github, github_cpu);
    }

    let journal_cpu = controller_cpu_time();
    let backend = execution_cache
        .load(&args.state_dir)
        .map_err(anyhow::Error::from)?;
    let executor = prove::observe_executor(&args.state_dir, backend);
    let snapshot = journal.materialized_state()?;
    let now = tokio::time::Instant::now();
    let mut proof_events = Vec::new();
    for index in 1..=total {
        let id = slot_id(&args.scope, index as usize);
        let generation = snapshot
            .slots
            .iter()
            .find(|slot| slot.slot_id == id)
            .map_or(Generation::INITIAL, |slot| slot.generation);
        if executor {
            proof_events.push(Event::ExecutorProven {
                slot_id: id.clone(),
                generation,
            });
        }
        let journal_pid = snapshot
            .slots
            .iter()
            .find(|slot| slot.slot_id == id)
            .and_then(|slot| slot.pid);
        if prove::observe_session(slots.get_mut(&id.0), journal_pid) {
            proof_events.push(Event::SessionLive {
                slot_id: id.clone(),
                generation,
            });
        }
        if let Some(slot) = snapshot.slots.iter().find(|slot| slot.slot_id == id) {
            if slot.ready_proof().is_ok() && !slot.registered && pacing.registration_due(&id.0, now)
            {
                proof_events.push(Event::RegistrationIntended {
                    slot_id: id,
                    generation,
                });
            }
        }
    }
    let proof_effects = journal
        .apply_many(proof_events)?
        .into_iter()
        .flat_map(|outcome| outcome.commands)
        .collect::<Vec<_>>();
    account_cpu(&mut cpu.journal, journal_cpu);
    let mut registrations = Vec::new();
    for command in proof_effects {
        match command {
            SideEffect::RegisterRunner {
                slot_id,
                generation,
            } => registrations.push((slot_id, generation)),
            command => execute_effect(args, journal, slots, &mut *pacing, command).await?,
        }
    }
    let github_cpu = controller_cpu_time();
    register_runners(args, journal, pacing, registrations, Some(jit_metrics)).await?;
    account_cpu(&mut cpu.github, github_cpu);

    // Controller-owned broker managers own the idle session lifecycle. The
    // controller only starts a transient child after receiving an assignment.
    // `JobOwned` is proof from that worker, never a second spawn trigger.
    reclaim_orphaned_jobs(args, journal)?;

    for row in journal.pending_outbox()? {
        preserve_outbox(
            args,
            journal,
            &row.job_id,
            row.generation,
            &row.payload_sha256,
        )?;
    }

    reap(slots);
    reap_jobs(&args.state_dir, jobs, job_generations);
    let mut health = journal.materialized_state()?.health();
    health.execution_backend = backend;
    server.publish(&health)?;
    Ok((LocalCycle::finished(), health))
}

async fn execute_effect(
    args: &ControllerArgs,
    journal: &mut Journal,
    slots: &mut HashMap<String, Child>,
    pacing: &mut GithubPacing,
    command: SideEffect,
) -> anyhow::Result<()> {
    match command {
        SideEffect::SpawnSlot {
            slot_id,
            generation,
        } => maybe_spawn_slot(args, journal, slots, &slot_id, generation),
        SideEffect::RegisterRunner {
            slot_id,
            generation,
        } => register_runner(args, journal, pacing, slot_id, generation).await,
        // Broker assignments are spawned by `drain_broker_assignments` before
        // run-service acquisition. `JobOwned` is the durable proof emitted by
        // that worker; spawning again here would create a second poller.
        SideEffect::StartJob { .. } => Ok(()),
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
        SideEffect::DeleteOutbox { .. } | SideEffect::FenceSlot { .. } => Ok(()),
    }
}

async fn register_runner(
    args: &ControllerArgs,
    journal: &mut Journal,
    pacing: &mut GithubPacing,
    slot_id: SlotId,
    generation: Generation,
) -> anyhow::Result<()> {
    register_runners(args, journal, pacing, vec![(slot_id, generation)], None).await
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
    mut jit_metrics: Option<&mut JitMetrics>,
) -> anyhow::Result<()> {
    if registrations.is_empty() {
        return Ok(());
    }
    super::scheduler::production_scheduler().activate_production()?;
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
    let slot_count = exec.slots.max(1);
    use futures_util::stream::{self, StreamExt as _};
    let concurrency = registrations.len().clamp(1, JIT_REGISTRATION_CONCURRENCY);
    let mut outcomes = stream::iter(registrations)
        .map(|(slot_id, generation)| {
            let exec = exec.clone();
            let config_base = config_base.clone();
            async move {
                let index = slot_index_from_id(&slot_id);
                let started = Instant::now();
                let result =
                    crate::runner::jit_configure_one_slot(&exec, &config_base, index, slot_count)
                        .await;
                (slot_id, generation, result, started.elapsed())
            }
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
    outcomes.sort_by_key(|(slot_id, _, _, _)| slot_id.0.clone());

    for (slot_id, generation, result, latency) in outcomes {
        if let Some(metrics) = jit_metrics.as_deref_mut() {
            metrics.create_attempts = metrics.create_attempts.saturating_add(1);
            metrics.create_latency_ms = metrics
                .create_latency_ms
                .saturating_add(u64::try_from(latency.as_millis()).unwrap_or(u64::MAX));
        }
        if let Err(error) = result {
            if let Some(metrics) = jit_metrics.as_deref_mut() {
                metrics.create_failures = metrics.create_failures.saturating_add(1);
            }
            // Per-slot backoff always. Quota 403/429 also sets rest_hold_until
            // so other unregistered slots do not keep calling generate-jitconfig
            // against an exhausted PAT. Permission 403 with remaining>0 does not.
            let now = tokio::time::Instant::now();
            pacing.record_registration_error(&slot_id.0, now, &error);
            let class = error
                .downcast_ref::<crate::protocol::GitHubApiError>()
                .map(|api_error| {
                    crate::protocol::classify_registration_error(
                        api_error.status,
                        api_error.remaining,
                        api_error.retry_after_seconds,
                    )
                });
            eprintln!(
                "Warning: JIT register {} failed (class={class:?}, slot stays unregistered; backing off {}s): {error:#}",
                slot_id.0,
                pacing
                    .registration_retry
                    .get(&slot_id.0)
                    .map_or(0, |(deadline, _)| deadline
                        .duration_since(now)
                        .as_secs())
            );
            continue;
        }
        if let Some(metrics) = jit_metrics.as_deref_mut() {
            metrics.create_successes = metrics.create_successes.saturating_add(1);
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
    recovery: &Arc<Mutex<RecoveryCoordinator>>,
) -> anyhow::Result<()> {
    let exec = match load_exec_config(&args.state_dir) {
        Ok(exec) => exec,
        Err(error) => {
            eprintln!(
                "registration reconciliation skipped: cannot load daemon execution config: {error:#}"
            );
            return Ok(());
        }
    };
    let (Some(url), Some(pat)) = (exec.url.as_deref(), exec.pat.as_deref()) else {
        eprintln!("registration reconciliation skipped: GitHub URL or PAT unavailable");
        return Ok(());
    };
    let scope = GitHubScope::parse(url)?;
    let remote = match RegistrationClient::new()?.list_runners(&scope, pat).await {
        Ok(remote) => remote,
        Err(error) => {
            let class = error.downcast_ref::<GitHubApiError>().map_or(
                RegistrationErrorClass::Transport,
                |api| {
                    classify_registration_error(api.status, api.remaining, api.retry_after_seconds)
                },
            );
            let broker_class = match class {
                RegistrationErrorClass::Permission | RegistrationErrorClass::Client => {
                    crate::protocol::BrokerPollErrorClass::Forbidden
                }
                RegistrationErrorClass::Missing => {
                    crate::protocol::BrokerPollErrorClass::MissingSession
                }
                RegistrationErrorClass::Conflict => crate::protocol::BrokerPollErrorClass::Conflict,
                RegistrationErrorClass::Quota => crate::protocol::BrokerPollErrorClass::RateLimited,
                RegistrationErrorClass::Transient => crate::protocol::BrokerPollErrorClass::Server,
                RegistrationErrorClass::Transport => {
                    crate::protocol::BrokerPollErrorClass::Transport
                }
            };
            let mut coordinator = recovery.lock().await;
            let action = coordinator.observe(
                RecoverySignal::Error(broker_class),
                Duration::from_secs(epoch_now()),
            );
            eprintln!("registration reconciliation lookup failed: {error:#}");
            eprintln!("registration recovery class={class:?} action={action:?}");
            return Ok(());
        }
    };
    let state = journal.materialized_state()?;
    let config_base = exec
        .config_dir
        .clone()
        .unwrap_or_else(|| args.state_dir.clone());
    let slot_count = exec.slots.max(1);
    let mut lost = Vec::new();
    for slot in state.slots.iter().filter(|slot| slot.registered) {
        let index = slot_index_from_id(&slot.slot_id);
        let slot_dir = crate::runner::daemon_slot_config_dir(&config_base, index, slot_count);
        let local = config::load(&slot_dir).ok();
        let local_id = local.as_ref().and_then(|stored| stored.settings.agent_id);
        let local_name = local
            .as_ref()
            .map(|stored| stored.settings.agent_name.as_str())
            .unwrap_or_default();
        if !remote_registration_present(local_id, local_name, &remote) {
            let active_job = state.jobs.iter().any(|job| {
                job.slot_id == slot.slot_id
                    && matches!(
                        job.phase,
                        ActorPhase::Assigned
                            | ActorPhase::Starting
                            | ActorPhase::Running
                            | ActorPhase::Completing
                    )
            });
            if !active_job {
                lost.push((slot.slot_id.clone(), slot.generation));
            }
        }
    }

    for (slot_id, generation) in lost {
        let outcome = journal.apply(Event::RegistrationLost {
            slot_id: slot_id.clone(),
            generation,
        })?;
        if outcome.rejected {
            continue;
        }
        if let Some(child) = jobs.remove(&slot_id.0) {
            request_child_shutdown(&child)?;
        }
        eprintln!(
            "registration lost for {}; local claim cleared for fresh JIT recovery",
            slot_id.0
        );
    }
    Ok(())
}

fn remote_registration_present(
    local_id: Option<i64>,
    local_name: &str,
    remote: &[ListedRunner],
) -> bool {
    remote.iter().any(|runner| {
        local_id.is_some_and(|id| runner.id == Some(id))
            || (local_id.is_none() && runner.name.as_deref() == Some(local_name))
    })
}

async fn observe_github_and_routing(
    args: &ControllerArgs,
    journal: &mut Journal,
    pacing: &mut GithubPacing,
) -> anyhow::Result<()> {
    let mut reachable = false;
    let mut dependency_observed = false;
    if let Ok(exec) = load_exec_config(&args.state_dir) {
        let group = exec
            .pool_name
            .clone()
            .or_else(|| exec.name.clone())
            .unwrap_or_else(|| "default".to_owned());
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
                let probe = prove::probe_github(prove::GitHubProbeRequest {
                    url,
                    token,
                    policy: policy.as_ref(),
                    pool_id: exec.pool_id,
                    configured_labels: &exec.labels,
                    configured_trust: &exec.trust_scope,
                })
                .await;
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
                if let Some(evidence) = probe.evidence {
                    prove::write_evidence(&args.state_dir, &evidence)?;
                }
            }
            // Probe skipped for pacing: keep the last observed dependency
            // state rather than stamping a false observation.
        }
    }
    let mut observations = Vec::new();
    if dependency_observed || !probe_configured(args) {
        observations.push(Event::Dependency {
            github_reachable: reachable,
        });
    }
    let _ = prove::reconcile_from_dir(&args.state_dir)?;
    let routing = prove::observe_routing(&args.state_dir);
    observations.push(Event::Routing {
        valid: routing.valid,
        group_valid: routing.group_valid,
    });
    journal.apply_many(observations)?;
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

/// Return slots occupied by job workers that died without a terminal
/// completion (daemon drain mid-run, OOM-kill, reboot). Without this the
/// slot stays `Assigned` forever and advertised capacity never recovers.
fn reclaim_orphaned_jobs(args: &ControllerArgs, journal: &mut Journal) -> anyhow::Result<()> {
    let state = journal.materialized_state()?;
    for job in &state.jobs {
        if !matches!(
            job.phase,
            ActorPhase::Assigned | ActorPhase::Starting | ActorPhase::Running
        ) {
            continue;
        }
        let worker_live = cleanup::read_owned_pid(&args.state_dir, &job.job_id.0, job.generation.0)
            .is_some_and(prove::pid_is_alive);
        if worker_live {
            continue;
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
    Ok(())
}

/// Keep a durable completion payload. Never replace it with the checksum
/// and never stamp `CompletionSendStarted` without an actual send.
fn preserve_outbox(
    args: &ControllerArgs,
    _journal: &mut Journal,
    job_id: &JobId,
    generation: Generation,
    payload_sha256: &str,
) -> anyhow::Result<()> {
    let path = cleanup::outbox_path(&args.state_dir, &job_id.0, generation.0);
    if !path.is_file() {
        return Ok(());
    }
    let bytes = std::fs::read(&path)?;
    let actual = velnor_control::journal::payload_checksum(&bytes);
    if actual != payload_sha256 {
        anyhow::bail!(
            "outbox checksum mismatch for {} generation {}",
            job_id.0,
            generation.0
        );
    }
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut pending = Vec::new();
    for index in 1..=total {
        let path = heartbeat_path(&args.state_dir, index);
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(heartbeat) = serde_json::from_slice::<SlotHeartbeat>(&bytes) else {
            continue;
        };
        let id = slot_id(&args.scope, index);
        let Some(slot) = state.slots.iter().find(|slot| slot.slot_id == id) else {
            continue;
        };
        if slot.generation.0 != heartbeat.generation
            || !prove::pid_is_alive(heartbeat.pid)
            || seen.get(&id.0).is_some_and(|(pid, sequence)| {
                *pid == heartbeat.pid && *sequence >= heartbeat.sequence
            })
            || (slot.pid == Some(heartbeat.pid)
                && now.saturating_sub(slot.heartbeat_unix) < HEARTBEAT_JOURNAL_INTERVAL.as_secs())
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
    slot_id: &SlotId,
    generation: Generation,
) -> anyhow::Result<()> {
    if !args.spawn_slots {
        return Ok(());
    }
    if let Some(child) = children.get(&slot_id.0) {
        let heartbeat = heartbeat_path(&args.state_dir, slot_index_from_id(slot_id));
        let stale = std::fs::read(&heartbeat)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SlotHeartbeat>(&bytes).ok())
            .is_some_and(|heartbeat| heartbeat.generation != generation.0);
        if stale {
            request_child_shutdown(child)?;
        }
        return Ok(());
    }
    if let Ok(state) = journal.materialized_state() {
        if let Some(slot) = state.slots.iter().find(|slot| slot.slot_id == *slot_id) {
            if slot.pid.is_some_and(prove::pid_is_alive) {
                return Ok(());
            }
        }
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
    Ok(())
}

fn reap(children: &mut HashMap<String, Child>) {
    let mut dead = Vec::new();
    for (id, child) in children.iter_mut() {
        if let Ok(Some(_)) = child.try_wait() {
            dead.push(id.clone());
        }
    }
    for id in dead {
        children.remove(&id);
    }
}

/// Wake the broker manager when a transient worker exits without publishing
/// its normal completion marker. This covers pre-acquisition crashes, where
/// no durable job record exists for `reclaim_orphaned_jobs` to recover.
fn reap_jobs(
    state_dir: &std::path::Path,
    children: &mut HashMap<String, Child>,
    generations: &mut HashMap<String, u64>,
) {
    let mut dead = Vec::new();
    for (id, child) in children.iter_mut() {
        if let Ok(Some(_)) = child.try_wait() {
            dead.push(id.clone());
        }
    }
    for id in dead {
        let generation = generations.remove(&id).unwrap_or_default();
        let _ = crate::node::handoff::write_completion(
            &crate::node::handoff::completion_path(state_dir, &id),
            &id,
            Generation(generation),
            crate::node::handoff::CompletionStatus::Exited,
        );
        children.remove(&id);
    }
}

/// Daemon production path: spawn one OS process per configured slot instead
/// of a shared-process `JoinSet`.
pub async fn supervise_from_daemon(
    state_dir: PathBuf,
    scope: String,
    desired_ready: u32,
    surge: u32,
    once: bool,
) -> anyhow::Result<()> {
    run(ControllerArgs {
        state_dir,
        scope,
        desired_ready,
        surge,
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Serializes tests that mutate `GITHUB_TOKEN` (process-global env):
    /// `load_exec_config` resolves the PAT from the environment at call time,
    /// so a parallel test's cleanup `remove_var` must not land mid-test.
    static GITHUB_TOKEN_ENV_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

    #[test]
    fn duration_quantiles_use_nearest_rank_for_small_samples() {
        assert_eq!(quantile(&[1, 100], 50), 1);
        assert_eq!(quantile(&[1, 100], 95), 100);
        assert_eq!(quantile(&[1, 2, 3, 4], 99), 4);
        assert_eq!(quantile(&[], 50), 0);
    }

    #[test]
    fn execution_backend_observation_cache_reloads_only_after_config_change() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-execution-cache-{}-{}",
            std::process::id(),
            epoch_now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("execution.toml"),
            "[execution]\nbackend = \"docker\"\n",
        )
        .unwrap();
        let mut cache = ExecutionObservationCache::default();
        assert_eq!(cache.load(&dir).unwrap(), ExecutionBackendKind::Docker);
        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(
            dir.join("execution.toml"),
            "[execution]\nbackend = \"microvm\"\n",
        )
        .unwrap();
        assert_eq!(cache.load(&dir).unwrap(), ExecutionBackendKind::MicroVm);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn broker_session_open_retry_is_exponential_and_capped() {
        assert_eq!(broker_open_retry_delay(1), Duration::from_secs(1));
        assert_eq!(broker_open_retry_delay(2), Duration::from_secs(2));
        assert_eq!(broker_open_retry_delay(3), Duration::from_secs(4));
        assert_eq!(broker_open_retry_delay(10), Duration::from_secs(512));
        assert_eq!(broker_open_retry_delay(11), Duration::from_secs(600));
        assert_eq!(broker_open_retry_delay(12), Duration::from_secs(600));
        assert_eq!(broker_open_retry_delay(100), Duration::from_secs(600));
    }

    #[test]
    fn zero_job_metrics_report_no_waiter_or_job_processes() {
        let health = velnor_model::HealthDocument::empty();
        let mut metrics = ControllerMetrics::default();
        metrics.record(
            Duration::from_millis(4),
            &health,
            16,
            0,
            &JournalStats::default(),
            &crate::runner::BrokerMetricsSnapshot::default(),
            &JitMetrics::default(),
            &CpuAttribution::default(),
        );
        let value = serde_json::to_value(metrics).unwrap();
        assert_eq!(value["slot_processes"], 16);
        assert_eq!(value["waiter_processes"], 0);
        assert_eq!(value["job_processes"], 0);
        assert_eq!(value["reconcile_overlap_count"], 0);
        assert_eq!(value["events_per_second"], 0.0);
    }

    #[test]
    fn repeated_noop_events_raise_a_bounded_control_alert() {
        let health = velnor_model::HealthDocument::empty();
        let mut metrics = ControllerMetrics::default();
        let mut journal = JournalStats::default();
        for count in 1..=3 {
            journal.events.insert(
                "routing".to_owned(),
                velnor_control::journal::JournalEventStats {
                    accepted: count,
                    no_op: count,
                    ..Default::default()
                },
            );
            metrics.record(
                Duration::from_millis(4),
                &health,
                1,
                0,
                &journal,
                &crate::runner::BrokerMetricsSnapshot::default(),
                &JitMetrics::default(),
                &CpuAttribution::default(),
            );
        }
        let value = serde_json::to_value(metrics).unwrap();
        assert_eq!(value["alerts"][0]["code"], "repeated_noop_events");
    }

    #[test]
    fn sustained_zero_job_cpu_raises_an_idle_budget_alert() {
        let health = velnor_model::HealthDocument::empty();
        let mut metrics = ControllerMetrics {
            last_metrics_at: Some(Instant::now() - Duration::from_secs(1)),
            ..Default::default()
        };
        for count in 1..=3 {
            let mut cpu = CpuAttribution::default();
            cpu.journal.user_us = count * 100_000;
            metrics.record(
                Duration::from_millis(4),
                &health,
                1,
                0,
                &JournalStats::default(),
                &crate::runner::BrokerMetricsSnapshot::default(),
                &JitMetrics::default(),
                &cpu,
            );
        }
        let value = serde_json::to_value(metrics).unwrap();
        assert_eq!(value["alerts"][0]["code"], "idle_high_cpu");
    }

    #[test]
    fn recurring_jit_mutations_raise_a_churn_alert() {
        let health = velnor_model::HealthDocument::empty();
        let mut metrics = ControllerMetrics::default();
        for count in 1..=3 {
            let jit = JitMetrics {
                create_attempts: count,
                ..Default::default()
            };
            metrics.record(
                Duration::from_millis(4),
                &health,
                1,
                0,
                &JournalStats::default(),
                &crate::runner::BrokerMetricsSnapshot::default(),
                &jit,
                &CpuAttribution::default(),
            );
        }
        let value = serde_json::to_value(metrics).unwrap();
        assert_eq!(value["alerts"][0]["code"], "registration_jit_churn");
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

    #[test]
    fn remote_registration_matches_durable_id_before_name() {
        let remote = vec![ListedRunner {
            id: Some(7),
            name: Some("slot-1".to_owned()),
            status: Some("offline".to_owned()),
            busy: Some(false),
            labels: Vec::new(),
        }];
        assert!(remote_registration_present(Some(7), "other-name", &remote));
        assert!(!remote_registration_present(Some(8), "slot-1", &remote));
        assert!(remote_registration_present(None, "slot-1", &remote));
        assert!(!remote_registration_present(None, "slot-2", &remote));
    }

    #[tokio::test]
    async fn missing_remote_registration_clears_local_claim() {
        let _token_guard = GITHUB_TOKEN_ENV_LOCK.lock().await;
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
            &dir,
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
        std::env::set_var("GITHUB_TOKEN", "ghs_test");

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
            Event::DesiredCapacity { ready: 1, surge: 0 },
            Event::PermitReserved {
                slot_id: SlotId("velnor-1".to_owned()),
                generation: Generation::INITIAL,
                surge: false,
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
            surge: 0,
            once: true,
            spawn_slots: false,
        };
        reconcile_remote_registrations(
            &args,
            &mut journal,
            &mut HashMap::new(),
            &Arc::new(Mutex::new(RecoveryCoordinator::default())),
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
        assert_eq!(slot.phase, ActorPhase::Provisioning);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn org_url_probe_bootstraps_policy_from_generated_allowlist() {
        let _token_guard = GITHUB_TOKEN_ENV_LOCK.lock().await;
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
                "runner_groups": [{"id": 7, "name": "velnor", "default": false}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v3/orgs/tailrocks/actions/runner-groups/7/repositories",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
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
        std::env::set_var("VELNOR_FLEET_POLICY_DIR", policy_dir.as_os_str());
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
        std::env::set_var("GITHUB_TOKEN", "ghs_test");
        std::env::set_var(crate::protocol::GITHUB_HTTP_TRANSPORT_ENV, "native");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".into(),
            desired_ready: 1,
            surge: 0,
            once: true,
            spawn_slots: false,
        };
        let mut pacing = GithubPacing::default();
        observe_github_and_routing(&args, &mut journal, &mut pacing)
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
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("VELNOR_FLEET_POLICY_DIR");
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
    #[tokio::test]
    async fn rate_limited_probe_parks_instead_of_retrying_per_tick() {
        let _token_guard = GITHUB_TOKEN_ENV_LOCK.lock().await;
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
        std::env::set_var("GITHUB_TOKEN", "ghs_test");
        std::env::set_var(crate::protocol::GITHUB_HTTP_TRANSPORT_ENV, "native");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".into(),
            desired_ready: 1,
            surge: 0,
            once: true,
            spawn_slots: false,
        };
        let mut pacing = GithubPacing::default();

        // First tick: the 403 is observed, health degrades honestly.
        observe_github_and_routing(&args, &mut journal, &mut pacing)
            .await
            .unwrap();
        let state = journal.load_state().unwrap();
        assert!(!state.github_reachable, "{state:?}");

        // Simulate a burst of reconcile ticks inside the reset window: the
        // pacer must not send another request (Mock::expect(1) enforces it).
        for _ in 0..10 {
            observe_github_and_routing(&args, &mut journal, &mut pacing)
                .await
                .unwrap();
        }
        server.verify().await;

        std::env::remove_var("GITHUB_TOKEN");
        std::fs::remove_dir_all(dir).ok();
    }
}

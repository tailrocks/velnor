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
use super::slot::{heartbeat_path, slot_id, SlotHeartbeat};
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

/// Capability to recover one recorded job after its worker and slot waiter
/// were both proven dead. The private fields make process-death proof a
/// controller-owned authority; the held marker lock binds that proof to the
/// exact in-flight generation through local and permit cleanup.
pub(crate) struct RecordedOwnerDeathProof {
    state_dir: PathBuf,
    service_instance: String,
    scope: String,
    slot_dir: PathBuf,
    slot_id: SlotId,
    generation: Generation,
    job_id: String,
    job_snapshot: Option<velnor_control::journal::JobRecord>,
    marker_record: crate::runner::InFlightJobRecord,
    marker_lock: crate::runner::InFlightMarkerLock,
}

impl RecordedOwnerDeathProof {
    #[allow(clippy::too_many_arguments)]
    fn capture(
        args: &ControllerArgs,
        slot_dir: &Path,
        slot_id: &SlotId,
        generation: Generation,
        job_id: &str,
        job_snapshot: Option<&velnor_control::journal::JobRecord>,
        marker_record: &crate::runner::InFlightJobRecord,
        marker_lock: crate::runner::InFlightMarkerLock,
    ) -> anyhow::Result<Self> {
        if marker_record.recorded_job_id()? != job_id
            || job_snapshot.is_some_and(|job| {
                job.job_id.0 != job_id || job.slot_id != *slot_id || job.generation != generation
            })
        {
            anyhow::bail!("owner-death proof inputs do not describe one recorded job attempt");
        }
        let state_dir = std::fs::canonicalize(&args.state_dir).with_context(|| {
            format!("canonicalize service instance {}", args.state_dir.display())
        })?;
        let proof = Self {
            service_instance: state_dir.to_string_lossy().into_owned(),
            state_dir,
            scope: args.scope.clone(),
            slot_dir: slot_dir.to_owned(),
            slot_id: slot_id.clone(),
            generation,
            job_id: job_id.to_owned(),
            job_snapshot: job_snapshot.cloned(),
            marker_record: marker_record.clone(),
            marker_lock,
        };
        proof.recheck_for_record(marker_record)?;
        Ok(proof)
    }

    /// Recheck durable slot/job authority and the exact marker while its flock
    /// is still held. Call immediately before claiming or releasing a permit.
    pub(crate) fn recheck_current(&self) -> anyhow::Result<()> {
        if crate::runner::recorded_in_flight_job_record(&self.slot_dir)?
            != Some(self.marker_record.clone())
        {
            anyhow::bail!("in-flight marker changed during owner-death recovery");
        }
        if !self.marker_lock.covers(&self.slot_dir)? {
            anyhow::bail!("owner-death proof marker lock no longer covers its slot");
        }
        let journal = Journal::open_for_service_instance(
            self.state_dir.join("journal.db"),
            &self.service_instance,
        )?;
        let state = journal.materialized_state()?;
        if !state
            .slots
            .iter()
            .any(|slot| slot.slot_id == self.slot_id && slot.generation == self.generation)
        {
            anyhow::bail!("slot generation changed during owner-death recovery");
        }
        match &self.job_snapshot {
            Some(expected) => {
                if !state.jobs.iter().any(|job| job == expected) {
                    anyhow::bail!("journal job attempt changed during owner-death recovery");
                }
                if state.jobs.iter().any(|job| {
                    job.slot_id == self.slot_id
                        && job.generation == self.generation
                        && job.job_id != expected.job_id
                        && job.phase.occupies_slot()
                }) {
                    anyhow::bail!("a replacement job now occupies the recovered slot");
                }
            }
            None => {
                if state.jobs.iter().any(|job| {
                    job.job_id.0 == self.job_id
                        || (job.slot_id == self.slot_id
                            && job.generation == self.generation
                            && job.phase.occupies_slot())
                }) {
                    anyhow::bail!("journal job appeared during marker-only recovery");
                }
            }
        }
        let waiter_id = format!("wait-{}", self.slot_id.0);
        let mut worker_enum: Option<prove::WorkerProcessEnumeration> = None;
        ensure_recorded_owners_dead(&self.job_id, &waiter_id, |owner_id| {
            persisted_worker_owns_slot_at(
                &self.state_dir,
                &self.scope,
                &journal,
                owner_id,
                &self.slot_id,
                self.generation,
                &mut worker_enum,
            )
        })?;
        Ok(())
    }

    pub(crate) fn recheck_for_record(
        &self,
        record: &crate::runner::InFlightJobRecord,
    ) -> anyhow::Result<()> {
        if &self.marker_record != record || record.recorded_job_id()? != self.job_id {
            anyhow::bail!("owner-death proof does not cover the requested in-flight job");
        }
        self.recheck_current()
    }

    pub(crate) fn marker_lock_for_record(
        &self,
        slot_dir: &Path,
        record: &crate::runner::InFlightJobRecord,
    ) -> anyhow::Result<&crate::runner::InFlightMarkerLock> {
        if self.slot_dir != slot_dir
            || &self.marker_record != record
            || self.job_id != record.recorded_job_id()?
        {
            anyhow::bail!("owner-death proof does not cover the requested in-flight job");
        }
        if !self.marker_lock.covers(slot_dir)? {
            anyhow::bail!("owner-death proof marker lock does not cover the requested slot");
        }
        Ok(&self.marker_lock)
    }

    /// Recheck ownership immediately before releasing local resources. A
    /// successful remote terminal acknowledgement may remove the original job
    /// row, so only the same attempt or its absence is acceptable here.
    pub(crate) fn recheck_before_cleanup(
        &self,
        record: &crate::runner::InFlightJobRecord,
    ) -> anyhow::Result<()> {
        if &self.marker_record != record
            || crate::runner::recorded_in_flight_job_record(&self.slot_dir)?
                != Some(self.marker_record.clone())
        {
            anyhow::bail!("in-flight marker changed before owner-death cleanup");
        }
        if !self.marker_lock.covers(&self.slot_dir)? {
            anyhow::bail!("owner-death proof marker lock no longer covers its slot");
        }
        let journal = Journal::open_for_service_instance(
            self.state_dir.join("journal.db"),
            &self.service_instance,
        )?;
        let state = journal.materialized_state()?;
        if !state
            .slots
            .iter()
            .any(|slot| slot.slot_id == self.slot_id && slot.generation == self.generation)
        {
            anyhow::bail!("slot generation changed before owner-death cleanup");
        }
        let current_job = state.jobs.iter().find(|job| job.job_id.0 == self.job_id);
        if let (Some(expected), Some(current)) = (&self.job_snapshot, current_job)
            && (current.slot_id != expected.slot_id
                || current.generation != expected.generation
                || current.attempt != expected.attempt
                || current.worker != expected.worker)
        {
            anyhow::bail!("journal job attempt changed before owner-death cleanup");
        }
        if self.job_snapshot.is_none() && current_job.is_some() {
            anyhow::bail!("journal job appeared before marker-only cleanup");
        }
        if state.jobs.iter().any(|job| {
            job.slot_id == self.slot_id
                && job.generation == self.generation
                && job.job_id.0 != self.job_id
                && job.phase.occupies_slot()
        }) {
            anyhow::bail!("replacement job now occupies the recovered slot");
        }
        let waiter_id = format!("wait-{}", self.slot_id.0);
        let mut worker_enum: Option<prove::WorkerProcessEnumeration> = None;
        ensure_recorded_owners_dead(&self.job_id, &waiter_id, |owner_id| {
            persisted_worker_owns_slot_at(
                &self.state_dir,
                &self.scope,
                &journal,
                owner_id,
                &self.slot_id,
                self.generation,
                &mut worker_enum,
            )
        })?;
        Ok(())
    }
}

fn ensure_recorded_owners_dead(
    job_id: &str,
    waiter_id: &str,
    mut is_live: impl FnMut(&str) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let job_worker_live = is_live(job_id)?;
    let waiter_live = is_live(waiter_id)?;
    if job_worker_live || waiter_live {
        anyhow::bail!("local job worker or slot waiter is still live");
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct DiskPressureGate {
    draining: bool,
    terminal: bool,
    unavailable: bool,
}

fn pressure_gate_blocks_waiters(gate: Option<&DiskPressureGate>) -> bool {
    gate.is_some_and(|gate| gate.unavailable || gate.draining || gate.terminal)
}

fn pressure_slots_are_fenced(
    args: &ControllerArgs,
    state: &velnor_control::journal::FleetState,
) -> bool {
    let desired_slots_fenced = (1..=args.desired_ready).all(|index| {
        let id = slot_id(&args.scope, index as usize);
        state
            .slots
            .iter()
            .find(|slot| slot.slot_id == id)
            .is_none_or(|slot| slot.phase == SlotPhase2::Fenced)
    });
    desired_slots_fenced && state.jobs.iter().all(|job| !job.phase.occupies_slot())
}

/// Read every config/work filesystem that blocks worker admission. Healthy
/// probes clear only the episode revision just observed; terminal low episodes
/// fence this controller generation until all roots have passed that CAS.
fn disk_pressure_gate(
    args: &ControllerArgs,
    journal: &Journal,
) -> anyhow::Result<Option<DiskPressureGate>> {
    let service_instance = std::fs::canonicalize(&args.state_dir)
        .with_context(|| format!("canonicalize service instance {}", args.state_dir.display()))?
        .to_string_lossy()
        .into_owned();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let exec = match load_exec_config(&args.state_dir) {
        Ok(exec) => exec,
        Err(error) => {
            eprintln!(
                "disk pressure config unavailable at {}; refusing slot permits: {error:#}",
                args.state_dir.display()
            );
            let (draining, terminal) = advance_unknown_pressure_roots(
                journal,
                &service_instance,
                std::slice::from_ref(&args.state_dir),
                now,
            )?;
            return Ok(Some(DiskPressureGate {
                draining,
                terminal,
                unavailable: true,
            }));
        }
    };
    let config_base = exec
        .config_dir
        .clone()
        .unwrap_or_else(|| args.state_dir.clone());
    let pressure_roots =
        crate::host_capacity::pressure_roots(&config_base, exec.work_dir.as_deref());
    let batch = match crate::host_capacity::probe_pressure_roots(&pressure_roots) {
        Ok(batch) => batch,
        Err(error) => {
            eprintln!(
                "disk pressure identity unavailable for configured roots {pressure_roots:?}; refusing new waiter launches: {error:#}"
            );
            let (draining, terminal) =
                advance_unknown_pressure_roots(journal, &service_instance, &pressure_roots, now)?;
            return Ok(Some(DiskPressureGate {
                draining,
                terminal,
                unavailable: true,
            }));
        }
    };
    let mut roots_unmeasurable = !batch.unmeasurable_roots.is_empty();
    for failure in &batch.unmeasurable_roots {
        eprintln!(
            "disk pressure root cannot be pinned at {}; refusing admission: {}",
            failure.root.display(),
            failure.reason
        );
    }
    let roots_pinned = batch.unmeasurable_roots.is_empty();
    let capacities = batch.filesystems;
    roots_unmeasurable |= capacities.iter().any(|filesystem| {
        !filesystem.measurable || filesystem.capacity.volume_fingerprint.is_none()
    });
    for filesystem in capacities
        .iter()
        .filter(|filesystem| !filesystem.measurable)
    {
        eprintln!(
            "disk pressure capacity is unmeasurable at {}; durable episode uses its pinned device identity and remains fail-closed",
            filesystem.root.display()
        );
    }
    for filesystem in &capacities {
        if let Err(error) = filesystem.revalidate() {
            eprintln!(
                "disk pressure root changed identity at {}; refusing slot permits: {error:#}",
                filesystem.root.display()
            );
            let (draining, terminal) =
                advance_unknown_pressure_roots(journal, &service_instance, &pressure_roots, now)?;
            return Ok(Some(DiskPressureGate {
                draining,
                terminal,
                unavailable: true,
            }));
        }
    }
    let mut fresh_capacities = Vec::with_capacity(capacities.len());
    let mut samples = Vec::with_capacity(capacities.len() + batch.unmeasurable_roots.len());
    for filesystem in &capacities {
        let fresh = filesystem.primary_pin().and_then(|pin| pin.probe().ok());
        let measurable = filesystem.measurable
            && fresh
                .as_ref()
                .is_some_and(|capacity| capacity.volume_fingerprint.is_some());
        let selected = fresh.unwrap_or_else(|| filesystem.capacity.clone());
        samples.push(velnor_control::journal::DiskPressureFilesystemSample {
            alias_ids: filesystem.root_ids.clone(),
            filesystem_id: filesystem.root_id.clone(),
            available_bytes: measurable.then_some(selected.available_bytes),
            min_free_bytes: crate::host_capacity::pressure_floor_bytes(
                &selected,
                crate::host_capacity::DEFAULT_MIN_FREE_BYTES,
            ),
            volume_fingerprint: selected.volume_fingerprint.clone(),
        });
        fresh_capacities.push(measurable.then_some(selected));
    }
    samples.extend(batch.unmeasurable_roots.iter().map(|failure| {
        velnor_control::journal::DiskPressureFilesystemSample {
            filesystem_id: failure.root_id.clone(),
            alias_ids: Vec::new(),
            available_bytes: None,
            min_free_bytes: crate::host_capacity::DEFAULT_MIN_FREE_BYTES,
            volume_fingerprint: None,
        }
    }));
    if samples.is_empty() {
        samples = unknown_pressure_samples(&pressure_roots);
    }
    journal.advance_disk_pressure_roots(
        &service_instance,
        &samples,
        crate::host_capacity::DEFAULT_DEGRADED_DEADLINE.as_secs(),
        crate::host_capacity::DEFAULT_DRAIN_DEADLINE.as_secs(),
        now,
    )?;
    let complete_identity_batch = roots_pinned
        && capacities.iter().all(|filesystem| filesystem.measurable)
        && fresh_capacities.iter().all(Option::is_some);
    if complete_identity_batch {
        let mut observed_ids = Vec::new();
        for sample in &samples {
            for id in std::iter::once(&sample.filesystem_id).chain(&sample.alias_ids) {
                if !observed_ids.contains(id) {
                    observed_ids.push(id.clone());
                }
            }
        }
        journal.retire_unobserved_disk_pressure_episodes(
            &service_instance,
            &observed_ids,
            complete_identity_batch,
        )?;
    }

    // Controller observation must be able to make the one bounded reclaim
    // while every worker is correctly blocked by the episode. The journal
    // claim commits first and is CAS-bound to the root UUID, episode revision,
    // and the fresh pinned sample. A crash after that claim consumes the one
    // attempt; the immutable D/E deadlines still provide the terminal bound.
    let backend = crate::execution::load_execution_file(&config_base, None)
        .ok()
        .map(|file| file.backend());
    let slot_count = exec.slots;
    let work_roots = crate::runner::daemon_pressure_work_roots(
        &config_base,
        exec.work_dir.as_deref(),
        slot_count,
    );
    for (filesystem, initial_capacity) in capacities.iter().zip(&fresh_capacities) {
        let Some(initial_capacity) = initial_capacity.as_ref() else {
            continue;
        };
        let Some(initial_uuid) = initial_capacity.volume_fingerprint.as_deref() else {
            continue;
        };
        let pressure_floor = crate::host_capacity::pressure_floor_bytes(
            initial_capacity,
            crate::host_capacity::DEFAULT_MIN_FREE_BYTES,
        );
        if initial_capacity.available_bytes >= pressure_floor {
            continue;
        }
        if filesystem.revalidate().is_err() {
            continue;
        }
        let Some(pin) = filesystem.primary_pin() else {
            continue;
        };
        let Ok(fresh_capacity) = pin.probe() else {
            continue;
        };
        if fresh_capacity.volume_fingerprint.as_deref() != Some(initial_uuid)
            || !filesystem.measurable
        {
            continue;
        }
        let fresh_floor = crate::host_capacity::pressure_floor_bytes(
            &fresh_capacity,
            crate::host_capacity::DEFAULT_MIN_FREE_BYTES,
        );
        if fresh_capacity.available_bytes >= fresh_floor {
            continue;
        }
        let Some(current) =
            journal.disk_pressure_episode(&service_instance, &filesystem.root_id)?
        else {
            continue;
        };
        if current.volume_fingerprint.as_deref() != Some(initial_uuid) {
            continue;
        }
        // Resolve every reclaim destination before claiming the one-shot
        // episode attempt. A layout error must not consume the durable claim.
        let layout = crate::storage::resolve_required_layout()?;
        if !journal.claim_disk_pressure_reclaim(
            &service_instance,
            &filesystem.root_id,
            &filesystem.root_ids,
            &current,
            fresh_capacity.available_bytes,
            fresh_floor,
            initial_uuid,
            now,
        )? {
            continue;
        }
        if filesystem.revalidate().is_err() {
            continue;
        }
        crate::runner::reclaim_pressure_filesystem(backend, filesystem, &work_roots, &layout);
    }

    let mut slots_fenced = None;
    let mut draining = false;
    let mut terminal = false;
    let all_roots_healthy = !roots_unmeasurable
        && samples.iter().all(|sample| {
            sample.volume_fingerprint.is_some()
                && sample
                    .available_bytes
                    .is_some_and(|available| available >= sample.min_free_bytes)
        });
    for (filesystem, fresh_capacity) in capacities.iter().zip(&fresh_capacities) {
        if let Err(error) = filesystem.revalidate() {
            eprintln!(
                "disk pressure root changed identity at {}; refusing slot permits: {error:#}",
                filesystem.root.display()
            );
            let (unmeasurable_draining, unmeasurable_terminal) =
                advance_unknown_pressure_roots(journal, &service_instance, &pressure_roots, now)?;
            return Ok(Some(DiskPressureGate {
                draining: draining || unmeasurable_draining,
                terminal: terminal || unmeasurable_terminal,
                unavailable: true,
            }));
        }
        let Some(capacity) = fresh_capacity.as_ref() else {
            continue;
        };
        let pressure_floor = crate::host_capacity::pressure_floor_bytes(
            capacity,
            crate::host_capacity::DEFAULT_MIN_FREE_BYTES,
        );
        let episode = journal.disk_pressure_episode(&service_instance, &filesystem.root_id)?;
        if let Some(current) = episode.as_ref() {
            let observed_fingerprint = capacity.volume_fingerprint.as_deref();
            let identity_changed = current
                .volume_fingerprint
                .as_deref()
                .zip(observed_fingerprint)
                .is_some_and(|(old, observed)| old != observed);
            let may_rebind = if identity_changed && current.terminal {
                if slots_fenced.is_none() {
                    slots_fenced = Some(pressure_slots_are_fenced(
                        args,
                        &journal.materialized_state()?,
                    ));
                }
                slots_fenced.unwrap_or(false)
            } else {
                false
            };
            if may_rebind {
                if let Err(error) = filesystem.revalidate() {
                    eprintln!(
                        "disk pressure root changed identity before volume rebind at {}; retaining episode: {error:#}",
                        filesystem.root.display()
                    );
                    let (unmeasurable_draining, unmeasurable_terminal) =
                        advance_unknown_pressure_roots(
                            journal,
                            &service_instance,
                            &pressure_roots,
                            now,
                        )?;
                    return Ok(Some(DiskPressureGate {
                        draining: draining || unmeasurable_draining,
                        terminal: terminal || unmeasurable_terminal,
                        unavailable: true,
                    }));
                }
                let _ = journal.rebind_disk_pressure_episode_if_safe(
                    &service_instance,
                    &filesystem.root_id,
                    &filesystem.root_ids,
                    current,
                    Some(capacity.available_bytes),
                    pressure_floor,
                    observed_fingerprint.unwrap_or_default(),
                    crate::host_capacity::DEFAULT_DEGRADED_DEADLINE.as_secs(),
                    crate::host_capacity::DEFAULT_DRAIN_DEADLINE.as_secs(),
                    now,
                )?;
            } else if all_roots_healthy
                && observed_fingerprint.is_some()
                && capacity.available_bytes >= pressure_floor
            {
                let may_clear = if current.terminal {
                    if slots_fenced.is_none() {
                        slots_fenced = Some(pressure_slots_are_fenced(
                            args,
                            &journal.materialized_state()?,
                        ));
                    }
                    slots_fenced.unwrap_or(false)
                } else {
                    true
                };
                if may_clear {
                    if let Err(error) = filesystem.revalidate() {
                        eprintln!(
                            "disk pressure root changed identity before healthy CAS at {}; retaining episode: {error:#}",
                            filesystem.root.display()
                        );
                        let (unmeasurable_draining, unmeasurable_terminal) =
                            advance_unknown_pressure_roots(
                                journal,
                                &service_instance,
                                &pressure_roots,
                                now,
                            )?;
                        return Ok(Some(DiskPressureGate {
                            draining: draining || unmeasurable_draining,
                            terminal: terminal || unmeasurable_terminal,
                            unavailable: true,
                        }));
                    }
                    let _ = journal.clear_disk_pressure_episode_if_healthy(
                        &service_instance,
                        &filesystem.root_id,
                        &filesystem.root_ids,
                        current,
                        capacity.available_bytes,
                        pressure_floor,
                        observed_fingerprint.unwrap_or_default(),
                        now,
                    )?;
                }
            }
        }
        draining |= episode.as_ref().is_some_and(|episode| episode.draining);
        terminal |= episode.is_some_and(|episode| episode.terminal);
    }
    draining = false;
    terminal = false;
    let (any_episode, persisted_draining, persisted_terminal) =
        journal.disk_pressure_state(&service_instance)?;
    draining |= persisted_draining;
    terminal |= persisted_terminal;
    Ok(Some(DiskPressureGate {
        draining,
        terminal,
        unavailable: any_episode,
    }))
}

fn unknown_pressure_samples(
    roots: &[std::path::PathBuf],
) -> Vec<velnor_control::journal::DiskPressureFilesystemSample> {
    let mut samples = Vec::new();
    for root in roots {
        let filesystem_id = crate::host_capacity::pressure_root_id(root);
        if samples.iter().any(
            |sample: &velnor_control::journal::DiskPressureFilesystemSample| {
                sample.filesystem_id == filesystem_id
            },
        ) {
            continue;
        }
        samples.push(velnor_control::journal::DiskPressureFilesystemSample {
            filesystem_id,
            alias_ids: Vec::new(),
            available_bytes: None,
            min_free_bytes: crate::host_capacity::DEFAULT_MIN_FREE_BYTES,
            volume_fingerprint: None,
        });
    }
    samples
}

fn advance_unknown_pressure_roots(
    journal: &Journal,
    service_instance: &str,
    roots: &[std::path::PathBuf],
    now_unix: u64,
) -> anyhow::Result<(bool, bool)> {
    let samples = unknown_pressure_samples(roots);
    if samples.is_empty() {
        return journal
            .advance_unmeasurable_disk_pressure(
                service_instance,
                crate::host_capacity::DEFAULT_DEGRADED_DEADLINE.as_secs(),
                crate::host_capacity::DEFAULT_DRAIN_DEADLINE.as_secs(),
                now_unix,
            )
            .map_err(Into::into);
    }
    journal.advance_disk_pressure_roots(
        service_instance,
        &samples,
        crate::host_capacity::DEFAULT_DEGRADED_DEADLINE.as_secs(),
        crate::host_capacity::DEFAULT_DRAIN_DEADLINE.as_secs(),
        now_unix,
    )?;
    let (mut draining, mut terminal) = journal.advance_unmeasurable_disk_pressure(
        service_instance,
        crate::host_capacity::DEFAULT_DEGRADED_DEADLINE.as_secs(),
        crate::host_capacity::DEFAULT_DRAIN_DEADLINE.as_secs(),
        now_unix,
    )?;
    for sample in samples {
        if let Some(episode) =
            journal.disk_pressure_episode(service_instance, &sample.filesystem_id)?
        {
            draining |= episode.draining;
            terminal |= episode.terminal;
        }
    }
    Ok((draining, terminal))
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
    reconcile_duration_ms: u64,
    jit_create_attempts: u64,
    jit_create_successes: u64,
    jit_create_failures: u64,
    jit_create_latency_ms: u64,
    journal_event_attempts: u64,
    journal_durable_events: u64,
    events_per_second: f64,
    durable_events_per_second: f64,
    reconcile_overlap_count: u64,
}

impl Default for MetricsSnapshot {
    fn default() -> Self {
        Self {
            slot_processes: 0,
            job_processes: 0,
            waiter_processes: 0,
            reconcile_duration_ms: 1,
            jit_create_attempts: 0,
            jit_create_successes: 0,
            jit_create_failures: 0,
            jit_create_latency_ms: 0,
            journal_event_attempts: 0,
            journal_durable_events: 0,
            events_per_second: 0.0,
            durable_events_per_second: 0.0,
            // The controller owns one reconcile loop and never re-enters it.
            // Keep the explicit invariant in the published contract.
            reconcile_overlap_count: 0,
        }
    }
}

struct MetricsPublisherState {
    snapshot: MetricsSnapshot,
    sequence: u64,
    stopped: bool,
    last_published_at: Option<Instant>,
    last_published_event_attempts: u64,
    last_published_durable_events: u64,
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
            last_published_at: None,
            last_published_event_attempts: 0,
            last_published_durable_events: 0,
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
        state.snapshot.reconcile_duration_ms = reconcile_duration_ms.max(1);
        if let Some(durable_events) = durable_journal_event_count(&self.state_dir) {
            state.snapshot.journal_durable_events = durable_events;
        }
    }

    fn record_journal_event(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.snapshot.journal_event_attempts =
            state.snapshot.journal_event_attempts.saturating_add(1);
    }

    fn record_jit_create(&self, latency: Duration, success: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.snapshot.jit_create_attempts = state.snapshot.jit_create_attempts.saturating_add(1);
        state.snapshot.jit_create_latency_ms = state
            .snapshot
            .jit_create_latency_ms
            .saturating_add(u64::try_from(latency.as_millis()).unwrap_or(u64::MAX));
        if success {
            state.snapshot.jit_create_successes =
                state.snapshot.jit_create_successes.saturating_add(1);
        } else {
            state.snapshot.jit_create_failures =
                state.snapshot.jit_create_failures.saturating_add(1);
        }
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
    let now = Instant::now();
    if let Some(previous) = state.last_published_at {
        let elapsed = previous.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            state.snapshot.events_per_second = state
                .snapshot
                .journal_event_attempts
                .saturating_sub(state.last_published_event_attempts)
                as f64
                / elapsed;
            state.snapshot.durable_events_per_second = state
                .snapshot
                .journal_durable_events
                .saturating_sub(state.last_published_durable_events)
                as f64
                / elapsed;
        }
    }
    state.last_published_at = Some(now);
    state.last_published_event_attempts = state.snapshot.journal_event_attempts;
    state.last_published_durable_events = state.snapshot.journal_durable_events;
    state.sequence = state.sequence.saturating_add(1);
    let current = state.snapshot;
    publish_controller_metrics(
        state_dir,
        state.sequence,
        current.slot_processes,
        current.job_processes,
        current.waiter_processes,
        current.reconcile_duration_ms,
        current,
    )?;
    Ok(true)
}

pub async fn run(args: ControllerArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.state_dir)?;
    let service_instance = std::fs::canonicalize(&args.state_dir)
        .with_context(|| format!("canonicalize service instance {}", args.state_dir.display()))?
        .to_string_lossy()
        .into_owned();
    let mut journal =
        Journal::open_for_service_instance(args.state_dir.join("journal.db"), &service_instance)?;
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
    publish_controller_metrics(&args.state_dir, 0, 0, 0, 0, 1, MetricsSnapshot::default())?;
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
            lifecycle.as_ref(),
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
    snapshot: MetricsSnapshot,
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
        "reconcile_overlap_count": snapshot.reconcile_overlap_count,
        "events_per_second": snapshot.events_per_second,
        "durable_events_per_second": snapshot.durable_events_per_second,
        "jit": {
            "create_attempts": snapshot.jit_create_attempts,
            "create_successes": snapshot.jit_create_successes,
            "create_failures": snapshot.jit_create_failures,
            "create_latency_ms": snapshot.jit_create_latency_ms
        },
        "journal": {
            "transactions": sequence.saturating_add(1),
            "wal_bytes": wal_bytes,
            "event_attempts": snapshot.journal_event_attempts,
            "durable_events": snapshot.journal_durable_events
        },
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

/// Read the authoritative durable event count without borrowing the journal's
/// private SQLite connection. The controller owns the journal path; opening a
/// short-lived read-only handle keeps the publisher independent of journal
/// mutation and includes events committed by worker processes too.
fn durable_journal_event_count(state_dir: &Path) -> Option<u64> {
    let connection = rusqlite::Connection::open_with_flags(
        state_dir.join("journal.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    connection.busy_timeout(Duration::from_millis(100)).ok()?;
    connection
        .query_row("SELECT COUNT(*) FROM events", [], |row| {
            row.get::<_, i64>(0)
        })
        .ok()
        .and_then(|count| u64::try_from(count).ok())
}

/// Apply one event through the current journal path and count the controller's
/// authoritative submission. Durable rows are sampled from SQLite by the
/// publisher, because worker processes can commit events through their own
/// journal handles.
fn apply_journal_event(
    journal: &mut Journal,
    metrics: Option<&MetricsPublisher>,
    event: Event,
) -> anyhow::Result<velnor_control::journal::ReduceOutcome> {
    let outcome = journal.apply(event)?;
    if let Some(metrics) = metrics {
        metrics.record_journal_event();
    }
    Ok(outcome)
}

fn apply_journal_events(
    journal: &mut Journal,
    metrics: Option<&MetricsPublisher>,
    events: impl IntoIterator<Item = Event>,
) -> anyhow::Result<Vec<velnor_control::journal::ReduceOutcome>> {
    let events = events.into_iter().collect::<Vec<_>>();
    let outcomes = journal.apply_many(events)?;
    if let Some(metrics) = metrics {
        for _ in &outcomes {
            metrics.record_journal_event();
        }
    }
    Ok(outcomes)
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
    slots: &mut HashMap<String, Child>,
    jobs: &mut HashMap<String, Child>,
) -> anyhow::Result<()> {
    for child in slots.values() {
        request_child_shutdown(child)?;
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
    slots: &mut HashMap<String, Child>,
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
    ingest_slot_heartbeats(args, journal, total as usize, heartbeats, Some(metrics))?;
    reconcile_lifecycle_admission(journal, lifecycle)?;
    let state = journal.materialized_state()?;
    let pressure_gate = disk_pressure_gate(args, journal)?;
    // One worker-process enumeration shared by every ownership scan in this
    // cycle (slot loop plus waiter spawn): re-enumerating per slot costs a
    // `ps` fork on macOS and breaks the idle-scaling bound.
    let mut worker_enum: Option<prove::WorkerProcessEnumeration> = None;
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
        if pressure_gate.as_ref().is_some_and(|gate| gate.terminal) {
            let Some(slot) = slot else {
                continue;
            };
            let service_instance = std::fs::canonicalize(&args.state_dir)?
                .to_string_lossy()
                .into_owned();
            journal.revoke_disk_pressure_launch(&service_instance, &id, slot.generation)?;
            let active_jobs: Vec<String> = state
                .jobs
                .iter()
                .filter(|job| {
                    job.slot_id == id
                        && job.generation == slot.generation
                        && job.phase.occupies_slot()
                })
                .map(|job| job.job_id.0.clone())
                .collect();
            if !active_jobs.is_empty() {
                let mut worker_ids = vec![format!("wait-{}", id.0)];
                worker_ids.extend(active_jobs);
                worker_ids.sort();
                worker_ids.dedup();
                for worker_id in worker_ids {
                    if let Some(child) = jobs.get(&worker_id) {
                        request_child_shutdown(child)?;
                    } else {
                        signal_persisted_pressure_worker(
                            args,
                            journal,
                            &worker_id,
                            &id,
                            slot.generation,
                        )?;
                    }
                }
                // Active job rows remain occupied until ordinary orphan
                // recovery records their terminal result. The lease is
                // already revoked, so those actors cannot commit more writes.
                continue;
            }
            if slot.phase != SlotPhase2::Fenced {
                effects.extend(
                    apply_journal_event(
                        journal,
                        Some(metrics),
                        Event::SlotStale {
                            slot_id: id.clone(),
                            generation: slot.generation,
                        },
                    )?
                    .commands,
                );
            }
            if args.spawn_slots {
                let fenced_state = journal.materialized_state()?;
                if let Some(fenced_slot) = fenced_state
                    .slots
                    .iter()
                    .find(|current| current.slot_id == id && current.phase == SlotPhase2::Fenced)
                {
                    terminate_fenced_slot_actor(args, slots, jobs, &fenced_state, &id, fenced_slot)
                        .await?;
                }
            }
            continue;
        }
        if pressure_gate.as_ref().is_some_and(|gate| gate.unavailable) {
            continue;
        }
        if pressure_gate.as_ref().is_some_and(|gate| gate.draining) {
            continue;
        }
        let fenced = slot.is_some_and(|slot| slot.phase == SlotPhase2::Fenced);
        let admission_blocked = slot_has_admission_block(&state, &id, generation);
        let heartbeat_fresh = prove::slot_heartbeat_is_fresh(
            &args.state_dir,
            &id,
            generation,
            SLOT_HEARTBEAT_MAX_AGE,
        );
        let acting = slot_is_acting(
            args,
            journal,
            &state,
            jobs,
            &id,
            generation,
            &mut worker_enum,
        )?;
        // A leftover deadline from before/during a job must not fire the
        // instant the journal row is gone. The waiter is still the broker
        // actor; fencing the supervisor while that waiter lives splits
        // GitHub's view from the journal and then `child_owns_slot` blocks
        // the only generation-recovery path.
        if heartbeat_fresh || acting {
            startup_deadlines.remove(&id.0);
        } else if args.spawn_slots
            && !fenced
            && stale_slot_deadline_reached(args, slot, &id, startup_deadlines, Instant::now())
        {
            fence_stale_slot_actor(args, journal, slots, jobs, &id, generation, metrics).await?;
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
            fenced_slot_recovery_generation(args, journal, slot, &state, jobs, &mut worker_enum)?;
        let generation = fenced_generation.unwrap_or(generation);
        let process_alive = heartbeat_fresh;
        if fenced && fenced_generation.is_none() {
            continue;
        }
        if fenced_generation.is_none()
            && (admission_blocked
                || child_owns_slot(
                    args,
                    journal,
                    &state,
                    jobs,
                    &id,
                    generation,
                    &mut worker_enum,
                )?
                || !permit_needs_reconciliation(slot, generation, args.spawn_slots, process_alive))
        {
            continue;
        }
        effects.extend(
            apply_journal_event(
                journal,
                Some(metrics),
                Event::PermitReserved {
                    slot_id: id,
                    generation,
                },
            )?
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
            metrics,
            command,
        )
        .await?;
    }

    metrics.update(slots, jobs, 1);

    observe_github_and_routing_with_metrics(args, journal, pacing, remote_deadline, Some(metrics))
        .await?;

    if last_registration_reconcile.elapsed() >= REGISTRATION_RECONCILE_INTERVAL
        && pacing.rest_requests_allowed(tokio::time::Instant::now())
    {
        *last_registration_reconcile = Instant::now();
        let reconciliation = run_bounded_remote_reconciliation(
            reconcile_remote_registrations_with_metrics(args, journal, jobs, pacing, Some(metrics)),
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
                apply_journal_event(
                    journal,
                    Some(metrics),
                    Event::ExecutorProven {
                        slot_id: id.clone(),
                        generation,
                    },
                )?
                .commands,
            );
        }
        if prove::slot_heartbeat_is_fresh(&args.state_dir, &id, generation, SLOT_HEARTBEAT_MAX_AGE)
        {
            proof_effects.extend(
                apply_journal_event(
                    journal,
                    Some(metrics),
                    Event::SessionLive {
                        slot_id: id.clone(),
                        generation,
                    },
                )?
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
                apply_journal_event(
                    journal,
                    Some(metrics),
                    Event::RegistrationIntended {
                        slot_id: id,
                        generation,
                    },
                )?
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
                    startup_deadlines,
                    &mut *pacing,
                    remote_deadline,
                    metrics,
                    command,
                )
                .await?
            }
        }
    }
    register_runners(
        args,
        journal,
        pacing,
        registrations,
        remote_deadline,
        metrics,
    )
    .await?;

    spawn_ready_waiters(args, journal, jobs, &mut worker_enum)?;
    reap(jobs);
    let outbox_reconcile_due = last_outbox_reconcile.elapsed() >= OUTBOX_RECONCILIATION_INTERVAL;
    reclaim_orphaned_jobs_with_metrics(
        args,
        journal,
        remote_deadline,
        outbox_reconcile_due,
        crate::docker::client::host_call,
        Some(metrics),
    )
    .await?;
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
    startup_deadlines: &mut HashMap<String, Instant>,
    pacing: &mut GithubPacing,
    remote_deadline: tokio::time::Instant,
    metrics: &MetricsPublisher,
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
        } => {
            register_runner(
                args,
                journal,
                pacing,
                slot_id,
                generation,
                remote_deadline,
                metrics,
            )
            .await
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
    metrics: &MetricsPublisher,
) -> anyhow::Result<()> {
    register_runners(
        args,
        journal,
        pacing,
        vec![(slot_id, generation)],
        remote_deadline,
        metrics,
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
    metrics: &MetricsPublisher,
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
                let started = Instant::now();
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
                (slot_id, generation, result, started.elapsed())
            }
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
    outcomes.sort_by_key(|(slot_id, _, _, _)| slot_id.0.clone());

    for (slot_id, generation, result, latency) in outcomes {
        metrics.record_jit_create(latency, result.is_ok());
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
        let registered = apply_journal_event(
            journal,
            Some(metrics),
            Event::Registered {
                slot_id: slot_id.clone(),
                generation,
            },
        )?;
        if registered.rejected {
            continue;
        }
        let ready = apply_journal_event(
            journal,
            Some(metrics),
            Event::ReadyAttempt {
                slot_id,
                generation,
            },
        )?;
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
#[cfg(test)]
async fn reconcile_remote_registrations(
    args: &ControllerArgs,
    journal: &mut Journal,
    jobs: &mut HashMap<String, Child>,
    pacing: &mut GithubPacing,
) -> anyhow::Result<()> {
    reconcile_remote_registrations_with_metrics(args, journal, jobs, pacing, None).await
}

async fn reconcile_remote_registrations_with_metrics(
    args: &ControllerArgs,
    journal: &mut Journal,
    jobs: &mut HashMap<String, Child>,
    pacing: &mut GithubPacing,
    metrics: Option<&MetricsPublisher>,
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
        let outcome = apply_journal_event(
            journal,
            metrics,
            Event::RegistrationLost {
                slot_id: slot_id.clone(),
                generation,
            },
        )?;
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

#[cfg(all(test, feature = "test-support"))]
async fn observe_github_and_routing(
    args: &ControllerArgs,
    journal: &mut Journal,
    pacing: &mut GithubPacing,
    remote_deadline: tokio::time::Instant,
) -> anyhow::Result<()> {
    observe_github_and_routing_with_metrics(args, journal, pacing, remote_deadline, None).await
}

async fn observe_github_and_routing_with_metrics(
    args: &ControllerArgs,
    journal: &mut Journal,
    pacing: &mut GithubPacing,
    remote_deadline: tokio::time::Instant,
    metrics: Option<&MetricsPublisher>,
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
        apply_journal_event(
            journal,
            metrics,
            Event::Dependency {
                github_reachable: reachable,
            },
        )?;
    }
    let _ = prove::reconcile_from_dir(&args.state_dir)?;
    let routing = prove::observe_routing(&args.state_dir);
    apply_journal_event(
        journal,
        metrics,
        Event::Routing {
            valid: routing.valid,
            group_valid: routing.group_valid,
        },
    )?;
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
    worker_enum: &mut Option<prove::WorkerProcessEnumeration>,
) -> anyhow::Result<()> {
    let Ok(exec) = load_exec_config(&args.state_dir) else {
        return Ok(());
    };
    let state = journal.materialized_state()?;
    if state.admission_blocked || state.drain_active {
        stop_idle_waiters(jobs, &state)?;
        return Ok(());
    }
    let pressure_gate = disk_pressure_gate(args, journal)?;
    if pressure_gate_blocks_waiters(pressure_gate.as_ref()) {
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
        // waiter pid after controller restart, or an in-flight lease whose
        // containers are still tearing down must keep this slot unspawnable.
        if child_owns_slot(
            args,
            journal,
            &state,
            jobs,
            &slot.slot_id,
            slot.generation,
            worker_enum,
        )? {
            continue;
        }
        match recovery_slot_config_dir(&args.state_dir, &exec, &state, &slot.slot_id) {
            Ok(slot_dir) => {
                if crate::runner::recorded_in_flight_job_exists(&slot_dir)? {
                    continue;
                }
            }
            Err(_) => continue,
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

/// Force-remove a provably dead worker's containers before terminal recovery
/// can release its global permit or restore its slot to Ready. Failure keeps
/// the journal row and in-flight marker as retryable ownership evidence.
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
    crate::docker_lease::force_remove_job_owned_containers(&job_container, docker).map_err(
        |error| anyhow::anyhow!("remove owned containers for orphan job {job_id}: {error:#}"),
    )
}

/// Fence a destructive orphan teardown with the marker snapshot captured
/// before process-death proof. The caller's journal predicate runs while the
/// marker lock is held, before the exact recorded permit is claimed and
/// before Docker can mutate any job-labeled resource.
fn teardown_orphaned_job_under_marker_snapshot(
    slot_dir: &Path,
    marker_snapshot: Option<&crate::runner::InFlightJobRecord>,
    job_id: &str,
    still_current: impl FnOnce() -> anyhow::Result<bool>,
    teardown: &mut impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<Option<crate::runner::InFlightMarkerLock>> {
    if marker_snapshot.is_some() {
        anyhow::bail!("recorded orphan teardown requires controller owner-death proof");
    }
    let lock = crate::runner::lock_in_flight_marker_snapshot(slot_dir, marker_snapshot)?;
    if !still_current()? {
        return Ok(None);
    }
    teardown(job_id)?;
    Ok(Some(lock))
}

#[allow(clippy::too_many_arguments)]
fn capture_recorded_owner_death_proof(
    args: &ControllerArgs,
    slot_dir: &Path,
    slot_id: &SlotId,
    generation: Generation,
    job_id: &str,
    job_snapshot: Option<&velnor_control::journal::JobRecord>,
    marker_record: &crate::runner::InFlightJobRecord,
) -> anyhow::Result<crate::node::controller::RecordedOwnerDeathProof> {
    let marker_lock = crate::runner::lock_in_flight_marker_snapshot(slot_dir, Some(marker_record))?;
    let proof = RecordedOwnerDeathProof::capture(
        args,
        slot_dir,
        slot_id,
        generation,
        job_id,
        job_snapshot,
        marker_record,
        marker_lock,
    )?;
    crate::runner::claim_recorded_attempt_for_terminal_recovery(marker_record, &proof)?;
    Ok(proof)
}

#[allow(clippy::too_many_arguments)]
fn teardown_recorded_job_with_death_proof(
    args: &ControllerArgs,
    slot_dir: &Path,
    slot_id: &SlotId,
    generation: Generation,
    job_id: &str,
    job_snapshot: Option<&velnor_control::journal::JobRecord>,
    marker_record: &crate::runner::InFlightJobRecord,
    teardown: &mut impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<crate::node::controller::RecordedOwnerDeathProof> {
    let proof = capture_recorded_owner_death_proof(
        args,
        slot_dir,
        slot_id,
        generation,
        job_id,
        job_snapshot,
        marker_record,
    )?;
    teardown(job_id)?;
    proof.recheck_for_record(marker_record)?;
    Ok(proof)
}

fn journal_job_matches_recovery_snapshot(
    journal: &mut Journal,
    expected: &velnor_control::journal::JobRecord,
) -> anyhow::Result<bool> {
    let state = journal.materialized_state()?;
    Ok(state.jobs.iter().any(|current| {
        current.job_id == expected.job_id
            && current.slot_id == expected.slot_id
            && current.generation == expected.generation
            && current.attempt == expected.attempt
            && current.worker == expected.worker
            && current.phase == expected.phase
    }))
}

/// Return slots occupied by job workers that died without a terminal
/// completion (daemon drain mid-run, OOM-kill, reboot). Without this the
/// slot stays `Assigned` forever and advertised capacity never recovers.
#[cfg(test)]
async fn reclaim_orphaned_jobs(
    args: &ControllerArgs,
    journal: &mut Journal,
    remote_deadline: tokio::time::Instant,
    scan_persisted_markers: bool,
    mut docker: impl FnMut(&[String]) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    reclaim_orphaned_jobs_with_metrics(
        args,
        journal,
        remote_deadline,
        scan_persisted_markers,
        &mut docker,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn reclaim_orphaned_jobs_with_metrics(
    args: &ControllerArgs,
    journal: &mut Journal,
    remote_deadline: tokio::time::Instant,
    scan_persisted_markers: bool,
    mut docker: impl FnMut(&[String]) -> anyhow::Result<String>,
    metrics: Option<&MetricsPublisher>,
) -> anyhow::Result<()> {
    reclaim_orphaned_jobs_with_backend_policy(
        args,
        journal,
        remote_deadline,
        scan_persisted_markers,
        &mut docker,
        metrics,
        || {
            match crate::execution::load_execution_file(&args.state_dir, None) {
                Ok(execution) => Ok(execution.backend()),
                // No execution.toml anywhere: proceed with legacy hermetic
                // recovery, which never touches host docker. (`backend` below
                // feeds only `permits_host_docker_maintenance`, so MicroVm
                // here encodes "proceed without docker", exactly as main's
                // `.ok().map()` did with `None`. A present-but-unresolvable
                // selection still fails closed and defers below.)
                Err(error) if error.is_missing_file() => {
                    Ok(velnor_model::ExecutionBackendKind::MicroVm)
                }
                Err(error) => Err(error).context("load execution backend for orphan recovery"),
            }
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn reclaim_orphaned_jobs_with_backend_policy(
    args: &ControllerArgs,
    journal: &mut Journal,
    remote_deadline: tokio::time::Instant,
    scan_persisted_markers: bool,
    mut docker: impl FnMut(&[String]) -> anyhow::Result<String>,
    metrics: Option<&MetricsPublisher>,
    resolve_backend: impl FnOnce() -> anyhow::Result<velnor_model::ExecutionBackendKind>,
) -> anyhow::Result<()> {
    let state = journal.materialized_state()?;
    let candidate_jobs: Vec<_> = state
        .jobs
        .iter()
        .filter(|job| job.phase.occupies_slot())
        .cloned()
        .collect();
    if candidate_jobs.is_empty() && !scan_persisted_markers {
        return Ok(());
    }

    // Resolve the backend before reading owner markers or proving process
    // death. An unresolved selection cannot authorize any orphan recovery:
    // Docker jobs require host teardown, while MicroVM jobs must avoid Docker.
    let backend = match resolve_backend() {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("orphan recovery deferred: cannot prove execution backend: {error:#}");
            return Ok(());
        }
    };

    // Resolve the slot layout before proving process death so each job's
    // marker snapshot is captured before PID/launch-nonce inspection. Recovery
    // defers when this evidence root cannot be resolved.
    let exec = match load_exec_config(&args.state_dir) {
        Ok(exec) => exec,
        Err(error) => {
            eprintln!("orphan recovery deferred: cannot load daemon execution config: {error:#}");
            return Ok(());
        }
    };

    let mut orphan_jobs = Vec::new();
    let mut worker_enum: Option<prove::WorkerProcessEnumeration> = None;
    for job in candidate_jobs {
        let slot_dir = recovery_slot_config_dir(&args.state_dir, &exec, &state, &job.slot_id)?;
        let marker_snapshot = crate::runner::recorded_in_flight_job_record(&slot_dir);
        // The ready-slot waiter retains its own PID marker while executing a
        // job, so prove both possible owners against the same lease. A live
        // unrelated process with either recycled PID never protects an
        // orphan row.
        let waiter_id = format!("wait-{}", job.slot_id.0);
        let job_worker_live = persisted_worker_owns_slot(
            args,
            journal,
            &job.job_id.0,
            &job.slot_id,
            job.generation,
            &mut worker_enum,
        )?;
        let waiter_live = persisted_worker_owns_slot(
            args,
            journal,
            &waiter_id,
            &job.slot_id,
            job.generation,
            &mut worker_enum,
        )?;
        if !job_worker_live && !waiter_live {
            orphan_jobs.push((job, marker_snapshot?));
        }
    }

    if orphan_jobs.is_empty() && !scan_persisted_markers {
        return Ok(());
    }

    let docker_backend =
        velnor_model::ExecutionBackendKind::permits_host_docker_maintenance(Some(backend));
    let mut teardown =
        |job_id: &str| teardown_orphaned_job_containers(job_id, docker_backend, &mut docker);

    for (job, marker_snapshot) in orphan_jobs {
        // One Completing row must not abort the fleet cycle. Log and move on
        // so other slots still reclaim; the next tick retries this job.
        if let Err(error) = recover_one_orphaned_job(
            args,
            journal,
            &exec,
            &state,
            &job,
            marker_snapshot.as_ref(),
            remote_deadline,
            &mut teardown,
            metrics,
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

    // A prior RemoteAcked event removes the journal job before local storage
    // release and marker deletion. Scan every exact persisted slot path so a
    // crash in that gap remains retryable even though no job row remains.
    let current = journal.materialized_state()?;
    for slot in &state.slots {
        let slot_dir = recovery_slot_config_dir(&args.state_dir, &exec, &state, &slot.slot_id)?;
        let Some(marker_record) = crate::runner::recorded_in_flight_job_record(&slot_dir)? else {
            continue;
        };
        let marker_job_id = marker_record.recorded_job_id()?.to_owned();
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
        // A ready-slot waiter persists the in-flight marker before journal
        // admission while its ownership pid is still keyed `wait-{slot}`;
        // the `write_owned_pid(job_id)` marker appears only when the job
        // worker spawns. Consult both markers independently. Even after
        // RemoteAcked, a live same-incarnation owner remains a barrier until
        // it exits and local cleanup can safely take over.
        let waiter_id = format!("wait-{}", slot.slot_id.0);
        // Destructive teardown fails closed toward deferral: any live owner
        // pid is a barrier, even when the strict command/nonce proof cannot
        // verify it. A recycled pid merely delays this marker one tick, while
        // tearing down a live owner would corrupt a running job. (The spawn
        // path fails closed in the other direction and stays strict via
        // `persisted_worker_owns_slot`.)
        let worker_live =
            cleanup::read_owned_pid(&args.state_dir, &marker_job_id, slot.generation.0)
                .is_some_and(prove::pid_is_alive);
        let waiter_live = cleanup::read_owned_pid(&args.state_dir, &waiter_id, slot.generation.0)
            .is_some_and(prove::pid_is_alive);
        if worker_live || waiter_live {
            // The marker can legitimately exist between persistence and
            // journal admission. Never terminalize a live owner in that
            // window; the next reconciliation tick will retry.
            continue;
        }
        // Marker-only recovery follows a prior RemoteAcked event, so the
        // normal journal row no longer fences capacity. Recheck the absence
        // of a replacement journal attempt under the marker lock, claim the
        // exact marker token, and only then remove Docker resources. A proof
        // race defers one tick, but an infrastructure teardown failure
        // propagates so stuck resources stay visible instead of warn-muted.
        let owner_death = match capture_recorded_owner_death_proof(
            args,
            &slot_dir,
            &slot.slot_id,
            slot.generation,
            &marker_job_id,
            None,
            &marker_record,
        ) {
            Ok(proof) => proof,
            Err(error) => {
                eprintln!(
                    "Warning: marker-only owner-death proof for job {} failed; retrying next cycle: {error:#}",
                    marker_job_id
                );
                continue;
            }
        };
        if let Err(error) = teardown(&marker_job_id) {
            crate::runner::rollback_recorded_cleanup_claim_to_uncertain(&marker_record)?;
            return Err(error);
        }
        if let Err(error) = owner_death.recheck_for_record(&marker_record) {
            eprintln!(
                "Warning: marker-only owner-death proof for job {} changed during teardown; retrying next cycle: {error:#}",
                marker_job_id
            );
            continue;
        }
        let remote_acked =
            journal.has_remote_terminal_ack(&JobId(marker_job_id.clone()), slot.generation)?;
        let cleaned = if remote_acked {
            crate::runner::cleanup_recorded_in_flight_job_for_record(
                &slot_dir,
                &marker_record,
                owner_death,
            )?
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
                    owner_death,
                ),
                "complete marker-only in-flight job after journal acceptance gap",
            )
            .await?
            else {
                continue;
            };
            cleaned
        };
        if !cleaned {
            return Err(anyhow::anyhow!(
                "marker-only in-flight job {} disappeared during recovery",
                marker_job_id
            ));
        }
        cleanup::remove_outbox(&args.state_dir, &marker_job_id, slot.generation.0)?;
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
    marker_snapshot: Option<&crate::runner::InFlightJobRecord>,
    remote_deadline: tokio::time::Instant,
    teardown: &mut impl FnMut(&str) -> anyhow::Result<()>,
    metrics: Option<&MetricsPublisher>,
) -> anyhow::Result<()> {
    let slot_dir = recovery_slot_config_dir(&args.state_dir, exec, state, &job.slot_id)?;
    let marker_job_id = marker_snapshot
        .map(crate::runner::InFlightJobRecord::recorded_job_id)
        .transpose()?
        .map(str::to_owned);
    let pending_completion = job.phase == JobPhase2::Completing
        && state.outbox.iter().any(|row| {
            row.job_id == job.job_id
                && row.generation == job.generation
                && row.intended
                && !row.remote_acked
        });
    if let Some(marker_job_id) = marker_job_id.as_deref()
        && marker_job_id != job.job_id.0
    {
        anyhow::bail!(
            "in-flight marker job {} does not match orphan job {}",
            marker_job_id,
            job.job_id.0
        );
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
    // A recorded job keeps the controller's process-death proof and marker
    // flock through remote completion and permit/reservation cleanup. Jobs
    // without a marker still need only the short lock around teardown.
    let (mut recorded_owner_death, mut marker_lock) = if let Some(record) = marker_snapshot {
        (
            Some(teardown_recorded_job_with_death_proof(
                args,
                &slot_dir,
                &job.slot_id,
                job.generation,
                &job.job_id.0,
                Some(job),
                record,
                teardown,
            )?),
            None,
        )
    } else {
        let marker_lock = teardown_orphaned_job_under_marker_snapshot(
            &slot_dir,
            None,
            &job.job_id.0,
            || journal_job_matches_recovery_snapshot(journal, job),
            teardown,
        )?;
        let Some(marker_lock) = marker_lock else {
            return Ok(());
        };
        (None, Some(marker_lock))
    };
    if let Some(stored) = load_local_runner_config(&slot_dir)? {
        if marker_job_id.is_some() {
            let marker_record = marker_snapshot
                .ok_or_else(|| anyhow::anyhow!("marker job id requires a recorded marker"))?;
            let cleanup = if pending_completion {
                let owner_death = recorded_owner_death
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("recorded recovery lost owner-death proof"))?;
                match defer_remote_recovery_on_timeout(
                    remaining_remote_budget(remote_deadline),
                    crate::runner::replay_recorded_completion_for_record(
                        &slot_dir,
                        &stored,
                        &args.state_dir,
                        marker_record,
                        owner_death,
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
                            // Teardown succeeded before replay started, so a
                            // spent budget may safely restore the slot.
                            abandon_if_budget_spent(args, journal, &row)?;
                        }
                        return Ok(());
                    }
                }
            } else if let Some(conclusion) = job.terminal_conclusion.as_deref() {
                let owner_death = recorded_owner_death
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("recorded recovery lost owner-death proof"))?;
                match defer_remote_recovery_on_timeout(
                    remaining_remote_budget(remote_deadline),
                    crate::runner::complete_recorded_in_flight_job_with_terminal_conclusion_for_record(
                        &slot_dir,
                        &stored,
                        marker_record,
                        owner_death,
                        conclusion,
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
                let owner_death = recorded_owner_death
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("recorded recovery lost owner-death proof"))?;
                match defer_remote_recovery_on_timeout(
                    remaining_remote_budget(remote_deadline),
                    crate::runner::complete_recorded_in_flight_job_for_record(
                        &slot_dir,
                        &stored,
                        marker_record,
                        owner_death,
                        Some("stale_busy".to_string()),
                        "GitHub DELETE 422 / offline+busy: fail-closed leftover job so the runner lease can be released",
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
    if !current.jobs.iter().any(|row| {
        row.job_id == job.job_id
            && row.slot_id == job.slot_id
            && row.generation == job.generation
            && row.attempt == job.attempt
            && row.worker == job.worker
            && row.phase == job.phase
    }) {
        // Recorded resources were removed before terminal acknowledgement.
        return Ok(());
    }
    if pending_completion {
        eprintln!(
            "Warning: completing job {} has no recoverable terminal acknowledgement; retrying next cycle",
            job.job_id.0
        );
        return Ok(());
    }
    // The worker is dead (both ownership pids are gone), so any
    // container still carrying this job's label is a leak. Remove them
    // before the slot returns to Ready; after JobWorkerLost no path
    // would ever touch them again.
    if let (Some(owner_death), Some(record)) = (&recorded_owner_death, marker_snapshot) {
        owner_death.recheck_for_record(record)?;
    }
    let lost = apply_journal_event(
        journal,
        metrics,
        Event::JobWorkerLost {
            job_id: job.job_id.clone(),
            generation: job.generation,
        },
    )?;
    drop(marker_lock.take());
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
            outbox_owner_is_live(args, journal, &state, &job_id, generation)?
        } else {
            row.is_some_and(|row| row.intended && !row.remote_acked)
                || row.is_none()
                    && outbox_owner_is_live(args, journal, &state, &job_id, generation)?
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

fn outbox_owner_is_live(
    args: &ControllerArgs,
    journal: &Journal,
    state: &velnor_control::journal::FleetState,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<bool> {
    let generation = Generation(generation);
    let Some(slot_id) = state
        .jobs
        .iter()
        .find(|job| job.job_id.0 == job_id && job.generation == generation)
        .map(|job| job.slot_id.clone())
    else {
        // A writer that can still be publishing a temporary outbox must have
        // an active journal row. Once that row is gone, any remaining PID is
        // insufficient proof of ownership and cannot authorize retaining an
        // orphan file. Its marker stays until a slot-scoped proof can retire it.
        let _ = read_valid_owned_pid(&args.state_dir, job_id, generation)?;
        return Ok(false);
    };

    let mut worker_enum: Option<prove::WorkerProcessEnumeration> = None;
    for owner_id in [job_id.to_owned(), format!("wait-{}", slot_id.0)] {
        if read_valid_owned_pid(&args.state_dir, &owner_id, generation)?.is_none() {
            continue;
        }
        if persisted_worker_owns_slot(
            args,
            journal,
            &owner_id,
            &slot_id,
            generation,
            &mut worker_enum,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
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
    args: &ControllerArgs,
    journal: &Journal,
    slot: Option<&SlotRecord>,
    state: &velnor_control::journal::FleetState,
    jobs: &HashMap<String, Child>,
    worker_enum: &mut Option<prove::WorkerProcessEnumeration>,
) -> anyhow::Result<Option<Generation>> {
    let Some(slot) = slot.filter(|slot| slot.phase == SlotPhase2::Fenced) else {
        return Ok(None);
    };
    if slot_has_admission_block(state, &slot.slot_id, slot.generation)
        || child_owns_slot(
            args,
            journal,
            state,
            jobs,
            &slot.slot_id,
            slot.generation,
            worker_enum,
        )?
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
    args: &ControllerArgs,
    journal: &Journal,
    state: &velnor_control::journal::FleetState,
    jobs: &HashMap<String, Child>,
    slot_id: &SlotId,
    generation: Generation,
    worker_enum: &mut Option<prove::WorkerProcessEnumeration>,
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
    // On restart, adopt only a process whose complete command and nonce match
    // the current durable lease. A reused PID is not process ownership.
    if persisted_worker_owns_slot(args, journal, &waiter_id, slot_id, generation, worker_enum)? {
        return Ok(true);
    }
    for job in state
        .jobs
        .iter()
        .filter(|job| job.slot_id == *slot_id && job.generation == generation)
    {
        if persisted_worker_owns_slot(
            args,
            journal,
            &job.job_id.0,
            slot_id,
            generation,
            worker_enum,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn persisted_worker_owns_slot(
    args: &ControllerArgs,
    journal: &Journal,
    job_id: &str,
    slot_id: &SlotId,
    generation: Generation,
    worker_enum: &mut Option<prove::WorkerProcessEnumeration>,
) -> anyhow::Result<bool> {
    persisted_worker_owns_slot_at(
        &args.state_dir,
        &args.scope,
        journal,
        job_id,
        slot_id,
        generation,
        worker_enum,
    )
}

fn persisted_worker_owns_slot_at(
    state_dir: &Path,
    scope: &str,
    journal: &Journal,
    job_id: &str,
    slot_id: &SlotId,
    generation: Generation,
    worker_enum: &mut Option<prove::WorkerProcessEnumeration>,
) -> anyhow::Result<bool> {
    let service_instance = std::fs::canonicalize(state_dir)
        .with_context(|| format!("canonicalize service instance {}", state_dir.display()))?
        .to_string_lossy()
        .into_owned();
    let current_nonce =
        journal.disk_pressure_launch_nonce(&service_instance, slot_id, generation)?;
    let Some(pid) = read_valid_owned_pid(state_dir, job_id, generation)? else {
        // `Command::spawn` and marker publication are separate filesystem and
        // process operations. If the controller died between them, recover a
        // live child by its complete command line and launch nonce. This also
        // leaves a live stale-nonce child as an ownership barrier until it
        // exits, instead of starting a second actor in the same slot. The
        // enumeration is shared per cycle: slot children never match worker
        // discovery (`slot` vs `job` argv), so mid-cycle spawns cannot hide a
        // match from a later scan in the same cycle.
        let enumeration = match worker_enum {
            Some(enumeration) => enumeration,
            None => worker_enum.insert(prove::enumerate_worker_processes()?),
        };
        let discovered = prove::discover_cached_worker_processes(
            enumeration,
            state_dir,
            job_id,
            slot_id,
            generation,
            scope,
            current_nonce.as_deref(),
        );
        if discovered.len() > 1 {
            anyhow::bail!(
                "multiple unmarked workers match slot {} generation {} job {}",
                slot_id.0,
                generation.0,
                job_id
            );
        }
        let Some((pid, proof)) = discovered.into_iter().next() else {
            return Ok(false);
        };
        cleanup::write_owned_pid(state_dir, job_id, generation.0, pid)
            .with_context(|| format!("adopt unmarked worker {job_id} pid {pid}"))?;
        return match proof {
            prove::JobWorkerProcessProof::Current
            | prove::JobWorkerProcessProof::StaleSameWorker
            | prove::JobWorkerProcessProof::Unknown => Ok(true),
            prove::JobWorkerProcessProof::OtherProcess => Ok(false),
        };
    };
    let marker_identity = cleanup::read_owned_process_identity(state_dir, job_id, generation.0);
    match prove::job_worker_process_proof(
        pid,
        state_dir,
        job_id,
        slot_id,
        generation,
        scope,
        current_nonce.as_deref(),
    ) {
        prove::JobWorkerProcessProof::Current => {
            let Some(live_identity) = prove::process_instance_identity(pid) else {
                return Ok(true);
            };
            match marker_identity {
                Some(marker) if marker == live_identity => Ok(true),
                Some(_) => {
                    retire_stale_owned_pid_marker(state_dir, job_id, generation, pid)?;
                    persisted_worker_owns_slot_at(
                        state_dir,
                        scope,
                        journal,
                        job_id,
                        slot_id,
                        generation,
                        worker_enum,
                    )
                }
                None => {
                    cleanup::write_owned_pid(state_dir, job_id, generation.0, pid)?;
                    Ok(true)
                }
            }
        }
        // A live Velnor worker with a replaced nonce is not adopted. Keep its
        // marker as a barrier until its next pressure poll observes the fence
        // and exits; spawning alongside it would split one slot's ownership.
        prove::JobWorkerProcessProof::StaleSameWorker => {
            let Some(live_identity) = prove::process_instance_identity(pid) else {
                return Ok(true);
            };
            match marker_identity {
                Some(marker) if marker == live_identity => Ok(true),
                Some(_) => {
                    retire_stale_owned_pid_marker(state_dir, job_id, generation, pid)?;
                    persisted_worker_owns_slot_at(
                        state_dir,
                        scope,
                        journal,
                        job_id,
                        slot_id,
                        generation,
                        worker_enum,
                    )
                }
                None => {
                    cleanup::write_owned_pid(state_dir, job_id, generation.0, pid)?;
                    Ok(true)
                }
            }
        }
        prove::JobWorkerProcessProof::Unknown => Ok(true),
        prove::JobWorkerProcessProof::OtherProcess => {
            retire_stale_owned_pid_marker(state_dir, job_id, generation, pid)?;
            Ok(false)
        }
    }
}

fn signal_persisted_pressure_worker(
    args: &ControllerArgs,
    journal: &Journal,
    job_id: &str,
    slot_id: &SlotId,
    generation: Generation,
) -> anyhow::Result<()> {
    let mut worker_enum: Option<prove::WorkerProcessEnumeration> = None;
    if !persisted_worker_owns_slot(args, journal, job_id, slot_id, generation, &mut worker_enum)? {
        return Ok(());
    }
    let Some(pid) = read_valid_owned_pid(&args.state_dir, job_id, generation)? else {
        anyhow::bail!("pressure worker {job_id} lost its process marker during terminal fencing");
    };
    let service_instance = std::fs::canonicalize(&args.state_dir)
        .with_context(|| format!("canonicalize service instance {}", args.state_dir.display()))?
        .to_string_lossy()
        .into_owned();
    match prove::job_worker_process_proof(
        pid,
        &args.state_dir,
        job_id,
        slot_id,
        generation,
        &args.scope,
        journal
            .disk_pressure_launch_nonce(&service_instance, slot_id, generation)?
            .as_deref(),
    ) {
        prove::JobWorkerProcessProof::Current | prove::JobWorkerProcessProof::StaleSameWorker => {
            let Some(live_identity) = prove::process_instance_identity(pid) else {
                anyhow::bail!(
                    "cannot prove process instance for pressure worker {job_id} pid {pid}"
                );
            };
            match cleanup::read_owned_process_identity(&args.state_dir, job_id, generation.0) {
                Some(marker) if marker == live_identity => {}
                Some(_) => {
                    retire_stale_owned_pid_marker(&args.state_dir, job_id, generation, pid)?;
                    return Ok(());
                }
                None => cleanup::write_owned_pid(&args.state_dir, job_id, generation.0, pid)?,
            }
            send_pid_signal(pid, libc::SIGTERM)
        }
        prove::JobWorkerProcessProof::OtherProcess => {
            retire_stale_owned_pid_marker(&args.state_dir, job_id, generation, pid)
        }
        prove::JobWorkerProcessProof::Unknown => {
            anyhow::bail!("cannot safely signal unproven pressure worker {job_id} pid {pid}")
        }
    }
}

fn read_valid_owned_pid(
    state_dir: &Path,
    job_id: &str,
    generation: Generation,
) -> anyhow::Result<Option<u32>> {
    let path = cleanup::owned_path(state_dir, job_id, generation.0);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("ownership marker must not be a symlink: {}", path.display());
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!("ownership marker is not a regular file: {}", path.display());
        }
        Ok(_) => cleanup::read_owned_pid(state_dir, job_id, generation.0)
            .map(Some)
            .ok_or_else(|| {
                anyhow::anyhow!("ownership marker has no valid pid: {}", path.display())
            }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn retire_stale_owned_pid_marker(
    state_dir: &Path,
    job_id: &str,
    generation: Generation,
    expected_pid: u32,
) -> anyhow::Result<()> {
    if cleanup::read_owned_pid(state_dir, job_id, generation.0) != Some(expected_pid) {
        return Ok(());
    }
    let path = cleanup::owned_path(state_dir, job_id, generation.0);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("retire stale worker ownership marker {}", path.display())),
    }
}

/// Journal work or a live waiter/worker means the slot is still acting.
/// Supervisor-heartbeat stale must not fence through that window: the waiter
/// is what GitHub talks to, and leaving it up after `SlotStale` deadlocks
/// generation recovery.
fn slot_is_acting(
    args: &ControllerArgs,
    journal: &Journal,
    state: &velnor_control::journal::FleetState,
    jobs: &HashMap<String, Child>,
    slot_id: &SlotId,
    generation: Generation,
    worker_enum: &mut Option<prove::WorkerProcessEnumeration>,
) -> anyhow::Result<bool> {
    Ok(slot_has_admission_block(state, slot_id, generation)
        || child_owns_slot(args, journal, state, jobs, slot_id, generation, worker_enum)?)
}

async fn reap_supervised_child(
    children: &mut HashMap<String, Child>,
    key: &str,
    label: &str,
) -> anyhow::Result<()> {
    if !children.contains_key(key) {
        return Ok(());
    }
    // Proof: the `contains_key` guard above holds with no `await` between
    // it and this lookup (shutdown is synchronous), so the entry is `Some`.
    #[allow(clippy::expect_used, reason = "contains_key just proved presence")]
    request_child_shutdown(children.get(key).expect("child still present"))?;
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
    slots: &mut HashMap<String, Child>,
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
    jobs: &mut HashMap<String, Child>,
    id: &SlotId,
    generation: Generation,
    metrics: &MetricsPublisher,
) -> anyhow::Result<()> {
    let outcome = apply_journal_event(
        journal,
        Some(metrics),
        Event::SlotStale {
            slot_id: id.clone(),
            generation,
        },
    )?;
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
    seen: &mut HashMap<String, (u32, u64)>,
    metrics: Option<&MetricsPublisher>,
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
    let outcomes = apply_journal_events(
        journal,
        metrics,
        pending.iter().map(|(id, heartbeat)| Event::SlotHeartbeat {
            slot_id: id.clone(),
            generation: Generation(heartbeat.generation),
            pid: heartbeat.pid,
        }),
    )?;
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
    let mut worker_enum: Option<prove::WorkerProcessEnumeration> = None;
    if persisted_worker_owns_slot(
        args,
        journal,
        job_id,
        &slot_id,
        generation,
        &mut worker_enum,
    )? {
        return Ok(());
    }
    let service_instance = std::fs::canonicalize(&args.state_dir)
        .with_context(|| format!("canonicalize service instance {}", args.state_dir.display()))?
        .to_string_lossy()
        .into_owned();
    let issued_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let pressure_nonce =
        journal.issue_disk_pressure_launch(&service_instance, &slot_id, generation, issued_unix)?;
    let pressure_journal_path = Path::new(&service_instance).join("journal.db");
    let exe = crate::service::node_service_executable()?;
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
        .arg("--pressure-launch-nonce")
        .arg(&pressure_nonce)
        .arg("--scope")
        .arg(&args.scope)
        .env(
            crate::host_capacity::PRESSURE_SERVICE_INSTANCE_ENV,
            &service_instance,
        )
        .env(
            crate::host_capacity::PRESSURE_LAUNCH_NONCE_ENV,
            &pressure_nonce,
        )
        .env(crate::host_capacity::PRESSURE_SLOT_ID_ENV, &slot_id.0)
        .env(
            crate::host_capacity::PRESSURE_GENERATION_ENV,
            generation.0.to_string(),
        )
        .env(
            crate::host_capacity::PRESSURE_JOURNAL_PATH_ENV,
            &pressure_journal_path,
        )
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
    use velnor_control::permit_ledger::PermitState;
    #[cfg(feature = "test-support")]
    use wiremock::matchers::{method, path};
    #[cfg(feature = "test-support")]
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn owner_death_authority_requires_both_worker_and_waiter_dead() {
        let mut checked = Vec::new();
        let error = ensure_recorded_owners_dead("job-1", "wait-slot-1", |owner_id| {
            checked.push(owner_id.to_owned());
            Ok(owner_id == "wait-slot-1")
        })
        .unwrap_err();
        assert!(error.to_string().contains("still live"));
        assert_eq!(checked, ["job-1", "wait-slot-1"]);

        ensure_recorded_owners_dead("job-1", "wait-slot-1", |_| Ok(false)).unwrap();
    }

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
            Event::JobAcquisitionIntended {
                slot_id: slot_id.clone(),
                job_id: job_id.clone(),
                generation,
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
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
            lifecycle: None,
        }
    }

    #[test]
    fn terminal_pressure_gate_holds_waiters_until_recovery_clears_it() {
        use velnor_control::journal::JobRecord;

        assert!(!pressure_gate_blocks_waiters(None));
        assert!(!pressure_gate_blocks_waiters(Some(&DiskPressureGate {
            draining: false,
            terminal: false,
            unavailable: false,
        })));
        assert!(pressure_gate_blocks_waiters(Some(&DiskPressureGate {
            draining: true,
            terminal: false,
            unavailable: false,
        })));
        assert!(pressure_gate_blocks_waiters(Some(&DiskPressureGate {
            draining: false,
            terminal: true,
            unavailable: false,
        })));
        assert!(pressure_gate_blocks_waiters(Some(&DiskPressureGate {
            draining: false,
            terminal: false,
            unavailable: true,
        })));

        let args = controller_test_args(PathBuf::from("/service-instance"));
        let mut state = FleetState::default();
        state.slots.push(SlotRecord {
            slot_id: slot_id(&args.scope, 1),
            generation: Generation::INITIAL,
            phase: SlotPhase2::Ready,
            permit_held: true,
            routing_valid: true,
            session_live: true,
            executor_proven: true,
            registered: true,
            pid: None,
            heartbeat_unix: 0,
        });
        assert!(!pressure_slots_are_fenced(&args, &state));
        state.slots[0].phase = SlotPhase2::Fenced;
        assert!(pressure_slots_are_fenced(&args, &state));
        state.jobs.push(JobRecord {
            job_id: JobId("outside-desired-range".to_owned()),
            slot_id: SlotId("scope-2".to_owned()),
            generation: Generation::INITIAL,
            attempt: 1,
            worker: "worker-1".to_owned(),
            phase: JobPhase2::Running,
            accepted_unix: 1,
            terminal_conclusion: None,
            provisional: false,
            plan_id: String::new(),
            run_service_url: String::new(),
            probe_attempts: 0,
            probe_deadline_unix: 0,
        });
        assert!(!pressure_slots_are_fenced(&args, &state));
    }

    #[test]
    fn missing_completion_payload_is_recorded_and_releases_exact_slot() {
        let dir = metrics_test_dir("missing-completion-payload");
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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

    fn open_controller_test_journal(
        state_dir: &Path,
        filename: &str,
    ) -> velnor_control::store::StoreResult<Journal> {
        let service_instance = std::fs::canonicalize(state_dir)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        Journal::open_for_service_instance(state_dir.join(filename), &service_instance)
    }

    /// Inject an impossible persisted state for corruption-recovery tests.
    /// The write gate mirrors Journal's transaction envelope; ordinary test
    /// setup must use the bound Journal APIs instead of raw SQL.
    fn mutate_journal_fixture_with_write_gate(database: &Path, sql: &str) {
        let mut connection = rusqlite::Connection::open(database).unwrap();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute("INSERT INTO journal_write_gate (id) VALUES (1)", [])
            .unwrap();
        transaction.execute_batch(sql).unwrap();
        transaction
            .execute("DELETE FROM journal_write_gate WHERE id = 1", [])
            .unwrap();
        transaction.commit().unwrap();
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
            MetricsSnapshot::default(),
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
        publish_controller_metrics(&dir, 1, 0, 0, 0, 1, MetricsSnapshot::default()).unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
            Event::JobAcquisitionIntended {
                slot_id: SlotId("velnor-1".to_owned()),
                job_id: JobId("job-1".to_owned()),
                generation: Generation::INITIAL,
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
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
        let args = controller_test_args(dir.clone());
        assert!(child_owns_slot(
            &args,
            &journal,
            &state,
            &jobs,
            &SlotId("velnor-1".to_owned()),
            Generation::INITIAL,
            &mut None,
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();

        assert!(!should_drain(false, &journal, None));
        assert!(should_drain(true, &journal, None));

        journal.set_drain(2).unwrap();
        assert!(should_drain(false, &journal, None));

        let draining = drain_test_lifecycle(&dir, "draining", MutationKind::Drain);
        let ready = drain_test_lifecycle(&dir, "ready", MutationKind::Uncordon);
        let fresh_journal = open_controller_test_journal(&dir, "other.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
        journal.set_admission_blocked(4).unwrap();
        let lifecycle = drain_test_lifecycle(&dir, "primary", MutationKind::Uncordon);
        mutate_journal_fixture_with_write_gate(
            &dir.join("journal.db"),
            "UPDATE meta SET value = 'corrupt' WHERE key = 'admission';",
        );

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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        rusqlite::Connection::open(dir.join("journal.db"))
            .unwrap()
            .execute_batch("DROP TRIGGER IF EXISTS drain_edge_test_block_meta_writes;")
            .unwrap();
        assert!(!journal.materialized_state().unwrap().drain_active);

        let child = || Command::new("sleep").arg("30").spawn().unwrap();
        let mut slots = HashMap::from([(String::from("velnor-1"), child())]);
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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

    #[tokio::test]
    async fn metrics_publisher_reports_supported_jit_and_journal_labels() {
        let dir = metrics_test_dir("telemetry-labels");
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
        journal.apply(Event::ControlLive).unwrap();

        let mut publisher = MetricsPublisher::start(&dir);
        publisher.record_jit_create(Duration::from_millis(3), true);
        publisher.record_jit_create(Duration::from_millis(7), false);
        publisher.record_journal_event();
        publisher.record_journal_event();
        publisher.update(&HashMap::new(), &HashMap::new(), 11);
        publisher.stop_and_publish().await.unwrap();

        let metrics: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("controller-metrics.json")).unwrap())
                .unwrap();
        assert_eq!(metrics["jit"]["create_attempts"], json!(2));
        assert_eq!(metrics["jit"]["create_successes"], json!(1));
        assert_eq!(metrics["jit"]["create_failures"], json!(1));
        assert_eq!(metrics["jit"]["create_latency_ms"], json!(10));
        assert_eq!(metrics["journal"]["event_attempts"], json!(2));
        assert_eq!(metrics["journal"]["durable_events"], json!(1));
        assert_eq!(metrics["reconcile_overlap_count"], json!(0));
        assert!(metrics["events_per_second"].is_number());
        assert!(metrics["durable_events_per_second"].is_number());

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
        let journal = open_controller_test_journal(&dir, "journal.db").unwrap();
        let args = controller_test_args(dir.clone());
        let mut slot = reserved_slot();
        let state = FleetState::default();
        let children = HashMap::new();
        assert_eq!(
            fenced_slot_recovery_generation(
                &args,
                &journal,
                Some(&slot),
                &state,
                &children,
                &mut None
            )
            .unwrap(),
            None,
        );

        slot.phase = SlotPhase2::Fenced;
        assert_eq!(
            fenced_slot_recovery_generation(
                &args,
                &journal,
                Some(&slot),
                &state,
                &children,
                &mut None
            )
            .unwrap(),
            Some(Generation(slot.generation.0 + 1)),
        );
        assert_eq!(
            fenced_slot_recovery_generation(&args, &journal, None, &state, &children, &mut None)
                .unwrap(),
            None,
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_live_waiter_is_an_acting_slot_and_blocks_fenced_recovery() {
        let dir = metrics_test_dir("live-waiter-in-memory");
        let journal = open_controller_test_journal(&dir, "journal.db").unwrap();
        let args = controller_test_args(dir.clone());
        let mut slot = reserved_slot();
        slot.phase = SlotPhase2::Ready;
        let state = FleetState::default();
        let waiter = Command::new("sleep").arg("5").spawn().unwrap();
        let mut jobs = HashMap::from([(String::from("wait-velnor-1"), waiter)]);

        assert!(slot_is_acting(
            &args,
            &journal,
            &state,
            &jobs,
            &slot.slot_id,
            slot.generation,
            &mut None,
        )
        .unwrap());
        slot.phase = SlotPhase2::Fenced;
        assert_eq!(
            fenced_slot_recovery_generation(&args, &journal, Some(&slot), &state, &jobs, &mut None)
                .unwrap(),
            None,
            "a live waiter must not be skipped: it is why generation recovery deadlocks"
        );

        let _ = jobs.get_mut("wait-velnor-1").unwrap().kill();
        let _ = jobs.get_mut("wait-velnor-1").unwrap().wait();
        jobs.remove("wait-velnor-1");
        assert!(!slot_is_acting(
            &args,
            &journal,
            &state,
            &jobs,
            &slot.slot_id,
            slot.generation,
            &mut None,
        )
        .unwrap());
        assert_eq!(
            fenced_slot_recovery_generation(&args, &journal, Some(&slot), &state, &jobs, &mut None)
                .unwrap(),
            Some(Generation(slot.generation.0 + 1)),
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn persisted_live_unrelated_pid_is_not_adopted_after_controller_restart() {
        let dir = metrics_test_dir("persisted-waiter-restart");
        let journal = open_controller_test_journal(&dir, "journal.db").unwrap();
        let args = controller_test_args(dir.clone());
        let mut slot = reserved_slot();
        let state = FleetState::default();
        let jobs = HashMap::<String, Child>::new();
        let mut waiter = Command::new("sleep").arg("30").spawn().unwrap();
        let waiter_pid = waiter.id();
        cleanup::write_owned_pid(&dir, "wait-velnor-1", slot.generation.0, waiter.id()).unwrap();

        assert!(
            !slot_is_acting(
                &args,
                &journal,
                &state,
                &jobs,
                &slot.slot_id,
                slot.generation,
                &mut None,
            )
            .unwrap(),
            "a live unrelated PID must not be adopted as the waiter"
        );
        assert!(prove::pid_is_alive(waiter_pid));
        assert!(cleanup::read_owned_pid(&dir, "wait-velnor-1", slot.generation.0).is_none());
        slot.phase = SlotPhase2::Fenced;
        assert_eq!(
            fenced_slot_recovery_generation(&args, &journal, Some(&slot), &state, &jobs, &mut None)
                .unwrap(),
            Some(Generation(slot.generation.0 + 1)),
            "a reused PID must not block new-generation recovery"
        );

        let _ = waiter.kill();
        let _ = waiter.wait();
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fencing_terminates_the_waiter_so_generation_can_advance() {
        let dir = metrics_test_dir("fence-waiter");
        let journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
            fenced_slot_recovery_generation(&args, &journal, Some(&slot), &state, &jobs, &mut None)
                .unwrap(),
            Some(Generation(slot.generation.0 + 1)),
        );
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

        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
            Event::JobAcquisitionIntended {
                slot_id: SlotId("velnor-1".to_owned()),
                job_id: JobId("job-1".to_owned()),
                generation: Generation::INITIAL,
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
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
            lifecycle: None,
        };
        reclaim_orphaned_jobs_with_backend_policy(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            |args| panic!("docker must not be invoked on this recovery path: {args:?}"),
            None,
            || Ok(velnor_model::ExecutionBackendKind::Docker),
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

        std::fs::remove_dir_all(dir).ok();
    }

    /// Journal with one Running job whose worker and waiter pids are both
    /// dead, plus an exec config whose two-slot layout maps `velnor-1` to
    /// `slots/slot-1`. With `backend`, the state dir also selects that
    /// execution backend; without it no `execution.toml` exists.
    fn stale_running_job_fixture(label: &str, backend: Option<&str>) -> (PathBuf, Journal) {
        stale_running_job_fixture_with_endpoints(label, backend, "https://run.example/run", None)
    }

    fn stale_running_job_fixture_with_endpoints(
        label: &str,
        backend: Option<&str>,
        acquire_run_service_url: &str,
        job_run_service_url: Option<&str>,
    ) -> (PathBuf, Journal) {
        let dir = std::env::temp_dir().join(format!(
            "velnor-orphan-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(backend) = backend {
            std::fs::write(
                dir.join("execution.toml"),
                format!("[execution]\nbackend = \"{backend}\"\n"),
            )
            .unwrap();
        }
        write_exec_config(&dir, &dummy_exec("https://github.com/tailrocks/fixture"), 2).unwrap();
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: SlotId("velnor-1".to_owned()),
                    job_id: JobId("job-1".to_owned()),
                    generation: Generation::INITIAL,
                    message_id: "msg-1".into(),
                    run_service_url: acquire_run_service_url.to_owned(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        if let Some(run_service_url) = job_run_service_url {
            assert!(
                !journal
                    .apply(Event::JobAcquisitionResolvedAtEndpoint {
                        provisional_job_id: JobId("job-1".to_owned()),
                        acquired_job_id: JobId("job-1".to_owned()),
                        plan_id: "plan-1".to_owned(),
                        generation: Generation::INITIAL,
                        run_service_url: run_service_url.to_owned(),
                    })
                    .unwrap()
                    .rejected
            );
        }
        for event in [
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
        cleanup::write_owned_pid(&dir, "job-1", Generation::INITIAL.0, stale_pid()).unwrap();
        cleanup::write_owned_pid(&dir, "wait-velnor-1", Generation::INITIAL.0, stale_pid())
            .unwrap();
        (dir, journal)
    }

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
        reclaim_orphaned_jobs_with_backend_policy(
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
            None,
            || Ok(velnor_model::ExecutionBackendKind::Docker),
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

    #[test]
    fn stale_recovery_rejects_redelivery_after_owner_proof_before_docker() {
        use velnor_control::permit_ledger::{
            AcquireAttemptOutcome, PermitLane, PermitLedger, PermitState,
        };

        let (dir, mut journal) =
            stale_running_job_fixture("redelivery-before-teardown", Some("docker"));
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let slot_id = SlotId("velnor-1".to_owned());
        let slot_dir = dir.join("slots").join("slot-1");
        std::fs::create_dir_all(&slot_dir).unwrap();
        let marker_path = slot_dir.join("in-flight-job.json");
        let ledger_path = dir.join("permit-ledger.db");
        let holder = "native/request-1";
        let mut ledger = PermitLedger::open(&ledger_path).unwrap();
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.begin_epoch().unwrap();
        ledger.reconcile_attempts(&[]).unwrap();
        let old_token = match ledger
            .acquire_attempt(
                holder,
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(stale_pid()),
            )
            .unwrap()
        {
            AcquireAttemptOutcome::Acquired { attempt_token } => attempt_token,
            outcome => panic!("expected acquired permit, got {outcome:?}"),
        };
        drop(ledger);
        let write_marker = |request_id: &str, token: &str| {
            std::fs::write(
                &marker_path,
                serde_json::to_vec(&json!({
                    "plan_id": "plan-1",
                    "job_id": "job-1",
                    "run_service_url": "https://run.example/job",
                    "billing_owner_id": null,
                    "runner_request_id": request_id,
                    "message_id": request_id,
                    "permit_holder": holder,
                    "permit_ledger": ledger_path.to_string_lossy(),
                    "permit_attempt_token": token
                }))
                .unwrap(),
            )
            .unwrap();
        };
        write_marker("request-old", &old_token);
        let old_snapshot = crate::runner::recorded_in_flight_job_record(&slot_dir)
            .unwrap()
            .unwrap();

        // This is the same dead-owner proof boundary used by fleet recovery.
        assert!(!persisted_worker_owns_slot(
            &args,
            &journal,
            "job-1",
            &slot_id,
            Generation::INITIAL,
            &mut None,
        )
        .unwrap());
        assert!(!persisted_worker_owns_slot(
            &args,
            &journal,
            "wait-velnor-1",
            &slot_id,
            Generation::INITIAL,
            &mut None,
        )
        .unwrap());

        let job_snapshot = journal.materialized_state().unwrap().jobs[0].clone();
        let marker_lock =
            crate::runner::lock_in_flight_marker_snapshot(&slot_dir, Some(&old_snapshot)).unwrap();
        let owner_death = RecordedOwnerDeathProof::capture(
            &args,
            &slot_dir,
            &slot_id,
            Generation::INITIAL,
            "job-1",
            Some(&job_snapshot),
            &old_snapshot,
            marker_lock,
        )
        .unwrap();

        // Simulate a same-slot journal redelivery after proof capture. The
        // proof must recheck attempt identity before claiming capacity or
        // entering Docker teardown.
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id: JobId("job-1".to_owned()),
                    slot_id: slot_id.clone(),
                    attempt: 2,
                    generation: Generation::INITIAL,
                    worker: "worker-2".to_owned(),
                    accepted_unix: 2_345,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobStarted {
                    job_id: JobId("job-1".to_owned()),
                    generation: Generation::INITIAL,
                })
                .unwrap()
                .rejected
        );
        let new_container = dir.join("same-job-label-new-container");
        std::fs::write(&new_container, b"new container").unwrap();
        let mut docker_calls = Vec::new();
        let mut teardown = |job_id: &str| -> anyhow::Result<()> {
            docker_calls.push(job_id.to_owned());
            Ok(())
        };
        let claim = crate::runner::claim_recorded_attempt_for_terminal_recovery(
            &old_snapshot,
            &owner_death,
        );
        let error = match claim {
            Ok(()) => {
                teardown("job-1").unwrap();
                panic!("stale recovery unexpectedly claimed replacement attempt");
            }
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("job attempt changed"),
            "{error:#}"
        );
        assert!(
            docker_calls.is_empty(),
            "stale recovery must not invoke Docker"
        );
        assert!(new_container.exists(), "replacement container must survive");
        assert_eq!(
            crate::runner::recorded_in_flight_job_record(&slot_dir).unwrap(),
            Some(old_snapshot.clone()),
            "recovery must retain the inspected marker"
        );
        let ledger = PermitLedger::open(&ledger_path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert!(ledger.is_current_attempt(holder, &old_token).unwrap());
        drop(ledger);
        let state = journal.materialized_state().unwrap();
        assert!(state.jobs.iter().any(|job| {
            job.job_id == JobId("job-1".to_owned())
                && job.attempt == 2
                && job.worker == "worker-2"
                && job.phase == JobPhase2::Running
        }));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn redelivery_during_locked_docker_teardown_cannot_adopt_or_publish() {
        use velnor_control::permit_ledger::{
            AcquireAttemptOutcome, PermitLane, PermitLedger, PermitState,
        };

        let (dir, journal) =
            stale_running_job_fixture("redelivery-during-teardown", Some("docker"));
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let job_snapshot = journal.materialized_state().unwrap().jobs[0].clone();
        let slot_dir = dir.join("slots").join("slot-1");
        std::fs::create_dir_all(&slot_dir).unwrap();
        let marker_path = slot_dir.join("in-flight-job.json");
        let ledger_path = dir.join("permit-ledger.db");
        let holder = "native/request-paused-teardown";
        let mut ledger = PermitLedger::open(&ledger_path).unwrap();
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.begin_epoch().unwrap();
        ledger.reconcile_attempts(&[]).unwrap();
        let old_token = match ledger
            .acquire_attempt(
                holder,
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(stale_pid()),
            )
            .unwrap()
        {
            AcquireAttemptOutcome::Acquired { attempt_token } => attempt_token,
            outcome => panic!("expected acquired permit, got {outcome:?}"),
        };
        drop(ledger);
        std::fs::write(
            &marker_path,
            serde_json::to_vec(&json!({
                "plan_id": "plan-1",
                "job_id": "job-1",
                "run_service_url": "https://run.example/job",
                "billing_owner_id": null,
                "runner_request_id": "request-paused-teardown",
                "message_id": "message-paused-teardown",
                "permit_holder": holder,
                "permit_ledger": ledger_path.to_string_lossy(),
                "permit_attempt_token": old_token
            }))
            .unwrap(),
        )
        .unwrap();
        let marker_snapshot = crate::runner::recorded_in_flight_job_record(&slot_dir)
            .unwrap()
            .unwrap();
        let marker_attempt_token = serde_json::to_value(&marker_snapshot).unwrap()
            ["permit_attempt_token"]
            .as_str()
            .unwrap()
            .to_owned();
        let old_container = dir.join("fake-old-job-container");
        let replacement_container = dir.join("fake-replacement-job-container");
        std::fs::write(&old_container, b"old container").unwrap();
        let (publisher_contended_tx, publisher_contended_rx) = std::sync::mpsc::channel();
        let (publisher_finished_tx, publisher_finished_rx) = std::sync::mpsc::channel();
        let mut publisher_thread = None;

        let mut teardown = |job_id: &str| {
            assert_eq!(job_id, "job-1");
            // This callback is the paused Docker effect. Recovery must already
            // own the marker lock and exact Cleaning claim before entering it.
            let ledger = PermitLedger::open(&ledger_path).unwrap();
            assert_eq!(ledger.occupied().unwrap(), 1);
            assert_eq!(
                ledger.holder_state(holder).unwrap(),
                Some(PermitState::Cleaning)
            );
            assert!(ledger
                .is_current_attempt(holder, &marker_attempt_token)
                .unwrap());
            drop(ledger);

            let redelivery = crate::permit_guard::NativePermitGuard::acquire(
                &ledger_path,
                holder.to_owned(),
                "test",
            )
            .unwrap();
            assert!(
                redelivery.is_none(),
                "Cleaning must not be adopted during teardown"
            );
            assert_eq!(
                crate::runner::recorded_in_flight_job_record(&slot_dir).unwrap(),
                Some(marker_snapshot.clone()),
                "replacement marker must not publish while teardown is paused"
            );

            // A publisher racing the paused Docker effect must wait on this
            // same interprocess lock. It checks the permit only after the
            // lock becomes available, when Cleaning still blocks adoption.
            let publisher_slot = slot_dir.clone();
            let publisher_marker = marker_snapshot.clone();
            let publisher_ledger = ledger_path.clone();
            let publisher_holder = holder.to_owned();
            let publisher_contended = publisher_contended_tx.clone();
            let publisher_finished = publisher_finished_tx.clone();
            publisher_thread = Some(std::thread::spawn(move || -> anyhow::Result<bool> {
                let lock_path = publisher_slot.join(".in-flight-job.lock");
                let lock_probe = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&lock_path)?;
                let probe = rustix::fs::flock(
                    &lock_probe,
                    rustix::fs::FlockOperation::NonBlockingLockExclusive,
                );
                let contended = probe == Err(rustix::io::Errno::WOULDBLOCK);
                if !contended {
                    if probe.is_ok() {
                        rustix::fs::flock(&lock_probe, rustix::fs::FlockOperation::Unlock)?;
                    } else {
                        probe?;
                    }
                }
                publisher_contended.send(contended).unwrap();
                let marker_lock = crate::runner::lock_in_flight_marker_snapshot(
                    &publisher_slot,
                    Some(&publisher_marker),
                )?;
                let redelivery = crate::permit_guard::NativePermitGuard::acquire(
                    &publisher_ledger,
                    publisher_holder,
                    "test",
                )?;
                let stayed_fenced = redelivery.is_none();
                drop(marker_lock);
                publisher_finished.send(stayed_fenced).unwrap();
                Ok(stayed_fenced)
            }));
            assert!(
                publisher_contended_rx.recv_timeout(Duration::from_secs(5))?,
                "a competing marker publisher must observe the teardown lock as busy"
            );
            assert!(old_container.exists());
            assert!(!replacement_container.exists());
            std::fs::remove_file(&old_container).unwrap();
            Ok(())
        };

        let owner_death = teardown_recorded_job_with_death_proof(
            &args,
            &slot_dir,
            &job_snapshot.slot_id,
            job_snapshot.generation,
            "job-1",
            Some(&job_snapshot),
            &marker_snapshot,
            &mut teardown,
        )
        .unwrap();
        drop(owner_death);

        assert!(publisher_finished_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap());
        assert!(publisher_thread
            .take()
            .expect("redelivery publisher thread was started")
            .join()
            .expect("redelivery publisher must not panic")
            .unwrap());

        let ledger = PermitLedger::open(&ledger_path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.holder_state(holder).unwrap(),
            Some(PermitState::Cleaning)
        );
        assert!(ledger
            .is_current_attempt(holder, &marker_attempt_token)
            .unwrap());
        assert_eq!(
            crate::runner::recorded_in_flight_job_record(&slot_dir).unwrap(),
            Some(marker_snapshot.clone()),
            "marker remains available for cleanup retry"
        );
        assert!(!old_container.exists());
        assert!(!replacement_container.exists());
        drop(ledger);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn stale_recovery_defers_when_execution_backend_selection_is_unresolved() {
        use velnor_control::permit_ledger::{
            AcquireAttemptOutcome, PermitLane, PermitLedger, PermitState,
        };

        let (dir, mut journal) = stale_running_job_fixture("invalid-backend", Some("unknown"));
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let holder = "native/request-invalid-backend";
        let ledger_path = dir.join("permit-ledger.db");
        let mut ledger = PermitLedger::open(&ledger_path).unwrap();
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.begin_epoch().unwrap();
        ledger.reconcile_attempts(&[]).unwrap();
        let attempt_token = match ledger
            .acquire_attempt(
                holder,
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(stale_pid()),
            )
            .unwrap()
        {
            AcquireAttemptOutcome::Acquired { attempt_token } => attempt_token,
            outcome => panic!("expected acquired permit, got {outcome:?}"),
        };
        drop(ledger);

        let slot_dir = dir.join("slots").join("slot-1");
        std::fs::create_dir_all(&slot_dir).unwrap();
        let marker_path = slot_dir.join("in-flight-job.json");
        let marker = serde_json::to_vec(&json!({
            "plan_id": "plan-1",
            "job_id": "job-1",
            "run_service_url": "https://run.example/job",
            "billing_owner_id": null,
            "runner_request_id": "request-invalid-backend",
            "message_id": "message-invalid-backend",
            "permit_holder": holder,
            "permit_ledger": ledger_path.to_string_lossy(),
            "permit_attempt_token": attempt_token
        }))
        .unwrap();
        std::fs::write(&marker_path, &marker).unwrap();
        let journal_before = journal.materialized_state().unwrap();
        let mut docker_calls = Vec::new();

        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            |docker_args| {
                docker_calls.push(docker_args.to_vec());
                Ok(String::new())
            },
        )
        .await
        .unwrap();

        // A syntactically valid execution table without a backend is equally
        // unresolved; exercise the production resolver rather than relying on
        // an injected policy for this missing-selection case.
        std::fs::write(dir.join("execution.toml"), "[execution]\n").unwrap();
        reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            |docker_args| {
                docker_calls.push(docker_args.to_vec());
                Ok(String::new())
            },
        )
        .await
        .unwrap();

        assert!(
            docker_calls.is_empty(),
            "unreadable backend must skip Docker"
        );
        assert_eq!(journal.materialized_state().unwrap(), journal_before);
        assert_eq!(std::fs::read(&marker_path).unwrap(), marker);
        let ledger = PermitLedger::open(&ledger_path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.holder_state(holder).unwrap(),
            Some(PermitState::Acquiring)
        );
        assert!(ledger.is_current_attempt(holder, &attempt_token).unwrap());
        drop(ledger);

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn unresolved_injected_backend_defers_before_marker_or_journal_mutation() {
        let (dir, mut journal) =
            stale_running_job_fixture("injected-backend-error", Some("docker"));
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: false,
            lifecycle: None,
        };
        let slot_dir = dir.join("slots").join("slot-1");
        std::fs::create_dir_all(&slot_dir).unwrap();
        let marker_path = slot_dir.join("in-flight-job.json");
        let marker = serde_json::to_vec(&json!({
            "plan_id": "plan-1",
            "job_id": "job-1",
            "run_service_url": "https://run.example/job",
            "billing_owner_id": null,
            "runner_request_id": "request-1",
            "message_id": "message-1",
            "permit_holder": "",
            "permit_ledger": "",
            "permit_attempt_token": ""
        }))
        .unwrap();
        std::fs::write(&marker_path, &marker).unwrap();
        let before = journal.materialized_state().unwrap();
        let mut resolver_calls = 0;

        reclaim_orphaned_jobs_with_backend_policy(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
            |_| panic!("unresolved backend must defer before Docker"),
            None,
            || {
                resolver_calls += 1;
                Err(anyhow::anyhow!("injected unresolved backend"))
            },
        )
        .await
        .unwrap();

        assert_eq!(resolver_calls, 1);
        assert_eq!(journal.materialized_state().unwrap(), before);
        assert_eq!(std::fs::read(&marker_path).unwrap(), marker);
        std::fs::remove_dir_all(dir).ok();
    }

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
        reclaim_orphaned_jobs_with_backend_policy(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            false,
            |docker_args| {
                panic!("docker must not be invoked on the microvm backend: {docker_args:?}")
            },
            None,
            || Ok(velnor_model::ExecutionBackendKind::MicroVm),
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
    #[tokio::test]
    async fn stale_recovery_marker_success_tears_down_containers_before_ready() {
        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        let broker_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/jobs/1/completejob"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/broker/jobs/123/completejob"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&broker_server)
            .await;

        let run_service_url = format!("{}/jobs/1", server.uri());
        let broker_run_service_url = format!("{}/broker/jobs/123", broker_server.uri());
        assert_ne!(run_service_url, broker_run_service_url);
        let (dir, journal) = stale_running_job_fixture_with_endpoints(
            "marker-success",
            Some("docker"),
            &broker_run_service_url,
            Some(&run_service_url),
        );
        assert_eq!(
            journal.materialized_state().unwrap().jobs[0].run_service_url,
            run_service_url,
            "the selected endpoint replaces the broker URL in the job row"
        );
        drop(journal);
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
        assert_eq!(
            journal.materialized_state().unwrap().jobs[0].run_service_url,
            run_service_url,
            "the selected endpoint survives journal replay"
        );
        // Marker cleanup releases the durable storage reservation through
        // the process-wide sink, which the daemon installs at startup.
        crate::ops::init_at("test-instance".to_owned(), Some(&dir.join("state.db"))).unwrap();
        let slot_dir = dir.join("slots").join("slot-1");
        std::fs::create_dir_all(&slot_dir).unwrap();
        std::fs::write(
            slot_dir.join("in-flight-job.json"),
            serde_json::to_vec(&json!({
                "plan_id": "plan-1",
                "job_id": "job-1",
                "run_service_url": run_service_url,
                "billing_owner_id": null
            }))
            .unwrap(),
        )
        .unwrap();
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
        broker_server.verify().await;

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

    /// Docker teardown failure must stop orphan recovery before Run Service
    /// completion can release the marker's host-wide permit.
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn stale_recovery_teardown_failure_retains_marker_and_permit() {
        let transport_guard = crate::test_support::github_http_transport_env().await;
        transport_guard.set_native();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/jobs/1/completejob"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let run_service_url = format!("{}/jobs/1", server.uri());
        let (dir, mut journal) = stale_running_job_fixture("teardown-failure", Some("docker"));
        let ledger_path = dir.join("permit-ledger.db");
        let holder = crate::permit_guard::native_permit_holder("request-1");
        let mut ledger = velnor_control::permit_ledger::PermitLedger::open(&ledger_path).unwrap();
        ledger.set_max_jobs(1).unwrap();
        ledger.begin_epoch().unwrap();
        ledger.reconcile_attempts(&[]).unwrap();
        drop(ledger);
        let mut guard = crate::permit_guard::NativePermitGuard::acquire(
            &ledger_path,
            holder.clone(),
            "test-scope",
        )
        .unwrap()
        .unwrap();
        guard.retain_until_terminal();
        let attempt_token = guard.attempt_token().to_owned();
        drop(guard);

        let slot_dir = dir.join("slots").join("slot-1");
        std::fs::create_dir_all(&slot_dir).unwrap();
        std::fs::write(
            slot_dir.join("in-flight-job.json"),
            serde_json::to_vec(&json!({
                "plan_id": "plan-1",
                "job_id": "job-1",
                "run_service_url": run_service_url,
                "billing_owner_id": null,
                "permit_holder": holder,
                "permit_ledger": ledger_path.to_string_lossy(),
                "permit_attempt_token": attempt_token
            }))
            .unwrap(),
        )
        .unwrap();
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
        let listing = format!("job-cid\t{job_container}\t{job_container}\trunning\n");
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
                    Ok(listing.clone())
                } else {
                    Err(anyhow::anyhow!("injected Docker removal failure"))
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(calls.lock().unwrap().len(), 2);
        server.verify().await;
        assert!(slot_dir.join("in-flight-job.json").exists());
        let state = journal.load_state().unwrap();
        assert!(state.jobs.iter().any(|job| {
            job.job_id == JobId("job-1".to_owned()) && job.phase == JobPhase2::Running
        }));
        let ledger = velnor_control::permit_ledger::PermitLedger::open(&ledger_path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.holder_state(&holder).unwrap(),
            Some(PermitState::Uncertain)
        );
        drop(ledger);
        std::fs::remove_dir_all(dir).ok();
    }

    /// A replay failure with a spent durable budget abandons the row and
    /// restores the slot to Ready. The dead worker's containers must be torn
    /// down on that abandon path, not stranded behind Ready. Replay fails on
    /// the missing outbox payload before any network, so no mock is needed.
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
        std::fs::create_dir_all(&slot_dir).unwrap();
        std::fs::write(
            slot_dir.join("in-flight-job.json"),
            serde_json::to_vec(&json!({
                "plan_id": "plan-1",
                "job_id": "job-1",
                "run_service_url": "https://example.invalid/run-service",
                "billing_owner_id": null
            }))
            .unwrap(),
        )
        .unwrap();
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
        let mut journal = open_controller_test_journal(dir, "journal.db").unwrap();
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
        std::fs::write(
            dir.join("execution.toml"),
            "[execution]\nbackend = \"microvm\"\n",
        )
        .unwrap();
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
        spawn_ready_waiters(&args, &journal, &mut jobs, &mut None).unwrap();
        assert!(
            jobs.is_empty(),
            "an in-flight lease is physical occupancy; Ready is not enough to spawn"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn ready_waiters_are_not_spawned_while_a_persisted_waiter_pid_lives() {
        let dir = metrics_test_dir("ready-waiter-live-pid");
        let journal = ready_slot_journal(&dir);
        write_exec_config(&dir, &dummy_exec("https://github.com/tailrocks/fixture"), 1).unwrap();
        let mut waiter = Command::new("sleep").arg("30").spawn().unwrap();
        cleanup::write_owned_pid(&dir, "wait-velnor-1", Generation::INITIAL.0, waiter.id())
            .unwrap();
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".to_owned(),
            desired_ready: 1,
            once: true,
            spawn_slots: true,
            lifecycle: None,
        };
        let mut jobs = HashMap::new();
        spawn_ready_waiters(&args, &journal, &mut jobs, &mut None).unwrap();
        assert!(
            jobs.is_empty(),
            "a live waiter pid after controller restart must keep the slot unspawnable"
        );
        let _ = waiter.kill();
        let _ = waiter.wait();
        std::fs::remove_dir_all(dir).ok();
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
        // The waiter marker is dead and process discovery finds no job
        // worker, so recovery advances past ownership and stops only at the
        // missing runner credentials.
        assert!(
            error.to_string().contains(
                "runner credentials missing while recovering marker-only in-flight job job-9"
            ),
            "{error:#}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn marker_only_without_pid_marker_uses_process_discovery() {
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
            error.to_string().contains(
                "runner credentials missing while recovering marker-only in-flight job job-9"
            ),
            "{error:#}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn remote_acked_marker_only_teardown_failure_retains_permit_and_marker() {
        let (dir, mut journal) =
            stale_running_job_fixture("marker-only-teardown-failure", Some("docker"));
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
        assert!(
            !journal
                .apply(Event::RemoteAcked {
                    job_id: job_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
        assert!(journal.load_state().unwrap().jobs.is_empty());

        let ledger_path = dir.join("permit-ledger.db");
        let holder = crate::permit_guard::native_permit_holder("request-marker-only");
        let mut ledger = velnor_control::permit_ledger::PermitLedger::open(&ledger_path).unwrap();
        ledger.set_max_jobs(1).unwrap();
        ledger.begin_epoch().unwrap();
        ledger.reconcile_attempts(&[]).unwrap();
        drop(ledger);
        let mut guard = crate::permit_guard::NativePermitGuard::acquire(
            &ledger_path,
            holder.clone(),
            "test-scope",
        )
        .unwrap()
        .unwrap();
        guard.retain_until_terminal();
        let attempt_token = guard.attempt_token().to_owned();
        drop(guard);

        let slot_dir = dir.join("slots").join("slot-1");
        std::fs::create_dir_all(&slot_dir).unwrap();
        std::fs::write(
            slot_dir.join("in-flight-job.json"),
            serde_json::to_vec(&json!({
                "plan_id": "plan-1",
                "job_id": "job-1",
                "run_service_url": "https://example.invalid/jobs/1",
                "billing_owner_id": null,
                "permit_holder": holder,
                "permit_ledger": ledger_path.to_string_lossy(),
                "permit_attempt_token": attempt_token
            }))
            .unwrap(),
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
        let listing = format!("job-cid\t{job_container}\t{job_container}\trunning\n");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let error = reclaim_orphaned_jobs(
            &args,
            &mut journal,
            tokio::time::Instant::now() + Duration::from_secs(15),
            true,
            move |docker_args: &[String]| {
                recorded.lock().unwrap().push(docker_args.to_vec());
                if docker_args.first().is_some_and(|command| command == "ps") {
                    Ok(listing.clone())
                } else {
                    Err(anyhow::anyhow!(
                        "injected marker-only Docker removal failure"
                    ))
                }
            },
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("remove owned containers"));
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert!(slot_dir.join("in-flight-job.json").exists());
        let ledger = velnor_control::permit_ledger::PermitLedger::open(&ledger_path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.holder_state(&holder).unwrap(),
            Some(PermitState::Uncertain)
        );
        drop(ledger);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn completing_job_without_payload_does_not_abort_orphan_reclaim() {
        let dir = metrics_test_dir("completing-no-payload-reclaim");
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        for (slot_id, job_id, message_id) in [
            (
                completing_slot.clone(),
                completing_job.clone(),
                "msg-completing",
            ),
            (running_slot.clone(), running_job.clone(), "msg-running"),
        ] {
            for event in [
                Event::JobAcquisitionIntended {
                    slot_id: slot_id.clone(),
                    job_id: job_id.clone(),
                    generation,
                    message_id: message_id.into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                },
                Event::JobOwned {
                    job_id: job_id.clone(),
                    slot_id: slot_id.clone(),
                    attempt: 1,
                    generation,
                    worker: format!("worker-{}", job_id.0),
                    accepted_unix: 1,
                },
                Event::JobStarted {
                    job_id: job_id.clone(),
                    generation,
                },
            ] {
                assert!(!journal.apply(event).unwrap().rejected);
            }
        }
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
        mutate_journal_fixture_with_write_gate(&dir.join("journal.db"), "DELETE FROM outbox;");
        write_exec_config(&dir, &dummy_exec("https://github.com/tailrocks/fixture"), 2).unwrap();
        let slot_dir = dir.join("slots").join("slot-1");
        std::fs::create_dir_all(&slot_dir).unwrap();
        std::fs::write(
            slot_dir.join("in-flight-job.json"),
            serde_json::to_vec(&json!({
                "plan_id": "plan-1",
                "job_id": completing_job.0,
                "run_service_url": "https://example.invalid/run-service",
                "billing_owner_id": null
            }))
            .unwrap(),
        )
        .unwrap();
        let stale = stale_pid();
        for isolation in [
            completing_job.0.as_str(),
            running_job.0.as_str(),
            "wait-velnor-1",
            "wait-velnor-2",
        ] {
            cleanup::write_owned_pid(&dir, isolation, generation.0, stale).unwrap();
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

    #[tokio::test]
    async fn completing_job_with_pending_outbox_does_not_reconstruct_after_replay_failure() {
        let dir = metrics_test_dir("completing-pending-outbox-no-reconstruct");
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        for (slot_id, job_id, message_id) in [
            (
                completing_slot.clone(),
                completing_job.clone(),
                "msg-completing",
            ),
            (running_slot.clone(), running_job.clone(), "msg-running"),
        ] {
            for event in [
                Event::JobAcquisitionIntended {
                    slot_id: slot_id.clone(),
                    job_id: job_id.clone(),
                    generation,
                    message_id: message_id.into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                },
                Event::JobOwned {
                    job_id: job_id.clone(),
                    slot_id: slot_id.clone(),
                    attempt: 1,
                    generation,
                    worker: format!("worker-{}", job_id.0),
                    accepted_unix: 1,
                },
                Event::JobStarted {
                    job_id: job_id.clone(),
                    generation,
                },
            ] {
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
        std::fs::write(
            slot_dir.join("in-flight-job.json"),
            serde_json::to_vec(&json!({
                "plan_id": "plan-1",
                "job_id": completing_job.0,
                "run_service_url": "https://example.invalid/run-service",
                "billing_owner_id": null
            }))
            .unwrap(),
        )
        .unwrap();
        let stale = stale_pid();
        for isolation in [
            completing_job.0.as_str(),
            running_job.0.as_str(),
            "wait-velnor-1",
            "wait-velnor-2",
        ] {
            cleanup::write_owned_pid(&dir, isolation, generation.0, stale).unwrap();
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
            slot_dir.join("in-flight-job.json").exists(),
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

        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
            Event::JobAcquisitionIntended {
                slot_id: SlotId("velnor-1".to_owned()),
                job_id: JobId("job-1".to_owned()),
                generation: Generation::INITIAL,
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
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

        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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
        let mut journal = open_controller_test_journal(&dir, "journal.db").unwrap();
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

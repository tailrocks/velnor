//! Per-scope controller: desired state, permits, slot/job child processes.
//!
//! Restarting this process must not stop existing slot or job workers: children
//! are spawned without kill-on-drop, and packaged units must not use
//! `PartOf=controller`. Every journal side effect is executed here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use clap::Args;
use velnor_control::journal::{Event, Journal, SideEffect};
use velnor_model::{ActorPhase, Generation, JobId, SlotId};

use super::assign;
use super::cleanup;
use super::exec::load_exec_config;
use super::health::HealthServer;
use super::prove;
use super::slot::slot_id;
use super::watchdog::{feed_after_cycle, LocalCycle};

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
    let mut ready_announced = false;
    loop {
        let cycle = reconcile_once(&args, &mut journal, &server, &mut slots, &mut jobs).await?;
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
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn reconcile_once(
    args: &ControllerArgs,
    journal: &mut Journal,
    server: &HealthServer,
    slots: &mut HashMap<String, Child>,
    jobs: &mut HashMap<String, Child>,
) -> anyhow::Result<LocalCycle> {
    let total = args.desired_ready.saturating_add(args.surge).max(1);
    let generation = Generation::INITIAL;
    let mut effects = Vec::new();
    for index in 1..=total {
        let id = slot_id(&args.scope, index as usize);
        let surge = index > args.desired_ready;
        effects.extend(
            journal
                .apply(Event::PermitReserved {
                    slot_id: id,
                    generation,
                    surge,
                })?
                .commands,
        );
    }
    for command in effects {
        execute_effect(args, journal, slots, jobs, command).await?;
    }

    observe_github_and_routing(args, journal).await?;

    let mut proof_effects = Vec::new();
    let executor = prove::observe_executor(&args.state_dir);
    let snapshot = journal.load_state()?;
    for index in 1..=total {
        let id = slot_id(&args.scope, index as usize);
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
        let journal_pid = snapshot
            .slots
            .iter()
            .find(|slot| slot.slot_id == id)
            .and_then(|slot| slot.pid);
        if prove::observe_session(slots.get_mut(&id.0), journal_pid) {
            proof_effects.extend(
                journal
                    .apply(Event::SessionLive {
                        slot_id: id.clone(),
                        generation,
                    })?
                    .commands,
            );
        }
        let state = journal.load_state()?;
        if let Some(slot) = state.slots.iter().find(|slot| slot.slot_id == id) {
            if slot.ready_proof().is_ok() && !slot.registered {
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
    }
    for command in proof_effects {
        execute_effect(args, journal, slots, jobs, command).await?;
    }

    bind_live_queued_jobs(args, journal).await?;
    let job_effects = ingest_assignments(args, journal)?;
    for command in job_effects {
        execute_effect(args, journal, slots, jobs, command).await?;
    }

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
    reap(jobs);
    let health = journal.load_state()?.health();
    server.publish(&health)?;
    Ok(LocalCycle::finished())
}

async fn execute_effect(
    args: &ControllerArgs,
    journal: &mut Journal,
    slots: &mut HashMap<String, Child>,
    jobs: &mut HashMap<String, Child>,
    command: SideEffect,
) -> anyhow::Result<()> {
    match command {
        SideEffect::SpawnSlot { slot_id, .. } => maybe_spawn_slot(args, journal, slots, &slot_id),
        SideEffect::RegisterRunner {
            slot_id,
            generation,
        } => register_runner(args, journal, slot_id, generation).await,
        SideEffect::StartJob { job_id, generation } => {
            maybe_spawn_job(args, journal, jobs, &job_id.0, generation.0)
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
        SideEffect::DeleteOutbox { .. } | SideEffect::FenceSlot { .. } => Ok(()),
    }
}

async fn register_runner(
    args: &ControllerArgs,
    journal: &mut Journal,
    slot_id: SlotId,
    generation: Generation,
) -> anyhow::Result<()> {
    super::scheduler::production_scheduler().activate_production()?;
    let Ok(exec) = load_exec_config(&args.state_dir) else {
        return Ok(());
    };
    let index = slot_index_from_id(&slot_id);
    if let Err(error) = crate::runner::jit_configure_one_slot(
        &exec,
        exec.config_dir.as_deref().unwrap_or(&args.state_dir),
        index,
        exec.slots.max(1),
    )
    .await
    {
        eprintln!(
            "Warning: JIT register {} failed (slot stays unregistered): {error:#}",
            slot_id.0
        );
        return Ok(());
    }
    let registered = journal.apply(Event::Registered {
        slot_id: slot_id.clone(),
        generation,
    })?;
    if registered.rejected {
        return Ok(());
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
    Ok(())
}

async fn observe_github_and_routing(
    args: &ControllerArgs,
    journal: &mut Journal,
) -> anyhow::Result<()> {
    let mut reachable = false;
    if let Ok(exec) = load_exec_config(&args.state_dir) {
        let group = exec
            .pool_name
            .clone()
            .or_else(|| exec.name.clone())
            .unwrap_or_else(|| "default".to_owned());
        let trust = prove::runtime_trust_scope(&exec.trust_scope);
        if let Some(url) = exec.url.as_deref() {
            if let Some(policy) =
                prove::policy_from_github_url(url, group, exec.labels.clone(), trust.clone())
            {
                prove::write_policy_if_absent(&args.state_dir, &policy)?;
            }
        }
        let policy = prove::read_policy(&args.state_dir);
        if let (Some(url), Some(token), Some(policy)) =
            (exec.url.as_deref(), exec.pat.as_deref(), policy.as_ref())
        {
            let probe = prove::probe_github(prove::GitHubProbeRequest {
                url,
                token,
                policy,
                pool_id: exec.pool_id,
                configured_labels: &exec.labels,
                configured_trust: &exec.trust_scope,
            })
            .await;
            reachable = probe.reachable;
            if let Some(evidence) = probe.evidence {
                prove::write_evidence(&args.state_dir, &evidence)?;
            }
        }
    }
    journal.apply(Event::Dependency {
        github_reachable: reachable,
    })?;
    let _ = prove::reconcile_from_dir(&args.state_dir)?;
    let routing = prove::observe_routing(&args.state_dir);
    journal.apply(Event::Routing {
        valid: routing.valid,
        group_valid: routing.group_valid,
    })?;
    Ok(())
}

async fn bind_live_queued_jobs(args: &ControllerArgs, journal: &mut Journal) -> anyhow::Result<()> {
    let state = journal.load_state()?;
    if !state.github_reachable {
        return Ok(());
    }
    let Ok(exec) = load_exec_config(&args.state_dir) else {
        return Ok(());
    };
    let (Some(url), Some(token), Some(policy)) = (
        exec.url.as_deref(),
        exec.pat.as_deref(),
        prove::read_policy(&args.state_dir),
    ) else {
        return Ok(());
    };
    let queued = prove::queued_job_ids(url, token, &policy).await;
    assign::bind_queued(&args.state_dir, journal, &queued)?;
    Ok(())
}

fn ingest_assignments(
    args: &ControllerArgs,
    journal: &mut Journal,
) -> anyhow::Result<Vec<velnor_control::journal::SideEffect>> {
    let mut effects = Vec::new();
    for assignment in assign::read_dir(&args.state_dir)? {
        let state = journal.load_state()?;
        if state
            .jobs
            .iter()
            .any(|job| job.job_id.0 == assignment.job_id)
        {
            continue;
        }
        let Some(slot) = state
            .slots
            .iter()
            .find(|slot| slot.slot_id.0 == assignment.slot_id && slot.phase == ActorPhase::Ready)
        else {
            continue;
        };
        let job_id = JobId(assignment.job_id.clone());
        let assigned = journal.apply(Event::Assigned {
            slot_id: slot.slot_id.clone(),
            job_id: job_id.clone(),
            generation: slot.generation,
        })?;
        if assigned.rejected {
            continue;
        }
        let owned = journal.apply(Event::JobOwned {
            job_id: job_id.clone(),
            slot_id: slot.slot_id.clone(),
            attempt: 1,
            generation: slot.generation,
            worker: format!("velnor-job@{}", job_id.0),
        })?;
        if owned.rejected {
            continue;
        }
        cleanup::claim_owned(&args.state_dir, &job_id.0, slot.generation.0)?;
        effects.extend(owned.commands);
    }
    Ok(effects)
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

fn maybe_spawn_slot(
    args: &ControllerArgs,
    journal: &Journal,
    children: &mut HashMap<String, Child>,
    slot_id: &SlotId,
) -> anyhow::Result<()> {
    if !args.spawn_slots {
        return Ok(());
    }
    if children.contains_key(&slot_id.0) {
        return Ok(());
    }
    if let Ok(state) = journal.load_state() {
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
        .spawn()?;
    children.insert(slot_id.0.clone(), child);
    Ok(())
}

fn maybe_spawn_job(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<()> {
    if jobs.contains_key(job_id) {
        return Ok(());
    }
    if cleanup::read_owned_pid(&args.state_dir, job_id, generation).is_some_and(prove::pid_is_alive)
    {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let slot_index = journal
        .load_state()
        .ok()
        .and_then(|state| {
            state
                .jobs
                .into_iter()
                .find(|job| job.job_id.0 == job_id)
                .map(|job| slot_index_from_id(&job.slot_id))
        })
        .unwrap_or(1);
    let child = Command::new(exe)
        .arg("job")
        .arg("--state-dir")
        .arg(&args.state_dir)
        .arg("--job-id")
        .arg(job_id)
        .arg("--generation")
        .arg(generation.to_string())
        .arg("--slot-index")
        .arg(slot_index.to_string())
        .arg("--scope")
        .arg(&args.scope)
        .spawn()?;
    cleanup::write_owned_pid(&args.state_dir, job_id, generation, child.id())?;
    jobs.insert(job_id.to_owned(), child);
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

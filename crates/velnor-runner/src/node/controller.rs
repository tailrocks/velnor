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

use super::cleanup;
use super::exec::{load_exec_config, EXEC_FILE};
use super::health::HealthServer;
use super::prove;
use super::slot::slot_id;
use super::watchdog::{feed_after_cycle, LocalCycle};

/// Bound live JIT requests during startup/recovery without making the GitHub
/// API a burst target. This matches the bounded configure path.
const JIT_REGISTRATION_CONCURRENCY: usize = 4;

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
        if crate::runner::draining() {
            drain_children(&mut slots, &mut jobs).await?;
            return Ok(());
        }
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

/// Stop idle controller-owned slot processes when the daemon receives SIGTERM.
/// Job workers are deliberately left alone so systemd's stop timeout remains
/// the outer bound for an in-flight job rather than turning an upgrade into a
/// lost job. The daemon's drain flag lives in the supervisor process, so this
/// explicit handoff is the process boundary that makes graceful drain real.
async fn drain_children(
    slots: &mut HashMap<String, Child>,
    jobs: &mut HashMap<String, Child>,
) -> anyhow::Result<()> {
    for child in slots.values() {
        request_child_shutdown(child)?;
    }

    loop {
        reap(slots);
        reap(jobs);
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

    let _ = prove::reconcile_from_dir(&args.state_dir)?;
    let routing = prove::observe_routing(&args.state_dir);
    journal.apply(Event::Routing {
        valid: routing.valid,
        group_valid: routing.group_valid,
    })?;

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
    let mut registrations = Vec::new();
    for command in proof_effects {
        match command {
            SideEffect::RegisterRunner {
                slot_id,
                generation,
            } => registrations.push((slot_id, generation)),
            command => execute_effect(args, journal, slots, jobs, command).await?,
        }
    }
    register_runners(args, journal, registrations).await?;

    let mut job_effects = Vec::new();
    if args.state_dir.join(EXEC_FILE).is_file() {
        let state = journal.load_state()?;
        for slot in &state.slots {
            if slot.phase != ActorPhase::Ready {
                continue;
            }
            if state.jobs.iter().any(|job| job.slot_id == slot.slot_id) {
                continue;
            }
            let index = slot_index_from_id(&slot.slot_id);
            let job_id = worker_job_id(index);
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
            job_effects.extend(owned.commands);
        }
    }
    for command in job_effects {
        execute_effect(args, journal, slots, jobs, command).await?;
    }

    for row in journal.pending_outbox()? {
        cleanup::write_outbox(
            &args.state_dir,
            &row.job_id.0,
            row.generation.0,
            row.payload_sha256.as_bytes(),
        )?;
        if !row.send_started {
            journal.apply(Event::CompletionSendStarted {
                job_id: row.job_id,
                generation: row.generation,
            })?;
        }
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
            maybe_spawn_job(args, jobs, &job_id.0, generation.0)
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
        } => {
            cleanup::write_outbox(
                &args.state_dir,
                &job_id.0,
                generation.0,
                payload_sha256.as_bytes(),
            )?;
            journal.apply(Event::CompletionSendStarted { job_id, generation })?;
            Ok(())
        }
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
    register_runners(args, journal, vec![(slot_id, generation)]).await
}

/// Configure independent, already-proven slots concurrently, then commit the
/// resulting journal events in slot order. Network work never mutates the
/// journal; routing, executor, session, and permit proofs remain prerequisites
/// for every request.
async fn register_runners(
    args: &ControllerArgs,
    journal: &mut Journal,
    registrations: Vec<(SlotId, Generation)>,
) -> anyhow::Result<()> {
    if registrations.is_empty() {
        return Ok(());
    }
    super::scheduler::production_scheduler().activate_production()?;
    let Ok(exec) = load_exec_config(&args.state_dir) else {
        return Ok(());
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
                let result =
                    crate::runner::jit_configure_one_slot(&exec, &config_base, index, slot_count)
                        .await;
                (slot_id, generation, result)
            }
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
    outcomes.sort_by_key(|(slot_id, _, _)| slot_id.0.clone());

    for (slot_id, generation, result) in outcomes {
        if let Err(error) = result {
            eprintln!(
                "Warning: JIT register {} failed (slot stays unregistered): {error:#}",
                slot_id.0
            );
            continue;
        }
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

fn slot_index_from_id(slot_id: &SlotId) -> usize {
    slot_id
        .0
        .rsplit('-')
        .next()
        .and_then(|part| part.parse::<usize>().ok())
        .unwrap_or(1)
}

fn worker_job_id(slot_index: usize) -> JobId {
    JobId(format!("slot-{slot_index}-worker"))
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
    let slot_index = job_id
        .strip_prefix("slot-")
        .and_then(|rest| rest.strip_suffix("-worker"))
        .and_then(|index| index.parse::<usize>().ok())
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

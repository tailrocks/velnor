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
use velnor_model::{Generation, SlotId};

use super::exec::load_exec_config;
use super::health::HealthServer;
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
    #[arg(long, default_value_t = true)]
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
    journal.apply(Event::Routing {
        valid: true,
        group_valid: true,
    })?;
    // Transitional executor is host Docker; routing is proven by a later
    // reconciler. Isolation tests seed routing/executor events themselves.
    // Default routing is unknown until reconciled; do not advertise ready.
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
                    slot_id: id.clone(),
                    generation,
                    surge,
                })?
                .commands,
        );
        effects.extend(
            journal
                .apply(Event::ExecutorProven {
                    slot_id: id.clone(),
                    generation,
                })?
                .commands,
        );
        effects.extend(
            journal
                .apply(Event::SessionLive {
                    slot_id: id.clone(),
                    generation,
                })?
                .commands,
        );
        effects.extend(
            journal
                .apply(Event::RegistrationIntended {
                    slot_id: id.clone(),
                    generation,
                })?
                .commands,
        );
    }
    for command in effects {
        execute_effect(args, journal, slots, jobs, command).await?;
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
        SideEffect::SpawnSlot { slot_id, .. } => maybe_spawn_slot(args, slots, &slot_id),
        SideEffect::RegisterRunner {
            slot_id,
            generation,
        } => {
            if let Ok(exec) = load_exec_config(&args.state_dir) {
                let index = slot_index_from_id(&slot_id);
                crate::runner::jit_configure_one_slot(
                    &exec,
                    exec.config_dir.as_deref().unwrap_or(&args.state_dir),
                    index,
                    exec.slots.max(1),
                )
                .await?;
            }
            let _ = journal.apply(Event::Registered {
                slot_id: slot_id.clone(),
                generation,
            })?;
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
            // Outbox already durable. Record send-started so crash during
            // transport still reconciles.
            let _ = journal.apply(Event::CompletionSendStarted { job_id, generation })?;
            let _ = payload_sha256;
            Ok(())
        }
        SideEffect::Cleanup {
            isolation_id,
            generation,
        } => {
            let _ = (isolation_id, generation);
            Ok(())
        }
        SideEffect::DeleteOutbox { .. } | SideEffect::FenceSlot { .. } => Ok(()),
    }
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
    children: &mut HashMap<String, Child>,
    slot_id: &SlotId,
) -> anyhow::Result<()> {
    if !args.spawn_slots {
        return Ok(());
    }
    if children.contains_key(&slot_id.0) {
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
    let exe = std::env::current_exe()?;
    let child = Command::new(exe)
        .arg("job")
        .arg("--state-dir")
        .arg(&args.state_dir)
        .arg("--job-id")
        .arg(job_id)
        .arg("--generation")
        .arg(generation.to_string())
        .spawn()?;
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

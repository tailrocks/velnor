//! Per-scope controller: desired state, permits, slot child processes.
//!
//! Restarting this process must not stop existing slot or job workers: children
//! are spawned without kill-on-drop, and packaged units must not use
//! `PartOf=controller`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use clap::Args;
use velnor_control::journal::{Event, Journal, SideEffect};
use velnor_model::{Generation, SlotId};

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
    // Transitional executor is host Docker; routing is proven by a later
    // reconciler. Isolation tests seed routing/executor events themselves.
    // Default routing is unknown until reconciled; do not advertise ready.
    let mut children: HashMap<String, Child> = HashMap::new();
    let mut ready_announced = false;
    loop {
        let cycle = reconcile_once(&args, &mut journal, &server, &mut children)?;
        let _ = feed_after_cycle(cycle, !ready_announced);
        ready_announced = true;
        if args.once {
            // Leave children running: a controller restart (or --once exit)
            // must not stop slot processes.
            for (_id, child) in children.drain() {
                std::mem::forget(child);
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn reconcile_once(
    args: &ControllerArgs,
    journal: &mut Journal,
    server: &HealthServer,
    children: &mut HashMap<String, Child>,
) -> anyhow::Result<LocalCycle> {
    let total = args.desired_ready.saturating_add(args.surge).max(1);
    let generation = Generation::INITIAL;
    for index in 1..=total {
        let id = slot_id(&args.scope, index as usize);
        let surge = index > args.desired_ready;
        let outcome = journal.apply(Event::PermitReserved {
            slot_id: id.clone(),
            generation,
            surge,
        })?;
        for command in outcome.commands {
            if let SideEffect::SpawnSlot { slot_id, .. } = command {
                maybe_spawn(args, children, &slot_id)?;
            }
        }
    }
    reap(children);
    let health = journal.load_state()?.health();
    server.publish(&health)?;
    Ok(LocalCycle::finished())
}

fn maybe_spawn(
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
    let index = slot_id
        .0
        .rsplit('-')
        .next()
        .and_then(|part| part.parse::<usize>().ok())
        .unwrap_or(1);
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
    once: bool,
) -> anyhow::Result<()> {
    run(ControllerArgs {
        state_dir,
        scope,
        desired_ready,
        surge: 1,
        once,
        spawn_slots: true,
    })
    .await
}

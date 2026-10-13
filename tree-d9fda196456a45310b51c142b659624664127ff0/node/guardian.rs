//! Smallest Velnor process: no GitHub token, no Docker socket, no jobs.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Args;
use velnor_control::journal::{Event, Journal};
use velnor_model::{ActorPhase, Generation, SlotId};

use super::health::HealthServer;
use super::watchdog::{feed_after_cycle, LocalCycle};

/// Stale heartbeat threshold used until measured under pressure.
#[derive(Debug, Clone, Args)]
pub struct GuardianArgs {
    /// Journal directory (journal.db + health.sock live here).
    #[arg(long)]
    pub state_dir: PathBuf,
    /// Complete one supervision cycle and exit.
    #[arg(long)]
    pub once: bool,
    /// Heartbeat age that fences a still-alive but silent slot.
    #[arg(long, default_value_t = 10)]
    pub stale_seconds: u64,
}

/// Run the guardian. Never reads a GitHub credential or opens the Docker socket.
pub async fn run(args: GuardianArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.state_dir)?;
    let journal_path = args.state_dir.join("journal.db");
    let mut journal = Journal::open(&journal_path)?;
    journal.apply(Event::ControlLive)?;
    journal.apply(Event::JournalWritable)?;
    let server = HealthServer::bind(&args.state_dir)?;
    let mut ready_announced = false;
    loop {
        let cycle = supervise_once(
            &mut journal,
            &server,
            Duration::from_secs(args.stale_seconds),
        )?;
        let _ = feed_after_cycle(cycle, !ready_announced);
        ready_announced = true;
        if args.once {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn supervise_once(
    journal: &mut Journal,
    server: &HealthServer,
    stale: Duration,
) -> anyhow::Result<LocalCycle> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let state = journal.materialized_state()?;
    for slot in &state.slots {
        if matches!(slot.phase, ActorPhase::Fenced | ActorPhase::Quarantined) {
            continue;
        }
        let pid_dead = slot.pid.is_some_and(|pid| !pid_is_alive(pid));
        let heartbeat_stale =
            slot.heartbeat_unix > 0 && now.saturating_sub(slot.heartbeat_unix) > stale.as_secs();
        if pid_dead || heartbeat_stale {
            journal.apply(Event::SlotStale {
                slot_id: SlotId(slot.slot_id.0.clone()),
                generation: Generation(slot.generation.0),
            })?;
        }
    }
    let health = journal.materialized_state()?.health();
    server.publish(&health)?;
    Ok(LocalCycle::finished())
}

fn pid_is_alive(pid: u32) -> bool {
    // SIGNAL 0: existence check, no delivery. Guardian never signals jobs.
    // SAFETY: kill(pid, 0) only tests whether `pid` exists; it does not
    // change the target's state.
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0
}

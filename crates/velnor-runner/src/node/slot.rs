//! One OS process per ready slot. No authority over siblings.

use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use serde::{Deserialize, Serialize};
use velnor_model::{Generation, SlotId};

use super::watchdog::{feed_after_cycle, LocalCycle};

#[derive(Debug, Clone, Args)]
pub struct SlotArgs {
    #[arg(long)]
    pub state_dir: PathBuf,
    #[arg(long)]
    pub scope: String,
    #[arg(long)]
    pub slot_index: usize,
    /// Generation reserved by the controller before this process was spawned.
    #[arg(long)]
    pub generation: u64,
    /// One heartbeat cycle then exit (tests). Production loops until SIGTERM.
    #[arg(long)]
    pub once: bool,
}

#[must_use]
pub fn slot_id(scope: &str, slot_index: usize) -> SlotId {
    SlotId(format!("{scope}-{slot_index}"))
}

/// Atomic, controller-consumed liveness signal. Slot processes never open the
/// shared journal: one SQLite writer owns durable heartbeat events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotHeartbeat {
    pub generation: u64,
    pub pid: u32,
    pub sequence: u64,
}

#[must_use]
pub fn heartbeat_path(state_dir: &std::path::Path, slot_index: usize) -> PathBuf {
    state_dir.join(format!(".slot-{slot_index}.heartbeat"))
}

fn write_heartbeat(
    state_dir: &std::path::Path,
    slot_index: usize,
    heartbeat: &SlotHeartbeat,
) -> anyhow::Result<()> {
    let path = heartbeat_path(state_dir, slot_index);
    let temp = state_dir.join(format!(
        ".slot-{slot_index}.heartbeat.{}.tmp",
        heartbeat.pid
    ));
    let mut file = std::fs::File::create(&temp)?;
    serde_json::to_writer(&mut file, heartbeat)?;
    file.sync_all()?;
    std::fs::rename(temp, path)?;
    Ok(())
}

pub async fn run(args: SlotArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.state_dir)?;
    let id = slot_id(&args.scope, args.slot_index);
    let generation = Generation(args.generation);
    let pid = std::process::id();
    let mut sequence: u64 = 0;
    let mut ready_announced = false;
    eprintln!("slot {} starting heartbeat loop", id.0);
    loop {
        sequence = sequence.wrapping_add(1);
        if let Err(error) = write_heartbeat(
            &args.state_dir,
            args.slot_index,
            &SlotHeartbeat {
                generation: generation.0,
                pid,
                sequence,
            },
        ) {
            eprintln!("slot {} heartbeat publish error: {error}", id.0);
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        if !ready_announced {
            eprintln!("slot {} published first heartbeat", id.0);
        }
        let _ = feed_after_cycle(LocalCycle::finished(), !ready_announced);
        ready_announced = true;
        if args.once {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

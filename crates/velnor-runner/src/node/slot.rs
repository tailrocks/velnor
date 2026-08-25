//! One OS process per ready slot. No authority over siblings.

use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use velnor_control::journal::{Event, Journal};
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
    /// One heartbeat cycle then exit (tests). Production loops until SIGTERM.
    #[arg(long)]
    pub once: bool,
}

#[must_use]
pub fn slot_id(scope: &str, slot_index: usize) -> SlotId {
    SlotId(format!("{scope}-{slot_index}"))
}

pub async fn run(args: SlotArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.state_dir)?;
    let mut journal = Journal::open(args.state_dir.join("journal.db"))?;
    let id = slot_id(&args.scope, args.slot_index);
    let mut ready_announced = false;
    let heartbeat = async {
        loop {
            let state = journal.load_state()?;
            let generation = state
                .slots
                .iter()
                .find(|slot| slot.slot_id == id)
                .map(|slot| slot.generation)
                .unwrap_or(Generation::INITIAL);
            journal.apply(Event::SlotHeartbeat {
                slot_id: id.clone(),
                generation,
                pid: std::process::id(),
            })?;
            let _ = feed_after_cycle(LocalCycle::finished(), !ready_announced);
            ready_announced = true;
            if args.once {
                return anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    };
    if args.once {
        return heartbeat.await;
    }
    // Assigned work runs in a job process, never in this slot process.
    let mut job = spawn_slot_job(&args)?;
    let result = heartbeat.await;
    if let Some(child) = job.as_mut() {
        let _ = child.try_wait();
        std::mem::forget(job.take());
    }
    result
}

fn spawn_slot_job(args: &SlotArgs) -> anyhow::Result<Option<std::process::Child>> {
    if !args.state_dir.join(super::exec::EXEC_FILE).is_file() {
        return Ok(None);
    }
    let exe = std::env::current_exe()?;
    let child = std::process::Command::new(exe)
        .arg("job")
        .arg("--state-dir")
        .arg(&args.state_dir)
        .arg("--job-id")
        .arg(format!("slot-{}-worker", args.slot_index))
        .arg("--generation")
        .arg("1")
        .arg("--slot-index")
        .arg(args.slot_index.to_string())
        .arg("--scope")
        .arg(&args.scope)
        .spawn()?;
    Ok(Some(child))
}

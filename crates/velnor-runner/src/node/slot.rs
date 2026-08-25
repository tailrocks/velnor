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
    let exec = args.state_dir.join("daemon-args.json");
    if exec.is_file() {
        let daemon: crate::args::DaemonArgs = serde_json::from_slice(&std::fs::read(&exec)?)?;
        let config_base = daemon
            .config_dir
            .clone()
            .unwrap_or_else(|| args.state_dir.clone());
        let slots = daemon.slots.max(1);
        tokio::select! {
            result = heartbeat => result,
            result = crate::runner::run_daemon_slot(
                daemon,
                config_base,
                args.slot_index,
                slots,
            ) => result,
        }
    } else {
        heartbeat.await
    }
}

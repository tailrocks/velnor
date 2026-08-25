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
    loop {
        let state = journal.load_state().ok();
        let generation = state
            .as_ref()
            .and_then(|state| {
                state
                    .slots
                    .iter()
                    .find(|slot| slot.slot_id == id)
                    .map(|slot| slot.generation)
            })
            .unwrap_or(Generation::INITIAL);
        match journal.apply(Event::SlotHeartbeat {
            slot_id: id.clone(),
            generation,
            pid: std::process::id(),
        }) {
            Ok(outcome) if !outcome.rejected => {}
            Ok(_) => {
                eprintln!(
                    "slot {} heartbeat rejected at generation {}",
                    id.0, generation.0
                );
            }
            Err(error) => {
                eprintln!("slot {} heartbeat journal error: {error}", id.0);
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        }
        let _ = feed_after_cycle(LocalCycle::finished(), !ready_announced);
        ready_announced = true;
        if args.once {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

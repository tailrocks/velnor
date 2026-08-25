//! Transient per-job worker. Control loop stays async; it must not block a
//! heartbeat on a child wait. Host Docker remains the named transitional
//! executor, not the Build L3 availability boundary.

use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use velnor_control::journal::{Event, Journal};
use velnor_model::{Generation, JobId};

use super::watchdog::{feed_after_cycle, LocalCycle};

#[derive(Debug, Clone, Args)]
pub struct JobArgs {
    #[arg(long)]
    pub state_dir: PathBuf,
    #[arg(long)]
    pub job_id: String,
    #[arg(long, default_value_t = 1)]
    pub generation: u64,
    #[arg(long)]
    pub once: bool,
}

pub async fn run(args: JobArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.state_dir)?;
    let mut journal = Journal::open(args.state_dir.join("journal.db"))?;
    let job_id = JobId(args.job_id.clone());
    let generation = Generation(args.generation);
    journal.apply(Event::JobStarted {
        job_id: job_id.clone(),
        generation,
    })?;
    let mut ready_announced = false;
    loop {
        // Heartbeat is a journal apply, never a blocking child wait.
        let _ = journal.load_state()?;
        let _ = feed_after_cycle(LocalCycle::finished(), !ready_announced);
        ready_announced = true;
        if args.once {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

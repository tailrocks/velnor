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
    pub slot_index: Option<usize>,
    #[arg(long)]
    pub scope: Option<String>,
    /// One GitHub job then persist completion. Never skips the worker.
    #[arg(long)]
    pub once: bool,
}

pub async fn run(args: JobArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.state_dir)?;
    let mut journal = Journal::open(args.state_dir.join("journal.db"))?;
    let job_id = JobId(args.job_id.clone());
    let generation = Generation(args.generation);
    let started = journal.apply(Event::JobStarted {
        job_id: job_id.clone(),
        generation,
    })?;
    let owned = !started.rejected;
    if !owned && args.once && super::exec::load_exec_config(&args.state_dir).is_err() {
        anyhow::bail!(
            "job {} is not owned at generation {}",
            args.job_id,
            args.generation
        );
    }
    if let Ok(mut daemon) = super::exec::load_exec_config(&args.state_dir) {
        if args.once {
            daemon.once = true;
        }
        let config_base = daemon
            .config_dir
            .clone()
            .unwrap_or_else(|| args.state_dir.clone());
        let slot_index = args.slot_index.unwrap_or(1);
        let slots = daemon.slots;
        if slots == 0 {
            anyhow::bail!("cannot execute job with zero configured daemon slots");
        }
        let mut ready_announced = false;
        let beat = async {
            loop {
                let _ = feed_after_cycle(LocalCycle::finished(), !ready_announced);
                ready_announced = true;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        };
        tokio::select! {
            () = beat => anyhow::bail!("job heartbeat ended"),
            result = crate::runner::run_daemon_slot(daemon, config_base, slot_index, slots) => {
                result
            }
        }
    } else if args.once {
        let _ = feed_after_cycle(LocalCycle::finished(), true);
        Ok(())
    } else {
        loop {
            let _ = feed_after_cycle(LocalCycle::finished(), true);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

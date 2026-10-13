//! Transient per-job worker. Control loop stays async; it must not block a
//! heartbeat on a child wait. Host Docker remains the named transitional
//! executor, not the Build L3 availability boundary.

use std::path::PathBuf;

use clap::Args;
use velnor_model::Generation;

#[derive(Debug, Clone, Args)]
pub struct JobArgs {
    #[arg(long)]
    pub state_dir: PathBuf,
    #[arg(long)]
    pub job_id: String,
    #[arg(long, default_value_t = 1)]
    pub generation: u64,
    #[arg(long)]
    pub slot_index: usize,
    #[arg(long)]
    pub scope: String,
    /// Secure broker assignment envelope produced by the controller.
    #[arg(long)]
    pub handoff: PathBuf,
    /// Completion marker consumed by the broker manager.
    #[arg(long)]
    pub done: PathBuf,
}

pub async fn run(args: JobArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.state_dir)?;
    let result = crate::runner::run_transient_job(&args, &args.handoff).await;
    let status = if result.is_ok() {
        super::handoff::CompletionStatus::Finished
    } else {
        super::handoff::CompletionStatus::Failed
    };
    super::handoff::write_completion(
        &args.done,
        &args.job_id,
        Generation(args.generation),
        status,
    )?;
    result
}

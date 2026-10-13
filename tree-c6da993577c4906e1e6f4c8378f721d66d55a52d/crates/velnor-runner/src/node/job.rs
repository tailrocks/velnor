//! Transient per-job worker. Control loop stays async; it must not block a
//! heartbeat on a child wait. Host Docker remains the named transitional
//! executor, not the Build L3 availability boundary.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use velnor_control::journal::{Event, FleetState, Journal};
use velnor_model::{Generation, JobId, SlotId, SlotPhase2};

use super::watchdog::{feed_after_cycle, LocalCycle};

#[derive(Debug, Clone, Args)]
pub struct JobArgs {
    #[arg(long)]
    pub state_dir: PathBuf,
    #[arg(long)]
    pub job_id: String,
    /// Slot identity reserved by the controller for this worker generation.
    #[arg(long)]
    pub slot_id: Option<String>,
    /// Per-process launch lease issued by the controller for this slot.
    #[arg(long)]
    pub pressure_launch_nonce: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerRole {
    Waiter,
    OwnedJob,
}

fn is_waiter(job_id: &JobId) -> bool {
    job_id.0.starts_with("wait-")
}

fn validate_slot_identity(
    state: &FleetState,
    job_id: &JobId,
    slot_id: &SlotId,
    generation: Generation,
) -> anyhow::Result<WorkerRole> {
    let slot = state
        .slots
        .iter()
        .find(|slot| slot.slot_id == *slot_id)
        .ok_or_else(|| {
            anyhow::anyhow!("worker {} references unknown slot {}", job_id.0, slot_id.0)
        })?;
    if slot.generation != generation {
        anyhow::bail!(
            "worker {} has stale slot {} generation {} (current generation {})",
            job_id.0,
            slot_id.0,
            generation.0,
            slot.generation.0
        );
    }

    if is_waiter(job_id) {
        if slot.phase != SlotPhase2::Ready {
            anyhow::bail!(
                "waiter {} rejected for slot {} phase {} (expected ready)",
                job_id.0,
                slot_id.0,
                slot.phase.as_str()
            );
        }
        return Ok(WorkerRole::Waiter);
    }

    let job = state
        .jobs
        .iter()
        .find(|job| job.job_id == *job_id)
        .ok_or_else(|| anyhow::anyhow!("job {} has no generation-owned record", job_id.0))?;
    if job.generation != generation {
        anyhow::bail!(
            "job {} has stale generation {} (worker generation {})",
            job_id.0,
            job.generation.0,
            generation.0
        );
    }
    if job.slot_id != *slot_id {
        anyhow::bail!(
            "job {} slot identity mismatch: record={} worker={}",
            job_id.0,
            job.slot_id.0,
            slot_id.0
        );
    }
    if slot.phase != SlotPhase2::Assigned {
        anyhow::bail!(
            "job {} rejected for slot {} phase {} (expected assigned)",
            job_id.0,
            slot_id.0,
            slot.phase.as_str()
        );
    }
    Ok(WorkerRole::OwnedJob)
}

pub async fn run(args: JobArgs) -> anyhow::Result<()> {
    let package_guard = crate::release::package_execution_guard()?;
    run_with_package_guard(args, package_guard).await
}

pub(crate) async fn run_with_package_guard(
    args: JobArgs,
    _package_guard: crate::release::PackageExecutionGuard,
) -> anyhow::Result<()> {
    let journal_path = args.state_dir.join("journal.db");
    if !journal_path.is_file() {
        anyhow::bail!("worker journal is missing at {}", journal_path.display());
    }
    let job_id = JobId(args.job_id.clone());
    let generation = Generation(args.generation);
    let slot_id = args
        .slot_id
        .as_deref()
        .map(|slot_id| SlotId(slot_id.to_owned()))
        .ok_or_else(|| {
            anyhow::anyhow!("worker {} has no launch-bound slot identity", args.job_id)
        })?;
    let service_instance = std::fs::canonicalize(&args.state_dir)
        .with_context(|| {
            format!(
                "canonicalize worker service instance {}",
                args.state_dir.display()
            )
        })?
        .to_string_lossy()
        .into_owned();
    let mut journal = Journal::open_for_launch(
        &journal_path,
        &service_instance,
        &slot_id,
        generation,
        &args.pressure_launch_nonce,
    )?;
    let state = journal.materialized_state()?;
    journal.validate_disk_pressure_launch(
        &service_instance,
        &slot_id,
        generation,
        &args.pressure_launch_nonce,
    )?;
    let role = validate_slot_identity(&state, &job_id, &slot_id, generation)?;
    crate::node::cleanup::write_owned_pid(
        &args.state_dir,
        &args.job_id,
        generation.0,
        std::process::id(),
    )
    .context("publish worker-owned process marker after lease validation")?;
    if role == WorkerRole::OwnedJob {
        let started = journal.apply_with_disk_pressure_launch(
            &service_instance,
            &slot_id,
            generation,
            &args.pressure_launch_nonce,
            Event::JobStarted {
                job_id: job_id.clone(),
                generation,
            },
        )?;
        if started.rejected {
            anyhow::bail!(
                "job {} start rejected at generation {}",
                args.job_id,
                args.generation
            );
        }
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
            result = crate::runner::run_daemon_slot(
                daemon,
                config_base,
                slot_index,
                slots,
                slot_id,
                generation,
            ) => {
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;
    use velnor_model::JobPhase2;

    fn state_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "velnor-job-slot-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn test_service_instance(state_dir: &std::path::Path) -> String {
        std::fs::canonicalize(state_dir)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn open_controller_test_journal(
        state_dir: &std::path::Path,
    ) -> velnor_control::store::StoreResult<Journal> {
        Journal::open_for_service_instance(
            state_dir.join("journal.db"),
            &test_service_instance(state_dir),
        )
    }

    fn prime_ready_slots(journal: &mut Journal, scope: &str, count: u32) {
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: count },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        for index in 1..=count {
            let slot_id = SlotId(format!("{scope}-{index}"));
            for event in [
                Event::PermitReserved {
                    slot_id: slot_id.clone(),
                    generation: Generation::INITIAL,
                },
                Event::ExecutorProven {
                    slot_id: slot_id.clone(),
                    generation: Generation::INITIAL,
                },
                Event::SessionLive {
                    slot_id: slot_id.clone(),
                    generation: Generation::INITIAL,
                },
                Event::RegistrationIntended {
                    slot_id: slot_id.clone(),
                    generation: Generation::INITIAL,
                },
                Event::Registered {
                    slot_id: slot_id.clone(),
                    generation: Generation::INITIAL,
                },
                Event::ReadyAttempt {
                    slot_id,
                    generation: Generation::INITIAL,
                },
            ] {
                assert!(!journal.apply(event).unwrap().rejected);
            }
        }
    }

    #[tokio::test]
    async fn pre_assignment_waiter_uses_ready_slot_without_job_record() {
        let dir = state_dir("waiter");
        std::fs::create_dir_all(&dir).unwrap();
        let mut journal = open_controller_test_journal(&dir).unwrap();
        prime_ready_slots(&mut journal, "waiter", 1);
        let waiter = SlotId("waiter-1".to_owned());
        let service_instance = test_service_instance(&dir);
        let pressure_launch_nonce = journal
            .issue_disk_pressure_launch(&service_instance, &waiter, Generation::INITIAL, 1)
            .unwrap();
        drop(journal);

        run(JobArgs {
            state_dir: dir.clone(),
            job_id: "wait-waiter-1".to_owned(),
            slot_id: Some(waiter.0),
            pressure_launch_nonce,
            generation: Generation::INITIAL.0,
            slot_index: None,
            scope: None,
            once: true,
        })
        .await
        .unwrap();

        let state = Journal::open(dir.join("journal.db"))
            .unwrap()
            .load_state()
            .unwrap();
        assert!(state.jobs.is_empty());
        assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn stale_or_mismatched_slot_identity_is_rejected_before_start() {
        let dir = state_dir("reject");
        std::fs::create_dir_all(&dir).unwrap();
        let mut journal = open_controller_test_journal(&dir).unwrap();
        prime_ready_slots(&mut journal, "reject", 2);
        let job_id = JobId("job-1".to_owned());
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: SlotId("reject-1".to_owned()),
                    job_id: job_id.clone(),
                    generation: Generation::INITIAL,
                    message_id: "msg-1".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id: job_id.clone(),
                    slot_id: SlotId("reject-1".to_owned()),
                    attempt: 1,
                    generation: Generation::INITIAL,
                    worker: "worker-1".to_owned(),
                    accepted_unix: 1,
                })
                .unwrap()
                .rejected
        );
        let service_instance = test_service_instance(&dir);
        let valid_pressure_nonce = journal
            .issue_disk_pressure_launch(
                &service_instance,
                &SlotId("reject-1".to_owned()),
                Generation::INITIAL,
                1,
            )
            .unwrap();
        drop(journal);

        let stale = run(JobArgs {
            state_dir: dir.clone(),
            job_id: job_id.0.clone(),
            slot_id: Some("reject-1".to_owned()),
            pressure_launch_nonce: valid_pressure_nonce.clone(),
            generation: 2,
            slot_index: None,
            scope: None,
            once: true,
        })
        .await;
        assert!(stale.is_err());

        let mismatch = run(JobArgs {
            state_dir: dir.clone(),
            job_id: job_id.0,
            slot_id: Some("reject-2".to_owned()),
            pressure_launch_nonce: valid_pressure_nonce,
            generation: Generation::INITIAL.0,
            slot_index: None,
            scope: None,
            once: true,
        })
        .await;
        assert!(mismatch.is_err());

        let state = Journal::open(dir.join("journal.db"))
            .unwrap()
            .load_state()
            .unwrap();
        assert_eq!(state.jobs[0].phase, JobPhase2::Assigned);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn stale_same_generation_launch_is_rejected_before_worker_side_effects() {
        let dir = state_dir("stale-launch-nonce");
        std::fs::create_dir_all(&dir).unwrap();
        let mut journal = open_controller_test_journal(&dir).unwrap();
        prime_ready_slots(&mut journal, "nonce", 1);
        let service_instance = test_service_instance(&dir);
        let slot_id = SlotId("nonce-1".to_owned());
        let stale_nonce = journal
            .issue_disk_pressure_launch(&service_instance, &slot_id, Generation::INITIAL, 1)
            .unwrap();
        let _current_nonce = journal
            .issue_disk_pressure_launch(&service_instance, &slot_id, Generation::INITIAL, 2)
            .unwrap();
        let job_id = JobId("job-nonce-1".to_owned());
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot_id.clone(),
                    job_id: job_id.clone(),
                    generation: Generation::INITIAL,
                    message_id: "message-nonce-1".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id: job_id.clone(),
                    slot_id: slot_id.clone(),
                    attempt: 1,
                    generation: Generation::INITIAL,
                    worker: "worker-nonce-1".to_owned(),
                    accepted_unix: 1,
                })
                .unwrap()
                .rejected
        );
        drop(journal);

        let result = run(JobArgs {
            state_dir: dir.clone(),
            job_id: job_id.0,
            slot_id: Some(slot_id.0),
            pressure_launch_nonce: stale_nonce,
            generation: Generation::INITIAL.0,
            slot_index: None,
            scope: None,
            once: true,
        })
        .await;
        assert!(result.is_err());

        let state = Journal::open(dir.join("journal.db"))
            .unwrap()
            .load_state()
            .unwrap();
        assert_eq!(state.jobs[0].phase, JobPhase2::Assigned);
        std::fs::remove_dir_all(dir).ok();
    }
}

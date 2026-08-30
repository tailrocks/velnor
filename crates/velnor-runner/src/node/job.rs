//! Transient per-job worker. Control loop stays async; it must not block a
//! heartbeat on a child wait. Host Docker remains the named transitional
//! executor, not the Build L3 availability boundary.

use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use velnor_control::journal::{Event, FleetState, Journal};
use velnor_model::{ActorPhase, Generation, JobId, SlotId};

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
        if slot.phase != ActorPhase::Ready {
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
    if slot.phase != ActorPhase::Assigned {
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
    std::fs::create_dir_all(&args.state_dir)?;
    let mut journal = Journal::open(args.state_dir.join("journal.db"))?;
    let job_id = JobId(args.job_id.clone());
    let generation = Generation(args.generation);
    let state = journal.materialized_state()?;
    let slot_id = args
        .slot_id
        .as_deref()
        .map(|slot_id| SlotId(slot_id.to_owned()))
        .or_else(|| {
            state
                .jobs
                .iter()
                .find(|job| job.job_id == job_id && job.generation == generation)
                .map(|job| job.slot_id.clone())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "worker {} has no generation-owned slot identity",
                args.job_id
            )
        })?;
    let role = validate_slot_identity(&state, &job_id, &slot_id, generation)?;
    if role == WorkerRole::OwnedJob {
        let started = journal.apply(Event::JobStarted {
            job_id: job_id.clone(),
            generation,
        })?;
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
mod tests {
    use super::*;

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
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        prime_ready_slots(&mut journal, "waiter", 1);
        drop(journal);

        run(JobArgs {
            state_dir: dir.clone(),
            job_id: "wait-waiter-1".to_owned(),
            slot_id: Some("waiter-1".to_owned()),
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
        assert_eq!(state.slots[0].phase, ActorPhase::Ready);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn stale_or_mismatched_slot_identity_is_rejected_before_start() {
        let dir = state_dir("reject");
        std::fs::create_dir_all(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        prime_ready_slots(&mut journal, "reject", 2);
        let job_id = JobId("job-1".to_owned());
        assert!(
            !journal
                .apply(Event::Assigned {
                    slot_id: SlotId("reject-1".to_owned()),
                    job_id: job_id.clone(),
                    generation: Generation::INITIAL,
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
        drop(journal);

        let stale = run(JobArgs {
            state_dir: dir.clone(),
            job_id: job_id.0.clone(),
            slot_id: Some("reject-1".to_owned()),
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
        assert_eq!(state.jobs[0].phase, ActorPhase::Assigned);
        std::fs::remove_dir_all(dir).ok();
    }
}

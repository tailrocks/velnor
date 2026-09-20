//! Slot waiter and provisional-acquisition recovery child. Control loop stays
//! async; it must not block a heartbeat on a child wait. Host Docker remains
//! the named transitional executor, not the Build L3 availability boundary.

use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use velnor_control::journal::{FleetState, Journal};
use velnor_model::{Generation, JobId, SlotId, SlotPhase2};

use super::watchdog::{feed_after_cycle, LocalCycle};

#[derive(Debug, Clone, Args)]
pub struct JobArgs {
    #[arg(long)]
    pub state_dir: PathBuf,
    #[arg(long)]
    pub job_id: String,
    /// Token from the controller's durable pre-spawn intent. The child must
    /// publish its own PID against this exact intent before opening state.
    #[arg(long)]
    pub launch_token: String,
    /// Run only the bounded renewjob recovery probe for a provisional
    /// acquisition. This role cannot start a broker session or execute work.
    #[arg(long)]
    pub recovery_only: bool,
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
    RecoveryWaiter,
}

fn is_waiter(job_id: &JobId) -> bool {
    job_id.0.starts_with("wait-")
}

fn validate_slot_identity(
    state: &FleetState,
    job_id: &JobId,
    slot_id: &SlotId,
    generation: Generation,
    recovery_only: bool,
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
        if recovery_only {
            if job_id.0 != format!("wait-{}", slot_id.0) {
                anyhow::bail!(
                    "recovery-only worker {} does not match slot {} waiter identity",
                    job_id.0,
                    slot_id.0
                );
            }
            let provisional_count = state
                .jobs
                .iter()
                .filter(|job| {
                    job.slot_id == *slot_id
                        && job.generation == generation
                        && job.phase.occupies_slot()
                        && job.provisional
                })
                .count();
            let other_owner = state.jobs.iter().any(|job| {
                job.slot_id == *slot_id
                    && job.generation == generation
                    && job.phase.occupies_slot()
                    && !job.provisional
            });
            if slot.phase != SlotPhase2::Assigned || provisional_count != 1 || other_owner {
                anyhow::bail!(
                    "recovery waiter {} requires exactly one provisional owner for slot {} generation {}",
                    job_id.0,
                    slot_id.0,
                    generation.0
                );
            }
            return Ok(WorkerRole::RecoveryWaiter);
        }
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
    anyhow::bail!("worker {} must use a wait- identity", job_id.0)
}

pub async fn run(args: JobArgs) -> anyhow::Result<()> {
    super::cleanup::write_owned_pid_for_intent(
        &args.state_dir,
        &args.job_id,
        args.generation,
        std::process::id(),
        &args.launch_token,
    )?;
    std::fs::create_dir_all(&args.state_dir)?;
    let journal = Journal::open(args.state_dir.join("journal.db"))?;
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
    let role = validate_slot_identity(&state, &job_id, &slot_id, generation, args.recovery_only)?;
    if role == WorkerRole::RecoveryWaiter {
        let exec = super::exec::load_exec_config(&args.state_dir)?;
        let slot_index = args
            .slot_index
            .ok_or_else(|| anyhow::anyhow!("recovery waiter is missing its slot index"))?;
        if exec.slots == 0 || slot_index == 0 || slot_index > exec.slots {
            anyhow::bail!(
                "recovery waiter slot index {slot_index} is outside configured slots 1..={}",
                exec.slots
            );
        }
        let config_base = exec
            .config_dir
            .clone()
            .unwrap_or_else(|| args.state_dir.clone());
        let slot_config_dir =
            crate::runner::daemon_slot_config_dir(&config_base, slot_index, exec.slots);
        crate::runner::resolve_provisional_acquisitions_for_recovery(&slot_config_dir).await?;
        return Ok(());
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
    use crate::node::cleanup;
    use velnor_control::journal::Event;
    use velnor_model::JobPhase2;

    #[cfg(unix)]
    const CHILD_TEST_FILTER: &str = "node::job::tests::job_entrypoint_child_for_ownership_fixtures";

    #[cfg(unix)]
    struct JobChild(std::process::Child);

    #[cfg(unix)]
    impl Drop for JobChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(unix)]
    fn spawn_job_child(
        state_dir: &std::path::Path,
        mode: &str,
        job_id: &str,
        launch_token: &str,
    ) -> JobChild {
        spawn_job_child_with_options(
            state_dir,
            mode,
            job_id,
            launch_token,
            "waiter-1",
            Generation::INITIAL.0,
            false,
            mode == "waiter-once",
            None,
        )
    }

    #[cfg(unix)]
    fn spawn_job_child_with_options(
        state_dir: &std::path::Path,
        mode: &str,
        job_id: &str,
        launch_token: &str,
        slot_id: &str,
        generation: u64,
        recovery_only: bool,
        once: bool,
        expected_error: Option<&str>,
    ) -> JobChild {
        use std::os::unix::process::CommandExt;

        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg0(format!("--launch-token={launch_token}"))
            .args(["--exact", CHILD_TEST_FILTER, "--test-threads=1"])
            .env("VELNOR_JOB_CHILD_MODE", mode)
            .env("VELNOR_JOB_CHILD_STATE_DIR", state_dir)
            .env("VELNOR_JOB_CHILD_JOB_ID", job_id)
            .env("VELNOR_JOB_CHILD_LAUNCH_TOKEN", launch_token)
            .env("VELNOR_JOB_CHILD_SLOT_ID", slot_id)
            .env("VELNOR_JOB_CHILD_GENERATION", generation.to_string())
            .env("VELNOR_JOB_CHILD_RECOVERY_ONLY", recovery_only.to_string())
            .env("VELNOR_JOB_CHILD_ONCE", once.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit());
        if let Some(expected_error) = expected_error {
            command.env("VELNOR_JOB_CHILD_EXPECTED_ERROR", expected_error);
        }
        JobChild(command.spawn().unwrap())
    }

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

    #[test]
    fn recovery_only_waiter_requires_one_exact_provisional_assigned_owner() {
        let dir = state_dir("recovery-role");
        std::fs::create_dir_all(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        prime_ready_slots(&mut journal, "recovery", 1);
        let provisional = JobId("request-1".to_owned());
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: SlotId("recovery-1".to_owned()),
                    job_id: provisional.clone(),
                    generation: Generation::INITIAL,
                    message_id: "message-1".to_owned(),
                    runner_request_id: Some(provisional.0.clone()),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobAcquisitionResolved {
                    provisional_job_id: provisional,
                    acquired_job_id: JobId("job-1".to_owned()),
                    plan_id: "plan-1".to_owned(),
                    generation: Generation::INITIAL,
                    runner_request_id: Some("request-1".to_owned()),
                    permit_lease: None,
                })
                .unwrap()
                .rejected
        );
        let state = journal.materialized_state().unwrap();
        let slot_id = SlotId("recovery-1".to_owned());
        assert_eq!(
            validate_slot_identity(
                &state,
                &JobId("wait-recovery-1".to_owned()),
                &slot_id,
                Generation::INITIAL,
                true,
            )
            .unwrap(),
            WorkerRole::RecoveryWaiter
        );
        assert!(validate_slot_identity(
            &state,
            &JobId("wait-other-slot".to_owned()),
            &slot_id,
            Generation::INITIAL,
            true,
        )
        .is_err());
        assert!(validate_slot_identity(
            &state,
            &JobId("wait-recovery-1".to_owned()),
            &slot_id,
            Generation::INITIAL,
            false,
        )
        .is_err());

        let mut no_provisional_owner = state.clone();
        no_provisional_owner.jobs.clear();
        assert!(validate_slot_identity(
            &no_provisional_owner,
            &JobId("wait-recovery-1".to_owned()),
            &slot_id,
            Generation::INITIAL,
            true,
        )
        .is_err());

        let mut duplicate_provisional_owner = state.clone();
        let duplicate = duplicate_provisional_owner.jobs[0].clone();
        duplicate_provisional_owner.jobs.push(duplicate);
        assert!(validate_slot_identity(
            &duplicate_provisional_owner,
            &JobId("wait-recovery-1".to_owned()),
            &slot_id,
            Generation::INITIAL,
            true,
        )
        .is_err());

        let mut competing_owner = state.clone();
        let mut second_owner = competing_owner.jobs[0].clone();
        second_owner.job_id = JobId("job-2".to_owned());
        second_owner.provisional = false;
        second_owner.phase = JobPhase2::Running;
        competing_owner.jobs.push(second_owner);
        assert!(validate_slot_identity(
            &competing_owner,
            &JobId("wait-recovery-1".to_owned()),
            &slot_id,
            Generation::INITIAL,
            true,
        )
        .is_err());

        let mut non_assigned_slot = state.clone();
        non_assigned_slot.slots[0].phase = SlotPhase2::Ready;
        assert!(validate_slot_identity(
            &non_assigned_slot,
            &JobId("wait-recovery-1".to_owned()),
            &slot_id,
            Generation::INITIAL,
            true,
        )
        .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pre_assignment_waiter_uses_ready_slot_without_job_record() {
        let dir = state_dir("waiter");
        std::fs::create_dir_all(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        prime_ready_slots(&mut journal, "waiter", 1);
        drop(journal);
        let launch_token =
            cleanup::write_owned_pid_intent(&dir, "wait-waiter-1", Generation::INITIAL.0).unwrap();
        let mut child = spawn_job_child(&dir, "waiter", "wait-waiter-1", &launch_token);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match cleanup::owned_pid_liveness(&dir, "wait-waiter-1", Generation::INITIAL.0).unwrap()
            {
                cleanup::OwnedPidLiveness::Live => break,
                cleanup::OwnedPidLiveness::UnpublishedIntentLive => {}
                state => {
                    panic!("token-bearing waiter child has unexpected ownership state: {state:?}")
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "worker child did not publish its PID before the deadline"
            );
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "worker child exited before publishing ownership"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let state = Journal::open(dir.join("journal.db"))
            .unwrap()
            .load_state()
            .unwrap();
        assert!(state.jobs.is_empty());
        assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
        child.0.kill().unwrap();
        let _ = child.0.wait();
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, "wait-waiter-1", Generation::INITIAL.0).unwrap(),
            cleanup::OwnedPidLiveness::Dead
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pre_assignment_waiter_returns_success_after_one_shot() {
        let dir = state_dir("waiter-once");
        std::fs::create_dir_all(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        prime_ready_slots(&mut journal, "waiter", 1);
        drop(journal);
        let launch_token =
            cleanup::write_owned_pid_intent(&dir, "wait-waiter-1", Generation::INITIAL.0).unwrap();
        let mut child = spawn_job_child(&dir, "waiter-once", "wait-waiter-1", &launch_token);

        assert!(child.0.wait().unwrap().success());
        assert_eq!(
            cleanup::owned_pid_liveness(&dir, "wait-waiter-1", Generation::INITIAL.0).unwrap(),
            cleanup::OwnedPidLiveness::Dead
        );
        let state = Journal::open(dir.join("journal.db"))
            .unwrap()
            .load_state()
            .unwrap();
        assert!(state.jobs.is_empty());
        assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_refuses_missing_launch_intent_before_opening_journal() {
        let dir = state_dir("missing-intent");
        std::fs::create_dir_all(&dir).unwrap();
        cleanup::initialize_owned_directory(&dir).unwrap();
        let mut child = spawn_job_child(&dir, "missing-intent", "wait-missing-1", "stale-token");
        assert!(child.0.wait().unwrap().success());
        assert!(
            !dir.join("journal.db").exists(),
            "a child without the exact durable intent must stop before opening state"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn job_entrypoint_child_for_ownership_fixtures() {
        let Ok(mode) = std::env::var("VELNOR_JOB_CHILD_MODE") else {
            return;
        };
        let state_dir =
            std::path::PathBuf::from(std::env::var_os("VELNOR_JOB_CHILD_STATE_DIR").unwrap());
        let job_id = std::env::var("VELNOR_JOB_CHILD_JOB_ID").unwrap();
        let launch_token = std::env::var("VELNOR_JOB_CHILD_LAUNCH_TOKEN").unwrap();
        let slot_id = std::env::var("VELNOR_JOB_CHILD_SLOT_ID").unwrap();
        let generation = std::env::var("VELNOR_JOB_CHILD_GENERATION")
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let recovery_only = std::env::var("VELNOR_JOB_CHILD_RECOVERY_ONLY")
            .unwrap()
            .parse::<bool>()
            .unwrap();
        let once = std::env::var("VELNOR_JOB_CHILD_ONCE")
            .unwrap()
            .parse::<bool>()
            .unwrap();
        let result = run(JobArgs {
            state_dir,
            job_id,
            launch_token,
            recovery_only,
            slot_id: Some(slot_id),
            generation,
            slot_index: None,
            scope: None,
            once,
        })
        .await;
        match mode.as_str() {
            "waiter" => result.unwrap(),
            "waiter-once" => result.unwrap(),
            "missing-intent" => assert!(result
                .unwrap_err()
                .to_string()
                .contains("spawn intent disappeared before child")),
            "expect-error" => {
                let expected = std::env::var("VELNOR_JOB_CHILD_EXPECTED_ERROR").unwrap();
                assert_eq!(result.unwrap_err().to_string(), expected);
            }
            _ => panic!("unknown job child fixture mode {mode}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_or_mismatched_slot_identity_is_rejected_before_start() {
        let dir = state_dir("reject");
        std::fs::create_dir_all(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        prime_ready_slots(&mut journal, "reject", 2);
        let job_id = JobId("job-1".to_owned());
        let provisional_job_id = JobId("request-1".to_owned());
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: SlotId("reject-1".to_owned()),
                    job_id: provisional_job_id.clone(),
                    generation: Generation::INITIAL,
                    message_id: "msg-1".into(),
                    runner_request_id: Some(provisional_job_id.0.clone()),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobAcquisitionResolved {
                    provisional_job_id,
                    acquired_job_id: job_id.clone(),
                    plan_id: "plan-1".into(),
                    generation: Generation::INITIAL,
                    runner_request_id: Some("request-1".into()),
                    permit_lease: None,
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

        let stale_generation = 2;
        let stale_token =
            cleanup::write_owned_pid_intent(&dir, &job_id.0, stale_generation).unwrap();
        let mut stale = spawn_job_child_with_options(
            &dir,
            "expect-error",
            &job_id.0,
            &stale_token,
            "reject-1",
            stale_generation,
            false,
            true,
            Some("worker job-1 has stale slot reject-1 generation 2 (current generation 1)"),
        );
        assert!(stale.0.wait().unwrap().success());

        let mismatch_token =
            cleanup::write_owned_pid_intent(&dir, &job_id.0, Generation::INITIAL.0).unwrap();
        let mut mismatch = spawn_job_child_with_options(
            &dir,
            "expect-error",
            &job_id.0,
            &mismatch_token,
            "reject-2",
            Generation::INITIAL.0,
            false,
            true,
            Some("job job-1 slot identity mismatch: record=reject-1 worker=reject-2"),
        );
        assert!(mismatch.0.wait().unwrap().success());

        let state = Journal::open(dir.join("journal.db"))
            .unwrap()
            .load_state()
            .unwrap();
        assert_eq!(state.jobs[0].phase, JobPhase2::Assigned);
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recovery_only_rejects_normal_job_identity_before_start() {
        let dir = state_dir("recovery-only-normal-job");
        std::fs::create_dir_all(&dir).unwrap();
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        prime_ready_slots(&mut journal, "recovery-normal", 1);
        let slot_id = SlotId("recovery-normal-1".to_owned());
        let provisional_job_id = JobId("request-1".to_owned());
        let job_id = JobId("job-1".to_owned());
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot_id.clone(),
                    job_id: provisional_job_id.clone(),
                    generation: Generation::INITIAL,
                    message_id: "message-1".to_owned(),
                    runner_request_id: Some(provisional_job_id.0.clone()),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobAcquisitionResolved {
                    provisional_job_id,
                    acquired_job_id: job_id.clone(),
                    plan_id: "plan-1".to_owned(),
                    generation: Generation::INITIAL,
                    runner_request_id: Some("request-1".to_owned()),
                    permit_lease: None,
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
                    worker: "worker-1".to_owned(),
                    accepted_unix: 1,
                })
                .unwrap()
                .rejected
        );
        drop(journal);

        let launch_token =
            cleanup::write_owned_pid_intent(&dir, &job_id.0, Generation::INITIAL.0).unwrap();
        let mut child = spawn_job_child_with_options(
            &dir,
            "expect-error",
            &job_id.0,
            &launch_token,
            &slot_id.0,
            Generation::INITIAL.0,
            true,
            true,
            Some("recovery-only worker job-1 must use a wait- identity"),
        );
        assert!(child.0.wait().unwrap().success());

        let state = Journal::open(dir.join("journal.db"))
            .unwrap()
            .load_state()
            .unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, job_id);
        assert_eq!(state.jobs[0].phase, JobPhase2::Assigned);
        std::fs::remove_dir_all(dir).ok();
    }
}

//! Node architecture: health vector, slot process isolation, guardian cycle.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use velnor_control::journal::{Event, Journal};
use velnor_model::{FleetHealthState, Generation, HealthDocument, SlotId};

fn scratch(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "velnor-node-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn runner() -> &'static str {
    env!("CARGO_BIN_EXE_velnor-runner")
}

fn prime_two_ready(journal: &mut Journal) {
    let g = Generation::INITIAL;
    for event in [
        Event::ControlLive,
        Event::JournalWritable,
        Event::Dependency {
            github_reachable: true,
        },
        Event::Routing {
            valid: true,
            group_valid: true,
        },
        Event::DesiredCapacity { ready: 2, surge: 1 },
    ] {
        journal.apply(event).unwrap();
    }
    for index in 1..=2 {
        let slot_id = SlotId(format!("iso-{index}"));
        for event in [
            Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation: g,
                surge: false,
            },
            Event::ExecutorProven {
                slot_id: slot_id.clone(),
                generation: g,
            },
            Event::SessionLive {
                slot_id: slot_id.clone(),
                generation: g,
            },
            Event::RegistrationIntended {
                slot_id: slot_id.clone(),
                generation: g,
            },
            Event::Registered {
                slot_id: slot_id.clone(),
                generation: g,
            },
            Event::ReadyAttempt {
                slot_id: slot_id.clone(),
                generation: g,
            },
        ] {
            let outcome = journal.apply(event.clone()).unwrap();
            assert!(!outcome.rejected, "{event:?}");
        }
    }
}

#[test]
fn github_down_health_is_not_ready_while_control_live() {
    let dir = scratch("gh-down");
    let mut journal = Journal::open(dir.join("journal.db")).unwrap();
    journal.apply(Event::ControlLive).unwrap();
    journal.apply(Event::JournalWritable).unwrap();
    journal
        .apply(Event::Dependency {
            github_reachable: false,
        })
        .unwrap();
    let health = journal.load_state().unwrap().health();
    assert!(health.control_live);
    assert_ne!(health.state, FleetHealthState::Ready);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn slot_kill_drops_one_unit_of_capacity() {
    let dir = scratch("iso");
    let mut journal = Journal::open(dir.join("journal.db")).unwrap();
    prime_two_ready(&mut journal);
    drop(journal);

    let mut children = Vec::new();
    for index in 1..=2 {
        let out = dir.join(format!("slot-{index}.out"));
        let err = dir.join(format!("slot-{index}.err"));
        let child = Command::new(runner())
            .args([
                "slot",
                "--state-dir",
                dir.to_str().unwrap(),
                "--scope",
                "iso",
                "--slot-index",
                &index.to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::fs::File::create(&out).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(&err).unwrap()))
            .spawn()
            .expect("spawn slot");
        children.push(child);
    }
    let mut pids = 0;
    for _ in 0..40 {
        if let Ok(journal) = Journal::open(dir.join("journal.db")) {
            if let Ok(state) = journal.load_state() {
                pids = state.slots.iter().filter(|slot| slot.pid.is_some()).count();
                if pids == 2 {
                    break;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        pids,
        2,
        "slot stderr: 1={:?} 2={:?}",
        std::fs::read_to_string(dir.join("slot-1.err")),
        std::fs::read_to_string(dir.join("slot-2.err"))
    );

    let guardian = Command::new(runner())
        .args(["guardian", "--state-dir", dir.to_str().unwrap(), "--once"])
        .output()
        .unwrap();
    assert!(
        guardian.status.success(),
        "{}",
        String::from_utf8_lossy(&guardian.stderr)
    );

    let before: HealthDocument =
        serde_json::from_slice(&std::fs::read(dir.join("health.json")).unwrap()).unwrap();
    assert_eq!(before.actual_ready_slots, 2, "{before:?}");

    children[0].kill().unwrap();
    let _ = children[0].wait();
    std::thread::sleep(std::time::Duration::from_millis(200));

    let guardian = Command::new(runner())
        .args(["guardian", "--state-dir", dir.to_str().unwrap(), "--once"])
        .output()
        .unwrap();
    assert!(guardian.status.success());
    let after: HealthDocument =
        serde_json::from_slice(&std::fs::read(dir.join("health.json")).unwrap()).unwrap();
    assert_eq!(after.actual_ready_slots, 1, "{after:?}");
    assert_ne!(after.actual_ready_slots, 0);

    let sibling_alive = children[1].try_wait().unwrap().is_none();
    assert!(sibling_alive, "sibling slot process must survive");
    children[1].kill().ok();
    let _ = children[1].wait();
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn guardian_completes_a_cycle_without_job_execution() {
    let dir = scratch("guard");
    Journal::open(dir.join("journal.db")).unwrap();
    let output = Command::new(runner())
        .args(["guardian", "--state-dir", dir.to_str().unwrap(), "--once"])
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(Path::new(&dir).join("health.json").exists());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn packaged_units_have_no_controller_partof_to_workers() {
    let controller = include_str!("../debian/velnor-controller@.service");
    let slot = include_str!("../debian/velnor-slot@.service");
    let job = include_str!("../debian/velnor-job@.service");
    assert!(!controller.lines().any(|line| line.starts_with("PartOf=")));
    assert!(!slot.lines().any(|line| line.starts_with("PartOf=")));
    assert!(!job.lines().any(|line| line.starts_with("PartOf=")));
    assert!(slot.contains("velnor-runner slot"));
    assert!(job.contains("KillMode=control-group"));
    assert!(include_str!("../debian/velnor-guardian.service").contains("velnor-runner guardian"));
    assert!(!include_str!("../debian/velnor-guardian.service")
        .lines()
        .any(|line| line.starts_with("EnvironmentFile=") && line.contains("secrets.env")));
    assert!(include_str!("../debian/velnor-jobs.slice").contains("transitional Docker"));
    let guardian_src = include_str!("../src/node/guardian.rs");
    let code = guardian_src
        .split("#[cfg(test)]")
        .next()
        .unwrap_or(guardian_src);
    assert!(!code.contains("GITHUB_TOKEN"));
    assert!(!code.contains("docker.sock"));
    assert!(!code.contains("reqwest"));
    let slot_src = include_str!("../src/node/slot.rs");
    assert!(
        !slot_src.contains("run_daemon_slot"),
        "assigned work must not run in the slot process"
    );
    assert!(
        !slot_src.contains("spawn_slot_job") && !slot_src.contains("JobOwned"),
        "slot process must not spawn jobs or claim ownership"
    );
    let job_src = include_str!("../src/node/job.rs");
    assert!(
        job_src.contains("run_daemon_slot"),
        "job process is the transitional executor"
    );
    assert!(
        job_src.contains("CompletionIntended"),
        "job process must persist completion before send"
    );
    let beat = job_src
        .split("if let Ok(mut daemon)")
        .next()
        .expect("job beat");
    assert!(
        !beat.contains("if args.once"),
        "heartbeat must not return on --once before the worker"
    );
    let controller_src = include_str!("../src/node/controller.rs");
    assert!(
        controller_src.contains("observe_routing") && controller_src.contains("observe_executor"),
        "controller must observe routing and executor, not stamp them"
    );
    assert!(
        !controller_src.contains("Event::Routing {\n        valid: true"),
        "controller must not stamp Routing valid:true"
    );
    assert!(
        controller_src.contains("JobOwned") && controller_src.contains("CompletionSendStarted"),
        "controller must claim jobs and send completions"
    );
    let daemon_src = include_str!("../src/runner.rs");
    let pass = daemon_src
        .split("async fn daemon_pass")
        .nth(1)
        .expect("daemon_pass")
        .split("fn supervised_retry_delay")
        .next()
        .expect("daemon_pass body");
    let reserve = pass
        .find("reserve_capacity_permits")
        .expect("permits first");
    let dry = pass
        .find("daemon_should_poll_after_jit_config")
        .expect("dry-run gate");
    let configure = pass
        .find("configure_daemon_slots(&resolved_args")
        .expect("jit configure");
    assert!(reserve < dry, "permits before dry-run gate");
    assert!(
        dry < configure,
        "production JIT must not run before the dry-run gate"
    );
    assert_eq!(
        pass.matches("configure_daemon_slots(&resolved_args")
            .count(),
        1,
        "only dry-run may bulk-configure JIT slots"
    );
    assert!(!daemon_src.contains("daemon-args.json"));
    assert!(
        !job.contains("--once"),
        "packaged job unit must not pass --once: {job}"
    );
    assert!(
        job_src.contains("daemon.once = true"),
        "--once must select one GitHub job, not skip the worker"
    );
}

fn prime_named_ready(journal: &mut Journal, scope: &str) {
    let g = Generation::INITIAL;
    for event in [
        Event::ControlLive,
        Event::JournalWritable,
        Event::Dependency {
            github_reachable: true,
        },
        Event::Routing {
            valid: true,
            group_valid: true,
        },
        Event::DesiredCapacity { ready: 1, surge: 0 },
    ] {
        journal.apply(event).unwrap();
    }
    let slot_id = SlotId(format!("{scope}-1"));
    for event in [
        Event::PermitReserved {
            slot_id: slot_id.clone(),
            generation: g,
            surge: false,
        },
        Event::ExecutorProven {
            slot_id: slot_id.clone(),
            generation: g,
        },
        Event::SessionLive {
            slot_id: slot_id.clone(),
            generation: g,
        },
        Event::RegistrationIntended {
            slot_id: slot_id.clone(),
            generation: g,
        },
        Event::Registered {
            slot_id: slot_id.clone(),
            generation: g,
        },
        Event::ReadyAttempt {
            slot_id: slot_id.clone(),
            generation: g,
        },
    ] {
        let outcome = journal.apply(event.clone()).unwrap();
        assert!(!outcome.rejected, "{event:?}");
    }
}

fn kill_pid(pid: u32) {
    let _ = Command::new("/bin/kill")
        .args(["-9", &pid.to_string()])
        .status();
}

fn run_runner(dir: &Path, args: &[&str]) -> std::process::ExitStatus {
    let stdout = std::fs::File::create(dir.join("cmd.out")).unwrap();
    let stderr = std::fs::File::create(dir.join("cmd.err")).unwrap();
    Command::new(runner())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .unwrap()
}

fn cmd_err(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("cmd.err")).unwrap_or_default()
}

#[test]
fn controller_does_not_stamp_ready_without_proofs() {
    let dir = scratch("no-proof");
    let status = run_runner(
        &dir,
        &[
            "controller",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "noproof",
            "--desired-ready",
            "1",
            "--surge",
            "0",
            "--once",
            "--spawn-slots",
            "false",
        ],
    );
    assert!(status.success(), "{}", cmd_err(&dir));
    let state = Journal::open(dir.join("journal.db"))
        .unwrap()
        .load_state()
        .unwrap();
    assert!(!state.routing_valid, "{state:?}");
    assert!(
        state.slots.iter().all(|slot| slot.ready_proof().is_err()),
        "{:?}",
        state.slots
    );
    assert!(state.slots.iter().all(|slot| !slot.registered));
    assert!(state.jobs.is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn controller_observes_live_session_and_executor_before_ready_proof() {
    let dir = scratch("proofs");
    let first = run_runner(
        &dir,
        &[
            "controller",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "proof",
            "--desired-ready",
            "1",
            "--surge",
            "0",
            "--once",
            "--spawn-slots",
            "false",
        ],
    );
    assert!(first.success(), "{}", cmd_err(&dir));

    let mut slot = Command::new(runner())
        .args([
            "slot",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "proof",
            "--slot-index",
            "1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut saw_pid = false;
    for _ in 0..40 {
        if let Ok(journal) = Journal::open(dir.join("journal.db")) {
            if let Ok(state) = journal.load_state() {
                if state.slots.iter().any(|item| item.pid.is_some()) {
                    saw_pid = true;
                    break;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(saw_pid, "slot heartbeat never landed");

    velnor_runner::node::prove::write_routing(&dir, true, true).unwrap();
    velnor_runner::node::prove::write_executor_ok(&dir).unwrap();

    let second = run_runner(
        &dir,
        &[
            "controller",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "proof",
            "--desired-ready",
            "1",
            "--surge",
            "0",
            "--once",
            "--spawn-slots",
            "false",
        ],
    );
    assert!(second.success(), "{}", cmd_err(&dir));
    let state = Journal::open(dir.join("journal.db"))
        .unwrap()
        .load_state()
        .unwrap();
    let slot_state = state
        .slots
        .iter()
        .find(|item| item.slot_id.0 == "proof-1")
        .expect("slot");
    assert!(slot_state.executor_proven, "{slot_state:?}");
    assert!(slot_state.session_live, "{slot_state:?}");
    assert!(slot_state.ready_proof().is_ok(), "{slot_state:?}");
    assert!(
        !slot_state.registered,
        "JIT must not register without exec config: {slot_state:?}"
    );
    slot.kill().ok();
    let _ = slot.wait();
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn controller_claims_job_owned_before_spawning_worker() {
    let dir = scratch("owned");
    let mut journal = Journal::open(dir.join("journal.db")).unwrap();
    prime_named_ready(&mut journal, "own");
    drop(journal);
    std::fs::write(dir.join("daemon-exec.json"), b"not-json").unwrap();

    let status = run_runner(
        &dir,
        &[
            "controller",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "own",
            "--desired-ready",
            "1",
            "--surge",
            "0",
            "--once",
            "--spawn-slots",
            "false",
        ],
    );
    assert!(status.success(), "{}", cmd_err(&dir));
    let state = Journal::open(dir.join("journal.db"))
        .unwrap()
        .load_state()
        .unwrap();
    assert_eq!(state.jobs.len(), 1, "{:?}", state.jobs);
    assert_eq!(state.jobs[0].job_id.0, "slot-1-worker");
    assert!(
        dir.join("owned").join("slot-1-worker.1").exists(),
        "JobOwned must create the ownership marker before StartJob"
    );
    if let Some(pid) = velnor_runner::node::cleanup::read_owned_pid(&dir, "slot-1-worker", 1) {
        assert!(
            velnor_runner::node::prove::pid_is_alive(pid),
            "worker must still be running (not exited on beat --once)"
        );
        kill_pid(pid);
    } else {
        panic!("StartJob must record the worker pid in the ownership marker");
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn job_once_without_ownership_fails() {
    let dir = scratch("job-noown");
    Journal::open(dir.join("journal.db")).unwrap();
    let output = Command::new(runner())
        .args([
            "job",
            "--state-dir",
            dir.to_str().unwrap(),
            "--job-id",
            "missing",
            "--generation",
            "1",
            "--once",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "unowned job must not run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn job_once_without_exec_persists_only_after_ownership() {
    let dir = scratch("job-own-once");
    let mut journal = Journal::open(dir.join("journal.db")).unwrap();
    prime_named_ready(&mut journal, "jobown");
    use velnor_model::JobId;
    journal
        .apply(Event::Assigned {
            slot_id: SlotId("jobown-1".into()),
            job_id: JobId("slot-1-worker".into()),
            generation: Generation::INITIAL,
        })
        .unwrap();
    journal
        .apply(Event::JobOwned {
            job_id: JobId("slot-1-worker".into()),
            slot_id: SlotId("jobown-1".into()),
            attempt: 1,
            generation: Generation::INITIAL,
            worker: "velnor-job@slot-1-worker".into(),
        })
        .unwrap();
    drop(journal);
    let output = Command::new(runner())
        .args([
            "job",
            "--state-dir",
            dir.to_str().unwrap(),
            "--job-id",
            "slot-1-worker",
            "--generation",
            "1",
            "--once",
            "--scope",
            "jobown",
            "--slot-index",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state = Journal::open(dir.join("journal.db"))
        .unwrap()
        .load_state()
        .unwrap();
    assert_eq!(state.jobs[0].phase, velnor_model::ActorPhase::Running);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn controller_sends_pending_completion_outbox() {
    let dir = scratch("outbox");
    let mut journal = Journal::open(dir.join("journal.db")).unwrap();
    prime_named_ready(&mut journal, "out");
    use velnor_control::journal::payload_checksum;
    use velnor_model::JobId;
    let job_id = JobId("slot-1-worker".into());
    journal
        .apply(Event::Assigned {
            slot_id: SlotId("out-1".into()),
            job_id: job_id.clone(),
            generation: Generation::INITIAL,
        })
        .unwrap();
    journal
        .apply(Event::JobOwned {
            job_id: job_id.clone(),
            slot_id: SlotId("out-1".into()),
            attempt: 1,
            generation: Generation::INITIAL,
            worker: "velnor-job@slot-1-worker".into(),
        })
        .unwrap();
    let payload = b"conclusion=success";
    journal
        .apply(Event::CompletionIntended {
            job_id,
            generation: Generation::INITIAL,
            payload_sha256: payload_checksum(payload),
        })
        .unwrap();
    drop(journal);

    let status = run_runner(
        &dir,
        &[
            "controller",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "out",
            "--desired-ready",
            "1",
            "--surge",
            "0",
            "--once",
            "--spawn-slots",
            "false",
        ],
    );
    assert!(status.success(), "{}", cmd_err(&dir));
    assert!(
        dir.join("outbox").join("slot-1-worker.1").exists(),
        "SendCompletion must write the outbox payload"
    );
    let pending = Journal::open(dir.join("journal.db"))
        .unwrap()
        .pending_outbox()
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].send_started);
    if let Some(pid) = velnor_runner::node::cleanup::read_owned_pid(&dir, "slot-1-worker", 1) {
        kill_pid(pid);
    }
    std::fs::remove_dir_all(dir).ok();
}

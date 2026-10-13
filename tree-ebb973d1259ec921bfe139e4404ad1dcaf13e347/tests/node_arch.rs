//! Node architecture: health vector, slot process isolation, guardian cycle.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use velnor_control::journal::{Event, Journal};
use velnor_model::{FleetHealthState, Generation, HealthDocument, SlotId};

use velnor_runner::node::slot::heartbeat_path;

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
    std::fs::write(
        path.join("execution.toml"),
        "[execution]\nbackend = \"docker\"\n",
    )
    .unwrap();
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
    for _ in 0..400 {
        if (1..=2).all(|index| heartbeat_path(&dir, index).is_file()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let controller = Command::new(runner())
        .args([
            "controller",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "iso",
            "--desired-ready",
            "2",
            "--surge",
            "1",
            "--once",
            "--spawn-slots",
            "false",
        ])
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert!(
        controller.status.success(),
        "controller stderr: {}",
        String::from_utf8_lossy(&controller.stderr)
    );
    let pids = Journal::open(dir.join("journal.db"))
        .unwrap()
        .load_state()
        .unwrap()
        .slots
        .iter()
        .filter(|slot| {
            (slot.slot_id.0 == "iso-1" || slot.slot_id.0 == "iso-2") && slot.pid.is_some()
        })
        .count();
    let live: Vec<bool> = children
        .iter_mut()
        .map(|child| child.try_wait().ok().flatten().is_none())
        .collect();
    assert_eq!(
        pids,
        2,
        "slot stderr: 1={:?} 2={:?} live={live:?} stdout=1={:?} 2={:?}",
        std::fs::read_to_string(dir.join("slot-1.err")),
        std::fs::read_to_string(dir.join("slot-2.err")),
        std::fs::read_to_string(dir.join("slot-1.out")),
        std::fs::read_to_string(dir.join("slot-2.out")),
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
fn scope_broker_manager_is_one_event_loop_and_idle_has_no_waiter_path() {
    let controller = include_str!("../src/node/controller.rs");
    let manager = controller
        .split("struct ScopeBrokerManager")
        .nth(1)
        .and_then(|source| source.split("async fn run_scope_broker_manager").next())
        .expect("scope broker manager source");
    assert!(manager.contains("ScopeBrokerSession"));
    assert!(manager.contains("for_each_concurrent(16"));
    assert!(
        !manager.contains("JoinHandle") && !manager.contains("tokio::spawn"),
        "scope broker manager must not create one polling task per slot"
    );
    let runner = include_str!("../src/runner.rs");
    assert!(runner.contains("struct ScopeBrokerSession"));
    assert!(
        !runner.contains("run_broker_manager(")
            || !runner.contains("pub(crate) async fn run_broker_manager"),
        "legacy per-slot broker manager entrypoint must be removed"
    );
    let job = include_str!("../src/node/job.rs");
    assert!(job.contains("run_transient_job"));
    assert!(
        !controller.contains(".arg(\"job\")") || controller.contains("drain_broker_assignments"),
        "job command is only an assignment handoff path"
    );
}

#[test]
fn live_idle_controller_reports_zero_job_workers() {
    let dir = scratch("idle-worker-budget");
    let mut controller = Command::new(runner())
        .args([
            "controller",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "idle-budget",
            "--desired-ready",
            "2",
            "--surge",
            "0",
            "--spawn-slots",
            "false",
        ])
        .env_remove("GITHUB_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn idle controller");
    let metrics_path = dir.join("controller-metrics.json");
    for _ in 0..40 {
        if metrics_path.is_file() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        metrics_path.is_file(),
        "idle controller did not publish metrics"
    );
    std::thread::sleep(std::time::Duration::from_millis(250));
    let metrics: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&metrics_path).unwrap()).unwrap();
    assert_eq!(metrics["jobs"], 0);
    assert_eq!(metrics["waiter_processes"], 0);
    assert_eq!(metrics["job_processes"], 0);
    assert!(metrics["reconcile_cycles"].as_u64().unwrap_or(0) >= 1);
    controller.kill().ok();
    let _ = controller.wait();
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn one_scope_controller_failure_does_not_stop_another_scope() {
    let failed_scope = scratch("scope-failure");
    let surviving_scope = scratch("scope-survivor");
    let spawn_controller = |dir: &Path, scope: &str| {
        Command::new(runner())
            .args([
                "controller",
                "--state-dir",
                dir.to_str().unwrap(),
                "--scope",
                scope,
                "--desired-ready",
                "1",
                "--surge",
                "0",
                "--spawn-slots",
                "false",
            ])
            .env_remove("GITHUB_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn scope controller")
    };

    let mut failed = spawn_controller(&failed_scope, "failed");
    let mut survivor = spawn_controller(&surviving_scope, "survivor");
    let metrics_path = surviving_scope.join("controller-metrics.json");
    let mut initial_sequence = None;
    for _ in 0..100 {
        if let Ok(bytes) = std::fs::read(&metrics_path) {
            if let Some(sequence) = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|metrics| metrics["sequence"].as_u64())
            {
                initial_sequence = Some(sequence);
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let initial_sequence = initial_sequence.expect("surviving scope did not publish metrics");

    failed.kill().expect("kill failed scope controller");
    let _ = failed.wait();

    let mut advanced = false;
    for _ in 0..260 {
        assert!(
            survivor.try_wait().unwrap().is_none(),
            "unrelated scope controller exited after sibling failure"
        );
        if let Ok(bytes) = std::fs::read(&metrics_path) {
            let sequence = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|metrics| metrics["sequence"].as_u64());
            if sequence.is_some_and(|sequence| sequence > initial_sequence) {
                advanced = true;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        advanced,
        "surviving scope stopped publishing control cycles after sibling failure"
    );

    survivor.kill().ok();
    let _ = survivor.wait();
    std::fs::remove_dir_all(failed_scope).ok();
    std::fs::remove_dir_all(surviving_scope).ok();
}

#[test]
fn packaged_units_have_no_controller_partof_to_workers() {
    let controller = include_str!("../debian/velnor-controller@.service");
    let slot = include_str!("../debian/velnor-slot@.service");
    assert!(!controller.lines().any(|line| line.starts_with("PartOf=")));
    assert!(!slot.lines().any(|line| line.starts_with("PartOf=")));
    assert!(slot.contains("velnor-runner slot"));
    assert!(include_str!("../debian/velnor-guardian.service").contains("velnor-runner guardian"));
    assert!(!include_str!("../debian/velnor-guardian.service")
        .lines()
        .any(|line| line.starts_with("EnvironmentFile=") && line.contains("secrets.env")));
    assert!(include_str!("../debian/velnor-jobs.slice").contains("job-worker slice"));
    assert!(!include_str!("../debian/velnor-jobs.slice").contains("transitional Docker"));
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
    assert!(job_src.contains("run_transient_job"));
    assert!(!daemon_src_has_args_json());
    let postinst = include_str!("../debian/postinst");
    assert!(
        postinst.contains("NEVER") && postinst.contains("restart"),
        "apt configure must not restart the fleet"
    );
}

fn daemon_src_has_args_json() -> bool {
    include_str!("../src/runner.rs").contains("daemon-args.json")
}

fn matching_routing() -> velnor_runner::node::prove::RoutingFields {
    velnor_runner::node::prove::RoutingFields {
        group: "velnor".into(),
        selected_repositories: vec!["tailrocks/velnor".into()],
        labels: vec!["velnor".into()],
        trust_scope: "trusted".into(),
    }
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
        .env_remove("GITHUB_TOKEN")
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
    assert!(
        state.slots.iter().all(|slot| !slot.executor_proven),
        "docker.sock is not executor proof: {:?}",
        state.slots
    );
    assert!(state.slots.iter().all(|slot| !slot.registered));
    assert!(state.jobs.is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn controller_rejects_boolean_routing_stamp() {
    let dir = scratch("bool-route");
    std::fs::write(
        dir.join("routing.json"),
        br#"{"valid":true,"group_valid":true}"#,
    )
    .unwrap();
    velnor_runner::node::prove::write_executor_ok(&dir).unwrap();
    let status = run_runner(
        &dir,
        &[
            "controller",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "boolroute",
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
    assert!(state.slots.iter().all(|slot| !slot.registered));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn controller_reconciles_routing_independently_of_scheduler() {
    let dir = scratch("route-recon");
    let policy = matching_routing();
    let mut evidence = matching_routing();
    evidence.selected_repositories = vec!["other/repo".into()];
    std::fs::write(
        dir.join("routing-policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("routing-evidence.json"),
        serde_json::to_vec(&evidence).unwrap(),
    )
    .unwrap();
    let status = run_runner(
        &dir,
        &[
            "controller",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "routerecon",
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
    let observed = velnor_runner::node::prove::observe_routing(&dir);
    assert!(!observed.valid, "{observed:?}");
    assert!(observed.group_valid);
    let state = Journal::open(dir.join("journal.db"))
        .unwrap()
        .load_state()
        .unwrap();
    assert!(!state.routing_valid, "{state:?}");
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
    let mut saw_heartbeat = false;
    for _ in 0..40 {
        if heartbeat_path(&dir, 1).is_file() {
            saw_heartbeat = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(saw_heartbeat, "slot heartbeat never landed");

    let fields = matching_routing();
    velnor_runner::node::prove::write_routing_document(&dir, fields.clone(), fields).unwrap();
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
fn controller_keeps_ready_when_exec_exists_without_assignment() {
    let dir = scratch("no-synth");
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
    assert!(state.jobs.is_empty(), "{:?}", state.jobs);
    let slot = state
        .slots
        .iter()
        .find(|item| item.slot_id.0 == "own-1")
        .expect("slot");
    assert_eq!(slot.phase, velnor_model::ActorPhase::Ready, "{slot:?}");
    assert!(!state.github_reachable, "{state:?}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn controller_does_not_assign_rest_queued_ids() {
    let dir = scratch("owned");
    let mut journal = Journal::open(dir.join("journal.db")).unwrap();
    prime_named_ready(&mut journal, "own");
    drop(journal);
    velnor_runner::node::assign::write(
        &dir,
        &velnor_runner::node::assign::Assignment {
            job_id: "424242".into(),
            slot_id: "own-1".into(),
        },
    )
    .unwrap();

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
    assert!(
        state.jobs.is_empty(),
        "REST queued ids must not become journal owners: {:?}",
        state.jobs
    );
    let slot = state
        .slots
        .iter()
        .find(|item| item.slot_id.0 == "own-1")
        .expect("slot");
    assert_eq!(slot.phase, velnor_model::ActorPhase::Ready, "{slot:?}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn controller_applies_dependency_false_without_github() {
    let dir = scratch("dep");
    let status = run_runner(
        &dir,
        &[
            "controller",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "dep",
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
    assert!(!state.github_reachable, "{state:?}");
    assert!(state.jobs.is_empty(), "{:?}", state.jobs);
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
            accepted_unix: 0,
        })
        .unwrap();
    let payload = b"conclusion=success";
    journal
        .apply(Event::CompletionIntended {
            job_id: job_id.clone(),
            generation: Generation::INITIAL,
            payload_sha256: payload_checksum(payload),
        })
        .unwrap();
    velnor_runner::node::cleanup::write_outbox(&dir, &job_id.0, 1, payload).unwrap();
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
    let outbox = dir.join("outbox").join("slot-1-worker.1");
    assert_eq!(
        std::fs::read(&outbox).unwrap(),
        payload,
        "controller must not replace the durable payload with a checksum"
    );
    let pending = Journal::open(dir.join("journal.db"))
        .unwrap()
        .pending_outbox()
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        !pending[0].send_started,
        "send-started is only after an actual GitHub send, not outbox observation"
    );
    if let Some(pid) = velnor_runner::node::cleanup::read_owned_pid(&dir, "slot-1-worker", 1) {
        kill_pid(pid);
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn controller_restart_reclaims_active_handoff_with_explicit_completion() {
    let dir = scratch("restart-handoff");
    let mut journal = Journal::open(dir.join("journal.db")).unwrap();
    prime_named_ready(&mut journal, "restart");
    drop(journal);

    // The worker needs the same execution envelope as a controller restart.
    // Its config directory is deliberately absent: the test exercises the
    // restart/recovery contract, not broker or Docker execution.
    std::fs::write(
        dir.join("daemon-exec.json"),
        serde_json::to_vec(&serde_json::json!({
            "url": "https://broker.example",
            "name": "restart",
            "labels": ["velnor"],
            "target_mvp_labels": false,
            "target_mvp_arm_label": false,
            "replace": false,
            "dry_run_registration": false,
            "slots": 1,
            "once": false,
            "complete_noop": false,
            "execute_scripts": false,
            "dry_run_jobs": false,
            "docker_image": "img",
            "job_cpus": "",
            "job_memory": "",
            "trust_scope": "trusted",
            "emergency_reserve_bytes": 0,
            "job_peak_bytes": 0,
            "node_action_image": "img",
            "skip_preflight": false,
            "require_docker_socket": false
        }))
        .unwrap(),
    )
    .unwrap();

    let nonce = "restart-assignment";
    let handoff_path = velnor_runner::node::handoff::path(&dir, nonce);
    let handoff = velnor_runner::node::handoff::AssignmentHandoff::new(
        "restart-1".into(),
        Generation::INITIAL,
        nonce.into(),
        "session-before-restart".into(),
        "https://broker.example".into(),
        1,
        dir.join("missing-runner-config"),
        velnor_runner::protocol::TaskAgentMessage {
            message_id: 1,
            message_type: velnor_runner::protocol::RUNNER_JOB_REQUEST.into(),
            body: "{\"runner_request_id\":\"restart-request\"}".into(),
            iv_base64: None,
        },
    );
    velnor_runner::node::handoff::write_atomic(&handoff_path, &handoff).unwrap();

    let status = run_runner(
        &dir,
        &[
            "controller",
            "--state-dir",
            dir.to_str().unwrap(),
            "--scope",
            "restart",
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

    let done_path = velnor_runner::node::handoff::completion_path(&dir, nonce);
    let mut completion = None;
    for _ in 0..200 {
        if let Ok(record) =
            velnor_runner::node::handoff::read_completion(&done_path, nonce, Generation::INITIAL)
        {
            completion = Some(record);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let completion = completion.expect("restarted controller did not resolve handoff");
    assert_eq!(
        completion.status,
        velnor_runner::node::handoff::CompletionStatus::Failed
    );
    assert!(
        !handoff_path.exists(),
        "restart must consume the durable handoff envelope"
    );
    std::fs::remove_dir_all(dir).ok();
}

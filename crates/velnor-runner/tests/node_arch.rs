//! Node architecture: health vector, slot process isolation, guardian cycle.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use velnor_control::journal::{Event, Journal};
use velnor_model::{FleetHealthState, Generation, HealthDocument, SlotId};

use velnor_runner::node::slot::{heartbeat_path, SlotHeartbeat};

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
        Event::DesiredCapacity { ready: 2 },
    ] {
        journal.apply(event).unwrap();
    }
    for index in 1..=2 {
        let slot_id = SlotId(format!("iso-{index}"));
        for event in [
            Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation: g,
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
                "--generation",
                "1",
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
    let state = Journal::open(dir.join("journal.db"))
        .unwrap()
        .load_state()
        .unwrap();
    assert_eq!(
        state.slots.len(),
        2,
        "configured capacity must not create slot 3"
    );
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
fn packaged_units_have_no_controller_partof_to_workers() {
    let controller = include_str!("../debian/velnor-controller@.service");
    let slot = include_str!("../debian/velnor-slot@.service");
    let job = include_str!("../debian/velnor-job@.service");
    assert!(!controller.lines().any(|line| line.starts_with("PartOf=")));
    assert!(!slot.lines().any(|line| line.starts_with("PartOf=")));
    assert!(!job.lines().any(|line| line.starts_with("PartOf=")));
    assert!(slot.contains("velnor-runner slot"));
    assert!(slot.contains("--generation 1"));
    assert!(job.contains("KillMode=control-group"));
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
    assert!(
        job_src.contains("run_daemon_slot"),
        "job process is the transitional executor"
    );
    assert!(!daemon_src_has_args_json());
    assert!(
        !job.contains("--once"),
        "packaged job unit must not pass --once: {job}"
    );
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
        Event::DesiredCapacity { ready: 1 },
    ] {
        journal.apply(event).unwrap();
    }
    let slot_id = SlotId(format!("{scope}-1"));
    for event in [
        Event::PermitReserved {
            slot_id: slot_id.clone(),
            generation: g,
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

#[test]
fn direct_controller_capacity_is_exact_for_zero_one_and_four() {
    for configured in [0_u32, 1, 4] {
        let dir = scratch(&format!("capacity-{configured}"));
        let status = run_runner(
            &dir,
            &[
                "controller",
                "--state-dir",
                dir.to_str().unwrap(),
                "--scope",
                "capacity",
                "--desired-ready",
                &configured.to_string(),
                "--once",
                "--spawn-slots",
                "false",
            ],
        );
        assert!(status.success(), "N={configured}: {}", cmd_err(&dir));

        let state = Journal::open(dir.join("journal.db"))
            .unwrap()
            .load_state()
            .unwrap();
        let health = state.health();
        let slot_count = state.slots.len() as u32;
        let permit_count = state.slots.iter().filter(|slot| slot.permit_held).count() as u32;

        assert_eq!(slot_count, configured, "N={configured}: {state:?}");
        assert_eq!(permit_count, configured, "N={configured}: {state:?}");
        assert_eq!(
            health.desired_ready_slots, configured,
            "N={configured}: {health:?}"
        );
        assert_eq!(
            health.capacity_permits, configured,
            "N={configured}: {health:?}"
        );
        assert_eq!(health.actual_ready_slots, 0, "N={configured}: {health:?}");
        assert_eq!(health.registered_slots, 0, "N={configured}: {health:?}");
        assert_eq!(health.executor_ready_slots, 0, "N={configured}: {health:?}");
        assert!(state.jobs.is_empty(), "N={configured}: {state:?}");
        assert!(state.outbox.is_empty(), "N={configured}: {state:?}");

        std::fs::remove_dir_all(dir).ok();
    }
}

#[test]
fn contaminated_capacity_fails_closed_across_controller_restart_reconcile() {
    let dir = scratch("legacy-restart-reconcile");
    let mut journal = Journal::open(dir.join("journal.db")).unwrap();
    assert!(
        !journal
            .apply(Event::DesiredCapacity { ready: 2 })
            .unwrap()
            .rejected
    );
    for index in 1..=3 {
        assert!(
            !journal
                .apply(Event::PermitReserved {
                    slot_id: SlotId(format!("legacy-{index}")),
                    generation: Generation::INITIAL,
                })
                .unwrap()
                .rejected
        );
    }
    drop(journal);

    for restart in 1..=2 {
        let status = run_runner(
            &dir,
            &[
                "controller",
                "--state-dir",
                dir.to_str().unwrap(),
                "--scope",
                "legacy",
                "--desired-ready",
                "2",
                "--once",
            ],
        );
        assert!(!status.success(), "restart {restart} must fail closed");
        assert!(cmd_err(&dir).contains("journal.capacity.invalid"));
        let reopened = Journal::open(dir.join("journal.db")).unwrap();
        let state = reopened.load_state().unwrap();
        assert!(state.capacity_invalid);
        assert_eq!(state.slots.len(), 3);
        assert_eq!(state.advertised_capacity(), 0);
        assert_eq!(state.health().state, FleetHealthState::NotReady);
        assert_no_owned_slot_processes(&dir, "legacy");
    }
    std::fs::remove_dir_all(dir).ok();
}

fn cmd_err(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("cmd.err")).unwrap_or_default()
}

fn assert_no_owned_slot_processes(dir: &Path, scope: &str) {
    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .expect("inspect test-owned processes");
    let dir_needle = dir.to_str().unwrap();
    let scope_needle = format!("--scope {scope}");
    let process_listing = String::from_utf8_lossy(&output.stdout);
    let owned = process_listing
        .lines()
        .filter(|line| line.contains(" slot "))
        .filter(|line| line.contains(dir_needle) && line.contains(&scope_needle))
        .collect::<Vec<_>>();
    assert!(
        owned.is_empty(),
        "unexpected owned slot processes: {owned:?}"
    );
}

struct TestChild(Option<std::process::Child>);

impl TestChild {
    fn new(child: std::process::Child) -> Self {
        Self(Some(child))
    }

    fn as_mut(&mut self) -> &mut std::process::Child {
        self.0.as_mut().expect("test child still owned")
    }

    fn id(&self) -> u32 {
        self.0.as_ref().expect("test child still owned").id()
    }
}

impl Drop for TestChild {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

fn process_snapshot(pid: u32) -> String {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "state=,command="])
        .output()
        .expect("inspect test-owned process");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn process_is_owned_and_live(pid: u32, dir: &Path, scope: &str) -> bool {
    let snapshot = process_snapshot(pid);
    let mut fields = snapshot.splitn(2, char::is_whitespace);
    let state = fields.next().unwrap_or_default();
    let command = fields.next().unwrap_or_default();
    !snapshot.is_empty()
        && !state.contains('Z')
        && command.contains(" slot ")
        && command.contains(dir.to_str().unwrap())
        && command.contains(&format!("--scope {scope}"))
}

fn controller_is_owned_and_live(pid: u32, dir: &Path, scope: &str) -> bool {
    let snapshot = process_snapshot(pid);
    let mut fields = snapshot.splitn(2, char::is_whitespace);
    let state = fields.next().unwrap_or_default();
    let command = fields.next().unwrap_or_default();
    !snapshot.is_empty()
        && !state.contains('Z')
        && command.contains(" controller ")
        && command.contains(dir.to_str().unwrap())
        && command.contains(&format!("--scope {scope}"))
}

struct SupervisedProcessGuard {
    controller: Option<std::process::Child>,
    dir: PathBuf,
    scope: String,
    slot_pids: Vec<u32>,
}

impl SupervisedProcessGuard {
    fn new(controller: std::process::Child, dir: &Path, scope: &str) -> Self {
        Self {
            controller: Some(controller),
            dir: dir.to_owned(),
            scope: scope.to_owned(),
            slot_pids: Vec::new(),
        }
    }

    fn record_slot_pids(&mut self, pids: &[u32]) {
        self.slot_pids.extend_from_slice(pids);
        self.slot_pids.sort_unstable();
        self.slot_pids.dedup();
    }

    fn controller_is_running(&mut self) -> bool {
        self.controller
            .as_mut()
            .is_some_and(|child| child.try_wait().expect("inspect controller").is_none())
    }

    fn discover_slot_pids(&mut self) {
        if let Ok(journal) = Journal::open(self.dir.join("journal.db"))
            && let Ok(state) = journal.load_state()
        {
            let pids = state
                .slots
                .iter()
                .filter_map(|slot| slot.pid)
                .collect::<Vec<_>>();
            self.record_slot_pids(&pids);
        }
    }

    fn cleanup(&mut self) {
        if let Some(mut controller) = self.controller.take() {
            if controller
                .try_wait()
                .expect("inspect controller during cleanup")
                .is_none()
            {
                let _ = controller.kill();
            }
            let _ = controller.wait();
        }

        self.discover_slot_pids();
        for pid in self.slot_pids.iter().copied() {
            terminate_test_process(pid, &self.dir, &self.scope);
        }
    }
}

impl Drop for SupervisedProcessGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn wait_for_supervised_slots(guard: &mut SupervisedProcessGuard, expected: usize) -> Vec<u32> {
    let dir = guard.dir.clone();
    let scope = guard.scope.clone();
    for _ in 0..100 {
        assert!(
            guard.controller_is_running(),
            "supervised controller exited before N={expected} slots: {}",
            cmd_err(&dir)
        );
        if let Ok(state) =
            Journal::open(dir.join("journal.db")).and_then(|journal| journal.load_state())
        {
            let pids = state
                .slots
                .iter()
                .filter_map(|slot| slot.pid)
                .collect::<Vec<_>>();
            guard.record_slot_pids(&pids);
            if state.slots.len() == expected
                && pids.len() == expected
                && pids
                    .iter()
                    .all(|pid| process_is_owned_and_live(*pid, &dir, &scope))
                && (1..=expected).all(|index| heartbeat_path(&dir, index).is_file())
            {
                return pids;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!(
        "timed out waiting for N={expected} supervised slots: {}",
        cmd_err(&dir)
    );
}

fn wait_for_supervised_slots_with_timeout(
    guard: &mut SupervisedProcessGuard,
    expected: usize,
    timeout: std::time::Duration,
) -> Result<Vec<u32>, String> {
    let dir = guard.dir.clone();
    let scope = guard.scope.clone();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if !guard.controller_is_running() {
            return Err(format!(
                "supervised controller exited before N={expected} slots: {}",
                cmd_err(&dir)
            ));
        }
        if let Ok(state) =
            Journal::open(dir.join("journal.db")).and_then(|journal| journal.load_state())
        {
            let pids = state
                .slots
                .iter()
                .filter_map(|slot| slot.pid)
                .collect::<Vec<_>>();
            guard.record_slot_pids(&pids);
            if state.slots.len() == expected
                && pids.len() == expected
                && pids
                    .iter()
                    .all(|pid| process_is_owned_and_live(*pid, &dir, &scope))
            {
                return Ok(pids);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for N={expected} supervised slots: {}",
                cmd_err(&dir)
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn terminate_test_process(pid: u32, dir: &Path, scope: &str) {
    if !process_is_owned_and_live(pid, dir, scope) {
        return;
    }
    #[cfg(unix)]
    {
        // SAFETY: command-line identity was checked immediately above and the
        // PID came from this test's journal.
        let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    }
    for _ in 0..40 {
        if !process_is_owned_and_live(pid, dir, scope) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    #[cfg(unix)]
    {
        // Re-prove ownership before the bounded escalation.
        if process_is_owned_and_live(pid, dir, scope) {
            let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

#[test]
fn supervised_controller_capacity_is_exact_for_one_and_four() {
    for configured in [1_usize, 4] {
        let dir = scratch(&format!("supervised-capacity-{configured}"));
        let scope = format!("supervised-{configured}");
        let controller = Command::new(runner())
            .args([
                "controller",
                "--state-dir",
                dir.to_str().unwrap(),
                "--scope",
                &scope,
                "--desired-ready",
                &configured.to_string(),
            ])
            .env_remove("GITHUB_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                std::fs::File::create(dir.join("controller.out")).unwrap(),
            ))
            .stderr(Stdio::from(
                std::fs::File::create(dir.join("controller.err")).unwrap(),
            ))
            .spawn()
            .unwrap();
        let mut processes = SupervisedProcessGuard::new(controller, &dir, &scope);

        let pids = wait_for_supervised_slots(&mut processes, configured);
        assert_eq!(pids.len(), configured);
        assert_eq!(
            pids.iter().collect::<std::collections::HashSet<_>>().len(),
            configured
        );

        let state = Journal::open(dir.join("journal.db"))
            .unwrap()
            .load_state()
            .unwrap();
        let health = velnor_runner::node::health::fetch(&dir).unwrap();
        assert_eq!(state.slots.len(), configured);
        assert_eq!(
            state.slots.iter().filter(|slot| slot.permit_held).count(),
            configured
        );
        assert_eq!(
            state.slots.iter().filter(|slot| slot.pid.is_some()).count(),
            configured
        );
        assert_eq!(health.desired_ready_slots, configured as u32);
        assert_eq!(health.capacity_permits, configured as u32);
        assert_eq!(health.actual_ready_slots, 0);
        assert_ne!(health.state, FleetHealthState::Ready);
        assert!(state.jobs.is_empty());
        assert!(state.outbox.is_empty());
        assert!(
            !dir.join("advertised-capacity").exists(),
            "unproven supervised slots must not advertise capacity"
        );
        assert!(dir.join("execution.toml").is_file());
        assert!(!heartbeat_path(&dir, configured + 1).exists());
        assert!(!dir
            .join(format!(".slot-{}.heartbeat", configured + 1))
            .exists());

        processes.cleanup();
        for pid in pids {
            assert!(!process_is_owned_and_live(pid, &dir, &scope));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn supervised_process_guard_cleans_up_after_fault_injected_panic() {
    let dir = scratch("supervised-cleanup-panic");
    let scope = "supervised-cleanup-panic";
    let owned_pids = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_pids = std::sync::Arc::clone(&owned_pids);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let controller = Command::new(runner())
            .args([
                "controller",
                "--state-dir",
                dir.to_str().unwrap(),
                "--scope",
                scope,
                "--desired-ready",
                "1",
            ])
            .env_remove("GITHUB_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                std::fs::File::create(dir.join("controller.out")).unwrap(),
            ))
            .stderr(Stdio::from(
                std::fs::File::create(dir.join("controller.err")).unwrap(),
            ))
            .spawn()
            .unwrap();
        let mut processes = SupervisedProcessGuard::new(controller, &dir, scope);
        let pids = wait_for_supervised_slots(&mut processes, 1);
        *observed_pids.lock().unwrap() = pids;

        panic!("intentional cleanup fault injection");
    }));

    assert!(result.is_err(), "fault injection did not panic");
    let pids = owned_pids.lock().unwrap().clone();
    assert_eq!(pids.len(), 1, "fault injection did not observe one slot");
    for pid in pids {
        assert!(
            !process_is_owned_and_live(pid, &dir, scope),
            "RAII cleanup left owned process {pid} alive"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn supervised_process_guard_cleans_up_after_fault_injected_timeout() {
    let dir = scratch("supervised-cleanup-timeout");
    let scope = "supervised-cleanup-timeout";
    let (controller_pid, slot_pids) = {
        let controller = Command::new(runner())
            .args([
                "controller",
                "--state-dir",
                dir.to_str().unwrap(),
                "--scope",
                scope,
                "--desired-ready",
                "1",
            ])
            .env_remove("GITHUB_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                std::fs::File::create(dir.join("controller.out")).unwrap(),
            ))
            .stderr(Stdio::from(
                std::fs::File::create(dir.join("controller.err")).unwrap(),
            ))
            .spawn()
            .unwrap();
        let mut processes = SupervisedProcessGuard::new(controller, &dir, scope);
        let controller_pid = processes.controller.as_ref().unwrap().id();
        let slot_pids = wait_for_supervised_slots(&mut processes, 1);
        assert!(controller_is_owned_and_live(controller_pid, &dir, scope));
        assert!(
            slot_pids
                .iter()
                .all(|pid| process_is_owned_and_live(*pid, &dir, scope)),
            "supervised slot stopped before timeout injection"
        );

        let timeout = wait_for_supervised_slots_with_timeout(
            &mut processes,
            2,
            std::time::Duration::from_millis(150),
        );
        assert!(
            timeout.is_err(),
            "impossible supervised capacity did not time out"
        );

        processes.cleanup();
        (controller_pid, slot_pids)
    };

    assert!(!controller_is_owned_and_live(controller_pid, &dir, scope));
    for pid in slot_pids {
        assert!(!process_is_owned_and_live(pid, &dir, scope));
    }
    std::fs::remove_dir_all(dir).unwrap();
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
            "--generation",
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
fn observe_slot_session_requires_fresh_generation_bound_heartbeat() {
    let dir = scratch("session-proof");
    let mut slot = TestChild::new(
        Command::new(runner())
            .args([
                "slot",
                "--state-dir",
                dir.to_str().unwrap(),
                "--scope",
                "session-proof",
                "--slot-index",
                "1",
                "--generation",
                "1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn slot"),
    );
    let slot_id = SlotId("session-proof-1".to_owned());
    for _ in 0..250 {
        if heartbeat_path(&dir, 1).is_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        heartbeat_path(&dir, 1).is_file(),
        "slot heartbeat never landed"
    );

    let pid = slot.id();
    assert!(velnor_runner::node::prove::observe_slot_session(
        Some(slot.as_mut()),
        Some(pid),
        &dir,
        &slot_id,
        Generation(1),
    ));
    assert!(!velnor_runner::node::prove::slot_heartbeat_is_fresh(
        &dir,
        &slot_id,
        Generation(1),
        Duration::ZERO,
    ));

    std::fs::write(heartbeat_path(&dir, 1), b"not-json").unwrap();
    assert!(!velnor_runner::node::prove::observe_slot_session(
        Some(slot.as_mut()),
        Some(pid),
        &dir,
        &slot_id,
        Generation(1),
    ));

    let mismatched = SlotHeartbeat {
        generation: 2,
        pid,
        sequence: 1,
    };
    std::fs::write(
        heartbeat_path(&dir, 1),
        serde_json::to_vec(&mismatched).unwrap(),
    )
    .unwrap();
    assert!(!velnor_runner::node::prove::observe_slot_session(
        Some(slot.as_mut()),
        Some(pid),
        &dir,
        &slot_id,
        Generation(1),
    ));

    drop(slot);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn observe_slot_session_rejects_inert_live_pid() {
    let dir = scratch("inert-session-proof");
    let mut inert = TestChild::new(
        Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn inert child"),
    );
    let pid = inert.id();
    let slot_id = SlotId("inert-session-proof-1".to_owned());
    let heartbeat = SlotHeartbeat {
        generation: 1,
        pid,
        sequence: 1,
    };
    std::fs::write(
        heartbeat_path(&dir, 1),
        serde_json::to_vec(&heartbeat).unwrap(),
    )
    .unwrap();

    let observed = velnor_runner::node::prove::observe_slot_session(
        Some(inert.as_mut()),
        Some(pid),
        &dir,
        &slot_id,
        Generation(1),
    );
    drop(inert);
    assert!(!observed, "a live inert child is not a proven slot session");
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
            accepted_unix: 0,
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

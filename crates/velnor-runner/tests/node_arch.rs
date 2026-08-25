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
}

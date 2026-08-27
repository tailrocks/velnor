use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use velnor_control::journal::Journal;

fn unique_temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("velnor-{name}-{}-{nanos}", std::process::id()))
}

fn load_runner_name(config_dir: &Path) -> String {
    let bytes = fs::read(config_dir.join("runner.json")).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["settings"]["agent_name"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn daemon_dry_run_jit_config_cli_writes_slot_configs_and_exits() {
    for configured in [1_usize, 4] {
        let config_dir = unique_temp_dir(&format!("daemon-cli-{configured}"));
        let output = Command::new(env!("CARGO_BIN_EXE_velnor-runner"))
            .args([
                "daemon",
                "--url",
                "https://github.com/owner/repo",
                "--name",
                "velnor-ci",
                "--labels",
                "velnor,ubuntu-24.04",
                "--slots",
                &configured.to_string(),
                "--once",
                "--config-dir",
                config_dir.to_str().unwrap(),
                "--dry-run-jit-config",
            ])
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "N={configured}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout)
            .contains("Daemon JIT config dry run complete; skipped polling GitHub for jobs."));
        let slot_root = if configured == 1 {
            config_dir.clone()
        } else {
            config_dir.join("slots")
        };
        for index in 1..=configured {
            let slot_dir = if configured == 1 {
                slot_root.clone()
            } else {
                slot_root.join(format!("slot-{index}"))
            };
            let expected_name = if configured == 1 {
                "velnor-ci".to_owned()
            } else {
                format!("velnor-ci-slot-{index}")
            };
            assert_eq!(load_runner_name(&slot_dir), expected_name);
        }
        let state = Journal::open(config_dir.join("journal.db"))
            .unwrap()
            .load_state()
            .unwrap();
        let health = state.health();
        assert_eq!(state.slots.len(), configured, "N={configured}: {state:?}");
        assert_eq!(
            state.slots.iter().filter(|slot| slot.permit_held).count(),
            configured,
            "N={configured}: {state:?}"
        );
        assert_eq!(
            health.desired_ready_slots, configured as u32,
            "N={configured}: {health:?}"
        );
        assert_eq!(
            health.capacity_permits, configured as u32,
            "N={configured}: {health:?}"
        );
        assert_eq!(health.actual_ready_slots, 0, "N={configured}: {health:?}");
        assert_eq!(health.registered_slots, 0, "N={configured}: {health:?}");
        assert_eq!(health.executor_ready_slots, 0, "N={configured}: {health:?}");
        if configured == 1 {
            assert!(!config_dir.join("slots").exists());
        } else {
            assert!(!slot_root.join(format!("slot-{}", configured + 1)).exists());
        }
        fs::remove_dir_all(config_dir).unwrap();
    }
}

#[test]
fn daemon_zero_slots_fails_before_configuration_side_effects() {
    let config_dir = unique_temp_dir("daemon-zero-slots");
    let output = Command::new(env!("CARGO_BIN_EXE_velnor-runner"))
        .args([
            "daemon",
            "--url",
            "https://github.com/owner/repo",
            "--slots",
            "0",
            "--once",
            "--config-dir",
            config_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--slots must be greater than zero"));
    assert!(config_dir.is_dir(), "state directory creation is allowed");
    assert!(
        !config_dir.join("runner.json").exists() && !config_dir.join("slots").exists(),
        "zero-slot validation created runner configuration"
    );
    assert!(
        !config_dir.join("journal.db").exists(),
        "zero-slot validation created capacity state"
    );
}

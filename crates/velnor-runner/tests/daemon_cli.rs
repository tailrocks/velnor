use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

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
    let config_dir = unique_temp_dir("daemon-cli");
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
            "2",
            "--once",
            "--config-dir",
            config_dir.to_str().unwrap(),
            "--dry-run-jit-config",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("Daemon JIT config dry run complete; skipped polling GitHub for jobs."));
    assert_eq!(
        load_runner_name(&config_dir.join("slots").join("slot-1")),
        "velnor-ci-slot-1"
    );
    assert_eq!(
        load_runner_name(&config_dir.join("slots").join("slot-2")),
        "velnor-ci-slot-2"
    );

    fs::remove_dir_all(config_dir).unwrap();
}

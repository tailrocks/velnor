#![cfg(target_os = "macos")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "the binary-level diagnostics test reports its own evidence"
)]

use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_velnorctl")
}

fn scratch() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "velnorctl-diagnostics-integration-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("scratch directory");
    path
}

fn tar_member(archive: &PathBuf, name: &str) -> Vec<u8> {
    let output = Command::new("tar")
        .args(["-xOf"])
        .arg(archive)
        .arg(name)
        .output()
        .expect("spawn tar");
    assert!(output.status.success(), "tar {} failed", name);
    output.stdout
}

#[test]
fn bundle_writes_local_evidence_without_a_control_context() {
    let root = scratch();
    let config = root.join("config");
    let storage = root.join("storage");
    let archive = root.join("evidence.tar");
    fs::create_dir_all(&config).expect("config directory");
    fs::create_dir_all(&storage).expect("storage directory");
    fs::write(
        config.join("execution.toml"),
        "[execution]\nbackend = \"microvm\"\n",
    )
    .expect("execution config");

    let output = Command::new(bin())
        .args([
            "--context",
            "missing-local-context",
            "--instance",
            "diagnostics-test",
            "--output",
            "json",
            "diagnostics",
            "bundle",
            "--archive",
        ])
        .arg(&archive)
        .env("VELNOR_CONFIG_DIR", &config)
        .env("VELNOR_STORAGE_ROOT", &storage)
        .env("GITHUB_TOKEN", "diagnostic-test-token")
        .env("GH_TOKEN", "diagnostic-gh-token")
        .output()
        .expect("spawn velnorctl");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let summary: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("diagnostics summary JSON");
    assert_eq!(summary["schemaVersion"], 1);
    assert_eq!(summary["members"], 5);
    assert_eq!(summary["commandSuccess"], false);

    let metadata: serde_json::Value =
        serde_json::from_slice(&tar_member(&archive, "metadata.json")).expect("metadata JSON");
    assert_eq!(
        metadata["configDir"],
        config
            .join("hosts/diagnostics-test")
            .to_string_lossy()
            .as_ref()
    );

    let listing = Command::new("tar")
        .args(["-tf"])
        .arg(&archive)
        .output()
        .expect("spawn tar");
    assert!(listing.status.success(), "tar listing failed");
    let names = String::from_utf8_lossy(&listing.stdout);
    for name in [
        "manifest.json",
        "metadata.json",
        "commands/status.json",
        "commands/preflight.json",
        "commands/host-status.json",
    ] {
        assert!(names.lines().any(|entry| entry == name), "{name}: {names}");
    }

    let manifest: serde_json::Value =
        serde_json::from_slice(&tar_member(&archive, "manifest.json")).expect("manifest JSON");
    assert_eq!(manifest["schemaVersion"], 1);
    assert_eq!(manifest["commandSuccess"], false);
    assert!(manifest["members"]
        .as_array()
        .is_some_and(|members| { members.iter().all(|member| member["redactionVersion"] == 1) }));

    let archive_bytes = fs::read(&archive).expect("archive bytes");
    assert!(!archive_bytes
        .windows(b"diagnostic-test-token".len())
        .any(|window| window == b"diagnostic-test-token"));
    assert!(!archive_bytes
        .windows(b"diagnostic-gh-token".len())
        .any(|window| window == b"diagnostic-gh-token"));

    fs::remove_dir_all(root).ok();
}

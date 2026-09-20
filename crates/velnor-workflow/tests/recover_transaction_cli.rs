//! Exercise the transaction-recovery flag through the shipped binary and the
//! schema dispatcher, not through private S2 functions.

#![expect(
    clippy::expect_used,
    reason = "fixture setup and subprocess launch must fail loudly"
)]
#![expect(
    clippy::unwrap_used,
    reason = "fixture setup and subprocess assertions must fail loudly"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

fn temporary_target(schema: u8) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "velnor-recover-cli-{}-{}-schema-{schema}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join(".github-gen")).expect("create recovery fixture");
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        format!("schema = {schema}\n\n[generator]\nrepository = \"example/fixture\"\n"),
    )
    .expect("write recovery fixture config");
    root
}

fn run_recovery(target: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--recover-transaction",
            "--plain",
            target.to_str().expect("fixture path is UTF-8"),
        ])
        .output()
        .expect("run recovery command")
}

#[test]
fn recovery_flag_routes_schema2_target_without_providers_switch() {
    let target = temporary_target(2);
    let output = run_recovery(&target);
    assert!(
        !output.status.success(),
        "no journal must be an explicit error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no pending transaction journal"),
        "unexpected recovery response: {stderr}"
    );
    assert!(
        !stderr.contains("unexpected argument '--recover-transaction'"),
        "dispatcher must route the S2 flag before the legacy parser: {stderr}"
    );
    fs::remove_dir_all(target).expect("remove recovery fixture");
}

#[test]
fn recovery_flag_routes_legacy_target_to_schema_gate() {
    let target = temporary_target(1);
    let output = run_recovery(&target);
    assert!(!output.status.success(), "legacy target must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires a local schema-2 target") && stderr.contains("schema = 2"),
        "legacy target must fail at the schema gate: {stderr}"
    );
    assert!(
        !stderr.contains("unexpected argument '--recover-transaction'"),
        "legacy parser must not reject the S2-only flag first: {stderr}"
    );
    fs::remove_dir_all(target).expect("remove recovery fixture");
}

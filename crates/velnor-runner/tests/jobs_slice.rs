#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
//! Packaging tests for the unbounded job cgroup boundary: the workload
//! slice is identity-only, the retired quota drop-in is deleted and never
//! recreated, and removal deletes it only after inactive proofs.

use std::path::PathBuf;

fn temp_dir(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "velnor-jobs-slice-{label}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn jobs_slice_is_identity_only_with_no_ceiling() {
    let slice = include_str!("../debian/velnor-jobs.slice");

    // No quota drop-in pin and no ceiling of any kind. The retired
    // drop-in may only be named in comments, never pinned.
    assert!(!slice.contains("AssertPathExists="));
    assert!(
        slice
            .lines()
            .filter(|line| line.contains("10-host-cpu.conf"))
            .all(|line| line.trim_start().starts_with('#')),
        "the stale drop-in must never be pinned:\n{slice}"
    );
    for ceiling in [
        "CPUQuota=",
        "CPUWeight=",
        "IOWeight=",
        "MemoryHigh=",
        "MemoryMax=",
        "MemorySwapMax=",
        "TasksMax=",
    ] {
        assert!(
            !slice.lines().any(|line| line.starts_with(ceiling)),
            "velnor-jobs.slice must not pin {ceiling}:\n{slice}"
        );
    }
    assert!(slice.contains("job-worker slice"));
}

#[test]
fn postinst_removes_the_stale_quota_dropin_and_never_writes_one() {
    let postinst = include_str!("../debian/postinst");

    // The stale drop-in from older packages is deleted, never recreated.
    assert!(postinst.contains("remove_stale_jobs_cpu_quota_dropin"));
    assert!(postinst.contains("rm -f \"$JOBS_SLICE_DROPIN\""));
    assert!(postinst.contains("systemctl daemon-reload"));
    // After reload, every effective ceiling property must read infinity.
    for property in [
        "CPUQuotaPerSecUSec",
        "MemoryHigh",
        "MemoryMax",
        "MemorySwapMax",
        "TasksMax",
    ] {
        assert!(
            postinst.contains(&format!("--property={property}")),
            "postinst must prove the effective {property} ceiling is absent"
        );
    }
    assert!(postinst.contains("infinity"));
    assert!(postinst.contains("-eq 5"));

    // No quota is ever derived or written.
    assert!(!postinst.contains("write_host_scaled_jobs_cpu_quota"));
    assert!(!postinst.contains("CPUQuota=${cpu_quota}%"));
    assert!(!postinst.contains("cpu_quota=$((cpu_count * 95))"));
    assert!(!postinst.contains("getconf _NPROCESSORS_ONLN"));
    assert!(!postinst.contains("busctl get-property"));
    assert!(!postinst.contains("expected_cpu_quota_usec"));
}

#[test]
fn stale_quota_dropin_removal_executes_and_is_idempotent() {
    use std::process::Command;

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("remove_stale_jobs_cpu_quota_dropin()")
        .unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let function = &postinst[function_start..function_end];

    for label in ["present", "absent"] {
        let dir = temp_dir(label);
        let dropin = dir.join("10-host-cpu.conf");
        if label == "present" {
            std::fs::write(&dropin, "[Slice]\nCPUQuota=1520%\n").unwrap();
        }
        let script = format!(
            "set -eu\nfail() {{ echo \"$1\" >&2; exit 1; }}\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{function}\nremove_stale_jobs_cpu_quota_dropin\n"
        );
        let output = Command::new("sh")
            .arg("-c")
            .arg(script)
            .env("TEST_DROPIN_DIR", &dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "drop-in removal must execute (label={label}): {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!dropin.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn postrm_keeps_unit_cleanup_without_quota_logic() {
    let postrm = include_str!("../debian/postrm");

    // Generic fleet cleanup survives: enumerate, mask, disable, stop, prove.
    assert!(postrm.contains("require_package_transaction_lock"));
    assert!(postrm.contains("WORKER_UNIT_GLOB=velnor-job@*.service"));
    assert!(postrm.contains("systemctl mask --runtime \"$unit\""));
    assert!(!postrm.contains("systemctl unmask --runtime"));
    assert!(postrm.contains("systemctl stop \"$unit\""));
    assert!(postrm.contains("systemctl daemon-reload"));
    assert!(postrm.contains("verify_worker_units_inactive"));
    assert!(postrm.contains("verify_jobs_slice_inactive"));
    assert!(postrm.contains("--property=LoadState --value velnor-jobs.slice"));
    assert!(postrm.contains("--property=ActiveState --value velnor-jobs.slice"));

    // Removal deletes only the old generated file, preserving operator
    // drop-ins. It runs after both inactive proofs and before daemon-reload.
    assert!(postrm.contains("JOBS_SLICE_DROPIN=$JOBS_SLICE_DROPIN_DIR/10-host-cpu.conf"));
    assert!(postrm.contains("remove_stale_jobs_cpu_quota_dropin"));
    assert!(postrm.contains("rm -f \"$JOBS_SLICE_DROPIN\""));
    assert!(!postrm.contains("CPUQuota"));

    // Ordering: enumerate < mask < disable < stop < proofs < reload < proofs.
    let lifecycle = postrm.split("case \"$1\" in").nth(1).unwrap();
    let unit_enumeration = lifecycle
        .find("all_units=$(systemctl list-unit-files")
        .unwrap();
    let unit_mask = lifecycle
        .find("systemctl mask --runtime \"$unit\"")
        .unwrap();
    let unit_disable = lifecycle.find("systemctl disable \"$unit\"").unwrap();
    let worker_stop = lifecycle.find("systemctl stop \"$unit\"").unwrap();
    let worker_proofs: Vec<_> = lifecycle
        .match_indices("verify_worker_units_inactive")
        .map(|(offset, _)| offset)
        .collect();
    let slice_proofs: Vec<_> = lifecycle
        .match_indices("verify_jobs_slice_inactive")
        .map(|(offset, _)| offset)
        .collect();
    let daemon_reload = lifecycle.find("systemctl daemon-reload").unwrap();
    let remove_dropin = lifecycle
        .find("remove_stale_jobs_cpu_quota_dropin")
        .unwrap();
    assert_eq!(worker_proofs.len(), 2);
    assert_eq!(slice_proofs.len(), 2);
    assert!(unit_enumeration < unit_mask);
    assert!(unit_mask < unit_disable);
    assert!(unit_mask < worker_stop);
    assert!(worker_stop < worker_proofs[0]);
    assert!(worker_proofs[0] < daemon_reload);
    assert!(slice_proofs[0] < daemon_reload);
    assert!(worker_proofs[0] < remove_dropin);
    assert!(slice_proofs[0] < remove_dropin);
    assert!(remove_dropin < daemon_reload);
    assert!(daemon_reload < worker_proofs[1]);
    assert!(worker_proofs[1] < slice_proofs[1]);
    assert!(daemon_reload < slice_proofs[1]);
}

#[test]
fn postrm_removes_only_the_legacy_dropin_and_is_idempotent() {
    use std::process::Command;

    let postrm = include_str!("../debian/postrm");
    let function_start = postrm.find("remove_stale_jobs_cpu_quota_dropin()").unwrap();
    let function_end = postrm[function_start..]
        .find("\ncase \"$1\" in")
        .map(|offset| function_start + offset)
        .unwrap();
    let function = &postrm[function_start..function_end];
    let dir = temp_dir("postrm-dropin");
    let dropin = dir.join("10-host-cpu.conf");
    let operator_dropin = dir.join("99-operator.conf");
    std::fs::write(&dropin, "[Slice]\nCPUQuota=1520%\n").unwrap();
    std::fs::write(&operator_dropin, "[Slice]\nCPUWeight=200\n").unwrap();

    let script = format!(
        "set -eu\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{function}\nremove_stale_jobs_cpu_quota_dropin\nremove_stale_jobs_cpu_quota_dropin\n"
    );
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("TEST_DROPIN_DIR", &dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "postrm drop-in cleanup must execute: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!dropin.exists());
    assert!(operator_dropin.exists());
    assert!(dir.exists(), "operator drop-ins keep the directory intact");

    std::fs::remove_file(operator_dropin).unwrap();
    let script = format!(
        "set -eu\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{function}\nremove_stale_jobs_cpu_quota_dropin\n"
    );
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("TEST_DROPIN_DIR", &dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "postrm should remove its empty drop-in directory: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!dir.exists());
}

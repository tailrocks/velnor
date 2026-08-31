//! Packaging tests for the host-scaled job cgroup boundary.

use std::path::{Path, PathBuf};

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
fn jobs_slice_cpu_quota_is_host_scaled_and_fail_closed() {
    let slice = include_str!("../debian/velnor-jobs.slice");
    let postinst = include_str!("../debian/postinst");
    let postrm = include_str!("../debian/postrm");

    assert!(
        slice.contains("AssertPathExists=/etc/systemd/system/velnor-jobs.slice.d/10-host-cpu.conf")
    );
    assert!(!slice.lines().any(|line| line.starts_with("CPUQuota=")));
    assert!(postinst.contains("/usr/bin/getconf _NPROCESSORS_ONLN"));
    assert!(postinst.contains("cpu_quota=$((cpu_count * 95))"));
    assert!(postinst.contains("CPUQuota=${cpu_quota}%"));
    assert!(postinst.contains("mv -f \"$temporary\" \"$JOBS_SLICE_DROPIN\""));
    assert!(postinst.contains("could not detect a positive online CPU count"));
    assert!(postinst.contains("systemctl daemon-reload"));
    assert!(postinst.contains("systemctl cat velnor-jobs.slice"));
    assert!(postinst.contains("effective_cpu_quota_percent"));
    assert!(postinst.contains("expected '${cpu_quota}%'"));
    assert!(postinst
        .contains("|| fail \"systemd refused to reload the host-scaled velnor-jobs.slice quota\""));
    assert!(postinst.contains("systemctl show"));
    assert!(postinst.contains("CPUQuotaPerSecUSec"));
    assert!(postinst.contains("busctl get-property"));
    assert!(postinst.contains("/org/freedesktop/systemd1/unit/velnor_2djobs_2eslice"));
    assert!(postinst.contains("expected_cpu_quota_usec=$((cpu_quota * 10000))"));
    assert!(postinst.contains("expected '$expected_cpu_quota_usec'"));
    assert!(postrm.contains("--property=LoadState --value velnor-jobs.slice"));
    assert!(postrm.contains("--property=ActiveState --value velnor-jobs.slice"));
    assert!(!postrm.contains("inactive|failed"));
    assert!(postrm.contains("require_package_transaction_lock"));
    assert!(postrm.contains("WORKER_UNIT_GLOB=velnor-job@*.service"));
    assert!(postrm.contains("systemctl mask --runtime \"$unit\""));
    assert!(!postrm.contains("systemctl unmask --runtime"));
    assert!(postrm.contains("active_workers"));
    assert!(postrm.contains("activating"));
    assert!(!postrm.contains("systemctl mask --runtime \"velnor*.service\""));
    assert!(postrm.contains("systemctl stop \"$unit\""));
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
    let dropin_remove = lifecycle.find("rm -f").unwrap();
    let daemon_reload = lifecycle.find("systemctl daemon-reload").unwrap();
    assert_eq!(worker_proofs.len(), 2);
    assert_eq!(slice_proofs.len(), 2);
    assert!(unit_enumeration < unit_mask);
    assert!(unit_mask < unit_disable);
    assert!(unit_mask < worker_stop);
    assert!(worker_stop < worker_proofs[0]);
    assert!(worker_proofs[0] < dropin_remove);
    assert!(slice_proofs[0] < dropin_remove);
    assert!(dropin_remove < daemon_reload);
    assert!(daemon_reload < worker_proofs[1]);
    assert!(worker_proofs[1] < slice_proofs[1]);
    assert!(daemon_reload < slice_proofs[1]);
    assert!(postrm.contains("not provably inactive; keeping CPU quota"));
    assert!(postrm.contains("systemctl daemon-reload"));
}

#[test]
fn jobs_slice_quota_writer_executes_and_rejects_invalid_cpu_detection() {
    use std::process::Command;

    if !Path::new("/usr/bin/getconf").is_file() {
        return;
    }
    let postinst = include_str!("../debian/postinst");
    let function_start = postinst.find("write_host_scaled_jobs_cpu_quota()").unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let function = &postinst[function_start..function_end];

    let success_dir = temp_dir("success");
    let mut success_script = String::from(
        "set -eu\nfail() { echo \"$1\" >&2; exit 1; }\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n",
    );
    success_script.push_str(function);
    success_script.push_str("\nwrite_host_scaled_jobs_cpu_quota\n");
    let success = Command::new("sh")
        .arg("-c")
        .arg(success_script)
        .env("TEST_DROPIN_DIR", &success_dir)
        .output()
        .unwrap();
    assert!(
        success.status.success(),
        "quota writer must execute: {}",
        String::from_utf8_lossy(&success.stderr)
    );
    let cpu_count = Command::new("/usr/bin/getconf")
        .arg("_NPROCESSORS_ONLN")
        .output()
        .unwrap();
    let cpu_count = String::from_utf8(cpu_count.stdout)
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(success_dir.join("10-host-cpu.conf"))
            .unwrap()
            .trim(),
        format!(
            "[Slice]\n# Generated from the host online CPU count during package configuration.\n# CPUQuota is 95% of one CPU per online logical CPU.\nCPUQuota={}%",
            cpu_count * 95
        )
    );

    let invalid_function = function.replace(
        "cpu_count=$(/usr/bin/getconf _NPROCESSORS_ONLN 2>/dev/null || true)",
        "cpu_count=invalid",
    );
    let failure_dir = temp_dir("failure");
    let mut failure_script = String::from(
        "set -eu\nfail() { echo \"$1\" >&2; exit 1; }\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n",
    );
    failure_script.push_str(&invalid_function);
    failure_script.push_str("\nwrite_host_scaled_jobs_cpu_quota\n");
    let failure = Command::new("sh")
        .arg("-c")
        .arg(failure_script)
        .env("TEST_DROPIN_DIR", &failure_dir)
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(String::from_utf8_lossy(&failure.stderr)
        .contains("could not detect a positive online CPU count"));
    assert!(!failure_dir.join("10-host-cpu.conf").exists());
    let _ = std::fs::remove_dir_all(success_dir);
    let _ = std::fs::remove_dir_all(failure_dir);
}

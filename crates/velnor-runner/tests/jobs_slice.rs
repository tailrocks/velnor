#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
//! Packaging tests for the unbounded job cgroup boundary and exact cleanup of
//! the retired Velnor-generated CPU quota drop-in.

use std::path::PathBuf;

const LEGACY_PREFIX: &str = "[Slice]\n# Generated from the host online CPU count during package configuration.\n# CPUQuota is 95% of one CPU per online logical CPU.\n";
const OPERATOR_DROPIN: &[u8] = b"[Slice]\nCPUWeight=200\n# Operator policy\n";

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

fn legacy_dropin(quota_percent: &str) -> String {
    format!("{LEGACY_PREFIX}CPUQuota={quota_percent}%\n")
}

fn dropin_cleanup_functions<'a>(source: &'a str, end_marker: &str) -> &'a str {
    let start = source.find("is_legacy_jobs_cpu_quota_dropin()").unwrap();
    let end = source[start..].find(end_marker).unwrap() + start;
    &source[start..end]
}

fn isolated_cleanup_script(source: &str, end_marker: &str) -> String {
    format!(
        "set -eu\nfail() {{ echo \"$1\" >&2; exit 1; }}\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{}\nremove_stale_jobs_cpu_quota_dropin\n",
        dropin_cleanup_functions(source, end_marker)
    )
}

fn run_cleanup(source: &str, end_marker: &str, dir: &std::path::Path) -> std::process::Output {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(isolated_cleanup_script(source, end_marker))
        .env("TEST_DROPIN_DIR", dir)
        .output()
        .unwrap()
}

fn isolated_postrm(postrm: &str) -> String {
    postrm
        .replace(
            "JOBS_SLICE_DROPIN_DIR=/etc/systemd/system/velnor-jobs.slice.d",
            "JOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"",
        )
        .replace(
            "if [ -d /run/systemd/system ]; then",
            "if [ -d \"$TEST_SYSTEMD_DIR\" ]; then",
        )
        .replace(
            "    require_package_transaction_lock\n",
            "    : # skip host lock proof in the isolated test\n",
        )
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
fn postinst_configure_removes_exact_legacy_dropin_and_never_writes_one() {
    let postinst = include_str!("../debian/postinst");

    // Both fresh install and upgrade reach this configure branch.
    assert!(postinst.contains("case \"$1\" in\n  configure)"));
    assert!(postinst.contains("remove_stale_jobs_cpu_quota_dropin"));
    assert!(postinst.contains("CPUQuotaPerSecUSec"));
    assert!(postinst.contains("MemoryMax"));
    assert!(postinst.contains("MemoryHigh"));
    assert!(postinst.contains("infinity"));

    let cleanup_call = postinst
        .find("\n    remove_stale_jobs_cpu_quota_dropin\n")
        .unwrap();
    let reload = postinst[cleanup_call..]
        .find("systemctl daemon-reload")
        .map(|offset| cleanup_call + offset)
        .unwrap();
    assert!(cleanup_call < reload);

    // No quota is ever derived or written by current packages.
    assert!(!postinst.contains("write_host_scaled_jobs_cpu_quota"));
    assert!(!postinst.contains("CPUQuota=${cpu_quota}%"));
    assert!(!postinst.contains("cpu_quota=$((cpu_count * 95))"));
    assert!(!postinst.contains("getconf _NPROCESSORS_ONLN"));
    assert!(!postinst.contains("busctl get-property"));
    assert!(!postinst.contains("expected_cpu_quota_usec"));
}

#[test]
fn maintainer_cleanup_removes_only_exact_legacy_file_and_preserves_admin_dropins() {
    let scripts = [
        (
            "postinst",
            include_str!("../debian/postinst"),
            "\nrequire_package_transaction_lock()",
        ),
        (
            "postrm",
            include_str!("../debian/postrm"),
            "\ncase \"$1\" in",
        ),
    ];

    for (name, source, end_marker) in scripts {
        for (label, quota) in [
            ("ordinary", "1520"),
            // floor(INT64_MAX / 95) * 95: greatest quota the retired
            // getconf + signed-shell-arithmetic writer could emit.
            ("maximum", "9223372036854775790"),
        ] {
            let dir = temp_dir(&format!("{name}-{label}"));
            let stale = dir.join("10-host-cpu.conf");
            let operator = dir.join("99-operator.conf");
            // No provenance sidecar existed. The exact generated bytes are
            // the migration signature; identical bytes mean retired policy.
            std::fs::write(&stale, legacy_dropin(quota)).unwrap();
            std::fs::write(&operator, OPERATOR_DROPIN).unwrap();

            let output = run_cleanup(source, end_marker, &dir);
            assert!(
                output.status.success(),
                "{name} should remove exact legacy drop-in ({label}): {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!stale.exists(), "{name} retained the exact Velnor file");
            assert_eq!(std::fs::read(&operator).unwrap(), OPERATOR_DROPIN);
            assert!(
                dir.exists(),
                "{name} removed the operator's drop-in directory"
            );

            // A second configure/remove is harmless after the exact file is gone.
            let output = run_cleanup(source, end_marker, &dir);
            assert!(
                output.status.success(),
                "{name} cleanup should be idempotent: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(std::fs::read(&operator).unwrap(), OPERATOR_DROPIN);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
fn maintainer_cleanup_rejects_unknown_same_path_conflicts() {
    let scripts = [
        (
            "postinst",
            include_str!("../debian/postinst"),
            "\nrequire_package_transaction_lock()",
        ),
        (
            "postrm",
            include_str!("../debian/postrm"),
            "\ncase \"$1\" in",
        ),
    ];
    let modified_quota = legacy_dropin("1521");
    let with_extra_policy = format!("{}CPUWeight=200\n", legacy_dropin("1520"));
    let above_intmax = legacy_dropin("9223372036854775885");
    let enormous = legacy_dropin(&format!("95{}", "0".repeat(100)));
    let custom = "[Slice]\nCPUQuota=250%\n# Operator policy\n";

    for (name, source, end_marker) in scripts {
        for (label, content) in [
            ("modified-quota", modified_quota.as_str()),
            ("extra-policy", with_extra_policy.as_str()),
            ("above-intmax", above_intmax.as_str()),
            ("oversized-decimal", enormous.as_str()),
            ("custom-file", custom),
        ] {
            let dir = temp_dir(&format!("{name}-{label}"));
            let dropin = dir.join("10-host-cpu.conf");
            std::fs::write(&dropin, content).unwrap();

            let output = run_cleanup(source, end_marker, &dir);
            assert!(
                !output.status.success(),
                "{name} accepted unknown conflict {label}"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("unknown conflict"),
                "{name} did not explain the conflict ({label}): {stderr}"
            );
            assert_eq!(
                std::fs::read(&dropin).unwrap(),
                content.as_bytes(),
                "{name} changed unknown file {label}"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn postinst_configure_removes_legacy_quota_before_systemd_reload() {
    use std::os::unix::fs::PermissionsExt;

    let postinst = include_str!("../debian/postinst");
    let functions = dropin_cleanup_functions(postinst, "\nrequire_package_transaction_lock()");
    let cleanup_call = postinst
        .find("\n    remove_stale_jobs_cpu_quota_dropin\n")
        .map(|offset| offset + 1)
        .unwrap();
    let reload_start = postinst[cleanup_call..]
        .find("    if [ -d /run/systemd/system ]; then\n      systemctl daemon-reload")
        .map(|offset| cleanup_call + offset)
        .unwrap();
    let reload_end = postinst[reload_start..]
        .find("\n    fi\n\n    echo \"velnorctl")
        .map(|offset| reload_start + offset + "\n    fi".len())
        .unwrap();
    let actions = postinst[cleanup_call..reload_end].replace(
        "if [ -d /run/systemd/system ]; then",
        "if [ -d \"$TEST_SYSTEMD_DIR\" ]; then",
    );

    for phase in ["fresh-install", "upgrade"] {
        let root = temp_dir(phase);
        let dropin = root.join("velnor-jobs.slice.d");
        std::fs::create_dir_all(&dropin).unwrap();
        std::fs::write(dropin.join("10-host-cpu.conf"), legacy_dropin("1520")).unwrap();
        std::fs::write(dropin.join("99-operator.conf"), OPERATOR_DROPIN).unwrap();

        let systemd_dir = root.join("systemd");
        let bin_dir = root.join("bin");
        std::fs::create_dir_all(&systemd_dir).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();
        let log = root.join("systemctl.log");
        let systemctl = bin_dir.join("systemctl");
        std::fs::write(
            &systemctl,
            r#"#!/bin/sh
set -eu
case "$1" in
  daemon-reload)
    printf 'daemon-reload\n' >> "$TEST_SYSTEMCTL_LOG"
    [ ! -e "$TEST_DROPIN_DIR/10-host-cpu.conf" ] || {
      echo "legacy quota file remained at daemon-reload" >&2
      exit 21
    }
    ;;
  show)
    printf 'show\n' >> "$TEST_SYSTEMCTL_LOG"
    if [ -e "$TEST_DROPIN_DIR/10-host-cpu.conf" ]; then
      printf '950ms\ninfinity\ninfinity\n'
    else
      printf 'infinity\ninfinity\ninfinity\n'
    fi
    ;;
  is-enabled)
    printf 'disabled\n'
    ;;
  enable)
    printf 'enable\n' >> "$TEST_SYSTEMCTL_LOG"
    ;;
  *)
    echo "unexpected systemctl call: $*" >&2
    exit 99
    ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();

        let script = format!(
            "set -eu\nfail() {{ echo \"$1\" >&2; exit 1; }}\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{functions}\n{actions}"
        );
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let path = format!("{}:{}", bin_dir.display(), inherited_path.to_string_lossy());
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .env("TEST_DROPIN_DIR", &dropin)
            .env("TEST_SYSTEMD_DIR", &systemd_dir)
            .env("TEST_SYSTEMCTL_LOG", &log)
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{phase} postinst configure failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!dropin.join("10-host-cpu.conf").exists());
        assert_eq!(
            std::fs::read(dropin.join("99-operator.conf")).unwrap(),
            OPERATOR_DROPIN
        );
        let calls = std::fs::read_to_string(log).unwrap();
        assert!(
            calls.starts_with("daemon-reload\nshow\n"),
            "postinst did not reload then verify effective quotas: {calls}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn cleanup_rejects_file_and_directory_symlinks_without_traversing_them() {
    use std::{os::unix::fs::symlink, process::Command};

    for (name, source, end_marker) in [
        (
            "postinst",
            include_str!("../debian/postinst"),
            "\nrequire_package_transaction_lock()",
        ),
        (
            "postrm",
            include_str!("../debian/postrm"),
            "\ncase \"$1\" in",
        ),
    ] {
        for dangling in [false, true] {
            let dir = temp_dir(&format!("{name}-file-symlink-{dangling}"));
            let dropin = dir.join("10-host-cpu.conf");
            let target = dir.join("operator.conf");
            if !dangling {
                std::fs::write(&target, OPERATOR_DROPIN).unwrap();
            }
            symlink("operator.conf", &dropin).unwrap();

            let output = Command::new("sh")
                .arg("-c")
                .arg(isolated_cleanup_script(source, end_marker))
                .env("TEST_DROPIN_DIR", &dir)
                .output()
                .unwrap();
            assert!(
                !output.status.success(),
                "{name} accepted a same-path symlink"
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("unknown conflict"));
            assert!(std::fs::symlink_metadata(&dropin)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                std::fs::read_link(&dropin).unwrap(),
                PathBuf::from("operator.conf")
            );
            if !dangling {
                assert_eq!(std::fs::read(&target).unwrap(), OPERATOR_DROPIN);
            }
            std::fs::remove_dir_all(dir).unwrap();
        }

        let root = temp_dir(&format!("{name}-directory-symlink"));
        let target_dir = root.join("operator-dir");
        std::fs::create_dir_all(&target_dir).unwrap();
        let operator = target_dir.join("10-host-cpu.conf");
        std::fs::write(&operator, OPERATOR_DROPIN).unwrap();
        let symlink_dir = root.join("velnor-jobs.slice.d");
        symlink(&target_dir, &symlink_dir).unwrap();

        let output = Command::new("sh")
            .arg("-c")
            .arg(isolated_cleanup_script(source, end_marker))
            .env("TEST_DROPIN_DIR", &symlink_dir)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{name} traversed a symlinked directory"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("unknown conflict"));
        assert!(std::fs::symlink_metadata(&symlink_dir)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&operator).unwrap(), OPERATOR_DROPIN);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn postrm_keeps_unit_cleanup_and_removes_quota_after_inactive_proofs() {
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
    assert!(postrm.contains("JOBS_SLICE_DROPIN=$JOBS_SLICE_DROPIN_DIR/10-host-cpu.conf"));
    assert!(postrm.contains("remove_stale_jobs_cpu_quota_dropin"));
    assert!(postrm.contains("CPUQuota is 95% of one CPU per online logical CPU."));
    assert!(!postrm.contains("rmdir \"$JOBS_SLICE_DROPIN_DIR\""));

    // Ordering: enumerate < mask < disable < stop < inactive proofs < cleanup
    // < daemon-reload < final proofs.
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
    let cleanup = lifecycle
        .find("    remove_stale_jobs_cpu_quota_dropin")
        .unwrap();
    let daemon_reload = lifecycle.find("systemctl daemon-reload").unwrap();
    assert_eq!(worker_proofs.len(), 2);
    assert_eq!(slice_proofs.len(), 2);
    assert!(unit_enumeration < unit_mask);
    assert!(unit_mask < unit_disable);
    assert!(unit_mask < worker_stop);
    assert!(worker_stop < worker_proofs[0]);
    assert!(worker_proofs[0] < cleanup);
    assert!(slice_proofs[0] < cleanup);
    assert!(cleanup < daemon_reload);
    assert!(daemon_reload < worker_proofs[1]);
    assert!(worker_proofs[1] < slice_proofs[1]);
    assert!(daemon_reload < slice_proofs[1]);
}

#[test]
fn postrm_remove_cleans_legacy_file_without_systemd_and_preserves_operator_dropins() {
    let postrm = include_str!("../debian/postrm");
    let script = isolated_postrm(postrm);
    let dir = temp_dir("postrm-remove");
    let dropin = dir.join("10-host-cpu.conf");
    let operator = dir.join("99-operator.conf");
    std::fs::write(&dropin, legacy_dropin("1520")).unwrap();
    std::fs::write(&operator, OPERATOR_DROPIN).unwrap();

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .arg("postrm")
        .arg("remove")
        .env("TEST_DROPIN_DIR", &dir)
        .env("TEST_SYSTEMD_DIR", dir.join("no-systemd"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "postrm remove without systemd failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!dropin.exists());
    assert_eq!(std::fs::read(&operator).unwrap(), OPERATOR_DROPIN);
    assert!(dir.exists());

    // Removal is idempotent after the legacy file disappears.
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .arg("postrm")
        .arg("remove")
        .env("TEST_DROPIN_DIR", &dir)
        .env("TEST_SYSTEMD_DIR", dir.join("no-systemd"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(std::fs::read(&operator).unwrap(), OPERATOR_DROPIN);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn postrm_remove_fails_closed_on_unknown_conflict_without_systemd() {
    let postrm = include_str!("../debian/postrm");
    let script = isolated_postrm(postrm);
    let dir = temp_dir("postrm-conflict");
    let dropin = dir.join("10-host-cpu.conf");
    let custom = b"[Slice]\nCPUQuota=250%\n# Operator policy\n";
    std::fs::write(&dropin, custom).unwrap();

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .arg("postrm")
        .arg("remove")
        .env("TEST_DROPIN_DIR", &dir)
        .env("TEST_SYSTEMD_DIR", dir.join("no-systemd"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown conflict"));
    assert_eq!(std::fs::read(&dropin).unwrap(), custom);
    std::fs::remove_dir_all(dir).unwrap();
}

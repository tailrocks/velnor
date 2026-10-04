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

const LEGACY_GENERATED_DROPIN: &str = "[Slice]\n# Generated from the host online CPU count during package configuration.\n# CPUQuota is 95% of one CPU per online logical CPU.\nCPUQuota=1520%\n";

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
    assert!(postinst.contains("mv -T -n -- \"$JOBS_SLICE_DROPIN\" \"$JOBS_SLICE_DROPIN_BACKUP\""));
    assert!(postinst.contains("restore_stale_jobs_cpu_quota_dropin"));
    assert!(postinst.contains("ln -L -T -- \"$restore_fd_path\" \"$destination\""));
    assert!(postinst.contains("rm -- \"$discard_file\""));
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
fn postinst_verifies_five_unbounded_resource_properties_fail_closed() {
    use std::process::Command;

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("verify_jobs_slice_has_no_resource_ceilings()")
        .unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let function = &postinst[function_start..function_end];
    let cases = [
        (
            "five-infinity-values",
            "infinity\ninfinity\ninfinity\ninfinity\ninfinity\n",
            false,
            true,
        ),
        (
            "finite-value",
            "infinity\ninfinity\n512M\ninfinity\ninfinity\n",
            false,
            false,
        ),
        (
            "missing-value",
            "infinity\ninfinity\ninfinity\ninfinity\n",
            false,
            false,
        ),
        (
            "malformed-value",
            "infinity\ninfinity\ninfinity\nnot-a-limit\ninfinity\n",
            false,
            false,
        ),
        ("systemctl-failure", "", true, false),
    ];

    for (label, output, systemctl_failure, expected_success) in cases {
        let script = format!(
            "set -eu\n\
             fail() {{ echo \"$1\" >&2; exit 1; }}\n\
             systemctl() {{\n\
               [ \"$*\" = 'show --property=CPUQuotaPerSecUSec --property=MemoryHigh --property=MemoryMax --property=MemorySwapMax --property=TasksMax --value velnor-jobs.slice' ] || return 99\n\
               if [ \"${{TEST_SYSTEMCTL_FAILURE:-0}}\" = 1 ]; then return 1; fi\n\
               printf '%s' \"$TEST_SYSTEMCTL_OUTPUT\"\n\
             }}\n\
             {function}\n\
             verify_jobs_slice_has_no_resource_ceilings\n",
        );
        let command = Command::new("sh")
            .arg("-c")
            .arg(script)
            .env("TEST_SYSTEMCTL_OUTPUT", output)
            .env(
                "TEST_SYSTEMCTL_FAILURE",
                if systemctl_failure { "1" } else { "0" },
            )
            .output()
            .unwrap();
        assert_eq!(
            command.status.success(),
            expected_success,
            "fixture {label} unexpected result; stderr: {}",
            String::from_utf8_lossy(&command.stderr)
        );
    }
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
            std::fs::write(&dropin, LEGACY_GENERATED_DROPIN).unwrap();
        }
        let script = format!(
            "set -eu\nfail() {{ echo \"$1\" >&2; exit 1; }}\nmv() {{ if [ \"$TEST_STAT_STYLE\" = bsd ] && [ \"${{1:-}}\" = -T ]; then source=$4; target=$5; if [ \"$source\" = \"${{TEST_BACKUP_PATH:-}}\" ] || [ \"$source\" = \"${{TEST_FD_TARGET_PATH:-}}\" ]; then TEST_FD_TARGET_PATH=$target; export TEST_FD_TARGET_PATH; elif [ \"$target\" != \"$TEST_DROPIN_DIR/10-host-cpu.conf\" ]; then TEST_BACKUP_PATH=$target; TEST_FD_TARGET_PATH=$target; export TEST_BACKUP_PATH TEST_FD_TARGET_PATH; fi; shift; [ \"${{1:-}}\" = -n ] && shift; [ \"${{1:-}}\" = -- ] && shift; command mv -n \"$@\"; else command mv \"$@\"; fi; }}\nrm() {{ if [ \"${{1:-}}\" = -- ]; then shift; fi; command rm \"$@\"; }}\nstat() {{ if [ \"$TEST_STAT_STYLE\" = bsd ]; then if [ \"${{1:-}}\" = -L ]; then shift; fi; [ \"${{1:-}}\" = -c ] || return 99; format=$2; target=$3; case \"$target\" in /proc/*/fd/8) target=$TEST_FD_TARGET_PATH ;; esac; command stat -L -f \"$format\" \"$target\"; else command stat \"$@\"; fi; }}\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{function}\nremove_stale_jobs_cpu_quota_dropin\n"
        );
        let output = Command::new("sh")
            .arg("-c")
            .arg(script)
            .env("TEST_DROPIN_DIR", &dir)
            .env(
                "TEST_STAT_STYLE",
                if cfg!(target_os = "macos") {
                    "bsd"
                } else {
                    "gnu"
                },
            )
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
fn postinst_restores_exact_dropin_when_a_later_proof_fails() {
    use std::process::Command;

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("remove_stale_jobs_cpu_quota_dropin()")
        .unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let functions = &postinst[function_start..function_end];

    let dir = temp_dir("postinst-restore");
    let dropin = dir.join("10-host-cpu.conf");
    let expected = LEGACY_GENERATED_DROPIN.as_bytes();
    std::fs::write(&dropin, expected).unwrap();
    let script = format!(
        "set -eu\nfail() {{ echo \"$1\" >&2; exit 1; }}\nsystemctl() {{ [ \"$*\" = 'daemon-reload' ]; }}\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{functions}\nremove_stale_jobs_cpu_quota_dropin\nexit 42\n"
    );
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("TEST_DROPIN_DIR", &dir)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert_eq!(std::fs::read(&dropin).unwrap(), expected);
    assert!(
        String::from_utf8_lossy(&output.stdout).is_empty(),
        "restoring stale drop-in should not print secrets or file bytes"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn postinst_preserves_backup_if_dropin_appears_during_restore() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("remove_stale_jobs_cpu_quota_dropin()")
        .unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let functions = &postinst[function_start..function_end];

    let dir = temp_dir("postinst-raced-restore");
    let dropin = dir.join("10-host-cpu.conf");
    let original = LEGACY_GENERATED_DROPIN.as_bytes();
    let concurrent = b"[Slice]\nCPUWeight=200\n";
    std::fs::write(&dropin, original).unwrap();

    let shim_dir = dir.join("bin");
    std::fs::create_dir(&shim_dir).unwrap();
    let ln_shim = shim_dir.join("ln");
    std::fs::write(
        &ln_shim,
        r##"#!/bin/sh
set -eu
if [ "${1:-}" = -L ] && [ "${2:-}" = -T ] && [ "${3:-}" = -- ] && [ "${5:-}" = "$TEST_DROPIN_PATH" ]; then
  printf '%s' "$TEST_CONCURRENT_DROPIN_CONTENT" > "$TEST_DROPIN_PATH"
fi
PATH=$TEST_REAL_PATH
export PATH
if [ "$TEST_LN_STYLE" = gnu ]; then
  exec ln "$@"
fi
exec ln "$4" "$5"
"##,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&ln_shim).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&ln_shim, permissions).unwrap();

    let real_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![shim_dir];
    path_entries.extend(std::env::split_paths(&real_path));
    let test_path = std::env::join_paths(path_entries).unwrap();
    let prefix = r##"set -eu
fail() { echo "$1" >&2; exit 1; }
systemctl() { [ "$*" = 'daemon-reload' ]; }
mv() {
  if [ "$TEST_STAT_STYLE" = bsd ] && [ "${1:-}" = -T ]; then
    shift
    [ "${1:-}" = -n ] && shift
    [ "${1:-}" = -- ] && shift
    command mv -n "$@"
  else
    command mv "$@"
  fi
}
stat() {
  if [ "$TEST_STAT_STYLE" = bsd ]; then
    if [ "${1:-}" = -L ]; then shift; fi
    [ "${1:-}" = -c ] || return 99
    format=$2
    target=$3
    case "$target" in
      /proc/*/fd/8)
        if [ -e "$TEST_BACKUP_PATH.original" ]; then
          target=$TEST_BACKUP_PATH.original
        else
          target=$TEST_BACKUP_PATH
        fi
        ;;
    esac
    command stat -L -f "$format" "$target"
  else
    command stat "$@"
  fi
}
JOBS_SLICE_DROPIN_DIR="$TEST_DROPIN_DIR"
JOBS_SLICE_DROPIN="$TEST_DROPIN_DIR/10-host-cpu.conf"
"##;
    let script = [
        prefix,
        functions,
        "\nremove_stale_jobs_cpu_quota_dropin\nTEST_BACKUP_PATH=$JOBS_SLICE_DROPIN_BACKUP\nexport TEST_BACKUP_PATH\nexit 42\n",
    ]
    .concat();
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("TEST_DROPIN_DIR", &dir)
        .env("TEST_DROPIN_PATH", &dropin)
        .env(
            "TEST_CONCURRENT_DROPIN_CONTENT",
            String::from_utf8_lossy(concurrent).as_ref(),
        )
        .env("TEST_REAL_PATH", &real_path)
        .env(
            "TEST_LN_STYLE",
            if cfg!(target_os = "linux") {
                "gnu"
            } else {
                "bsd"
            },
        )
        .env(
            "TEST_STAT_STYLE",
            if cfg!(target_os = "macos") {
                "bsd"
            } else {
                "gnu"
            },
        )
        .env("PATH", test_path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(42));
    assert_eq!(std::fs::read(&dropin).unwrap(), concurrent);
    let backups: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".10-host-cpu.conf.velnor-backup-")
        })
        .collect();
    assert_eq!(
        backups.len(),
        1,
        "the staged backup must remain on collision"
    );
    assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
    assert!(String::from_utf8_lossy(&output.stderr).contains("preserving the staged backup"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn postinst_staging_does_not_overwrite_a_backup_created_during_move() {
    use std::os::unix::fs::MetadataExt;
    use std::process::Command;

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("remove_stale_jobs_cpu_quota_dropin()")
        .unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let functions = &postinst[function_start..function_end];

    let dir = temp_dir("postinst-stage-collision");
    let dropin = dir.join("10-host-cpu.conf");
    let sentinel = dir.join("operator-sentinel.conf");
    let systemctl_log = dir.join("systemctl.log");
    let original = LEGACY_GENERATED_DROPIN.as_bytes();
    let sentinel_content = b"[Slice]\nCPUWeight=200\n";
    std::fs::write(&dropin, original).unwrap();
    std::fs::write(&sentinel, sentinel_content).unwrap();
    let original_metadata = std::fs::metadata(&dropin).unwrap();
    let sentinel_metadata = std::fs::metadata(&sentinel).unwrap();

    let script = format!(
        "set -eu\nfail() {{ echo \"$1\" >&2; exit 1; }}\nsystemctl() {{ [ \"$*\" = 'daemon-reload' ] || return 99; printf '%s\\n' \"$*\" >> \"$TEST_SYSTEMCTL_LOG\"; }}\nmv() {{ if [ \"${{1:-}}\" = -T ] && [ \"${{4:-}}\" = \"$JOBS_SLICE_DROPIN\" ]; then command ln \"$TEST_SENTINEL_PATH\" \"$5\"; fi; if [ \"$TEST_STAT_STYLE\" = bsd ] && [ \"${{1:-}}\" = -T ]; then shift; [ \"${{1:-}}\" = -n ] && shift; [ \"${{1:-}}\" = -- ] && shift; command mv -n \"$@\"; else command mv \"$@\"; fi; }}\nstat() {{ if [ \"$TEST_STAT_STYLE\" = bsd ]; then command stat -f \"$2\" \"$3\"; else command stat \"$@\"; fi; }}\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{functions}\nremove_stale_jobs_cpu_quota_dropin\nexit 0\n"
    );
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("TEST_DROPIN_DIR", &dir)
        .env("TEST_SENTINEL_PATH", &sentinel)
        .env("TEST_SYSTEMCTL_LOG", &systemctl_log)
        .env(
            "TEST_STAT_STYLE",
            if cfg!(target_os = "macos") {
                "bsd"
            } else {
                "gnu"
            },
        )
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        std::fs::read(&dropin).unwrap_or_default(),
        original,
        "rollback did not restore the pinned inode: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after_dropin = std::fs::metadata(&dropin).unwrap();
    assert_eq!(after_dropin.dev(), original_metadata.dev());
    assert_eq!(after_dropin.ino(), original_metadata.ino());
    assert_eq!(after_dropin.nlink(), original_metadata.nlink());
    assert_eq!(std::fs::read(&sentinel).unwrap(), sentinel_content);
    let backups: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".10-host-cpu.conf.velnor-backup-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read(&backups[0]).unwrap(), sentinel_content);
    let after_sentinel = std::fs::metadata(&sentinel).unwrap();
    let backup_metadata = std::fs::metadata(&backups[0]).unwrap();
    assert_eq!(backup_metadata.dev(), sentinel_metadata.dev());
    assert_eq!(backup_metadata.ino(), sentinel_metadata.ino());
    assert_eq!(after_sentinel.dev(), sentinel_metadata.dev());
    assert_eq!(after_sentinel.ino(), sentinel_metadata.ino());
    assert_eq!(
        after_sentinel.nlink(),
        2,
        "no-clobber move must retain the sentinel link"
    );
    assert_eq!(backup_metadata.nlink(), 2);
    assert_eq!(
        std::fs::read_to_string(systemctl_log).unwrap(),
        "daemon-reload\n"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn postinst_restores_opened_inode_when_backup_path_changes_during_link() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::process::Command;

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("remove_stale_jobs_cpu_quota_dropin()")
        .unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let functions = &postinst[function_start..function_end];

    let dir = temp_dir("postinst-source-link-race");
    let dropin = dir.join("10-host-cpu.conf");
    let original = LEGACY_GENERATED_DROPIN.as_bytes();
    let replacement = b"[Slice]\nCPUWeight=200\n";
    std::fs::write(&dropin, original).unwrap();

    let shim_dir = dir.join("bin");
    std::fs::create_dir(&shim_dir).unwrap();
    let ln_shim = shim_dir.join("ln");
    std::fs::write(
        &ln_shim,
        r##"#!/bin/sh
set -eu
case "${4:-}" in
  /proc/*/fd/8)
    mv "$TEST_BACKUP_PATH" "$TEST_BACKUP_PATH.original"
    printf '%s' "$TEST_REPLACEMENT_CONTENT" > "$TEST_BACKUP_PATH"
    ;;
esac
PATH=$TEST_REAL_PATH
export PATH
if [ "$TEST_LN_STYLE" = gnu ]; then
  exec ln "$@"
fi
exec ln "$TEST_BACKUP_PATH.original" "$5"
"##,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&ln_shim).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&ln_shim, permissions).unwrap();

    let real_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![shim_dir];
    path_entries.extend(std::env::split_paths(&real_path));
    let test_path = std::env::join_paths(path_entries).unwrap();
    let prefix = r##"set -eu
fail() { echo "$1" >&2; exit 1; }
systemctl() { [ "$*" = 'daemon-reload' ]; }
mv() {
  if [ "$TEST_STAT_STYLE" = bsd ] && [ "${1:-}" = -T ]; then
    shift
    [ "${1:-}" = -n ] && shift
    [ "${1:-}" = -- ] && shift
    command mv -n "$@"
  else
    command mv "$@"
  fi
}
stat() {
  if [ "$TEST_STAT_STYLE" = bsd ]; then
    if [ "${1:-}" = -L ]; then shift; fi
    [ "${1:-}" = -c ] || return 99
    format=$2
    target=$3
    case "$target" in
      /proc/*/fd/8)
        if [ -e "$TEST_BACKUP_PATH.original" ]; then
          target=$TEST_BACKUP_PATH.original
        else
          target=$TEST_BACKUP_PATH
        fi
        ;;
    esac
    command stat -L -f "$format" "$target"
  else
    command stat "$@"
  fi
}
JOBS_SLICE_DROPIN_DIR="$TEST_DROPIN_DIR"
JOBS_SLICE_DROPIN="$TEST_DROPIN_DIR/10-host-cpu.conf"
"##;
    let script = [
        prefix,
        functions,
        "\nremove_stale_jobs_cpu_quota_dropin\nTEST_BACKUP_PATH=$JOBS_SLICE_DROPIN_BACKUP\nexport TEST_BACKUP_PATH\nexit 42\n",
    ]
    .concat();
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("TEST_DROPIN_DIR", &dir)
        .env(
            "TEST_REPLACEMENT_CONTENT",
            String::from_utf8_lossy(replacement).as_ref(),
        )
        .env("TEST_REAL_PATH", &real_path)
        .env(
            "TEST_LN_STYLE",
            if cfg!(target_os = "linux") {
                "gnu"
            } else {
                "bsd"
            },
        )
        .env(
            "TEST_STAT_STYLE",
            if cfg!(target_os = "macos") {
                "bsd"
            } else {
                "gnu"
            },
        )
        .env("PATH", test_path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(42));
    assert_eq!(std::fs::read(&dropin).unwrap(), original);
    let backups: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".10-host-cpu.conf.velnor-backup-")
        })
        .collect();
    assert_eq!(
        backups.len(),
        2,
        "the backup and swapped-out inode must remain"
    );
    let saved_original = backups
        .iter()
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "original")
        })
        .unwrap();
    let replacement_backup = backups.iter().find(|path| *path != saved_original).unwrap();
    assert_eq!(std::fs::read(saved_original).unwrap(), original);
    assert_eq!(std::fs::read(replacement_backup).unwrap(), replacement);
    assert_eq!(
        std::fs::metadata(&dropin).unwrap().ino(),
        std::fs::metadata(saved_original).unwrap().ino(),
        "destination must be the inode opened before the backup pathname was swapped"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("preserving the staged backup"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn postinst_keeps_backup_when_proc_fd_identity_cannot_be_verified() {
    use std::process::Command;

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("remove_stale_jobs_cpu_quota_dropin()")
        .unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let functions = &postinst[function_start..function_end];

    let dir = temp_dir("postinst-proc-fd-unavailable");
    let dropin = dir.join("10-host-cpu.conf");
    let systemctl_log = dir.join("systemctl.log");
    let original = LEGACY_GENERATED_DROPIN.as_bytes();
    std::fs::write(&dropin, original).unwrap();
    let script = format!(
        "set -eu\nfail() {{ echo \"$1\" >&2; exit 1; }}\nsystemctl() {{ [ \"$*\" = 'daemon-reload' ] || return 99; printf '%s\\n' \"$*\" >> \"$TEST_SYSTEMCTL_LOG\"; }}\nmv() {{ if [ \"$TEST_STAT_STYLE\" = bsd ] && [ \"${{1:-}}\" = -T ]; then shift; [ \"${{1:-}}\" = -n ] && shift; [ \"${{1:-}}\" = -- ] && shift; command mv -n \"$@\"; else command mv \"$@\"; fi; }}\nstat() {{ case \"$*\" in *'/fd/8'*) return 1 ;; esac; if [ \"$TEST_STAT_STYLE\" = bsd ]; then command stat -f \"$2\" \"$3\"; else command stat \"$@\"; fi; }}\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{functions}\nremove_stale_jobs_cpu_quota_dropin\nTEST_BACKUP_PATH=$JOBS_SLICE_DROPIN_BACKUP\nexport TEST_BACKUP_PATH\nexit 42\n"
    );
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("TEST_DROPIN_DIR", &dir)
        .env("TEST_SYSTEMCTL_LOG", &systemctl_log)
        .env(
            "TEST_STAT_STYLE",
            if cfg!(target_os = "macos") {
                "bsd"
            } else {
                "gnu"
            },
        )
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(42));
    assert_eq!(
        std::fs::symlink_metadata(&dropin).unwrap_err().kind(),
        std::io::ErrorKind::NotFound,
        "destination must remain absent when the proc-fd inode cannot be proved"
    );
    assert_eq!(
        std::fs::read_to_string(&systemctl_log).unwrap(),
        "daemon-reload\n"
    );
    let backups: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".10-host-cpu.conf.velnor-backup-")
        })
        .collect();
    assert_eq!(backups.len(), 1, "unprovable FD must preserve the backup");
    assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn postinst_cleanup_quarantines_before_unlinking_backup() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::process::Command;

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("remove_stale_jobs_cpu_quota_dropin()")
        .unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let functions = &postinst[function_start..function_end];

    let dir = temp_dir("postinst-cleanup-quarantine");
    let dropin = dir.join("10-host-cpu.conf");
    let systemctl_log = dir.join("systemctl.log");
    let original = LEGACY_GENERATED_DROPIN.as_bytes();
    let sentinel = b"[Slice]\nCPUWeight=200\n";
    std::fs::write(&dropin, original).unwrap();

    let shim_dir = dir.join("bin");
    std::fs::create_dir(&shim_dir).unwrap();
    let ln_shim = shim_dir.join("ln");
    std::fs::write(
        &ln_shim,
        r##"#!/bin/sh
set -eu
PATH=$TEST_REAL_PATH
export PATH
if [ "$TEST_LN_STYLE" = gnu ]; then
  exec ln "$@"
fi
exec ln "$TEST_FD_TARGET_PATH" "$5"
"##,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&ln_shim).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&ln_shim, permissions).unwrap();

    let real_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![shim_dir];
    path_entries.extend(std::env::split_paths(&real_path));
    let test_path = std::env::join_paths(path_entries).unwrap();
    let prefix = r##"set -eu
fail() { echo "$1" >&2; exit 1; }
systemctl() { [ "$*" = 'daemon-reload' ] || return 99; printf '%s\n' "$*" >> "$TEST_SYSTEMCTL_LOG"; }
mv() {
  if [ "${1:-}" = -T ] && [ "${4:-}" = "${TEST_BACKUP_PATH:-}" ] \
    && [ -z "${TEST_FD_TARGET_PATH:-}" ]; then
    TEST_FD_TARGET_PATH=$5
    export TEST_FD_TARGET_PATH
  fi
  if [ "$TEST_STAT_STYLE" = bsd ] && [ "${1:-}" = -T ]; then
    shift
    [ "${1:-}" = -n ] && shift
    [ "${1:-}" = -- ] && shift
    command mv -n "$@"
  else
    command mv "$@"
  fi
}
rm() {
  if [ "${1:-}" = -- ]; then
    case "${2:-}" in
      "$TEST_DROPIN_DIR"/.10-host-cpu.conf.cleanup-*/backup)
        mv "$2" "$2.saved"
        TEST_FD_TARGET_PATH=$2.saved
        export TEST_FD_TARGET_PATH
        printf '%s' "$TEST_SENTINEL_CONTENT" > "$TEST_BACKUP_PATH"
        command rm "$2"
        return $?
        ;;
    esac
  fi
  command rm "$@"
}
stat() {
  if [ "$TEST_STAT_STYLE" = bsd ]; then
    if [ "${1:-}" = -L ]; then shift; fi
    [ "${1:-}" = -c ] || return 99
    format=$2
    target=$3
    case "$target" in
      /proc/*/fd/8)
        if [ -n "${TEST_FD_TARGET_PATH:-}" ]; then
          target=$TEST_FD_TARGET_PATH
        else
          target=$TEST_BACKUP_PATH
        fi
        ;;
    esac
    command stat -L -f "$format" "$target"
  else
    command stat "$@"
  fi
}
JOBS_SLICE_DROPIN_DIR="$TEST_DROPIN_DIR"
JOBS_SLICE_DROPIN="$TEST_DROPIN_DIR/10-host-cpu.conf"
"##;
    let script = [
        prefix,
        functions,
        "\nremove_stale_jobs_cpu_quota_dropin\nTEST_BACKUP_PATH=$JOBS_SLICE_DROPIN_BACKUP\nexport TEST_BACKUP_PATH\nexit 0\n",
    ]
    .concat();
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("TEST_DROPIN_DIR", &dir)
        .env("TEST_SYSTEMCTL_LOG", &systemctl_log)
        .env(
            "TEST_SENTINEL_CONTENT",
            String::from_utf8_lossy(sentinel).as_ref(),
        )
        .env("TEST_REAL_PATH", &real_path)
        .env(
            "TEST_LN_STYLE",
            if cfg!(target_os = "linux") {
                "gnu"
            } else {
                "bsd"
            },
        )
        .env(
            "TEST_STAT_STYLE",
            if cfg!(target_os = "macos") {
                "bsd"
            } else {
                "gnu"
            },
        )
        .env("PATH", test_path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        std::fs::read(&dropin).unwrap_or_default(),
        original,
        "rollback did not restore the pinned inode: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&systemctl_log).unwrap(),
        "daemon-reload\n"
    );
    let backups: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".10-host-cpu.conf.velnor-backup-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read(&backups[0]).unwrap(), sentinel);

    let cleanup_dirs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".10-host-cpu.conf.cleanup-")
        })
        .collect();
    assert_eq!(
        cleanup_dirs.len(),
        1,
        "failed unlink retains its private quarantine"
    );
    assert_eq!(
        std::fs::metadata(&cleanup_dirs[0])
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "quarantine directory must exclude non-root path mutators"
    );
    let saved_original = std::fs::read_dir(&cleanup_dirs[0])
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "saved")
        })
        .unwrap();
    assert_eq!(std::fs::read(&saved_original).unwrap(), original);
    let destination = std::fs::metadata(&dropin).unwrap();
    let saved = std::fs::metadata(saved_original).unwrap();
    assert_eq!(destination.dev(), saved.dev());
    assert_eq!(destination.ino(), saved.ino());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn postinst_reloads_systemd_when_dropin_exists_before_rollback() {
    use std::process::Command;

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("remove_stale_jobs_cpu_quota_dropin()")
        .unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let functions = &postinst[function_start..function_end];

    let dir = temp_dir("postinst-preexisting-rollback-collision");
    let dropin = dir.join("10-host-cpu.conf");
    let systemctl_log = dir.join("systemctl.log");
    let original = LEGACY_GENERATED_DROPIN.as_bytes();
    let concurrent = b"[Slice]\nCPUWeight=200\n";
    std::fs::write(&dropin, original).unwrap();
    let script = format!(
        "set -eu\nfail() {{ echo \"$1\" >&2; exit 1; }}\nsystemctl() {{ [ \"$*\" = 'daemon-reload' ] || return 99; printf '%s\\n' \"$*\" >> \"$TEST_SYSTEMCTL_LOG\"; }}\nmv() {{ if [ \"$TEST_STAT_STYLE\" = bsd ] && [ \"${{1:-}}\" = -T ]; then shift; [ \"${{1:-}}\" = -n ] && shift; [ \"${{1:-}}\" = -- ] && shift; command mv -n \"$@\"; else command mv \"$@\"; fi; }}\nstat() {{ if [ \"$TEST_STAT_STYLE\" = bsd ]; then command stat -f \"$2\" \"$3\"; else command stat \"$@\"; fi; }}\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{functions}\nremove_stale_jobs_cpu_quota_dropin\nprintf '%s' \"$TEST_CONCURRENT_DROPIN_CONTENT\" > \"$JOBS_SLICE_DROPIN\"\nexit 42\n"
    );
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("TEST_DROPIN_DIR", &dir)
        .env("TEST_SYSTEMCTL_LOG", &systemctl_log)
        .env(
            "TEST_CONCURRENT_DROPIN_CONTENT",
            String::from_utf8_lossy(concurrent).as_ref(),
        )
        .env(
            "TEST_STAT_STYLE",
            if cfg!(target_os = "macos") {
                "bsd"
            } else {
                "gnu"
            },
        )
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(42));
    assert_eq!(std::fs::read(&dropin).unwrap(), concurrent);
    assert_eq!(
        std::fs::read_to_string(systemctl_log).unwrap(),
        "daemon-reload\n"
    );
    let backups: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".10-host-cpu.conf.velnor-backup-")
        })
        .collect();
    assert_eq!(
        backups.len(),
        1,
        "rollback collision must preserve the backup"
    );
    assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("cannot restore stale jobs-slice drop-in over a new file"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn postinst_preserves_replaced_dropin_backup() {
    use std::process::Command;

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("remove_stale_jobs_cpu_quota_dropin()")
        .unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let functions = &postinst[function_start..function_end];

    let dir = temp_dir("postinst-replaced-backup");
    let dropin = dir.join("10-host-cpu.conf");
    let operator_file = dir.join("operator.conf");
    std::fs::write(&dropin, LEGACY_GENERATED_DROPIN).unwrap();
    std::fs::write(&operator_file, b"operator data\n").unwrap();
    let script = format!(
        "set -eu\nfail() {{ echo \"$1\" >&2; exit 1; }}\nsystemctl() {{ [ \"$*\" = 'daemon-reload' ]; }}\nmv() {{ if [ \"$TEST_STAT_STYLE\" = bsd ] && [ \"${{1:-}}\" = -T ]; then shift; [ \"${{1:-}}\" = -n ] && shift; [ \"${{1:-}}\" = -- ] && shift; command mv -n \"$@\"; else command mv \"$@\"; fi; }}\nstat() {{ if [ \"$TEST_STAT_STYLE\" = bsd ]; then if [ \"${{1:-}}\" = -L ]; then shift; fi; [ \"${{1:-}}\" = -c ] || return 99; command stat -f \"$2\" \"$3\"; else command stat \"$@\"; fi; }}\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{functions}\nremove_stale_jobs_cpu_quota_dropin\nrm \"$JOBS_SLICE_DROPIN_BACKUP\"\nln -s \"$TEST_OPERATOR_FILE\" \"$JOBS_SLICE_DROPIN_BACKUP\"\nexit 0\n"
    );
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("TEST_DROPIN_DIR", &dir)
        .env("TEST_OPERATOR_FILE", &operator_file)
        .env(
            "TEST_STAT_STYLE",
            if cfg!(target_os = "macos") {
                "bsd"
            } else {
                "gnu"
            },
        )
        .output()
        .unwrap();

    assert!(!output.status.success());
    let destination_metadata = std::fs::symlink_metadata(&dropin);
    assert!(
        destination_metadata.is_err(),
        "destination must remain absent when the staged backup identity changes; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        destination_metadata.unwrap_err().kind(),
        std::io::ErrorKind::NotFound,
        "destination must remain absent when the staged backup identity changes"
    );
    let backups: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".10-host-cpu.conf.velnor-backup-")
        })
        .collect();
    assert_eq!(backups.len(), 1, "replacement backup symlink must remain");
    assert!(std::fs::symlink_metadata(&backups[0])
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read_link(&backups[0]).unwrap(), operator_file);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not prove staged drop-in removal; preserving its inode"));
    assert!(stderr.contains("failed to restore"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn postrm_keeps_unit_cleanup_without_quota_logic() {
    let postrm = include_str!("../debian/postrm");

    // Generic fleet cleanup survives: enumerate, mask, disable, stop, prove the jobs slice.
    assert!(postrm.contains("require_package_transaction_lock"));
    assert!(postrm.contains("systemctl mask --runtime \"$unit\""));
    assert!(!postrm.contains("systemctl unmask --runtime"));
    assert!(postrm.contains("systemctl stop \"$unit\""));
    assert!(postrm.contains("systemctl daemon-reload"));
    assert!(postrm.contains("verify_jobs_slice_inactive"));
    assert!(postrm.contains("--property=LoadState --value velnor-jobs.slice"));
    assert!(postrm.contains("--property=ActiveState --value velnor-jobs.slice"));

    // Removal deletes only the old generated file, preserving operator
    // drop-ins. It runs after the inactive slice proof and before daemon-reload.
    assert!(postrm.contains("JOBS_SLICE_DROPIN=$JOBS_SLICE_DROPIN_DIR/10-host-cpu.conf"));
    assert!(postrm.contains("remove_stale_jobs_cpu_quota_dropin"));
    assert!(postrm.contains("rm -- \"$JOBS_SLICE_DROPIN\""));
    assert!(
        postrm.contains("# Generated from the host online CPU count during package configuration.")
    );
    assert!(postrm.contains("^CPUQuota=[123456789][0123456789]*%$"));
    assert!(!postrm.contains("CPUQuota=${"));

    // Ordering: enumerate < mask < disable < stop < slice proof < reload < slice proof.
    let lifecycle = postrm.split("case \"$1\" in").nth(1).unwrap();
    let unit_enumeration = lifecycle
        .find("all_units=$(systemctl list-unit-files")
        .unwrap();
    let unit_mask = lifecycle
        .find("systemctl mask --runtime \"$unit\"")
        .unwrap();
    let unit_disable = lifecycle.find("systemctl disable \"$unit\"").unwrap();
    let unit_stop = lifecycle.find("systemctl stop \"$unit\"").unwrap();
    let slice_proofs: Vec<_> = lifecycle
        .match_indices("verify_jobs_slice_inactive")
        .map(|(offset, _)| offset)
        .collect();
    let daemon_reload = lifecycle.find("systemctl daemon-reload").unwrap();
    let remove_dropin = lifecycle
        .find("remove_stale_jobs_cpu_quota_dropin")
        .unwrap();
    assert_eq!(slice_proofs.len(), 2);
    assert!(unit_enumeration < unit_mask);
    assert!(unit_mask < unit_disable);
    assert!(unit_mask < unit_stop);
    assert!(unit_stop < slice_proofs[0]);
    assert!(slice_proofs[0] < daemon_reload);
    assert!(slice_proofs[0] < remove_dropin);
    assert!(remove_dropin < daemon_reload);
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
    std::fs::write(&dropin, LEGACY_GENERATED_DROPIN).unwrap();
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

#[test]
fn postinst_preserves_a_modified_operator_dropin() {
    use std::process::Command;

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("remove_stale_jobs_cpu_quota_dropin()")
        .unwrap();
    let function_end = postinst[function_start..]
        .find("\nrequire_package_transaction_lock()")
        .map(|offset| function_start + offset)
        .unwrap();
    let functions = &postinst[function_start..function_end];

    for quota in ["1%", "100%", "1000%"] {
        let dir = temp_dir("postinst-modified-operator-dropin");
        let dropin = dir.join("10-host-cpu.conf");
        let modified = format!(
            "[Slice]\n# Generated from the host online CPU count during package configuration.\n# CPUQuota is 95% of one CPU per online logical CPU.\nCPUQuota={quota}\n"
        );
        std::fs::write(&dropin, &modified).unwrap();
        let script = format!(
            "set -eu\nfail() {{ echo \"$1\" >&2; exit 1; }}\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{functions}\nremove_stale_jobs_cpu_quota_dropin\n"
        );
        let output = Command::new("sh")
            .arg("-c")
            .arg(script)
            .env("TEST_DROPIN_DIR", &dir)
            .output()
            .unwrap();

        assert!(!output.status.success());
        assert_eq!(std::fs::read(&dropin).unwrap(), modified.as_bytes());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("refusing to remove unrecognized jobs-slice drop-in"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn postrm_preserves_a_modified_operator_dropin() {
    use std::process::Command;

    let postrm = include_str!("../debian/postrm");
    let function_start = postrm.find("remove_stale_jobs_cpu_quota_dropin()").unwrap();
    let function_end = postrm[function_start..]
        .find("\ncase \"$1\" in")
        .map(|offset| function_start + offset)
        .unwrap();
    let function = &postrm[function_start..function_end];
    for quota in ["1%", "100%", "1000%"] {
        let dir = temp_dir("postrm-modified-operator-dropin");
        let dropin = dir.join("10-host-cpu.conf");
        let modified = format!(
            "[Slice]\n# Generated from the host online CPU count during package configuration.\n# CPUQuota is 95% of one CPU per online logical CPU.\nCPUQuota={quota}\n"
        );
        std::fs::write(&dropin, &modified).unwrap();

        let script = format!(
            "set -eu\nJOBS_SLICE_DROPIN_DIR=\"$TEST_DROPIN_DIR\"\nJOBS_SLICE_DROPIN=\"$TEST_DROPIN_DIR/10-host-cpu.conf\"\n{function}\nremove_stale_jobs_cpu_quota_dropin\n"
        );
        let output = Command::new("sh")
            .arg("-c")
            .arg(script)
            .env("TEST_DROPIN_DIR", &dir)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(std::fs::read(&dropin).unwrap(), modified.as_bytes());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("preserving unrecognized jobs-slice drop-in"));
        let _ = std::fs::remove_dir_all(dir);
    }
}

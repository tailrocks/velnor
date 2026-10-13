#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "package regression tests may panic"
)]

use std::{
    fs,
    os::fd::AsRawFd,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

struct TestTempDir(PathBuf);

impl TestTempDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "velnor-stale-trust-package-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create package test temp directory: {error}"),
            }
        }
        panic!("could not reserve a unique package test temp directory")
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn package_transaction_lock_helper(script: &str) -> &str {
    let start = script
        .find("require_package_transaction_lock() {")
        .expect("package transaction lock helper");
    let end = script[start..]
        .find("\n}\n\nsystemd_property")
        .map(|offset| start + offset + 2)
        .expect("package transaction lock helper end");
    &script[start..end]
}

#[cfg(target_os = "linux")]
fn run_package_transaction_lock_check(lock_path: &Path, package_script: &str) -> Output {
    let helper = package_transaction_lock_helper(package_script);
    let script = format!(
        "set -eu\nPACKAGE_TRANSACTION_LOCK=\"$TEST_PACKAGE_TRANSACTION_LOCK\"\nfail() {{ printf '%s\\n' \"$1\" >&2; exit 1; }}\n{helper}\nrequire_package_transaction_lock\n"
    );
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .env("TEST_PACKAGE_TRANSACTION_LOCK", lock_path)
        .output()
        .expect("run package lock proof helper")
}

#[test]
fn package_test_directories_are_exclusively_reserved() {
    let first = TestTempDir::new();
    let second = TestTempDir::new();
    assert_ne!(first.path(), second.path());
    assert!(first.path().is_dir());
    assert!(second.path().is_dir());
}

#[test]
fn package_lock_helpers_are_byte_identical() {
    assert_eq!(
        package_transaction_lock_helper(include_str!("../debian/preinst")),
        package_transaction_lock_helper(include_str!("../debian/postinst"))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn package_lock_proof_matches_the_live_kernel_lock_identity() {
    const LOCK_EX: i32 = 2;
    const LOCK_UN: i32 = 8;

    let temp = TestTempDir::new();
    let package_lock_path = temp.path().join("package-transaction.lock");
    let unrelated_lock_path = temp.path().join("unrelated.lock");
    let package_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&package_lock_path)
        .expect("create package lock fixture");
    let unrelated_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&unrelated_lock_path)
        .expect("create unrelated lock fixture");

    // SAFETY: package_lock owns the descriptor kept locked through helper
    // calls; each child must identify this exact inode through fd 9.
    assert_eq!(unsafe { flock(package_lock.as_raw_fd(), LOCK_EX) }, 0);
    let stat = Command::new("stat")
        .args(["-L", "-c", "%Hd:%Ld:%i"])
        .arg(&package_lock_path)
        .output()
        .expect("stat package lock fixture");
    assert!(
        stat.status.success(),
        "GNU stat could not report the package lock device identity: {stat:?}"
    );
    let stat_identity = String::from_utf8(stat.stdout).unwrap();
    let mut stat_parts = stat_identity.trim().split(':');
    let major = stat_parts.next().unwrap().parse::<u64>().unwrap();
    let minor = stat_parts.next().unwrap().parse::<u64>().unwrap();
    let inode = stat_parts.next().unwrap().parse::<u64>().unwrap();
    assert!(
        stat_parts.next().is_none(),
        "unexpected stat identity: {stat_identity}"
    );
    let pid = std::process::id().to_string();
    let observed_lock = fs::read_to_string("/proc/locks").expect("read live kernel lock table");
    assert!(
        observed_lock.lines().any(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 6 || fields[1] != "FLOCK" || fields[3] != "WRITE" || fields[4] != pid
            {
                return false;
            }
            let mut device_inode = fields[5].split(':');
            let Some(observed_major) = device_inode
                .next()
                .and_then(|value| u64::from_str_radix(value, 16).ok())
            else {
                return false;
            };
            let Some(observed_minor) = device_inode
                .next()
                .and_then(|value| u64::from_str_radix(value, 16).ok())
            else {
                return false;
            };
            let Some(observed_inode) = device_inode
                .next()
                .and_then(|value| value.parse::<u64>().ok())
            else {
                return false;
            };
            device_inode.next().is_none()
                && observed_major == major
                && observed_minor == minor
                && observed_inode == inode
        }),
        "live /proc/locks has no matching exclusive lock for fd 9's target: {observed_lock}"
    );

    for (script_name, package_script) in [
        ("preinst", include_str!("../debian/preinst")),
        ("postinst", include_str!("../debian/postinst")),
    ] {
        let output = run_package_transaction_lock_check(&package_lock_path, package_script);
        assert!(
            output.status.success(),
            "{script_name} did not recognize the live package lock: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // SAFETY: this test acquired the exclusive package lock above.
    assert_eq!(unsafe { flock(package_lock.as_raw_fd(), LOCK_UN) }, 0);
    // SAFETY: the unrelated lock is held only to prove that a lock owned by
    // the same ancestor cannot substitute for the package lock inode.
    assert_eq!(unsafe { flock(unrelated_lock.as_raw_fd(), LOCK_EX) }, 0);
    for (script_name, package_script) in [
        ("preinst", include_str!("../debian/preinst")),
        ("postinst", include_str!("../debian/postinst")),
    ] {
        let output = run_package_transaction_lock_check(&package_lock_path, package_script);
        assert!(
            !output.status.success(),
            "{script_name} accepted an exclusive lock held on another inode"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("no unique exclusive lock owner"),
            "{script_name} failed for an unrelated reason: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // SAFETY: this test acquired the unrelated exclusive lock above.
    assert_eq!(unsafe { flock(unrelated_lock.as_raw_fd(), LOCK_UN) }, 0);
}

#[cfg(unix)]
#[test]
fn package_transaction_lock_child() {
    let Ok(mode) = std::env::var("VELNOR_PACKAGE_LOCK_CHILD_MODE") else {
        return;
    };
    let lock_path = std::env::var_os("VELNOR_PACKAGE_LOCK_CHILD_PATH").unwrap();
    let marker = PathBuf::from(std::env::var_os("VELNOR_PACKAGE_LOCK_CHILD_MARKER").unwrap());
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    fs::write(marker.with_extension("started"), b"started").unwrap();
    let operation = if mode == "try" { 1 | 4 } else { 1 };
    // SAFETY: `lock` keeps this descriptor open for the call and until unlock.
    let acquired = unsafe { flock(lock.as_raw_fd(), operation) } == 0;
    if acquired {
        fs::write(&marker, b"entered").unwrap();
        // SAFETY: this process still owns the successful shared lock.
        assert_eq!(unsafe { flock(lock.as_raw_fd(), 8) }, 0);
        return;
    } else if mode == "try" {
        fs::write(marker.with_extension("blocked"), b"blocked").unwrap();
        return;
    }
    panic!("blocking shared-lock child failed to acquire after transaction lock release");
}

#[cfg(unix)]
#[test]
fn post_snapshot_activation_child_waits_for_exclusive_package_lock() {
    const LOCK_EX: i32 = 2;
    const LOCK_UN: i32 = 8;

    let temp = TestTempDir::new();
    let lock_path = temp.path().join("package-transaction.lock");
    let lock = fs::File::create(&lock_path).unwrap();
    // SAFETY: `lock` owns a valid descriptor held until explicit unlock.
    assert_eq!(unsafe { flock(lock.as_raw_fd(), LOCK_EX) }, 0);

    let waiting_marker = temp.path().join("waiting-child-entered");
    let mut waiting_child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "package_transaction_lock_child", "--nocapture"])
        .env("VELNOR_PACKAGE_LOCK_CHILD_MODE", "wait")
        .env("VELNOR_PACKAGE_LOCK_CHILD_PATH", &lock_path)
        .env("VELNOR_PACKAGE_LOCK_CHILD_MARKER", &waiting_marker)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let waiting_started = waiting_marker.with_extension("started");
    for _ in 0..500 {
        if waiting_started.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(waiting_started.exists(), "shared-lock child did not start");
    assert!(
        !waiting_marker.exists(),
        "post-snapshot shared-lock child entered while the package held an exclusive lock"
    );

    let probe_marker = temp.path().join("nonblocking-probe-entered");
    let probe = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "package_transaction_lock_child", "--nocapture"])
        .env("VELNOR_PACKAGE_LOCK_CHILD_MODE", "try")
        .env("VELNOR_PACKAGE_LOCK_CHILD_PATH", &lock_path)
        .env("VELNOR_PACKAGE_LOCK_CHILD_MARKER", &probe_marker)
        .output()
        .unwrap();
    assert!(
        probe.status.success(),
        "shared-lock probe failed: {probe:?}"
    );
    assert!(
        probe_marker.with_extension("blocked").exists(),
        "nonblocking shared-lock probe acquired the exclusive package lock"
    );
    assert!(!probe_marker.exists(), "probe entered while lock was held");

    // SAFETY: this test acquired the exclusive lock above.
    assert_eq!(unsafe { flock(lock.as_raw_fd(), LOCK_UN) }, 0);
    let status = waiting_child.wait().unwrap();
    assert!(status.success(), "shared-lock child failed after unlock");
    assert_eq!(fs::read(&waiting_marker).unwrap(), b"entered");
}

#[test]
fn every_shipped_runner_exec_start_uses_the_shared_transaction_lock() {
    let unit_files = [
        include_str!("../debian/velnor-controller@.service"),
        include_str!("../debian/velnor-daemon.service"),
        include_str!("../debian/velnor-daemon@.service"),
        include_str!("../debian/velnor-doctor.service"),
        include_str!("../debian/velnor-doctor@.service"),
        include_str!("../debian/velnor-guardian.service"),
        include_str!("../debian/velnor-job@.service"),
        include_str!("../debian/velnor-slot@.service"),
    ];
    let mut exec_start_count = 0;
    for contents in unit_files {
        for line in contents.lines() {
            let Some(command) = line.strip_prefix("ExecStart=") else {
                continue;
            };
            exec_start_count += 1;
            assert!(
                command.starts_with(
                    "/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock "
                ),
                "shipped ExecStart lacks the shared package lock: {line}"
            );
            let runner_command = command
                .strip_prefix(
                    "/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock ",
                )
                .expect("shared lock wrapper prefix");
            assert!(
                runner_command.starts_with("/usr/bin/velnor-runner ")
                    || runner_command.starts_with("/usr/bin/velnorctl "),
                "shipped ExecStart does not hold the shared lock before runner work: {line}"
            );
        }
    }
    assert_eq!(exec_start_count, unit_files.len());
}

fn run_identity_check(
    manifest_present: bool,
    package_version: &str,
    shipped_crate_version: &str,
) -> (Output, TestTempDir) {
    let temp = TestTempDir::new();
    let share = temp.path().join("share");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&share).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();

    let manifest = b"{\"manifest\":\"fixture-v1\"}\n";
    if manifest_present {
        fs::write(share.join("manifest.json"), manifest).unwrap();
    }
    let manifest_hash = if manifest_present {
        let output = Command::new("sha256sum")
            .arg(share.join("manifest.json"))
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned()
    } else {
        "0".repeat(64)
    };
    fs::write(
        share.join("build-identity.json"),
        format!(
            "{{\"source_sha\":\"{}\",\"crate_version\":\"{shipped_crate_version}\",\"debian_version\":\"1.2.3\",\"manifest_sha256\":\"{manifest_hash}\"}}\n",
            "a".repeat(40),
        ),
    )
    .unwrap();

    let runner = bin_dir.join("velnor-runner");
    fs::write(
        &runner,
        format!(
            "#!/bin/sh\ncase \"$1:$2\" in\n  release:export) printf '%s\\n' '{{\"source_sha\":\"{}\",\"crate_version\":\"1.2.3\"}}' ;;\n  capabilities:export) printf '%s\\n' '{{\"manifest\":\"fixture-v1\"}}' ;;\n  *) exit 2 ;;\nesac\n",
            "a".repeat(40)
        ),
    )
    .unwrap();
    fs::set_permissions(&runner, fs::Permissions::from_mode(0o755)).unwrap();

    let sha256sum = bin_dir.join("sha256sum");
    fs::write(
        &sha256sum,
        "#!/bin/sh\nif command -v shasum >/dev/null 2>&1; then exec shasum -a 256 \"$@\"; fi\nexec /usr/bin/sha256sum \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&sha256sum, fs::Permissions::from_mode(0o755)).unwrap();

    let dpkg_query = bin_dir.join("dpkg-query");
    fs::write(
        &dpkg_query,
        "#!/bin/sh\nprintf '%s\\n' \"$TEST_PACKAGE_VERSION\"\n",
    )
    .unwrap();
    fs::set_permissions(&dpkg_query, fs::Permissions::from_mode(0o755)).unwrap();

    let postinst = include_str!("../debian/postinst");
    let function_start = postinst
        .find("verify_package_identity()")
        .expect("package identity verifier");
    let function_end = postinst[function_start..]
        .find("\ncase \"$1\" in")
        .map(|offset| function_start + offset)
        .unwrap();
    let function = &postinst[function_start..function_end];
    let script = format!(
        "set -eu\nfail() {{ echo \"$1\" >&2; exit 1; }}\nBIN=\"{}\"\nSHARE=\"{}\"\n{function}\nverify_package_identity\n",
        runner.display(),
        share.display()
    );
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .env("TEST_PACKAGE_VERSION", package_version)
        .output()
        .unwrap();
    (output, temp)
}

fn run_drain_check(service_rows: &str, timer_rows: &str, query_failure: bool) -> Output {
    run_drain_check_with(service_rows, timer_rows, query_failure, "normal", false)
}

fn run_drain_check_with(
    service_rows: &str,
    timer_rows: &str,
    query_failure: bool,
    mode: &str,
    custom_runner: bool,
) -> Output {
    run_drain_check_with_unit_files(
        service_rows,
        timer_rows,
        query_failure,
        mode,
        custom_runner,
        "",
    )
}

fn run_drain_check_with_unit_files(
    service_rows: &str,
    timer_rows: &str,
    query_failure: bool,
    mode: &str,
    custom_runner: bool,
    service_unit_file_rows: &str,
) -> Output {
    run_drain_check_with_unit_files_and_dbus_file(
        service_rows,
        timer_rows,
        query_failure,
        mode,
        custom_runner,
        service_unit_file_rows,
        None,
        None,
    )
}

fn run_drain_check_with_dbus_file(
    service_rows: &str,
    timer_rows: &str,
    mode: &str,
    custom_runner: bool,
    activation_contents: &str,
) -> Output {
    run_drain_check_with_unit_files_and_dbus_file(
        service_rows,
        timer_rows,
        false,
        mode,
        custom_runner,
        "",
        Some(("org.example.Service.service", activation_contents)),
        None,
    )
}

fn run_drain_check_with_dbus_config(config_contents: &str) -> Output {
    run_drain_check_with_unit_files_and_dbus_file(
        "",
        "",
        false,
        "dbus-file-config-error",
        false,
        "",
        None,
        Some(config_contents),
    )
}

fn run_drain_check_with_unit_files_and_dbus_file(
    service_rows: &str,
    timer_rows: &str,
    query_failure: bool,
    mode: &str,
    custom_runner: bool,
    service_unit_file_rows: &str,
    activation_file: Option<(&str, &str)>,
    config_contents: Option<&str>,
) -> Output {
    run_drain_check_with_unit_files_and_dbus_file_from_script(
        service_rows,
        timer_rows,
        query_failure,
        mode,
        custom_runner,
        service_unit_file_rows,
        activation_file,
        config_contents,
        include_str!("../debian/postinst"),
    )
}

fn run_drain_check_with_unit_files_and_dbus_file_from_script(
    service_rows: &str,
    timer_rows: &str,
    query_failure: bool,
    mode: &str,
    custom_runner: bool,
    service_unit_file_rows: &str,
    activation_file: Option<(&str, &str)>,
    config_contents: Option<&str>,
    package_script: &str,
) -> Output {
    let temp = TestTempDir::new();
    let activation_dir = temp.path().join("dbus-system-services");
    let extensionless_wrapper = temp.path().join("extensionless-wrapper");
    let dbus_reload_marker = temp.path().join("dbus-reload-called");
    let owner_query_count = temp.path().join("systemd-bus-owner-query-count");
    let job_barrier_marker = temp.path().join("systemd-list-jobs-barrier-called");
    let job_query_marker = temp.path().join("systemctl-list-jobs-called");
    let systemctl_query_marker = temp.path().join("systemctl-query-called");
    let service_list_count = temp.path().join("service-list-count");
    let purge_sentinel = temp.path().join("legacy-purge-sentinel");
    let wrapper_contents = match mode {
        "dbus-file-python-common" => "print('unrelated service')\n",
        "dbus-file-direct-extensionless" => {
            "#!/bin/sh\nexec /opt/copied-runner daemon --name alpha\n"
        }
        _ => "exec /opt/copied-runner daemon --name alpha\n",
    };
    fs::write(&extensionless_wrapper, wrapper_contents).unwrap();
    if mode == "dbus-file-direct-extensionless" {
        let mut permissions = fs::metadata(&extensionless_wrapper).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&extensionless_wrapper, permissions).unwrap();
    }
    fs::create_dir_all(&activation_dir).unwrap();
    fs::write(&purge_sentinel, b"preserve until drain proof").unwrap();
    if let Some((file_name, contents)) = activation_file {
        let contents = contents.replace(
            "/opt/extensionless-wrapper",
            &extensionless_wrapper.display().to_string(),
        );
        fs::write(activation_dir.join(file_name), contents).unwrap();
    }
    let config_dir = temp.path().join("dbus-config");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("system.conf"),
        config_contents
            .unwrap_or("<busconfig><include>nested.conf</include></busconfig>\n")
            .replace(
                "@DBUS_ACTIVATION_DIR@",
                &activation_dir.display().to_string(),
            ),
    )
    .unwrap();
    if config_contents.is_none() {
        fs::write(
            config_dir.join("nested.conf"),
            format!(
                "<busconfig><servicedir>{}</servicedir></busconfig>\n",
                activation_dir.display()
            ),
        )
        .unwrap();
    }
    let function_start = package_script
        .find("systemd_property()")
        .expect("systemd property and drain helpers");
    let function_end = ["\nmicrovm_verify_sha256()", "\ncase \"$1\" in"]
        .into_iter()
        .filter_map(|marker| {
            package_script[function_start..]
                .find(marker)
                .map(|offset| function_start + offset)
        })
        .min()
        .expect("maintainer script helper boundary");
    let functions = &package_script[function_start..function_end];
    let script = r#"set -eu
PACKAGE_TRANSACTION_LOCK=/run/velnor/package-transaction.lock
sha256sum() {
  case "$TEST_MODE:$1" in
    tampered-cache-fragment:/usr/lib/systemd/system/velnor-cache-gc.service|tampered-cache-fragment:/lib/systemd/system/velnor-cache-gc.service) printf '%s  %s\n' 0000000000000000000000000000000000000000000000000000000000000000 "$1" ;;
    :/usr/lib/systemd/system/velnor-cache-gc.service|:/lib/systemd/system/velnor-cache-gc.service|*/usr/lib/systemd/system/velnor-cache-gc.service|*/lib/systemd/system/velnor-cache-gc.service) printf '%s  %s\n' ad79a2d4b611b79b96c62fba8d0ef4faf7c5756b6105dea181aeeef4d19af060 "$1" ;;
    */usr/lib/systemd/system/velnor-cache-gc.timer|*/lib/systemd/system/velnor-cache-gc.timer) printf '%s  %s\n' 07b39f9d1e8ffcaa36e294ebec2328b10ad0d8b4f9779aa4ffb427a4af667cca "$1" ;;
    */usr/lib/systemd/system/velnor-doctor.service|*/lib/systemd/system/velnor-doctor.service) printf '%s  %s\n' 54497b95693b59b9afcd820551d7caa320ae3fbfa8a2c2d76639c828f61498e4 "$1" ;;
    */usr/lib/systemd/system/velnor-doctor.timer|*/lib/systemd/system/velnor-doctor.timer) printf '%s  %s\n' e5875dc62c7dd165c939890d38bc7112fba3765f13ef0324c457cb71043bafc3 "$1" ;;
    */usr/lib/systemd/system/velnor-doctor@.service|*/lib/systemd/system/velnor-doctor@.service) printf '%s  %s\n' 0df90fe8ef64187cb4e9aa9cb25da19f2f10f6d4438b1945b487a9a93dd3a831 "$1" ;;
    */usr/lib/systemd/system/velnor-doctor@.timer|*/lib/systemd/system/velnor-doctor@.timer) printf '%s  %s\n' 1a5fc6f3b410b18fc2af230e6e86366d4e964d26e6ced22beb32aa4dad45bb0f "$1" ;;
    fleet-audit:/usr/lib/systemd/system/velnor-fleet-policy-audit.service|fleet-audit:/lib/systemd/system/velnor-fleet-policy-audit.service) printf '%s  %s\n' 09252a8f67de6dec269c2aaf50fe1e0a750942d55fbbd3bf1c60adfa482981c1 "$1" ;;
    fleet-audit:/usr/lib/systemd/system/velnor-fleet-policy-audit.timer|fleet-audit:/lib/systemd/system/velnor-fleet-policy-audit.timer) printf '%s  %s\n' 6560ebfd20be889f6a1a3d1a474582d1ea8a7438be287073cf619a36b3481889 "$1" ;;
    */usr/lib/systemd/system/velnor-fleet-policy-audit.service|*/lib/systemd/system/velnor-fleet-policy-audit.service) printf '%s  %s\n' 09252a8f67de6dec269c2aaf50fe1e0a750942d55fbbd3bf1c60adfa482981c1 "$1" ;;
    */usr/lib/systemd/system/velnor-fleet-policy-audit.timer|*/lib/systemd/system/velnor-fleet-policy-audit.timer) printf '%s  %s\n' 6560ebfd20be889f6a1a3d1a474582d1ea8a7438be287073cf619a36b3481889 "$1" ;;
    *) command sha256sum "$@" ;;
  esac
}
    systemctl() {
    case "$1" in
    list-jobs)
      [ -f "$TEST_DBUS_RELOAD_MARKER" ] || return 91
      : > "$TEST_JOB_QUERY_MARKER"
      [ "$TEST_QUERY_FAILURE" = 0 ] || return 1
      printf '%s' "$TEST_JOB_ROWS" ;;
    list-units)
      [ -f "$TEST_DBUS_RELOAD_MARKER" ] || return 91
      : > "$TEST_SYSTEMCTL_QUERY_MARKER"
      [ "$TEST_QUERY_FAILURE" = 0 ] || return 1
      case "$*" in
        *--type=service*)
          if [ "$TEST_MODE" = dbus-activation-starts-during-scan ]; then
            service_count=0
            [ ! -f "$TEST_SERVICE_LIST_COUNT" ] || IFS= read -r service_count < "$TEST_SERVICE_LIST_COUNT"
            service_count=$((service_count + 1))
            printf '%s\n' "$service_count" > "$TEST_SERVICE_LIST_COUNT"
            if [ "$service_count" -ge 2 ]; then
              printf '%s\n' 'foo.service loaded activating start'
            else
              printf '%s' "$TEST_SERVICE_ROWS"
            fi
          else
            printf '%s' "$TEST_SERVICE_ROWS"
          fi ;;
        *--type=timer*) printf '%s' "$TEST_TIMER_ROWS" ;;
        *--type=socket*) printf '%s' "$TEST_SOCKET_ROWS" ;;
        *--type=path*) printf '%s' "$TEST_PATH_ROWS" ;;
        *) return 1 ;;
      esac ;;
    list-unit-files)
      [ "$TEST_QUERY_FAILURE" = 0 ] || return 1
      case "$*" in
        *--type=service*) printf '%s' "$TEST_SERVICE_UNIT_FILES" ;;
        *) return 1 ;;
      esac ;;
    show)
      property=
      unit=
      property_count=0
      for arg in "$@"; do
        case "$arg" in
          --property=*) property=${arg#--property=}; property_count=$((property_count + 1)) ;;
          show|--value|--no-pager) ;;
          *) unit=$arg ;;
        esac
      done
      if [ -z "$unit" ] && [ "$property" = Version ]; then
        if [ "$TEST_MODE" = no-systemd ]; then
          return 1
        elif [ "$TEST_MODE" = old-systemd ]; then
          printf '%s\n' '234 (old manager fixture)'
        else
          printf '%s\n' '252 (test manager fixture)'
        fi
        return 0
      fi
      if [ "$property_count" -gt 1 ]; then
        if { [ "$unit" = foo.service ] || [ "$unit" = alias.service ]; } \
          && [ "$TEST_CUSTOM_RUNNER" = 1 ]; then
          if [ "$TEST_MODE" = custom-shell ]; then
            printf '%s' '{ path=/bin/sh ; argv[]=/bin/sh /opt/maintain.sh ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }'
          elif [ "$TEST_MODE" = dbus-python ]; then
            printf '{ path=/usr/bin/python3.12 ; argv[]=/usr/bin/python3.12 %s ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }' "$TEST_WRAPPER_PATH"
          elif [ "$TEST_MODE" = dbus-common ]; then
            printf '%s' '{ path=/usr/bin/true ; argv[]=/usr/bin/true ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }'
          else
            printf '%s' '{ path=/opt/copied-runner ; argv[]=/opt/copied-runner daemon --name alpha --labels test --slots 2 --work-dir /srv/work ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }'
          fi
        fi
        return 0
      fi
      if [ "$TEST_MODE" = property-query-failure ] \
        && [ "$unit" = velnor-cache-gc.service ] \
        && [ "$property" = DropInPaths ]; then
        return 1
      fi
      if [ "$TEST_MODE" = dbus-query-failure ] \
        && [ "$unit" = foo.service ] && [ "$property" = BusName ]; then
        return 1
      fi
      case "$unit:$property" in
        dbus.service:ActiveState) printf active ;;
        dbus.service:NeedDaemonReload) printf no ;;
        dbus.service:ExecStart)
          if [ "$TEST_MODE" = dbus-spoof-binary ]; then
            printf '%s' '{ path=/opt/custom/dbus-daemon ; argv[]=/opt/custom/dbus-daemon --system ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=1 ; code=(null) ; status=0/0 }'
          elif [ "$TEST_MODE" = dbus-legacy-binary ]; then
            printf '%s' '{ path=/usr/sbin/dbus-daemon ; argv[]=/usr/sbin/dbus-daemon --system ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=1 ; code=(null) ; status=0/0 }'
          elif [ "$TEST_MODE" = dbus-argv-path-mismatch ]; then
            printf '%s' '{ path=/usr/bin/dbus-daemon ; argv[]=/usr/sbin/dbus-daemon --system ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=1 ; code=(null) ; status=0/0 }'
          elif [ "$TEST_MODE" = dbus-broker ]; then
            printf '%s' '{ path=/usr/bin/dbus-broker-launch ; argv[]=/usr/bin/dbus-broker-launch --scope system ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=1 ; code=(null) ; status=0/0 }'
          else
            printf '{ path=/usr/bin/dbus-daemon ; argv[]=/usr/bin/dbus-daemon --config-file=%s ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=1 ; code=(null) ; status=0/0 }' "$TEST_DBUS_CONFIG"
          fi ;;
        foo.service:Type)
          case "$TEST_MODE" in dbus-python|dbus-common|dbus-query-failure) printf dbus ;; *) printf simple ;; esac ;;
        foo.service:BusName)
          case "$TEST_MODE" in dbus-busname|dbus-common) printf org.example.Service ;; esac ;;
        velnor-cache-gc.service:Type|velnor-doctor.service:Type|velnor-doctor@alpha.service:Type|velnor-fleet-policy-audit.service:Type) printf oneshot ;;
        velnor-cache-gc.service:NeedDaemonReload|velnor-cache-gc.timer:NeedDaemonReload|velnor-doctor.service:NeedDaemonReload|velnor-doctor.timer:NeedDaemonReload|velnor-doctor@alpha.service:NeedDaemonReload|velnor-doctor@alpha.timer:NeedDaemonReload|velnor-fleet-policy-audit.service:NeedDaemonReload|velnor-fleet-policy-audit.timer:NeedDaemonReload) printf no ;;
        velnor-cache-gc.service:DropInPaths|velnor-cache-gc.timer:DropInPaths|velnor-cache-gc.service:ExecCondition|velnor-cache-gc.service:ExecStartPre|velnor-cache-gc.service:ExecStartPost|velnor-cache-gc.service:ExecReload|velnor-cache-gc.service:ExecStop|velnor-cache-gc.service:ExecStopPost|velnor-doctor.service:DropInPaths|velnor-doctor.timer:DropInPaths|velnor-doctor.service:ExecCondition|velnor-doctor.service:ExecStartPre|velnor-doctor.service:ExecStartPost|velnor-doctor.service:ExecReload|velnor-doctor.service:ExecStop|velnor-doctor.service:ExecStopPost|velnor-doctor@alpha.service:DropInPaths|velnor-doctor@alpha.timer:DropInPaths|velnor-doctor@alpha.service:ExecCondition|velnor-doctor@alpha.service:ExecStartPre|velnor-doctor@alpha.service:ExecStartPost|velnor-doctor@alpha.service:ExecReload|velnor-doctor@alpha.service:ExecStop|velnor-doctor@alpha.service:ExecStopPost|velnor-fleet-policy-audit.service:DropInPaths|velnor-fleet-policy-audit.timer:DropInPaths|velnor-fleet-policy-audit.service:ExecCondition|velnor-fleet-policy-audit.service:ExecStartPre|velnor-fleet-policy-audit.service:ExecStartPost|velnor-fleet-policy-audit.service:ExecReload|velnor-fleet-policy-audit.service:ExecStop|velnor-fleet-policy-audit.service:ExecStopPost)
          if [ "$TEST_MODE" = extra-condition ] && [ "$property" = ExecCondition ]; then printf '/bin/true'; fi ;;
        velnor-cache-gc.service:FragmentPath) printf /usr/lib/systemd/system/velnor-cache-gc.service ;;
        velnor-cache-gc.service:ExecStart) printf '%s' "$TEST_EXEC_START" ;;
        velnor-cache-gc.timer:FragmentPath) printf /usr/lib/systemd/system/velnor-cache-gc.timer ;;
        velnor-cache-gc.timer:Triggers) printf velnor-cache-gc.service ;;
        velnor-doctor.service:FragmentPath) printf /usr/lib/systemd/system/velnor-doctor.service ;;
        velnor-doctor.service:ExecStart) printf '%s' "$TEST_DOCTOR_EXEC_START" ;;
        velnor-doctor.timer:FragmentPath) printf /usr/lib/systemd/system/velnor-doctor.timer ;;
        velnor-doctor.timer:Triggers) printf velnor-doctor.service ;;
        velnor-doctor@alpha.service:FragmentPath) printf /usr/lib/systemd/system/velnor-doctor@.service ;;
        velnor-doctor@alpha.service:ExecStart) printf '%s' "$TEST_DOCTOR_EXEC_START" ;;
        velnor-doctor@alpha.timer:FragmentPath) printf /usr/lib/systemd/system/velnor-doctor@.timer ;;
        velnor-doctor@alpha.timer:Triggers) printf velnor-doctor@alpha.service ;;
        velnor-fleet-policy-audit.service:FragmentPath) printf /usr/lib/systemd/system/velnor-fleet-policy-audit.service ;;
        velnor-fleet-policy-audit.service:ExecStart) printf '%s' "$TEST_FLEET_EXEC_START" ;;
        velnor-fleet-policy-audit.timer:FragmentPath) printf /usr/lib/systemd/system/velnor-fleet-policy-audit.timer ;;
        velnor-fleet-policy-audit.timer:Triggers) printf velnor-fleet-policy-audit.service ;;
        foo.socket:Triggers|foo.path:Triggers) printf foo.service ;;
        *:Type) printf simple ;;
        *:BusName) ;;
        *) return 1 ;;
      esac ;;
    *) return 1 ;;
  esac
}
busctl() {
  case "$*" in
    *GetNameOwner*)
      owner_count=0
      [ ! -f "$TEST_OWNER_QUERY_COUNT" ] || IFS= read -r owner_count < "$TEST_OWNER_QUERY_COUNT"
      owner_count=$((owner_count + 1))
      printf '%s\n' "$owner_count" > "$TEST_OWNER_QUERY_COUNT"
      if [ "$TEST_MODE" = dbus-owner-disconnect ] && [ "$owner_count" -ge 2 ]; then
        return 1
      fi
      if [ "$TEST_MODE" = dbus-owner-reconnect ] && [ "$owner_count" -ge 2 ]; then
        printf '%s\n' 's ":1.45"'
      else
        printf '%s\n' 's ":1.44"'
      fi ;;
    *ReloadConfig*)
      : > "$TEST_DBUS_RELOAD_MARKER"
      [ "$TEST_MODE" != dbus-reload-failure ] ;;
    *org.freedesktop.systemd1.Manager\ ListJobs*)
      : > "$TEST_JOB_BARRIER_MARKER"
      [ "$TEST_MODE" != dbus-job-barrier-failure ] || return 1
      printf '%s\n' "$TEST_BUSCTL_JOBS" ;;
    *) return 1 ;;
  esac
}
{functions}
if [ "$TEST_MODE" = dbus-selinux-enabled ]; then
  dbus_selinux_enabled() { return 0; }
elif [ "$TEST_MODE" = dbus-selinux-disabled ]; then
  dbus_selinux_enabled() { return 1; }
fi
no_live_packaged_runner_process() { return 0; }
active_units=
if all_velnor_units_drained; then
  rm -f "$TEST_PURGE_SENTINEL"
else
  [ ! -f "$TEST_PURGE_SENTINEL" ] || printf 'purge-marker-preserved\n' >&2
  [ -f "$TEST_DBUS_RELOAD_MARKER" ] || printf 'dbus-reload-not-called\n' >&2
  [ -f "$TEST_JOB_BARRIER_MARKER" ] || printf 'systemd-job-barrier-not-called\n' >&2
  [ -f "$TEST_SYSTEMCTL_QUERY_MARKER" ] || printf 'drain-failed-before-systemctl-query\n' >&2
  printf 'active units: %s\n' "$active_units" >&2
  exit 1
fi
"#
    .replace("{functions}", functions);
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        // Keep command resolution stable if another test changes the parent
        // process environment. systemctl and busctl are shell mocks above.
        .env("PATH", "/usr/bin:/bin")
        .env("TEST_SERVICE_ROWS", service_rows)
        .env("TEST_SERVICE_UNIT_FILES", service_unit_file_rows)
        .env("TEST_DBUS_CONFIG", config_dir.join("system.conf"))
        .env("TEST_TIMER_ROWS", timer_rows)
        .env(
            "TEST_SOCKET_ROWS",
            if mode == "custom-socket" {
                "foo.socket loaded active listening\n"
            } else {
                ""
            },
        )
        .env(
            "TEST_PATH_ROWS",
            if mode == "custom-path" {
                "foo.path loaded active waiting\n"
            } else {
                ""
            },
        )
        .env("TEST_QUERY_FAILURE", if query_failure { "1" } else { "0" })
        .env("TEST_CUSTOM_RUNNER", if custom_runner { "1" } else { "0" })
        .env("TEST_MODE", mode)
        .env("TEST_OWNER_QUERY_COUNT", &owner_query_count)
        .env("TEST_WRAPPER_PATH", &extensionless_wrapper)
        .env("TEST_DBUS_RELOAD_MARKER", &dbus_reload_marker)
        .env("TEST_JOB_BARRIER_MARKER", &job_barrier_marker)
        .env("TEST_JOB_QUERY_MARKER", &job_query_marker)
        .env(
            "TEST_JOB_ROWS",
            if mode == "dbus-pending-runner-job" {
                "123 foo.service start waiting\n"
            } else {
                "No jobs running.\n"
            },
        )
        .env(
            "TEST_BUSCTL_JOBS",
            if mode == "dbus-pending-runner-job" {
                "a(usssoo) 1 123 foo.service start waiting /org/freedesktop/systemd1/job/123 /org/freedesktop/systemd1/unit/foo_2eservice"
            } else {
                "a(usssoo) 0"
            },
        )
        .env("TEST_SYSTEMCTL_QUERY_MARKER", &systemctl_query_marker)
        .env("TEST_SERVICE_LIST_COUNT", &service_list_count)
        .env("TEST_PURGE_SENTINEL", &purge_sentinel)
        .env(
            "TEST_EXEC_START",
            "{ path=/usr/bin/flock ; argv[]=/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock /usr/bin/velnorctl cache gc --yes ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }",
        )
        .env(
            "TEST_DOCTOR_EXEC_START",
            "{ path=/usr/bin/flock ; argv[]=/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock /usr/bin/velnorctl doctor --url ${VELNOR_URL} --name ${VELNOR_NAME} --slots ${VELNOR_SLOTS} ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }",
        )
        .env(
            "TEST_FLEET_EXEC_START",
            "{ path=/usr/bin/flock ; argv[]=/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock /bin/sh -c audit ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }",
        )
        .output()
        .unwrap()
}

#[test]
fn postinst_proves_every_gate_before_package_mutation_and_purge() {
    let postinst = include_str!("../debian/postinst");
    let preinst = include_str!("../debian/preinst");
    for script in [preinst, postinst] {
        let drain_start = script
            .find("all_velnor_units_drained()")
            .expect("host drain helper");
        let drain_end = script[drain_start..]
            .find("\ncase \"$1\" in")
            .map(|offset| drain_start + offset)
            .expect("host drain helper end");
        let drain_helper = &script[drain_start..drain_end];
        let version = drain_helper
            .find("systemd_activation_version_supported")
            .expect("supported systemd activation boundary");
        let reload = drain_helper
            .find("reload_system_bus_activation_cache")
            .expect("synchronous D-Bus cache reload");
        let first_inventory = drain_helper
            .find("services_raw=$(systemctl list-units")
            .expect("initial service inventory");
        let activation_scan = drain_helper
            .find("dbus_activation_service_files_safe")
            .expect("strict D-Bus activation file scan");
        let jobs_barrier = drain_helper
            .find("verify_pending_systemd_activation_jobs")
            .expect("post-reload systemd ListJobs job snapshot");
        let final_inventory = drain_helper
            .find("services_after_reload_raw=$(systemctl list-units")
            .expect("post-scan service inventory");
        let process_recheck = drain_helper
            .find("if ! no_live_packaged_runner_process")
            .expect("post-scan process inventory");
        assert!(version < reload && reload < first_inventory);
        assert!(first_inventory < activation_scan);
        assert!(activation_scan < jobs_barrier && jobs_barrier < final_inventory);
        assert!(final_inventory < process_recheck);
        assert!(script.contains("org.freedesktop.systemd1.Manager ListJobs"));
        assert!(drain_helper.contains("not an activation fence"));
        assert!(drain_helper.contains("add extra detection for work already in flight"));
    }
    let configure = postinst
        .split_once("case \"$1\" in\n  configure)")
        .expect("postinst configure branch")
        .1;
    let lock = configure
        .find("require_package_transaction_lock")
        .expect("package transaction lock gate");
    let systemd_guard = configure
        .find("[ -d /run/systemd/system ]")
        .expect("mandatory systemd runtime guard");
    let systemd_required = configure
        .find("refusing package configuration without systemd")
        .expect("non-systemd configure refusal");
    let drain = configure
        .find("all_velnor_units_drained || fail")
        .expect("whole-host drain gate");
    let identity = configure
        .find("verify_package_identity\n")
        .expect("required package identity proof");
    let reload = configure
        .find("systemctl daemon-reload")
        .expect("manager reload before root proof");
    let roots = configure
        .find("storage verify-legacy")
        .expect("non-mutating configured-root proof");
    let remove_dropin = configure
        .find("remove_stale_jobs_cpu_quota_dropin\n")
        .expect("stale drop-in staging");
    let slice = configure
        .find("verify_jobs_slice_has_no_resource_ceilings\n")
        .expect("post-cleanup resource proof");
    let first_other_mutation = configure
        .find("systemd-tmpfiles --create")
        .expect("first ordinary package setup mutation");
    let token_migration = configure
        .find("sed -i 's/^GITHUB_TOKEN")
        .expect("token migration");
    let timer = configure
        .find("systemctl enable --now velnor-cache-gc.timer")
        .expect("maintenance timer setup");
    let purge = configure
        .find("storage purge-legacy")
        .expect("legacy storage purge hook");

    assert!(lock < systemd_guard && systemd_guard < systemd_required && systemd_required < drain);
    assert!(drain < identity && identity < reload && reload < roots);
    assert!(roots < remove_dropin && remove_dropin < slice);
    assert!(slice < first_other_mutation && first_other_mutation < token_migration);
    assert!(token_migration < timer && timer < purge);
    assert!(postinst.contains("restore_stale_jobs_cpu_quota_dropin"));
    assert!(postinst.contains("trap 'restore_stale_jobs_cpu_quota_dropin"));
    assert!(postinst.contains("manifest.json\" ] && [ ! -L \"$SHARE/manifest.json\""));
    assert!(postinst.contains("installed_crate_version\" = \"$shipped_crate_version\""));
    assert!(postinst.contains("installed_package_version\" = \"$shipped_debian_version\""));
    assert!(postinst.contains("shipped_manifest_sha\" = \"$shipped_manifest_file_sha\""));
    assert!(!postinst.contains("VELNOR_DRAINED_UNITS"));
    assert!(!postinst.contains("scoped_units_drained"));
    assert_eq!(configure.matches("storage purge-legacy").count(), 1);
}

#[test]
fn preinst_requires_systemd_drain_proof_before_upgrade_or_install() {
    let preinst = include_str!("../debian/preinst");
    for action in ["upgrade", "install"] {
        let branch = preinst.split_once(&format!("  {action})")).unwrap().1;
        let systemd_guard = branch
            .find("[ -d /run/systemd/system ]")
            .expect("mandatory systemd runtime guard");
        let systemd = branch
            .find(&format!(
                "refusing {action} before package replacement without systemd"
            ))
            .expect("systemd refusal before package replacement");
        let drain = branch
            .find("all_velnor_units_drained")
            .expect("pre-unpack drain proof");
        assert!(
            systemd_guard < systemd && systemd < drain,
            "{action} must require systemd before examining even an empty inventory"
        );
    }
    assert!(!preinst.contains("VELNOR_DRAINED_UNITS"));
    assert!(!preinst.contains("scoped_units_drained"));
}

#[test]
fn missing_systemd_manager_fails_closed_with_empty_unit_inventory() {
    let output = run_drain_check_with("", "", false, "no-systemd", false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "empty inventory passed without a usable systemd manager"
    );
    assert!(stderr.contains("purge-marker-preserved"), "{stderr}");
    assert!(stderr.contains("dbus-reload-not-called"), "{stderr}");
    assert!(
        stderr.contains("drain-failed-before-systemctl-query"),
        "systemd proof must fail before inventory with no units present: {stderr}"
    );
}

#[test]
fn drain_gate_rejects_malformed_rows_active_services_and_query_errors() {
    assert!(run_drain_check("", "", false).status.success());
    assert!(run_drain_check(
        "velnor-daemon@alpha.service loaded inactive dead\n",
        "",
        false
    )
    .status
    .success());
    assert!(!run_drain_check("No units found.\n", "", false)
        .status
        .success());
    assert!(!run_drain_check(
        "velnor-daemon@alpha.service loaded active running\n",
        "",
        false
    )
    .status
    .success());
    assert!(!run_drain_check("", "", true).status.success());
    assert!(!run_drain_check_with(
        "velnor-cache-gc.service loaded active running\n",
        "",
        false,
        "property-query-failure",
        false,
    )
    .status
    .success());
    assert!(!run_drain_check_with(
        "velnor-cache-gc.service loaded active running\n",
        "",
        false,
        "tampered-cache-fragment",
        false,
    )
    .status
    .success());
}

#[test]
fn failed_dbus_reload_aborts_before_inventory_or_purge() {
    let output = run_drain_check_with("", "", false, "dbus-reload-failure", false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "failed D-Bus ReloadConfig passed");
    assert!(stderr.contains("purge-marker-preserved"), "{stderr}");
    assert!(
        stderr.contains("systemd-job-barrier-not-called"),
        "failed ReloadConfig must stop before the ListJobs barrier: {stderr}"
    );
    assert!(
        stderr.contains("drain-failed-before-systemctl-query"),
        "ReloadConfig failure must precede unit inventory: {stderr}"
    );
}

#[test]
fn unsupported_kdbus_era_systemd_aborts_before_reload_or_purge() {
    let output = run_drain_check_with("", "", false, "old-systemd", false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "systemd v234 passed the activation drain"
    );
    assert!(stderr.contains("purge-marker-preserved"), "{stderr}");
    assert!(stderr.contains("dbus-reload-not-called"), "{stderr}");
    assert!(
        stderr.contains("drain-failed-before-systemctl-query"),
        "unsupported systemd version must fail before unit inventory: {stderr}"
    );
}

#[test]
fn unsupported_dbus_broker_aborts_before_reload_or_purge() {
    let output = run_drain_check_with("", "", false, "dbus-broker", false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "dbus-broker passed the activation drain"
    );
    assert!(stderr.contains("purge-marker-preserved"), "{stderr}");
    assert!(stderr.contains("dbus-reload-not-called"), "{stderr}");
    assert!(
        stderr.contains("systemd-job-barrier-not-called"),
        "unsupported broker must fail before the ListJobs barrier: {stderr}"
    );
}

#[test]
fn dbus_daemon_path_and_argv_must_match_the_exact_systemd_binary_for_both_scripts() {
    for (script_name, package_script) in [
        ("preinst", include_str!("../debian/preinst")),
        ("postinst", include_str!("../debian/postinst")),
    ] {
        for mode in ["dbus-legacy-binary", "dbus-argv-path-mismatch"] {
            let output = run_drain_check_with_unit_files_and_dbus_file_from_script(
                "",
                "",
                false,
                mode,
                false,
                "",
                None,
                None,
                package_script,
            );
            assert!(
                !output.status.success(),
                "{script_name} accepted {mode}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("dbus-reload-not-called"),
                "{script_name} did not reject {mode} before reloading D-Bus: {stderr}"
            );
            assert!(
                stderr.contains("drain-failed-before-systemctl-query"),
                "{script_name} did not stop before inventory for {mode}: {stderr}"
            );
        }
    }
}

#[test]
fn changed_or_disconnected_systemd_bus_owner_aborts_before_purge() {
    for mode in ["dbus-owner-reconnect", "dbus-owner-disconnect"] {
        let output = run_drain_check_with("", "", false, mode, false);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "systemd bus owner change passed the activation drain in {mode}"
        );
        assert!(
            stderr.contains("purge-marker-preserved"),
            "{mode}: {stderr}"
        );
        assert!(
            !stderr.contains("systemd-job-barrier-not-called"),
            "{mode}: owner check must reach the barrier: {stderr}"
        );
    }
}

#[test]
fn drain_rechecks_services_started_during_dbus_cache_scan() {
    let output = run_drain_check_with("", "", false, "dbus-activation-starts-during-scan", true);
    assert!(
        !output.status.success(),
        "service activated during the D-Bus cache scan escaped the final unit check"
    );
}

#[test]
fn queued_dbus_systemd_activation_is_checked_after_list_jobs_snapshot() {
    let output = run_drain_check_with("", "", false, "dbus-pending-runner-job", true);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "queued D-Bus SystemdService activation escaped ListJobs: {stderr}"
    );
    assert!(
        stderr.contains("pending-service-job(foo.service:waiting)"),
        "queued service job was not identified: {stderr}"
    );
}

#[test]
fn failed_systemd_list_jobs_snapshot_refuses_before_purge() {
    let output = run_drain_check_with("", "", false, "dbus-job-barrier-failure", false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "failed systemd ListJobs barrier passed"
    );
    assert!(stderr.contains("purge-marker-preserved"), "{stderr}");
    assert!(
        !stderr.contains("systemd-job-barrier-not-called"),
        "ListJobs barrier did not run: {stderr}"
    );
    assert!(stderr.contains("active units: unknown"), "{stderr}");
}

#[test]
fn drain_gate_allows_only_exact_locked_maintenance_and_finds_custom_runner_units() {
    let cache_service = run_drain_check_with(
        "velnor-cache-gc.service loaded active running\n",
        "",
        false,
        "normal",
        false,
    );
    assert!(
        cache_service.status.success(),
        "exact cache-GC service failed: {}",
        String::from_utf8_lossy(&cache_service.stderr)
    );

    let cache_timer = run_drain_check("", "velnor-cache-gc.timer loaded active waiting\n", false);
    assert!(
        cache_timer.status.success(),
        "exact cache-GC timer failed: {}",
        String::from_utf8_lossy(&cache_timer.stderr)
    );

    let allowed = run_drain_check(
        "velnor-cache-gc.service loaded active running\n",
        "velnor-cache-gc.timer loaded active waiting\n",
        false,
    );
    assert!(
        allowed.status.success(),
        "exact locked maintenance helper failed: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );

    let bare_doctor = run_drain_check_with(
        "velnor-doctor.service loaded active running\n",
        "velnor-doctor.timer loaded active waiting\n",
        false,
        "doctor",
        false,
    );
    assert!(
        bare_doctor.status.success(),
        "exact bare doctor maintenance failed: {}",
        String::from_utf8_lossy(&bare_doctor.stderr)
    );

    assert!(!run_drain_check_with(
        "velnor-cache-gc.service loaded active running\n",
        "",
        false,
        "extra-condition",
        false,
    )
    .status
    .success());
    assert!(!run_drain_check_with(
        "foo.service loaded active running\n",
        "",
        false,
        "normal",
        true,
    )
    .status
    .success());
    assert!(!run_drain_check_with(
        "foo.service loaded active running\n",
        "",
        false,
        "custom-shell",
        true,
    )
    .status
    .success());

    let inactive_dbus_python = run_drain_check_with(
        "foo.service loaded inactive dead\n",
        "",
        false,
        "dbus-python",
        true,
    );
    assert!(
        !inactive_dbus_python.status.success(),
        "inactive Type=dbus service with a versioned Python wrapper passed"
    );

    let bus_name_only = run_drain_check_with(
        "foo.service loaded inactive dead\n",
        "",
        false,
        "dbus-busname",
        true,
    );
    assert!(
        !bus_name_only.status.success(),
        "inactive BusName service with a runner wrapper passed"
    );

    let installed_unloaded_dbus = run_drain_check_with_unit_files(
        "",
        "",
        false,
        "dbus-python",
        true,
        "foo.service disabled enabled\n",
    );
    assert!(
        !installed_unloaded_dbus.status.success(),
        "installed but never-loaded D-Bus service escaped the drain scan"
    );

    let ordinary_inactive_dbus = run_drain_check_with_unit_files(
        "",
        "",
        false,
        "dbus-common",
        true,
        "foo.service disabled enabled\n",
    );
    assert!(
        ordinary_inactive_dbus.status.success(),
        "ordinary inactive D-Bus service was rejected: {}",
        String::from_utf8_lossy(&ordinary_inactive_dbus.stderr)
    );

    let dbus_property_query_failure = run_drain_check_with(
        "foo.service loaded inactive dead\n",
        "",
        false,
        "dbus-query-failure",
        false,
    );
    assert!(
        !dbus_property_query_failure.status.success(),
        "failed effective BusName query did not fail closed"
    );

    let unsupported_dbus_binary = run_drain_check_with_dbus_file(
        "",
        "",
        "dbus-spoof-binary",
        false,
        "[D-BUS Service]\nName=org.example.Service\nExec=/usr/bin/example\n",
    );
    assert!(
        !unsupported_dbus_binary.status.success(),
        "custom executable named dbus-daemon did not fail closed"
    );

    let dbus_exec_python_wrapper = run_drain_check_with_dbus_file(
        "",
        "",
        "dbus-file-exec",
        false,
        "[D-BUS Service]\nName=org.example.Service\nExec=/usr/bin/python3.12 /opt/extensionless-wrapper\n",
    );
    assert!(
        !dbus_exec_python_wrapper.status.success(),
        "D-Bus Exec versioned Python extensionless wrapper passed"
    );

    let dbus_direct_extensionless_wrapper = run_drain_check_with_dbus_file(
        "",
        "",
        "dbus-file-direct-extensionless",
        false,
        "[D-BUS Service]\nName=org.example.Service\nExec=/opt/extensionless-wrapper\n",
    );
    assert!(
        !dbus_direct_extensionless_wrapper.status.success(),
        "direct extensionless shebang wrapper passed"
    );

    let dbus_systemd_alias = run_drain_check_with_dbus_file(
        "",
        "",
        "dbus-file-systemd-alias",
        true,
        "[D-BUS Service]\nName=org.example.Service\nExec=/usr/bin/example\nSystemdService=alias.service\n",
    );
    assert!(
        !dbus_systemd_alias.status.success(),
        "D-Bus SystemdService alias to a Type=notify Velnor wrapper passed"
    );

    let ordinary_dbus_exec = run_drain_check_with_dbus_file(
        "",
        "",
        "dbus-file-common",
        false,
        "[D-BUS Service]\nName=org.example.Service\nExec=/usr/bin/true\n",
    );
    assert!(
        ordinary_dbus_exec.status.success(),
        "ordinary D-Bus Exec was rejected: {}",
        String::from_utf8_lossy(&ordinary_dbus_exec.stderr)
    );

    let ordinary_dbus_python_exec = run_drain_check_with_dbus_file(
        "",
        "",
        "dbus-file-python-common",
        false,
        "[D-BUS Service]\nName=org.example.PythonService\nExec=/usr/bin/python3.12 /opt/extensionless-wrapper\n",
    );
    assert!(
        ordinary_dbus_python_exec.status.success(),
        "unrelated Python D-Bus service was rejected: {}",
        String::from_utf8_lossy(&ordinary_dbus_python_exec.stderr)
    );

    let ordinary_dbus_python_inline_exec = run_drain_check_with_dbus_file(
        "",
        "",
        "dbus-file-python-inline-common",
        false,
        "[D-BUS Service]\nName=org.example.PythonInlineService\nExec=/usr/bin/python3.12 -c pass\n",
    );
    assert!(
        ordinary_dbus_python_inline_exec.status.success(),
        "ordinary inline Python D-Bus service was rejected: {}",
        String::from_utf8_lossy(&ordinary_dbus_python_inline_exec.stderr)
    );

    let constructed_python_command = run_drain_check_with_dbus_file(
        "",
        "",
        "dbus-file-python-constructed-inline",
        false,
        "[D-BUS Service]\nName=org.example.ConstructedPythonService\nExec=/usr/bin/python3.12 -c __import__(chr(111)+chr(115)).system(chr(47)+chr(111)+chr(112)+chr(116)+chr(47)+chr(118)+chr(101)+chr(108)+chr(110)+chr(111)+chr(114)+chr(45)+chr(114)+chr(117)+chr(110)+chr(110)+chr(101)+chr(114)+chr(32)+chr(100)+chr(97)+chr(101)+chr(109)+chr(111)+chr(110))\n",
    );
    assert!(
        !constructed_python_command.status.success(),
        "constructed daemon command in inline Python passed: {}",
        String::from_utf8_lossy(&constructed_python_command.stderr)
    );

    let attached_python_code = run_drain_check_with_dbus_file(
        "",
        "",
        "dbus-file-python-attached-inline",
        false,
        "[D-BUS Service]\nName=org.example.AttachedPythonService\nExec=/usr/bin/python3.12 -cpass\n",
    );
    assert!(
        !attached_python_code.status.success(),
        "attached inline Python code option passed"
    );

    let nested_env_python_pass = "[D-BUS Service]\nName=org.example.NestedEnvPassService\nExec=/usr/bin/env /usr/bin/env python3.12 -c pass\n";
    for (script_name, package_script) in [
        ("preinst", include_str!("../debian/preinst")),
        ("postinst", include_str!("../debian/postinst")),
    ] {
        let inert_nested_env = run_drain_check_with_unit_files_and_dbus_file_from_script(
            "",
            "",
            false,
            "dbus-file-nested-env-python-pass",
            false,
            "",
            Some((
                "org.example.NestedEnvPassService.service",
                nested_env_python_pass,
            )),
            None,
            package_script,
        );
        assert!(
            inert_nested_env.status.success(),
            "Rust-equivalent nested env python -c pass was not inert in {script_name}: {}",
            String::from_utf8_lossy(&inert_nested_env.stderr)
        );
    }

    let nested_env_activation = "[D-BUS Service]\nName=org.example.NestedEnvService\nExec=/usr/bin/env /usr/bin/env python3.12 -c __import__(chr(111)+chr(115)).system(chr(47)+chr(111)+chr(112)+chr(116)+chr(47)+chr(118)+chr(101)+chr(108)+chr(110)+chr(111)+chr(114)+chr(45)+chr(114)+chr(117)+chr(110)+chr(110)+chr(101)+chr(114)+chr(32)+chr(100)+chr(97)+chr(101)+chr(109)+chr(111)+chr(110))\n";
    for (script_name, package_script) in [
        ("preinst", include_str!("../debian/preinst")),
        ("postinst", include_str!("../debian/postinst")),
    ] {
        let nested_env_constructed_command =
            run_drain_check_with_unit_files_and_dbus_file_from_script(
                "",
                "",
                false,
                "dbus-file-nested-env-constructed-inline",
                false,
                "",
                Some((
                    "org.example.NestedEnvService.service",
                    nested_env_activation,
                )),
                None,
                package_script,
            );
        assert!(
            !nested_env_constructed_command.status.success(),
            "nested env command hid constructed inline Python in {script_name}: {}",
            String::from_utf8_lossy(&nested_env_constructed_command.stderr)
        );
    }

    let unsupported_dbus_continuation = run_drain_check_with_dbus_file(
        "",
        "",
        "dbus-file-continuation",
        false,
        "[D-BUS Service]\nName=org.example.Service\nExec=/usr/bin/example \\\n --option\n",
    );
    assert!(
        !unsupported_dbus_continuation.status.success(),
        "unsupported D-Bus activation continuation did not fail closed"
    );

    let unresolved_dbus_include = run_drain_check_with_dbus_config(
        "<busconfig><include>missing.conf</include></busconfig>\n",
    );
    assert!(
        !unresolved_dbus_include.status.success(),
        "unresolved required D-Bus config include did not fail closed"
    );

    let unresolved_dbus_service_path = run_drain_check_with_dbus_config(
        "<busconfig><servicedir>/missing/dbus/system-services</servicedir></busconfig>\n",
    );
    assert!(
        !unresolved_dbus_service_path.status.success(),
        "unresolved configured D-Bus servicedir did not fail closed"
    );

    let entity_hidden_dbus_activation = run_drain_check_with_unit_files_and_dbus_file(
        "",
        "",
        false,
        "dbus-file-entity",
        false,
        "",
        Some((
            "org.example.Entity.service",
            "[D-BUS Service]\nName=org.example.Entity\nExec=/usr/bin/python3.12 /opt/extensionless-wrapper\n",
        )),
        Some(
            "<!DOCTYPE busconfig [<!ENTITY services \"&lt;servicedir&gt;@DBUS_ACTIVATION_DIR@&lt;/servicedir&gt;\">]>\n<busconfig>&services;</busconfig>\n",
        ),
    );
    assert!(
        !entity_hidden_dbus_activation.status.success(),
        "custom XML entity hid a D-Bus activation service directory"
    );

    let unresolved_selinux_relative_dbus_include =
        run_drain_check_with_unit_files_and_dbus_file(
            "",
            "",
            false,
            "dbus-selinux-enabled",
            false,
            "",
            None,
            Some(
                "<busconfig><include if_selinux_enabled=\"yes\" selinux_root_relative=\"yes\">contexts/dbus_contexts</include></busconfig>\n",
            ),
        );
    assert!(
        !unresolved_selinux_relative_dbus_include.status.success(),
        "active SELinux-policy-relative D-Bus include did not fail closed"
    );

    let skipped_selinux_relative_dbus_include =
        run_drain_check_with_unit_files_and_dbus_file(
            "",
            "",
            false,
            "dbus-selinux-disabled",
            false,
            "",
            None,
            Some(
                "<busconfig><include if_selinux_enabled=\"yes\" selinux_root_relative=\"yes\">contexts/dbus_contexts</include></busconfig>\n",
            ),
        );
    assert!(
        !skipped_selinux_relative_dbus_include.status.success(),
        "SELinux-root-relative D-Bus include was silently skipped when the host policy was disabled"
    );

    let doctor_instance = run_drain_check_with(
        "velnor-doctor@alpha.service loaded active running\n",
        "velnor-doctor@alpha.timer loaded active waiting\n",
        false,
        "doctor-instance",
        false,
    );
    assert!(
        doctor_instance.status.success(),
        "exact doctor template maintenance failed: {}",
        String::from_utf8_lossy(&doctor_instance.stderr)
    );

    let fleet_audit = run_drain_check_with(
        "velnor-fleet-policy-audit.service loaded active running\n",
        "velnor-fleet-policy-audit.timer loaded active waiting\n",
        false,
        "fleet-audit",
        false,
    );
    assert!(
        fleet_audit.status.success(),
        "exact fleet audit maintenance failed: {}",
        String::from_utf8_lossy(&fleet_audit.stderr)
    );

    let socket_activation = run_drain_check_with(
        "foo.service loaded inactive dead\n",
        "",
        false,
        "custom-socket",
        true,
    );
    assert!(!socket_activation.status.success());

    let path_activation = run_drain_check_with(
        "foo.service loaded inactive dead\n",
        "",
        false,
        "custom-path",
        false,
    );
    assert!(!path_activation.status.success());
}

#[test]
fn missing_manifest_fails_identity_before_mutation() {
    let (output, _temp) = run_identity_check(false, "1.2.3", "1.2.3");
    assert!(
        !output.status.success(),
        "identity unexpectedly passed without a manifest: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("manifest.json is missing"));
}

#[test]
fn package_version_mismatch_fails_identity_before_mutation() {
    let (output, _temp) = run_identity_check(true, "1.2.4", "1.2.3");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Debian package version"));
}

#[test]
fn crate_version_mismatch_fails_identity_before_mutation() {
    let (output, _temp) = run_identity_check(true, "1.2.3", "1.2.4");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("crate version"),
        "unexpected identity failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn matching_compiled_manifest_and_package_identity_pass() {
    let (output, _temp) = run_identity_check(true, "1.2.3", "1.2.3");
    assert!(
        output.status.success(),
        "identity fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn purge_uses_configured_instances_and_mount_anchored_deletion() {
    let source = include_str!("../src/stale_trust_scope.rs");
    assert!(source.contains("crate::daemon_instance::enumerate"));
    assert!(!source.contains("std::env::var"));
    assert!(source.contains("remove_dir_all_on_device_under_identities"));
    assert!(source.contains("filesystem_entries_under"));
    assert!(source.contains("filesystem_directory_identity_under"));
    assert!(source.contains("LEGACY_LEASE_ROOT"));
    assert!(source.contains("KEYED_LEASE_ROOT"));
    assert!(source.contains("FILESYSTEM_COORDINATOR_LOCK"));
    assert!(source.contains("GC_LOCK"));
    assert!(source.contains("recheck_physical_plan_paths"));
    assert!(source.contains("prospective_physical_path"));
    assert!(source.contains("legacy_shared_work_root"));
    assert!(
        source.find("run_root.join(GC_LOCK)").unwrap()
            < source
                .find("run_root.join(FILESYSTEM_COORDINATOR_LOCK)")
                .unwrap(),
        "GC lock must precede filesystem coordinator lock"
    );
}

#[test]
fn package_guards_scan_custom_units_and_live_runner_processes() {
    for script in [
        include_str!("../debian/preinst"),
        include_str!("../debian/postinst"),
    ] {
        assert!(script.contains("--type=service --no-legend --no-pager --plain --full"));
        assert!(script.contains("service_invokes_packaged_runner"));
        assert!(script.contains("no_live_packaged_runner_process"));
        assert!(script.contains("velnor-runner (deleted)"));
        assert!(script.contains("!= \"/proc/$$/exe\""));
        assert!(script.contains("exact_maintenance_exec_start"));
        assert!(script.contains("ExecCondition"));
        assert!(script.contains("--type=\"$activation_kind\""));
        assert!(
            script.contains("lock_identity_decimal=$(stat -L -c '%Hd:%Ld:%i' \"/proc/$$/fd/9\"")
        );
        assert!(script.contains("sprintf(\"%x\", $1 + 0)"));
        assert!(script.contains("sha256sum \"$executable_link\""));
        assert!(script.contains("list-unit-files --type=service"));
        assert!(script.contains("systemd_service_is_dbus_activatable"));
        assert!(script.contains("dbus_activation_service_files_safe"));
        assert!(script.contains("dbus.service"));
        assert!(script.contains("NeedDaemonReload"));
        assert!(script.contains("dbus_walk_config"));
        assert!(script.contains("dbus_config_directives"));
        assert!(script.contains("/etc/dbus-1/system-services"));
        assert!(script.contains("/run/dbus-1/system-services"));
        assert!(script.contains("SystemdService"));
        assert!(script.contains("BusName"));
        assert!(script.contains("^python([0-9]+([.][0-9]+)*[a-z]?)?$"));
        assert!(script.contains("od -An -N 2 -tx1"));
    }
}

#[test]
fn purge_fails_closed_on_effective_unit_root_divergence() {
    let source = include_str!("../src/stale_trust_scope.rs");
    assert!(source.contains("systemctl_show(instance, \"ExecStart\")"));
    assert!(source.contains("systemctl_show(instance, \"WorkingDirectory\")"));
    assert!(source.contains("systemctl_show(instance, \"NeedDaemonReload\")"));
    assert!(source.contains("expected_exec_start()"));
    assert!(source.contains("validate_effective_exec_start(instance"));
    assert!(
        source.contains("assert!(validate_effective_exec_start(&instance, &divergent).is_err())")
    );
    assert!(source.contains(
        "assert!(validate_effective_working_directory(&instance, \"/custom/work\\n\").is_err())"
    ));
    assert!(source.contains("effective ExecStart"));
    assert!(source.contains("VELNOR_WORK_DIR"));
    assert!(source.contains("VELNOR_TRUST_SCOPE"));
    assert!(source.contains("RootDirectory"));
    assert!(source.contains("RootImage"));
    assert!(source.contains("EnvironmentFile"));
    assert!(source.contains("verify_secret_environment_file"));
    assert!(source.contains("verify_complete_instance_inventory"));
    assert!(source.contains("validate_systemd_unit_inventory"));
    assert!(source.contains("systemctl_listing"));
    assert!(source.contains("verify_dropin_paths_property"));
    assert!(source.contains("daemon environment filename cannot be replayed safely"));
    assert!(source.contains("manager environment overrides a daemon root"));
}

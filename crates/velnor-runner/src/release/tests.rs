//! Plan 010 release-model tests. Every fixture is built from fixed seed bytes so
//! the digests are reproducible and no test depends on a real tagged build, live
//! OCI registry, or host state (the `release-build` feature is off here).

use std::path::{Path, PathBuf};

use super::*;

#[test]
fn debian_lifecycle_preserves_operator_units_and_covers_instances() {
    let preinst = include_str!("../../debian/preinst");
    let postinst = include_str!("../../debian/postinst");
    let prerm = include_str!("../../debian/prerm");
    let postrm = include_str!("../../debian/postrm");

    for forbidden in [
        "systemctl enable velnor-daemon.service",
        "systemctl enable --now velnor-doctor.timer",
        "systemctl restart",
        "systemctl try-restart",
        "docker build",
        "docker pull",
    ] {
        assert!(
            !preinst.contains(forbidden),
            "preinst must not restart or start operator units: {forbidden}"
        );
        assert!(
            !postinst.contains(forbidden),
            "postinst must not contain operator-state/network mutation: {forbidden}"
        );
    }
    assert!(postinst.contains("install -d -m 0750 \"$RELEASE_DIR\" \"$RELEASE_DIR/records\""));
    assert!(!postinst.contains("install -d -m 0750 \"$ACTIVE_DIR\""));
    assert!(postinst.contains("rmdir \"$ACTIVE_DIR\""));
    assert!(postinst.contains("require_package_transaction_lock"));
    assert!(postinst.contains("/proc/locks"));
    assert!(postinst.contains("all_velnor_units_drained"));
    assert!(
        postinst.find("require_package_transaction_lock").unwrap()
            < postinst.find("all_velnor_units_drained || fail").unwrap()
    );
    assert!(
        postinst.find("all_velnor_units_drained || fail").unwrap()
            < postinst.find("install -d -m 0750 /var/lib/velnor").unwrap()
    );
    assert!(postinst.contains("legacy active directory is nonempty; refusing pointer migration"));
    assert!(
        !postinst.contains("release verify-installed"),
        "postinst must not compare new package bytes with the active rollback predecessor"
    );
    assert!(prerm.contains("'velnor-daemon@*.service'"));
    assert!(prerm.contains("systemctl stop \"$unit\""));
    // postrm enumerates every Velnor service and timer with the broad glob,
    // which covers `velnor-daemon@*.service` instances.
    assert!(postrm.contains("'velnor*.service' 'velnor*.timer'"));
    assert!(postrm.contains("systemctl disable \"$unit\""));
}

#[test]
fn debian_preinst_requires_whole_host_drain() {
    let preinst = include_str!("../../debian/preinst");

    assert!(preinst.contains("PACKAGE_TRANSACTION_LOCK=/run/velnor/package-transaction.lock"));
    assert!(preinst.contains("/proc/locks"));
    assert!(preinst.contains("$2 == \"FLOCK\""));
    assert!(preinst.contains("exclusive lock owner is not an apt-wrapper ancestor"));
    assert!(preinst.contains("systemctl show --property=LoadState --value velnor-guardian.service"));
    assert!(preinst.contains("not-found) return 0"));
    assert!(preinst.contains(
        "[ \"$(systemctl show --property=ActiveState --value velnor-guardian.service 2>/dev/null || true)\" = inactive ]"
    ));
    assert!(preinst.contains(
        "systemctl list-units --type=service --all --no-legend --plain 'velnor*.service'"
    ));
    assert!(preinst
        .contains("systemctl list-units --type=timer --all --no-legend --plain 'velnor*.timer'"));
    assert!(preinst.contains("all_velnor_units_drained"));
    assert!(preinst.contains("guardian_inactive"));
    assert!(!preinst.contains("VELNOR_DRAINED_UNITS"));
    assert!(!preinst.contains("scoped_units_drained"));
    assert!(!preinst.contains("refusing scoped"));
}

#[cfg(target_os = "linux")]
#[test]
fn maintainer_lock_proof_rejects_marker_and_shared_lock_spoofs() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    if !Path::new("/proc/locks").is_file() || !Path::new("/usr/bin/flock").is_file() {
        return;
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "velnor-package-lock-test-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let lock = root.join("package-transaction.lock");
    let script = root.join("preinst");
    let systemd_dir = root.join("systemd");
    let mock_bin = root.join("mock-bin");
    let dbus_config = root.join("system.conf");
    let systemctl_log = root.join("systemctl.log");
    let busctl_log = root.join("busctl.log");
    fs::create_dir_all(&systemd_dir).unwrap();
    fs::create_dir_all(&mock_bin).unwrap();
    fs::write(&dbus_config, "<busconfig>\n</busconfig>\n").unwrap();
    let systemctl = mock_bin.join("systemctl");
    fs::write(
        &systemctl,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "${SYSTEMCTL_LOG:-/dev/null}"
case "$*" in
  'show --property=Version --value') printf '256.1\n' ;;
  'show --property=ActiveState --value dbus.service') printf 'active\n' ;;
  'show --property=NeedDaemonReload --value dbus.service') printf 'no\n' ;;
  'show --property=ExecStart --value dbus.service') printf '{ path=/usr/bin/dbus-daemon ; argv[]=/usr/bin/dbus-daemon --config-file=%s --nofork --nopidfile --systemd-activation --syslog ; ignore_errors=no ; start_time=[] ; stop_time=[] ; pid=1 ; code=(exited) ; status=0 }\n' "$DBUS_CONFIG" ;;
  'list-units --all --type=service --no-legend --no-pager --plain --full') exit 0 ;;
  'list-units --all --type=timer --no-legend --no-pager --plain --full') exit 0 ;;
  'list-units --all --type=socket --no-legend --no-pager --plain --full') exit 0 ;;
  'list-units --all --type=path --no-legend --no-pager --plain --full') exit 0 ;;
  'list-unit-files --type=service --no-legend --no-pager --full') exit 0 ;;
  'list-jobs --no-legend --no-pager --full') printf 'No jobs running.\n' ;;
  'show --property=LoadState --value velnor-guardian.service') printf 'not-found\n' ;;
  *) exit 1 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let busctl = mock_bin.join("busctl");
    fs::write(
        &busctl,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "${BUSCTL_LOG:-/dev/null}"
case "$*" in
  '--system call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus GetNameOwner s org.freedesktop.systemd1') printf 's ":1.42"\n' ;;
  '--system call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig') exit 0 ;;
  '--system call org.freedesktop.systemd1 /org/freedesktop/systemd1 org.freedesktop.systemd1.Manager ListJobs') printf 'a(usssoo) 0\n' ;;
  *) exit 1 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&busctl, fs::Permissions::from_mode(0o755)).unwrap();
    let script_source = include_str!("../../debian/preinst")
        .replace(
            "PACKAGE_TRANSACTION_LOCK=/run/velnor/package-transaction.lock",
            &format!("PACKAGE_TRANSACTION_LOCK={}", lock.display()),
        )
        .replace(
            "[ -d /run/systemd/system ]",
            &format!("[ -d {} ]", systemd_dir.display()),
        );
    fs::write(&script, script_source).unwrap();

    let marker_spoof = Command::new("sh")
        .arg(&script)
        .arg("install")
        .output()
        .unwrap();
    assert!(
        !marker_spoof.status.success(),
        "an unwrapped package transaction must fail: {}",
        String::from_utf8_lossy(&marker_spoof.stderr)
    );

    let run_wrapped = |mode: &str| {
        Command::new("sh")
            .arg("-c")
            .arg(
                "/usr/bin/flock --$3 --nonblock --no-fork \"$1\" \
                 sh \"$2\" install",
            )
            .arg("velnor-lock-test")
            .arg(&lock)
            .arg(&script)
            .arg(mode)
            .env("PATH", format!("{}:/usr/bin:/bin", mock_bin.display()))
            .env("DBUS_CONFIG", &dbus_config)
            .env("SYSTEMCTL_LOG", &systemctl_log)
            .env("BUSCTL_LOG", &busctl_log)
            .output()
            .unwrap()
    };
    let shared_spoof = run_wrapped("shared");
    assert!(
        !shared_spoof.status.success(),
        "a shared package lock must fail: {}",
        String::from_utf8_lossy(&shared_spoof.stderr)
    );
    let exclusive_wrapper = run_wrapped("exclusive");
    assert!(
        exclusive_wrapper.status.success(),
        "the explicit exclusive wrapper must pass: {}",
        String::from_utf8_lossy(&exclusive_wrapper.stderr)
    );

    let systemctl_calls = fs::read_to_string(&systemctl_log).unwrap();
    for expected_call in [
        "show --property=Version --value",
        "show --property=ActiveState --value dbus.service",
        "show --property=NeedDaemonReload --value dbus.service",
        "show --property=ExecStart --value dbus.service",
        "list-units --all --type=service --no-legend --no-pager --plain --full",
        "list-units --all --type=timer --no-legend --no-pager --plain --full",
        "list-units --all --type=socket --no-legend --no-pager --plain --full",
        "list-units --all --type=path --no-legend --no-pager --plain --full",
        "list-unit-files --type=service --no-legend --no-pager --full",
        "list-jobs --no-legend --no-pager --full",
        "show --property=LoadState --value velnor-guardian.service",
    ] {
        assert!(
            systemctl_calls.lines().any(|call| call == expected_call),
            "preinst did not query systemctl with `{expected_call}`; calls were:\n{systemctl_calls}"
        );
    }
    let busctl_calls = fs::read_to_string(&busctl_log).unwrap();
    for expected_call in [
        "--system call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus GetNameOwner s org.freedesktop.systemd1",
        "--system call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig",
        "--system call org.freedesktop.systemd1 /org/freedesktop/systemd1 org.freedesktop.systemd1.Manager ListJobs",
    ] {
        assert!(
            busctl_calls.lines().any(|call| call == expected_call),
            "preinst did not query busctl with `{expected_call}`; calls were:\n{busctl_calls}"
        );
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn runner_service_roles_acquire_their_lock_in_process() {
    let debian_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("debian");
    let runner_roles = ["daemon", "guardian", "controller", "slot"];
    let mut service_count = 0;

    for entry in std::fs::read_dir(&debian_dir).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("service") {
            continue;
        }
        service_count += 1;
        let unit = std::fs::read_to_string(entry.path()).unwrap();
        let exec_starts = unit
            .lines()
            .filter(|line| line.starts_with("ExecStart="))
            .collect::<Vec<_>>();
        assert_eq!(
            exec_starts.len(),
            1,
            "{} must have one ExecStart",
            entry.path().display()
        );
        for line in exec_starts {
            if line.contains("/usr/bin/velnor-runner ") {
                let role = line
                    .split("/usr/bin/velnor-runner ")
                    .nth(1)
                    .and_then(|command| command.split_whitespace().next())
                    .unwrap_or_default();
                assert!(
                    runner_roles.contains(&role),
                    "unexpected runner role: {line}"
                );
                assert!(
                    !line.contains("/usr/bin/flock"),
                    "{role} must acquire exactly one in-process package guard: {line}"
                );
            } else {
                assert!(
                    line.contains(
                        "/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock"
                    ),
                    "{} has an unguarded Velnor command: {line}",
                    entry.path().display()
                );
            }
        }
    }

    assert_eq!(
        service_count, 7,
        "all shipped Velnor service units must be audited"
    );

    let service_source = include_str!("../service.rs");
    assert!(service_source.contains("runner_role_requires_package_guard"));
    assert!(service_source.contains("Some(crate::release::package_runner_execution_guard()?)"));
    let node_slot_source = include_str!("../node/slot.rs");
    let node_job_source = include_str!("../node/job.rs");
    assert_eq!(
        node_slot_source
            .matches("crate::release::package_execution_guard()?")
            .count()
            + node_job_source
                .matches("crate::release::package_execution_guard()?")
                .count(),
        2,
        "direct ctl slot/job commands must each acquire one package guard"
    );

    for (name, unit, role) in [
        (
            "daemon",
            include_str!("../../debian/velnor-daemon.service"),
            "daemon",
        ),
        (
            "daemon instance",
            include_str!("../../debian/velnor-daemon@.service"),
            "daemon",
        ),
        (
            "controller",
            include_str!("../../debian/velnor-controller@.service"),
            "controller",
        ),
        (
            "guardian",
            include_str!("../../debian/velnor-guardian.service"),
            "guardian",
        ),
        (
            "slot",
            include_str!("../../debian/velnor-slot@.service"),
            "slot",
        ),
    ] {
        let exec_start = unit
            .lines()
            .find_map(|line| line.strip_prefix("ExecStart="))
            .unwrap_or_else(|| panic!("{name} unit has no ExecStart"));
        assert!(
            !exec_start.contains("/usr/bin/flock"),
            "{name} must not stack an outer flock on its in-process package guard"
        );
        assert!(
            exec_start.contains(&format!("/usr/bin/velnor-runner {role}")),
            "{name} no longer dispatches the guarded runner role: {exec_start}"
        );
        assert!(
            !unit.lines().any(|line| line.starts_with("ExecStartPre=")),
            "{name} must verify inside its process-lifetime guard"
        );
        assert!(
            !unit.contains("release verify-installed"),
            "{name} must not split tuple verification from service exec"
        );
    }
    let service_source = include_str!("../service.rs");
    let entrypoint = service_source
        .split("pub async fn execute()")
        .nth(1)
        .expect("service execute entrypoint");
    let verification = entrypoint
        .find("package_runner_execution_guard()")
        .expect("service execute must acquire and verify its package guard");
    let dispatch = entrypoint
        .find("match cli.command")
        .expect("service dispatch");
    assert!(
        verification < dispatch,
        "the common runner entrypoint must guard package identity before dispatch"
    );
}

#[test]
fn debian_package_includes_fleet_policy_audit_units() {
    let assets = include_str!("../../Cargo.toml");
    for (source, destination) in [
        (
            "../velnor-tools/debian/velnor-fleet-policy-audit.service",
            "lib/systemd/system/velnor-fleet-policy-audit.service",
        ),
        (
            "../velnor-tools/debian/velnor-fleet-policy-audit.timer",
            "lib/systemd/system/velnor-fleet-policy-audit.timer",
        ),
        (
            "../../config/fleet/release-refs.toml",
            "usr/share/velnor/fleet/release-refs.toml",
        ),
    ] {
        let asset = format!("[\"{source}\", \"{destination}\", \"644\"]");
        assert!(
            assets.contains(&asset),
            "cargo-deb assets missing {source} -> {destination}"
        );
    }

    let service = include_str!("../../../velnor-tools/debian/velnor-fleet-policy-audit.service");
    assert!(
        service.contains(
            "ExecStart=/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock /bin/sh -c "
        ),
        "fleet-policy audit service must hold the shared package transaction lock"
    );
    assert!(
        service.contains(
            "RuntimeDirectory=velnor\nRuntimeDirectoryMode=0750\nRuntimeDirectoryPreserve=yes"
        ),
        "fleet-policy audit service must preserve its shared runtime directory securely"
    );
    assert!(
        service.contains("/usr/bin/velnor-tools fleet-policy audit --policy "),
        "fleet-policy audit service must invoke the packaged audit command"
    );
    assert!(
        service.contains("--ledger /usr/share/velnor/fleet/release-refs.toml"),
        "fleet-policy audit service must pass the packaged release-ref ledger"
    );
    assert!(
        include_str!("../../../velnor-tools/debian/velnor-fleet-policy-audit.timer")
            .contains("OnCalendar=")
    );
    assert!(
        include_str!("../../../../config/fleet/release-refs.toml").contains("schema_version = 1"),
        "packaged fleet ledger must be the repository's authoritative schema-v1 ledger"
    );
}

#[test]
fn debian_package_declares_flock_provider() {
    let manifest = include_str!("../../Cargo.toml");
    assert!(manifest.contains("util-linux (>= 2.37.2)"));
}

#[test]
fn debian_package_ships_its_own_package_record() {
    // Issue #673: a preview install has no out-of-band release record to
    // activate from, so the deb must carry its own activatable identity.
    let assets = include_str!("../../Cargo.toml");
    assert!(
        assets.contains(
            "[\"release/package-record.json\", \"usr/share/velnor/package-record.json\", \"644\"]"
        ),
        "cargo-deb assets must ship the deb's package record"
    );
}

#[test]
fn debian_package_ships_boot_persistent_transaction_lock_path() {
    // /run is tmpfs: without a tmpfiles.d entry the preinst/postinst lock gates
    // fail closed after every reboot until an operator recreates the path.
    let assets = include_str!("../../Cargo.toml");
    assert!(
        assets.contains(
            "[\"debian/velnor-runner.tmpfiles\", \"usr/lib/tmpfiles.d/velnor-runner.conf\", \"644\"]"
        ),
        "cargo-deb assets must ship the tmpfiles.d entry"
    );
    let tmpfiles = include_str!("../../debian/velnor-runner.tmpfiles");
    assert!(tmpfiles.contains("d /run/velnor 0750 root root - -"));
    assert!(tmpfiles.contains("f /run/velnor/package-transaction.lock 0640 root root - -"));
    let postinst = include_str!("../../debian/postinst");
    assert!(
        postinst.contains("systemd-tmpfiles --create /usr/lib/tmpfiles.d/velnor-runner.conf"),
        "postinst must recreate the lock path immediately, not only at boot"
    );
    assert!(
        assets.contains("systemd-tmpfiles | systemd"),
        "postinst calls systemd-tmpfiles unconditionally, so a provider must be declared"
    );
}

#[test]
fn runner_units_do_not_own_the_shared_runtime_directory() {
    // /run/velnor is shared by every runner unit and by velnor-tools. If each
    // unit owns it through RuntimeDirectory, stopping one unit removes the
    // transaction lock and breaks namespace setup for the others. The package
    // tmpfiles entry owns this shared path instead.
    for (name, service) in [
        ("daemon", include_str!("../../debian/velnor-daemon.service")),
        (
            "daemon instance",
            include_str!("../../debian/velnor-daemon@.service"),
        ),
        (
            "controller",
            include_str!("../../debian/velnor-controller@.service"),
        ),
        ("doctor", include_str!("../../debian/velnor-doctor.service")),
        (
            "doctor instance",
            include_str!("../../debian/velnor-doctor@.service"),
        ),
        (
            "guardian",
            include_str!("../../debian/velnor-guardian.service"),
        ),
        ("slot", include_str!("../../debian/velnor-slot@.service")),
    ] {
        assert!(
            !service
                .lines()
                .any(|line| line.trim() == "RuntimeDirectory=velnor"),
            "{name} unit must not own the shared /run/velnor directory"
        );
    }
}

#[test]
fn activate_hashes_the_shipped_daemon_binary() {
    let src = include_str!("../release.rs");
    assert!(
        !src.contains("/usr/bin/velnor-runner"),
        "activate must not hash the retired velnor-runner path after the velnorctl cutover"
    );
    assert!(
        src.contains("INSTALLED_BINARY_PATH"),
        "activate must hash the same shipped binary verify-installed uses"
    );
    assert_eq!(crate::args::INSTALLED_BINARY_PATH, "/usr/bin/velnor-runner");
}

// --- deterministic fixtures ------------------------------------------------

fn digest_of(seed: &str) -> Sha256Hex {
    Sha256Hex::of_bytes(seed.as_bytes())
}

fn oci_of(seed: &str) -> OciDigest {
    OciDigest::parse(&format!("sha256:{}", digest_of(seed))).unwrap()
}

fn source_sha(seed: &str) -> SourceSha {
    SourceSha::parse(&digest_of(seed).as_str()[..40]).unwrap()
}

fn arch_identity(arch: Arch, bin: &str, deb: &str, oci: &str) -> ArchitectureIdentity {
    ArchitectureIdentity {
        arch,
        target: arch.target().to_string(),
        binary_sha256: digest_of(bin),
        deb_sha256: digest_of(deb),
        oci_platform_digest: oci_of(oci),
    }
}

fn valid_record() -> ReleaseRecord {
    let commit = source_sha("commit-seed");
    let manifest = digest_of("manifest");
    let index = oci_of("index");
    ReleaseRecord {
        schema: RELEASE_RECORD_SCHEMA.to_string(),
        build: BuildIdentity {
            repository: SOURCE_REPOSITORY.to_string(),
            tag: "v0.1.121".to_string(),
            commit: commit.clone(),
            crate_version: "0.1.121".to_string(),
            debian_version: "0.1.121".to_string(),
            manifest_version: crate::manifest::MANIFEST_VERSION,
            manifest_sha256: manifest.clone(),
        },
        architectures: vec![
            arch_identity(Arch::Amd64, "bin-amd64", "deb-amd64", "oci-amd64"),
            arch_identity(Arch::Arm64, "bin-arm64", "deb-arm64", "oci-arm64"),
        ],
        oci_index_digest: index.clone(),
        oci_image_ref: format!("ghcr.io/tailrocks/velnor-job-ubuntu@{index}"),
        oci_labels: OciLabels {
            version: "0.1.121".to_string(),
            revision: commit,
            source: SOURCE_URL.to_string(),
            manifest_sha256: manifest,
        },
        apt: AptCoordinate {
            origin: "Velnor".to_string(),
            suite: "stable".to_string(),
            component: "main".to_string(),
        },
    }
}

fn deployed_for(record: &ReleaseRecord, host: Arch) -> DeployedIdentity {
    let arch = record.architecture(host).unwrap();
    DeployedIdentity {
        schema: DEPLOYED_IDENTITY_SCHEMA.to_string(),
        package_version: record.build.debian_version.clone(),
        crate_version: record.build.crate_version.clone(),
        source_commit: record.build.commit.clone(),
        binary_sha256: arch.binary_sha256.clone(),
        manifest_version: record.build.manifest_version,
        manifest_sha256: record.build.manifest_sha256.clone(),
        oci_image_digest: Some(record.oci_index_digest.clone()),
        record_sha256: record.digest(),
    }
}

fn write_fixture_runner_binary(record: &ActiveRecord, path: &Path) {
    let host = Arch::host().unwrap();
    let bytes = format!("bin-{}", host.as_str());
    assert_eq!(
        record.binary_sha256(host),
        Some(&Sha256Hex::of_bytes(bytes.as_bytes())),
        "fixture bytes must match the tuple's host runner digest"
    );
    std::fs::write(path, bytes).unwrap();
}

// --- newtype parsing -------------------------------------------------------

#[test]
fn source_sha_requires_40_lowercase_hex() {
    assert!(SourceSha::parse(&"a".repeat(40)).is_ok());
    assert!(SourceSha::parse(&"a".repeat(39)).is_err());
    assert!(SourceSha::parse(&"a".repeat(41)).is_err());
    assert!(SourceSha::parse(&"A".repeat(40)).is_err());
    assert!(SourceSha::parse(&"g".repeat(40)).is_err());
    assert!(SourceSha::parse("").is_err());
}

#[test]
fn sha256_requires_64_lowercase_hex() {
    assert!(Sha256Hex::parse(&"0".repeat(64)).is_ok());
    assert!(Sha256Hex::parse(&"0".repeat(63)).is_err());
    assert!(Sha256Hex::parse(&"F".repeat(64)).is_err());
    assert!(Sha256Hex::parse(&"z".repeat(64)).is_err());
}

#[test]
fn oci_digest_requires_prefix_and_64_hex() {
    assert!(OciDigest::parse(&format!("sha256:{}", "0".repeat(64))).is_ok());
    assert!(OciDigest::parse(&"0".repeat(64)).is_err());
    assert!(OciDigest::parse(&format!("sha512:{}", "0".repeat(64))).is_err());
    assert!(OciDigest::parse(&format!("sha256:{}", "0".repeat(63))).is_err());
    assert!(OciDigest::parse(&format!("sha256:{}", "A".repeat(64))).is_err());
}

#[test]
fn malformed_commit_fails_record_deserialization() {
    let mut value = serde_json::to_value(valid_record()).unwrap();
    value["build"]["commit"] = serde_json::json!("not-a-sha");
    assert!(serde_json::from_value::<ReleaseRecord>(value).is_err());
}

#[test]
fn unknown_credential_like_field_is_rejected() {
    let mut value = serde_json::to_value(valid_record()).unwrap();
    value["apt_token"] = serde_json::json!("ghp_secretsecretsecret");
    let err = serde_json::from_value::<ReleaseRecord>(value).unwrap_err();
    assert!(err.to_string().contains("apt_token") || err.to_string().contains("unknown field"));
}

// --- determinism & acyclicity ---------------------------------------------

#[test]
fn canonical_json_is_deterministic() {
    let record = valid_record();
    assert_eq!(record.to_canonical_json(), record.to_canonical_json());
    // Reversed architecture order canonicalizes to the same bytes + digest.
    let mut reversed = record.clone();
    reversed.architectures.reverse();
    assert_eq!(reversed.to_canonical_json(), record.to_canonical_json());
    assert_eq!(reversed.digest(), record.digest());
}

#[test]
fn round_trip_serialization_is_stable() {
    let record = valid_record();
    let parsed: ReleaseRecord = serde_json::from_str(&record.to_canonical_json()).unwrap();
    assert_eq!(parsed, {
        let mut sorted = record.clone();
        sorted.architectures.sort_by_key(|a| a.arch);
        sorted
    });
    assert_eq!(parsed.verify(), Ok(()));
}

#[test]
fn record_never_contains_its_own_digest() {
    let record = valid_record();
    let digest = record.digest();
    // Acyclicity: the record body must not embed its own digest anywhere.
    assert!(!record.to_canonical_json().contains(digest.as_str()));
    // Nor may any single string field equal it.
    let value = serde_json::to_value(&record).unwrap();
    assert!(!json_contains_string(&value, digest.as_str()));
}

fn json_contains_string(value: &serde_json::Value, needle: &str) -> bool {
    match value {
        serde_json::Value::String(text) => text == needle,
        serde_json::Value::Array(items) => items.iter().any(|v| json_contains_string(v, needle)),
        serde_json::Value::Object(map) => map.values().any(|v| json_contains_string(v, needle)),
        _ => false,
    }
}

// --- record verification ---------------------------------------------------

#[test]
fn valid_record_verifies() {
    assert_eq!(valid_record().verify(), Ok(()));
}

#[test]
fn verify_record_bytes_happy_path() {
    let record = valid_record();
    let bytes = record.to_canonical_json();
    let checksum = Sha256Hex::of_bytes(bytes.as_bytes());
    let parsed = verify_record_bytes(bytes.as_bytes(), &checksum).unwrap();
    assert_eq!(parsed.digest(), record.digest());
}

#[test]
fn verify_record_bytes_rejects_wrong_checksum() {
    let record = valid_record();
    let bytes = record.to_canonical_json();
    let wrong = digest_of("wrong");
    assert_eq!(
        verify_record_bytes(bytes.as_bytes(), &wrong),
        Err(CoherenceError::RecordChecksum)
    );
}

#[test]
fn verify_record_bytes_rejects_non_canonical_bytes() {
    let record = valid_record();
    let mut bytes = record.to_canonical_json().into_bytes();
    bytes.extend_from_slice(b"   \n"); // trailing whitespace: valid JSON, non-canonical
    let checksum = Sha256Hex::of_bytes(&bytes);
    assert_eq!(
        verify_record_bytes(&bytes, &checksum),
        Err(CoherenceError::NonCanonical)
    );
}

#[test]
fn verify_record_bytes_rejects_malformed_json() {
    let bytes = b"{not json";
    let checksum = Sha256Hex::of_bytes(bytes);
    assert_eq!(
        verify_record_bytes(bytes, &checksum),
        Err(CoherenceError::Malformed)
    );
}

/// A single-field mutation and the coherence error it must trigger.
type MismatchCase = (fn(&mut ReleaseRecord), CoherenceError);

/// Every single-field defect maps to a distinct coherence error.
#[test]
fn every_single_field_mismatch_is_caught() {
    let cases: Vec<MismatchCase> = vec![
        (
            |r| r.schema = "velnor.release-record/v2".into(),
            CoherenceError::Schema {
                want: RELEASE_RECORD_SCHEMA,
            },
        ),
        (
            |r| r.build.repository = "evil/fork".into(),
            CoherenceError::Repository,
        ),
        (
            |r| r.build.crate_version = String::new(),
            CoherenceError::EmptyField("crate_version"),
        ),
        (
            |r| r.build.tag = "v0.1.58".into(),
            CoherenceError::TagVersion,
        ),
        (
            |r| r.build.debian_version = "0.1.999".into(),
            CoherenceError::DebianVersion,
        ),
        (
            |r| r.build.manifest_version = 999,
            CoherenceError::ManifestVersion,
        ),
        (
            |r| {
                r.architectures.pop();
            },
            CoherenceError::ArchitectureSet,
        ),
        (
            |r| {
                let dup = r.architectures[0].clone();
                r.architectures[1] = dup;
            },
            CoherenceError::DuplicateArch,
        ),
        (
            |r| r.architectures[0].target = "wrong-triple".into(),
            CoherenceError::ArchTarget,
        ),
        (
            |r| r.oci_image_ref = "ghcr.io/tailrocks/velnor-job-ubuntu@sha256:deadbeef".into(),
            CoherenceError::OciRef,
        ),
        (
            |r| r.oci_labels.version = "0.1.58".into(),
            CoherenceError::OciVersion,
        ),
        (
            |r| r.oci_labels.revision = source_sha("other"),
            CoherenceError::OciRevision,
        ),
        (
            |r| r.oci_labels.source = String::new(),
            CoherenceError::OciSource,
        ),
        (
            |r| r.oci_labels.manifest_sha256 = digest_of("other-manifest"),
            CoherenceError::OciManifestHash,
        ),
        (
            |r| r.apt.origin = String::new(),
            CoherenceError::EmptyField("apt.origin"),
        ),
        (
            |r| r.apt.suite = String::new(),
            CoherenceError::EmptyField("apt.suite"),
        ),
        (
            |r| r.apt.component = String::new(),
            CoherenceError::EmptyField("apt.component"),
        ),
    ];
    for (mutate, expected) in cases {
        let mut record = valid_record();
        mutate(&mut record);
        assert_eq!(record.verify(), Err(expected));
    }
}

#[test]
fn per_arch_completeness_requires_both() {
    let mut only_amd = valid_record();
    only_amd.architectures.retain(|a| a.arch == Arch::Amd64);
    assert_eq!(only_amd.verify(), Err(CoherenceError::ArchitectureSet));
}

// --- installed verification ------------------------------------------------

#[test]
fn verify_installed_happy_path() {
    let record = valid_record();
    let host = Arch::Amd64;
    let deployed = deployed_for(&record, host);
    assert_eq!(
        verify_installed(
            &deployed,
            &ActiveRecord::Release(record.clone()),
            host,
            &deployed.binary_sha256
        ),
        Ok(())
    );
}

#[test]
fn verify_installed_catches_each_field() {
    let record = valid_record();
    let host = Arch::Amd64;
    let active = ActiveRecord::Release(record.clone());

    let mut wrong_pointer = deployed_for(&record, host);
    wrong_pointer.record_sha256 = digest_of("x");
    assert_eq!(
        verify_installed(&wrong_pointer, &active, host, &wrong_pointer.binary_sha256),
        Err(CoherenceError::InstalledRecordPointer)
    );

    let mut wrong_source = deployed_for(&record, host);
    wrong_source.source_commit = source_sha("other");
    wrong_source.record_sha256 = record.digest();
    assert_eq!(
        verify_installed(&wrong_source, &active, host, &wrong_source.binary_sha256),
        Err(CoherenceError::InstalledSource)
    );

    let mut wrong_pkg = deployed_for(&record, host);
    wrong_pkg.package_version = "0.1.58".into();
    assert_eq!(
        verify_installed(&wrong_pkg, &active, host, &wrong_pkg.binary_sha256),
        Err(CoherenceError::InstalledPackageVersion)
    );

    let mut wrong_oci = deployed_for(&record, host);
    wrong_oci.oci_image_digest = Some(oci_of("other-index"));
    assert_eq!(
        verify_installed(&wrong_oci, &active, host, &wrong_oci.binary_sha256),
        Err(CoherenceError::InstalledOci)
    );

    // Installed binary on disk disagrees with the recorded digest.
    let deployed = deployed_for(&record, host);
    assert_eq!(
        verify_installed(&deployed, &active, host, &digest_of("tampered-binary")),
        Err(CoherenceError::InstalledBinary)
    );
}

#[test]
fn verify_installed_rejects_missing_host_arch() {
    let mut record = valid_record();
    record.architectures.retain(|a| a.arch == Arch::Amd64);
    // record itself now fails the arch-set check first; construct a deployed and
    // ensure the arch lookup path is unreachable behind verify().
    let host = Arch::Arm64;
    let deployed = DeployedIdentity {
        schema: DEPLOYED_IDENTITY_SCHEMA.to_string(),
        package_version: record.build.debian_version.clone(),
        crate_version: record.build.crate_version.clone(),
        source_commit: record.build.commit.clone(),
        binary_sha256: digest_of("bin-arm64"),
        manifest_version: record.build.manifest_version,
        manifest_sha256: record.build.manifest_sha256.clone(),
        oci_image_digest: Some(record.oci_index_digest.clone()),
        record_sha256: record.digest(),
    };
    assert_eq!(
        verify_installed(
            &deployed,
            &ActiveRecord::Release(record.clone()),
            host,
            &deployed.binary_sha256
        ),
        Err(CoherenceError::ArchitectureSet)
    );
}

// --- assemble --------------------------------------------------------------

fn write_assemble_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let record = valid_record();
    let record_path = root.join("record.json");
    let artifacts = root.join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    std::fs::write(&record_path, record.to_canonical_json()).unwrap();
    for arch in REQUIRED_ARCHES {
        let record_arch = record.architecture(arch).unwrap();
        std::fs::write(
            artifacts.join(format!("velnor-runner-{}.bin.sha256", arch.as_str())),
            format!("{}\n", record_arch.binary_sha256),
        )
        .unwrap();
        std::fs::write(
            artifacts.join(format!(
                "velnor-runner-{}-{}.deb.sha256",
                record.build.debian_version,
                arch.as_str()
            )),
            format!("{}\n", record_arch.deb_sha256),
        )
        .unwrap();
        std::fs::write(
            artifacts.join(format!(
                "velnor-runner-{}-{}.deb",
                record.build.debian_version,
                arch.as_str()
            )),
            format!("deb-{}", arch.as_str()),
        )
        .unwrap();
    }
    (record_path, artifacts)
}

fn assemble_args(record: PathBuf, artifacts: PathBuf) -> ReleaseAssembleArgs {
    ReleaseAssembleArgs {
        record,
        artifacts,
        out: None,
    }
}

#[test]
fn assemble_command_requires_complete_binary_and_deb_artifacts() {
    let root = TempDir::new("assemble-complete");
    let (record, artifacts) = write_assemble_fixture(root.path());

    assemble_command(assemble_args(record, artifacts)).unwrap();
}

#[test]
fn assemble_command_rejects_missing_binary_sidecar() {
    let root = TempDir::new("assemble-missing-binary-sidecar");
    let (record, artifacts) = write_assemble_fixture(root.path());
    std::fs::remove_file(artifacts.join("velnor-runner-amd64.bin.sha256")).unwrap();

    let error = assemble_command(assemble_args(record, artifacts)).unwrap_err();
    assert!(error.to_string().contains("read binary checksum"));
}

#[test]
fn assemble_command_rejects_missing_deb_sidecar() {
    let root = TempDir::new("assemble-missing-deb-sidecar");
    let (record, artifacts) = write_assemble_fixture(root.path());
    std::fs::remove_file(artifacts.join("velnor-runner-0.1.121-arm64.deb.sha256")).unwrap();

    let error = assemble_command(assemble_args(record, artifacts)).unwrap_err();
    assert!(error.to_string().contains("read deb checksum"));
}

#[test]
fn assemble_command_rejects_missing_deb_payload() {
    let root = TempDir::new("assemble-missing-deb-payload");
    let (record, artifacts) = write_assemble_fixture(root.path());
    std::fs::remove_file(artifacts.join("velnor-runner-0.1.121-arm64.deb")).unwrap();

    let error = assemble_command(assemble_args(record, artifacts)).unwrap_err();
    assert!(error.to_string().contains("hash deb artifact"));
}

#[test]
fn assemble_command_rejects_malformed_artifact_checksum() {
    let root = TempDir::new("assemble-malformed-checksum");
    let (record, artifacts) = write_assemble_fixture(root.path());
    std::fs::write(
        artifacts.join("velnor-runner-amd64.bin.sha256"),
        "not-a-sha256\n",
    )
    .unwrap();

    let error = assemble_command(assemble_args(record, artifacts)).unwrap_err();
    assert!(error.to_string().contains("sha-256 must be exactly 64"));
}

#[test]
fn assemble_command_rejects_extra_artifact_checksum_tokens() {
    let root = TempDir::new("assemble-extra-checksum-token");
    let (record, artifacts) = write_assemble_fixture(root.path());
    std::fs::write(
        artifacts.join("velnor-runner-amd64.bin.sha256"),
        format!("{}  velnor-runner\n", digest_of("bin-amd64")),
    )
    .unwrap();

    let error = assemble_command(assemble_args(record, artifacts)).unwrap_err();
    assert!(error
        .to_string()
        .contains("artifact checksum file must contain only one checksum"));
}

#[test]
fn assemble_command_rejects_oversized_artifact_checksum() {
    let root = TempDir::new("assemble-oversized-checksum");
    let (record, artifacts) = write_assemble_fixture(root.path());
    std::fs::write(
        artifacts.join("velnor-runner-amd64.bin.sha256"),
        "a".repeat(MAX_ARTIFACT_CHECKSUM_BYTES + 1),
    )
    .unwrap();

    let error = assemble_command(assemble_args(record, artifacts)).unwrap_err();
    assert!(error.to_string().contains("exceeds 4096 bytes"));
}

#[test]
fn assemble_command_rejects_deb_sidecar_mismatch() {
    let root = TempDir::new("assemble-mismatched-deb-sidecar");
    let (record, artifacts) = write_assemble_fixture(root.path());
    std::fs::write(
        artifacts.join("velnor-runner-0.1.121-amd64.deb.sha256"),
        format!("{}\n", digest_of("tampered-deb")),
    )
    .unwrap();

    let error = assemble_command(assemble_args(record, artifacts)).unwrap_err();
    assert!(error
        .to_string()
        .contains("assembled deb checksum for amd64 disagrees with the artifact"));
}

#[test]
fn assemble_command_rejects_tampered_deb_even_with_refreshed_sidecar() {
    let root = TempDir::new("assemble-tampered-deb");
    let (record, artifacts) = write_assemble_fixture(root.path());
    std::fs::write(
        artifacts.join("velnor-runner-0.1.121-amd64.deb"),
        b"tampered-deb",
    )
    .unwrap();
    std::fs::write(
        artifacts.join("velnor-runner-0.1.121-amd64.deb.sha256"),
        format!("{}\n", digest_of("tampered-deb")),
    )
    .unwrap();

    let error = assemble_command(assemble_args(record, artifacts)).unwrap_err();
    assert!(error
        .to_string()
        .contains("assembled deb artifact digest for amd64 disagrees with the record"));
}

#[test]
fn assemble_command_rejects_unsafe_artifact_version_components() {
    for (index, version) in ["", ".", "..", "a/b", r"a\b", "a\nb"]
        .into_iter()
        .enumerate()
    {
        let root = TempDir::new(&format!("assemble-unsafe-version-{index}"));
        let mut record = valid_record();
        record.build.debian_version = version.to_string();
        let record_path = root.path().join("record.json");
        let artifacts = root.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(&record_path, record.to_canonical_json()).unwrap();

        let error = assemble_command(assemble_args(record_path, artifacts)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "release version is not a safe artifact path component",
            "version {version:?} must not become an artifact path"
        );
    }
}

#[test]
fn assemble_sorts_and_verifies() {
    let record = valid_record();
    let inputs = AssembleInputs {
        build: record.build.clone(),
        // Reversed on input; assemble must sort deterministically.
        architectures: vec![
            arch_identity(Arch::Arm64, "bin-arm64", "deb-arm64", "oci-arm64"),
            arch_identity(Arch::Amd64, "bin-amd64", "deb-amd64", "oci-amd64"),
        ],
        oci_index_digest: record.oci_index_digest.clone(),
        oci_image_ref: record.oci_image_ref.clone(),
        oci_labels: record.oci_labels.clone(),
        apt: record.apt.clone(),
    };
    let assembled = assemble(inputs).unwrap();
    assert_eq!(assembled.digest(), record.digest());
}

#[test]
fn assemble_rejects_incoherent_input() {
    let record = valid_record();
    let inputs = AssembleInputs {
        build: BuildIdentity {
            tag: "v9.9.9".into(),
            ..record.build.clone()
        },
        architectures: record.architectures.clone(),
        oci_index_digest: record.oci_index_digest.clone(),
        oci_image_ref: record.oci_image_ref.clone(),
        oci_labels: record.oci_labels.clone(),
        apt: record.apt.clone(),
    };
    assert_eq!(assemble(inputs), Err(CoherenceError::TagVersion));
}

// --- development guard ------------------------------------------------------

#[test]
fn development_build_cannot_emit_a_publishable_record() {
    // The test binary is built with `release-build` OFF -> embedded identity is
    // `development` and must refuse to emit.
    let identity = embedded();
    assert!(identity.is_development());
    let err = emit_record(&identity, &valid_record()).unwrap_err();
    assert!(err.to_string().contains("development"));
}

// --- redacted diagnostics ---------------------------------------------------

#[test]
fn coherence_error_messages_are_redacted() {
    let record = valid_record();
    let secret_hex = record.build.commit.to_string();
    let manifest_hex = record.build.manifest_sha256.to_string();
    // Exercise a representative spread of variants and prove no value leaks.
    let messages = [
        CoherenceError::OciRevision.to_string(),
        CoherenceError::OciManifestHash.to_string(),
        CoherenceError::InstalledBinary.to_string(),
        CoherenceError::RecordChecksum.to_string(),
        CoherenceError::InstalledSource.to_string(),
    ];
    for message in messages {
        assert!(
            !message.contains(&secret_hex),
            "leaked commit in: {message}"
        );
        assert!(
            !message.contains(&manifest_hex),
            "leaked manifest hash in: {message}"
        );
    }
}

// --- publication + deployed round trip -------------------------------------

#[test]
fn publication_and_deployed_round_trip() {
    let record = valid_record();
    let publication = PublicationRecord {
        schema: PUBLICATION_RECORD_SCHEMA.to_string(),
        source_record_sha256: record.digest(),
        tag: record.build.tag.clone(),
        crate_version: record.build.crate_version.clone(),
        inrelease_sha256: digest_of("InRelease"),
        packages: vec![
            PackagesIndex {
                arch: Arch::Amd64,
                sha256: digest_of("packages-amd64"),
            },
            PackagesIndex {
                arch: Arch::Arm64,
                sha256: digest_of("packages-arm64"),
            },
        ],
        signer_fingerprint: "7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801".to_string(),
        previous: Some(PreviousPointer::Coherent {
            tag: "v0.1.120".to_string(),
            source_record_sha256: digest_of("prev-record"),
        }),
    };
    let json = serde_json::to_string_pretty(&publication).unwrap();
    let parsed: PublicationRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, publication);
    // The publication points AT the record digest (acyclic edge, wrapper->record).
    assert_eq!(parsed.source_record_sha256, record.digest());

    let deployed = deployed_for(&record, Arch::Amd64);
    let dj = serde_json::to_string_pretty(&deployed).unwrap();
    assert_eq!(
        serde_json::from_str::<DeployedIdentity>(&dj).unwrap(),
        deployed
    );
}

fn publication_for(record: &ReleaseRecord) -> PublicationRecord {
    PublicationRecord {
        schema: PUBLICATION_RECORD_SCHEMA.to_string(),
        source_record_sha256: record.digest(),
        tag: record.build.tag.clone(),
        crate_version: record.build.crate_version.clone(),
        inrelease_sha256: digest_of("InRelease"),
        packages: vec![
            PackagesIndex {
                arch: Arch::Amd64,
                sha256: digest_of("packages-amd64"),
            },
            PackagesIndex {
                arch: Arch::Arm64,
                sha256: digest_of("packages-arm64"),
            },
        ],
        signer_fingerprint: "7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801".to_string(),
        previous: Some(PreviousPointer::Coherent {
            tag: "v0.1.120".to_string(),
            source_record_sha256: digest_of("prev-record"),
        }),
    }
}

#[test]
fn verify_publication_binds_happy_path() {
    let record = valid_record();
    assert_eq!(
        verify_publication_binds(&publication_for(&record), &record),
        Ok(())
    );
}

#[test]
fn verify_publication_binds_catches_each_field() {
    let record = valid_record();

    let mut wrong_schema = publication_for(&record);
    wrong_schema.schema = "velnor.publication-record/v2".into();
    assert_eq!(
        verify_publication_binds(&wrong_schema, &record),
        Err(CoherenceError::Schema {
            want: PUBLICATION_RECORD_SCHEMA,
        })
    );

    let mut wrong_binding = publication_for(&record);
    wrong_binding.source_record_sha256 = digest_of("not-the-record");
    assert_eq!(
        verify_publication_binds(&wrong_binding, &record),
        Err(CoherenceError::PublicationBinding)
    );

    let mut wrong_version = publication_for(&record);
    wrong_version.crate_version = "0.1.58".into();
    assert_eq!(
        verify_publication_binds(&wrong_version, &record),
        Err(CoherenceError::PublicationVersion)
    );

    // Previous pointer must reference a DIFFERENT release than the current one.
    let mut self_previous = publication_for(&record);
    self_previous.previous = Some(PreviousPointer::Coherent {
        tag: record.build.tag.clone(),
        source_record_sha256: record.digest(),
    });
    assert_eq!(
        verify_publication_binds(&self_previous, &record),
        Err(CoherenceError::PublicationPrevious)
    );

    let mut missing_arch = publication_for(&record);
    missing_arch.packages.pop();
    assert_eq!(
        verify_publication_binds(&missing_arch, &record),
        Err(CoherenceError::PublicationPackageArch)
    );

    let mut empty_inrelease = publication_for(&record);
    empty_inrelease.inrelease_sha256 = Sha256Hex::parse(&"0".repeat(64)).unwrap();
    assert_eq!(
        verify_publication_binds(&empty_inrelease, &record),
        Err(CoherenceError::PublicationDigestEmpty)
    );

    let mut empty_index = publication_for(&record);
    empty_index.packages[0].sha256 = Sha256Hex::parse(&"0".repeat(64)).unwrap();
    assert_eq!(
        verify_publication_binds(&empty_index, &record),
        Err(CoherenceError::PublicationPackageArch)
    );
}

#[test]
fn publication_accepts_only_the_explicit_legacy_rollback_bridge() {
    let record = valid_record();
    let mut publication = publication_for(&record);
    publication.previous = Some(PreviousPointer::LegacyObserved("v0.1.120".into()));
    assert_eq!(
        verify_publication_binds(&publication, &record),
        Err(CoherenceError::PublicationPrevious)
    );
    publication.previous = Some(PreviousPointer::LegacyObserved("v0.1.121".into()));
    // The fixture's current tag is v0.1.121, so self-reference remains denied.
    assert_eq!(
        verify_publication_binds(&publication, &record),
        Err(CoherenceError::PublicationPrevious)
    );
    let mut candidate = record.clone();
    candidate.build.tag = "v0.1.131".into();
    candidate.build.crate_version = "0.1.131".into();
    candidate.build.debian_version = "0.1.131".into();
    candidate.oci_labels.version = "0.1.131".into();
    publication.tag = "v0.1.131".into();
    publication.crate_version = "0.1.131".into();
    publication.source_record_sha256 = candidate.digest();
    assert_eq!(verify_publication_binds(&publication, &candidate), Ok(()));
}

fn apt_artifact(seed: &str) -> AptArtifactMetadata {
    AptArtifactMetadata {
        sha256: digest_of(seed),
        size: seed.len() as u64,
    }
}

fn apt_metadata_for(
    publication: &PublicationRecord,
    record: &ReleaseRecord,
) -> (ExpectedAptPublicationMetadata, ActualAptPublicationMetadata) {
    let release = apt_artifact("Release");
    let inrelease = apt_artifact("InRelease");
    let release_gpg = apt_artifact("Release.gpg");
    let signer = publication.signer_fingerprint.clone();
    let package_indexes = publication
        .packages
        .iter()
        .map(|package| AptPackageIndexMetadata {
            arch: package.arch,
            path: apt_packages_path(record, package.arch),
            artifact: apt_artifact(&format!("packages-{}", package.arch.as_str())),
        })
        .collect::<Vec<_>>();
    let packages = REQUIRED_ARCHES
        .iter()
        .map(|&arch| AptPackageMetadata {
            arch,
            path: apt_deb_path(record, arch),
            artifact: AptArtifactMetadata {
                sha256: record.architecture(arch).unwrap().deb_sha256.clone(),
                size: 16,
            },
        })
        .collect::<Vec<_>>();
    let expected = ExpectedAptPublicationMetadata {
        schema: APT_PUBLICATION_METADATA_SCHEMA.to_string(),
        release: AptReleaseMetadata {
            artifact: release.clone(),
            package_indexes: package_indexes.clone(),
            self_row: None,
            self_row_checked: true,
        },
        inrelease: AptSignatureMetadata {
            artifact: inrelease.clone(),
            signed_release_sha256: release.sha256.clone(),
            signer_fingerprint: signer.clone(),
        },
        release_gpg: AptSignatureMetadata {
            artifact: release_gpg.clone(),
            signed_release_sha256: release.sha256,
            signer_fingerprint: signer.clone(),
        },
        packages: packages.clone(),
    };
    let actual = ActualAptPublicationMetadata {
        schema: APT_PUBLICATION_METADATA_SCHEMA.to_string(),
        release: Some(expected.release.clone()),
        inrelease: Some(expected.inrelease.clone()),
        release_gpg: Some(expected.release_gpg.clone()),
        packages: Some(packages),
    };
    (expected, actual)
}

#[test]
fn verify_apt_publication_metadata_accepts_preverified_metadata_claims() {
    let record = valid_record();
    let publication = publication_for(&record);
    let (expected, actual) = apt_metadata_for(&publication, &record);

    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &actual),
        Ok(())
    );
}

#[test]
fn verify_apt_publication_metadata_fails_closed_for_missing_and_bad_artifacts() {
    let record = valid_record();
    let publication = publication_for(&record);
    let (expected, actual) = apt_metadata_for(&publication, &record);

    let mut missing = actual.clone();
    missing.release = None;
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &missing),
        Err(CoherenceError::PublicationMetadataMissing)
    );

    let mut wrong_hash = actual.clone();
    wrong_hash.release.as_mut().unwrap().artifact.sha256 = digest_of("tampered");
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &wrong_hash),
        Err(CoherenceError::PublicationMetadataMismatch)
    );

    let mut wrong_size = actual.clone();
    wrong_size.release.as_mut().unwrap().artifact.size += 1;
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &wrong_size),
        Err(CoherenceError::PublicationMetadataMismatch)
    );

    let mut empty = actual.clone();
    empty.release.as_mut().unwrap().artifact.size = 0;
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &empty),
        Err(CoherenceError::PublicationMetadataEmpty)
    );
}

#[test]
fn verify_apt_publication_metadata_rejects_self_rows_and_bad_signatures() {
    let record = valid_record();
    let publication = publication_for(&record);
    let (expected, actual) = apt_metadata_for(&publication, &record);

    let mut self_row = actual.clone();
    self_row.release.as_mut().unwrap().self_row = Some(apt_artifact("self"));
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &self_row),
        Err(CoherenceError::PublicationReleaseSelfRow)
    );

    let mut uninspected = actual.clone();
    uninspected.release.as_mut().unwrap().self_row_checked = false;
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &uninspected),
        Err(CoherenceError::PublicationMetadataMissing)
    );

    let mut expected_uninspected = expected.clone();
    expected_uninspected.release.self_row_checked = false;
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected_uninspected, &actual),
        Err(CoherenceError::PublicationMetadataMissing)
    );

    let mut bad_signature = actual.clone();
    bad_signature
        .inrelease
        .as_mut()
        .unwrap()
        .signed_release_sha256 = digest_of("other-release");
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &bad_signature),
        Err(CoherenceError::PublicationSignature)
    );

    let mut short_signer = actual.clone();
    short_signer.inrelease.as_mut().unwrap().signer_fingerprint = "261EDAC957DEB801".into();
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &short_signer),
        Err(CoherenceError::PublicationMetadataEmpty)
    );

    let mut wrong_expected_signer = expected.clone();
    wrong_expected_signer.inrelease.signer_fingerprint =
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into();
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &wrong_expected_signer, &actual),
        Err(CoherenceError::PublicationSignature)
    );

    let mut bad_binding = publication.clone();
    bad_binding.inrelease_sha256 = digest_of("other-inrelease");
    assert_eq!(
        verify_apt_publication_metadata(&bad_binding, &record, &expected, &actual),
        Err(CoherenceError::PublicationMetadataBinding)
    );
}

#[test]
fn verify_apt_publication_metadata_rejects_bad_package_hash_size_and_binding() {
    let record = valid_record();
    let publication = publication_for(&record);
    let (expected, actual) = apt_metadata_for(&publication, &record);

    let mut wrong_hash = actual.clone();
    wrong_hash.packages.as_mut().unwrap()[0].artifact.sha256 = digest_of("tampered-package");
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &wrong_hash),
        Err(CoherenceError::PublicationPackageMetadata)
    );

    let mut wrong_expected_hash = expected.clone();
    wrong_expected_hash.packages[0].artifact.sha256 = digest_of("tampered-package");
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &wrong_expected_hash, &actual),
        Err(CoherenceError::PublicationPackageMetadata)
    );

    let mut wrong_both_hashes = expected.clone();
    wrong_both_hashes.packages[0].artifact.sha256 = digest_of("tampered-package");
    let mut wrong_both_actual = actual.clone();
    wrong_both_actual.packages.as_mut().unwrap()[0]
        .artifact
        .sha256 = digest_of("tampered-package");
    assert_eq!(
        verify_apt_publication_metadata(
            &publication,
            &record,
            &wrong_both_hashes,
            &wrong_both_actual
        ),
        Err(CoherenceError::PublicationPackageMetadata)
    );

    let mut wrong_size = actual.clone();
    wrong_size.packages.as_mut().unwrap()[0].artifact.size += 1;
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &wrong_size),
        Err(CoherenceError::PublicationPackageMetadata)
    );

    let mut wrong_binding = publication.clone();
    wrong_binding.packages[0].sha256 = digest_of("other-package");
    assert_eq!(
        verify_apt_publication_metadata(&wrong_binding, &record, &expected, &actual),
        Err(CoherenceError::PublicationPackageMetadata)
    );

    let mut wrong_path = expected.clone();
    wrong_path.release.package_indexes[0].path = "dists/other/Packages".into();
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &wrong_path, &actual),
        Err(CoherenceError::PublicationPackageMetadata)
    );

    let mut duplicate_arch = expected.clone();
    duplicate_arch.release.package_indexes[1].arch = Arch::Amd64;
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &duplicate_arch, &actual),
        Err(CoherenceError::PublicationPackageMetadata)
    );

    let mut missing = actual.clone();
    missing.packages = Some(Vec::new());
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &missing),
        Err(CoherenceError::PublicationMetadataEmpty)
    );
}

#[test]
fn apt_publication_metadata_rejects_wrong_schema_and_unknown_fields() {
    let record = valid_record();
    let publication = publication_for(&record);
    let (expected, mut actual) = apt_metadata_for(&publication, &record);

    actual.schema = "velnor.apt-publication-metadata/v2".into();
    assert_eq!(
        verify_apt_publication_metadata(&publication, &record, &expected, &actual),
        Err(CoherenceError::Schema {
            want: APT_PUBLICATION_METADATA_SCHEMA,
        })
    );

    let mut encoded = serde_json::to_value(&expected).unwrap();
    encoded
        .as_object_mut()
        .unwrap()
        .insert("unexpected".into(), serde_json::Value::Bool(true));
    assert!(serde_json::from_value::<ExpectedAptPublicationMetadata>(encoded).is_err());
}

#[cfg(unix)]
#[test]
fn verify_record_command_rejects_hardlinked_apt_metadata_sources() {
    let dir = TempDir::new("verify-record-apt-hardlink");
    let record = valid_record();
    let record_bytes = record.to_canonical_json();
    let record_path = dir.path().join("record.json");
    let checksum_path = dir.path().join("record.json.sha256");
    let publication_path = dir.path().join("publication.json");
    let expected_path = dir.path().join("expected-apt.json");
    let served_path = dir.path().join("served-apt.json");
    let digest = Sha256Hex::of_bytes(record_bytes.as_bytes());
    let publication = publication_for(&record);

    std::fs::write(&record_path, record_bytes).unwrap();
    std::fs::write(&checksum_path, format!("{digest}  record.json\n")).unwrap();
    std::fs::write(&publication_path, serde_json::to_vec(&publication).unwrap()).unwrap();
    std::fs::write(&expected_path, b"{}").unwrap();
    std::fs::hard_link(&expected_path, &served_path).unwrap();

    let error = verify_record_command(ReleaseVerifyRecordArgs {
        record: record_path,
        checksum: Some(checksum_path),
        sha256: None,
        publication: Some(publication_path),
        expected_apt_metadata: Some(expected_path),
        served_apt_metadata: Some(served_path),
    })
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "expected and served APT metadata must use different files"
    );
}

#[test]
fn verify_record_command_reaches_apt_metadata_verifier() {
    let dir = TempDir::new("verify-record-apt");
    let record = valid_record();
    let record_path = dir.path().join("record.json");
    let checksum_path = dir.path().join("record.json.sha256");
    let publication_path = dir.path().join("publication.json");
    let expected_path = dir.path().join("expected-apt.json");
    let served_path = dir.path().join("served-apt.json");
    let record_bytes = record.to_canonical_json();
    let record_digest = Sha256Hex::of_bytes(record_bytes.as_bytes());
    let publication = publication_for(&record);
    let (expected, served) = apt_metadata_for(&publication, &record);

    std::fs::write(&record_path, record_bytes).unwrap();
    std::fs::write(&checksum_path, format!("{record_digest}  record.json\n")).unwrap();
    std::fs::write(&publication_path, serde_json::to_vec(&publication).unwrap()).unwrap();
    std::fs::write(&expected_path, serde_json::to_vec(&expected).unwrap()).unwrap();
    std::fs::write(&served_path, serde_json::to_vec(&served).unwrap()).unwrap();

    let result = verify_record_command(ReleaseVerifyRecordArgs {
        record: record_path,
        checksum: Some(checksum_path),
        sha256: None,
        publication: Some(publication_path),
        expected_apt_metadata: Some(expected_path),
        served_apt_metadata: Some(served_path),
    });
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn verify_record_command_rejects_publication_without_apt_metadata() {
    let dir = TempDir::new("verify-record-apt-required");
    let record = valid_record();
    let record_bytes = record.to_canonical_json();
    let record_path = dir.path().join("record.json");
    let checksum_path = dir.path().join("record.json.sha256");
    let publication_path = dir.path().join("publication.json");
    let digest = Sha256Hex::of_bytes(record_bytes.as_bytes());

    std::fs::write(&record_path, record_bytes).unwrap();
    std::fs::write(&checksum_path, format!("{digest}  record.json\n")).unwrap();
    std::fs::write(
        &publication_path,
        serde_json::to_vec(&publication_for(&record)).unwrap(),
    )
    .unwrap();

    let error = verify_record_command(ReleaseVerifyRecordArgs {
        record: record_path,
        checksum: Some(checksum_path),
        sha256: None,
        publication: Some(publication_path),
        expected_apt_metadata: None,
        served_apt_metadata: None,
    })
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "--publication requires preverified APT claims for coherence checking"
    );
}

#[test]
fn verify_record_command_requires_both_apt_metadata_paths() {
    let dir = TempDir::new("verify-record-apt-missing");
    let record = valid_record();
    let record_bytes = record.to_canonical_json();
    let record_path = dir.path().join("record.json");
    let checksum_path = dir.path().join("record.json.sha256");
    let expected_path = dir.path().join("expected-apt.json");
    let digest = Sha256Hex::of_bytes(record_bytes.as_bytes());

    std::fs::write(&record_path, record_bytes).unwrap();
    std::fs::write(&checksum_path, format!("{digest}  record.json\n")).unwrap();

    let error = verify_record_command(ReleaseVerifyRecordArgs {
        record: record_path,
        checksum: Some(checksum_path),
        sha256: None,
        publication: None,
        expected_apt_metadata: Some(expected_path),
        served_apt_metadata: None,
    })
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "provide both --expected-apt-metadata and --served-apt-metadata"
    );
}

#[test]
fn verify_record_command_rejects_one_file_for_both_apt_metadata_sources() {
    let dir = TempDir::new("verify-record-apt-same-source");
    let record = valid_record();
    let record_bytes = record.to_canonical_json();
    let record_path = dir.path().join("record.json");
    let checksum_path = dir.path().join("record.json.sha256");
    let metadata_path = dir.path().join("apt.json");
    let publication_path = dir.path().join("publication.json");
    let digest = Sha256Hex::of_bytes(record_bytes.as_bytes());
    let publication = publication_for(&record);

    std::fs::write(&record_path, record_bytes).unwrap();
    std::fs::write(&checksum_path, format!("{digest}  record.json\n")).unwrap();
    std::fs::write(&metadata_path, b"{}").unwrap();
    std::fs::write(&publication_path, serde_json::to_vec(&publication).unwrap()).unwrap();

    let error = verify_record_command(ReleaseVerifyRecordArgs {
        record: record_path,
        checksum: Some(checksum_path),
        sha256: None,
        publication: Some(publication_path),
        expected_apt_metadata: Some(metadata_path.clone()),
        served_apt_metadata: Some(metadata_path),
    })
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "expected and served APT metadata must use different files"
    );
}

#[test]
fn verify_record_command_requires_publication_for_distinct_apt_metadata() {
    let dir = TempDir::new("verify-record-apt-publication");
    let record = valid_record();
    let record_bytes = record.to_canonical_json();
    let record_path = dir.path().join("record.json");
    let checksum_path = dir.path().join("record.json.sha256");
    let expected_path = dir.path().join("expected-apt.json");
    let served_path = dir.path().join("served-apt.json");
    let digest = Sha256Hex::of_bytes(record_bytes.as_bytes());

    std::fs::write(&record_path, record_bytes).unwrap();
    std::fs::write(&checksum_path, format!("{digest}  record.json\n")).unwrap();
    std::fs::write(&expected_path, b"{}").unwrap();
    std::fs::write(&served_path, b"{}").unwrap();

    let error = verify_record_command(ReleaseVerifyRecordArgs {
        record: record_path,
        checksum: Some(checksum_path),
        sha256: None,
        publication: None,
        expected_apt_metadata: Some(expected_path),
        served_apt_metadata: Some(served_path),
    })
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "--publication is required with preverified APT claims coherence checking"
    );
}

// --- atomic on-disk activation ---------------------------------------------

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("velnor-rel-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn transaction_lock_for(dir: &TempDir) -> PackageTransactionLock {
    let path = dir.path().join("package-transaction.lock");
    std::fs::write(&path, b"").unwrap();
    PackageTransactionLock::exclusive_at(&path).unwrap()
}

#[test]
fn write_atomic_writes_exact_bytes() {
    let dir = TempDir::new("atomic");
    let target = dir.path().join("record.json");
    write_atomic(&target, b"exact-bytes").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"exact-bytes");
    // Overwrite is atomic and complete.
    write_atomic(&target, b"second").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"second");
}

#[test]
fn explicit_release_outputs_refuse_live_store_and_symlink_aliases() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("external-release-output");
    let live_store = dir.path().join("live/release");
    let active = live_store.join("active");
    std::fs::create_dir_all(&active).unwrap();
    let active_record = active.join("record.json");
    std::fs::write(&active_record, b"active-record").unwrap();

    let direct_error = write_external_output_pair(
        &active_record,
        b"replacement",
        b"replacement checksum",
        &live_store,
    )
    .unwrap_err();
    assert!(direct_error
        .to_string()
        .contains("inside live release store"));

    let alias = dir.path().join("external-alias");
    symlink(&active, &alias).unwrap();
    let alias_record = alias.join("record.json");
    let alias_error = write_external_output_pair(
        &alias_record,
        b"replacement",
        b"replacement checksum",
        &live_store,
    )
    .unwrap_err();
    assert!(alias_error
        .to_string()
        .contains("inside live release store"));
    assert_eq!(std::fs::read(&active_record).unwrap(), b"active-record");
    assert!(!active.join("record.json.sha256").exists());
}

#[test]
fn pinned_external_output_directory_survives_parent_symlink_swap() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("external-release-output-swap");
    let live_store = dir.path().join("live/release");
    let active = live_store.join("active");
    let external = dir.path().join("external");
    std::fs::create_dir_all(&active).unwrap();
    std::fs::create_dir_all(&external).unwrap();
    let alias = dir.path().join("output-parent");
    symlink(&external, &alias).unwrap();
    let output = alias.join("record.json");

    let target = ExternalOutputTarget::open(&output, &live_store).unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&active, &alias).unwrap();
    target.write(b"external-record").unwrap();

    assert_eq!(
        std::fs::read(external.join("record.json")).unwrap(),
        b"external-record"
    );
    assert!(!active.join("record.json").exists());
}

#[test]
fn store_activate_and_rollback_restore_exact_tuple() {
    let dir = TempDir::new("store");
    let store = ReleaseStore::new(dir.path());
    let transaction_lock = transaction_lock_for(&dir);

    let mut v120 = valid_record();
    v120.build.tag = "v0.1.120".into();
    v120.build.crate_version = "0.1.120".into();
    v120.build.debian_version = "0.1.120".into();
    v120.oci_labels.version = "0.1.120".into();
    assert_eq!(v120.verify(), Ok(()));

    let v121 = valid_record();
    let host = Arch::host().unwrap();
    let deployed120 = deployed_for(&v120, host);
    let deployed121 = deployed_for(&v121, host);

    store
        .activate(
            &ActiveRecord::Release(v120.clone()),
            &deployed120,
            &transaction_lock,
            &v120.build.debian_version,
        )
        .unwrap();
    store
        .activate(
            &ActiveRecord::Release(v121.clone()),
            &deployed121,
            &transaction_lock,
            &v121.build.debian_version,
        )
        .unwrap();

    assert_eq!(store.active_tag().unwrap().as_deref(), Some("v0.1.121"));
    assert_eq!(store.previous_tag().unwrap().as_deref(), Some("v0.1.120"));
    assert_eq!(
        std::fs::read(dir.path().join("active/record.json")).unwrap(),
        v121.to_canonical_json().as_bytes()
    );
    assert!(dir.path().join("active/deployed.json").is_file());

    let installed_binary = dir.path().join("velnor-runner");
    write_fixture_runner_binary(&ActiveRecord::Release(v120.clone()), &installed_binary);
    let restored = store
        .rollback(
            &transaction_lock,
            &installed_binary,
            &v120.build.debian_version,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(restored, "v0.1.120");
    assert_eq!(store.active_tag().unwrap().as_deref(), Some("v0.1.120"));
}

#[test]
fn rollback_refuses_corrupt_previous_deployed_identity_before_pointer_mutation() {
    let dir = TempDir::new("rollback-corrupt-deployed");
    let store = ReleaseStore::new(dir.path());
    let transaction_lock = transaction_lock_for(&dir);
    let host = Arch::host().unwrap();
    let mut older = package_record_for(PACKAGE_KIND_PREVIEW, host);
    older.build.crate_version = "0.1.120".into();
    older.build.debian_version = preview_debian_version("0.1.120");
    let newer = package_record_for(PACKAGE_KIND_PREVIEW, host);

    store
        .activate(
            &ActiveRecord::Package(older.clone()),
            &deployed_for_package(&older),
            &transaction_lock,
            &older.build.debian_version,
        )
        .unwrap();
    store
        .activate(
            &ActiveRecord::Package(newer.clone()),
            &deployed_for_package(&newer),
            &transaction_lock,
            &newer.build.debian_version,
        )
        .unwrap();
    let installed_binary = dir.path().join("velnor-runner");
    write_fixture_runner_binary(&ActiveRecord::Package(older.clone()), &installed_binary);

    let mut corrupt = deployed_for_package(&older);
    corrupt.record_sha256 = digest_of("wrong-prior-record");
    std::fs::write(
        store.deployed_path(&older.build.debian_version),
        serde_json::to_vec_pretty(&corrupt).unwrap(),
    )
    .unwrap();

    assert!(store
        .rollback(
            &transaction_lock,
            &installed_binary,
            &older.build.debian_version,
            |_| Ok(()),
        )
        .is_err());
    assert_eq!(
        store.active_tag().unwrap().as_deref(),
        Some(newer.build.debian_version.as_str())
    );
    assert_eq!(
        store.previous_tag().unwrap().as_deref(),
        Some(older.build.debian_version.as_str())
    );
}

#[test]
fn rollback_refuses_noncanonical_previous_record_before_pointer_mutation() {
    let dir = TempDir::new("rollback-noncanonical-record");
    let store = ReleaseStore::new(dir.path());
    let transaction_lock = transaction_lock_for(&dir);
    let host = Arch::host().unwrap();
    let mut older = package_record_for(PACKAGE_KIND_PREVIEW, host);
    older.build.crate_version = "0.1.120".into();
    older.build.debian_version = preview_debian_version("0.1.120");
    let newer = package_record_for(PACKAGE_KIND_PREVIEW, host);

    store
        .activate(
            &ActiveRecord::Package(older.clone()),
            &deployed_for_package(&older),
            &transaction_lock,
            &older.build.debian_version,
        )
        .unwrap();
    store
        .activate(
            &ActiveRecord::Package(newer.clone()),
            &deployed_for_package(&newer),
            &transaction_lock,
            &newer.build.debian_version,
        )
        .unwrap();
    let installed_binary = dir.path().join("velnor-runner");
    write_fixture_runner_binary(&ActiveRecord::Package(older.clone()), &installed_binary);

    let record_path = store.record_path(&older.build.debian_version);
    let mut noncanonical = older.to_canonical_json().into_bytes();
    noncanonical.push(b' ');
    std::fs::write(record_path, noncanonical).unwrap();

    assert!(store
        .rollback(
            &transaction_lock,
            &installed_binary,
            &older.build.debian_version,
            |_| Ok(()),
        )
        .is_err());
    assert_eq!(
        store.active_tag().unwrap().as_deref(),
        Some(newer.build.debian_version.as_str())
    );
    assert_eq!(
        store.previous_tag().unwrap().as_deref(),
        Some(older.build.debian_version.as_str())
    );
}

#[test]
fn rollback_refuses_installed_runner_that_matches_only_the_new_tuple() {
    let dir = TempDir::new("rollback-current-runner");
    let store = ReleaseStore::new(dir.path());
    let transaction_lock = transaction_lock_for(&dir);
    let host = Arch::host().unwrap();
    let mut older = package_record_for(PACKAGE_KIND_PREVIEW, host);
    older.build.crate_version = "0.1.120".into();
    older.build.debian_version = preview_debian_version("0.1.120");
    older.architecture.binary_sha256 = Sha256Hex::of_bytes(b"old-runner-bytes");
    let mut newer = package_record_for(PACKAGE_KIND_PREVIEW, host);
    newer.architecture.binary_sha256 = Sha256Hex::of_bytes(b"new-runner-bytes");

    store
        .activate(
            &ActiveRecord::Package(older.clone()),
            &deployed_for_package(&older),
            &transaction_lock,
            &older.build.debian_version,
        )
        .unwrap();
    store
        .activate(
            &ActiveRecord::Package(newer.clone()),
            &deployed_for_package(&newer),
            &transaction_lock,
            &newer.build.debian_version,
        )
        .unwrap();
    let installed_binary = dir.path().join("velnor-runner");
    std::fs::write(&installed_binary, b"new-runner-bytes").unwrap();

    assert!(store
        .rollback(
            &transaction_lock,
            &installed_binary,
            &older.build.debian_version,
            |_| Ok(()),
        )
        .is_err());
    assert_eq!(
        store.active_tag().unwrap().as_deref(),
        Some(newer.build.debian_version.as_str())
    );
    assert_eq!(
        store.previous_tag().unwrap().as_deref(),
        Some(older.build.debian_version.as_str())
    );
}

#[test]
fn activation_and_rollback_wait_for_service_process_locks() {
    let dir = TempDir::new("transition-lock");
    let lock_path = dir.path().join("package-transaction.lock");
    let store_path = dir.path().join("store");
    let store = ReleaseStore::new(&store_path);
    let transaction_lock = transaction_lock_for(&dir);
    let host = Arch::host().unwrap();
    let installed_binary = dir.path().join("velnor-runner");

    let mut older = package_record_for(PACKAGE_KIND_PREVIEW, host);
    older.build.crate_version = "0.1.120".into();
    older.build.debian_version = preview_debian_version("0.1.120");
    let newer = package_record_for(PACKAGE_KIND_PREVIEW, host);
    let newer_key = newer.build.debian_version.clone();
    store
        .activate(
            &ActiveRecord::Package(older.clone()),
            &deployed_for_package(&older),
            &transaction_lock,
            &older.build.debian_version,
        )
        .unwrap();
    store
        .activate(
            &ActiveRecord::Package(newer.clone()),
            &deployed_for_package(&newer),
            &transaction_lock,
            &newer.build.debian_version,
        )
        .unwrap();
    write_fixture_runner_binary(&ActiveRecord::Package(older.clone()), &installed_binary);
    drop(transaction_lock);

    let mut candidate = package_record_for(PACKAGE_KIND_PREVIEW, host);
    candidate.build.crate_version = "0.1.122".into();
    candidate.build.debian_version = preview_debian_version("0.1.122");
    candidate.verify().unwrap();
    let candidate_path = dir.path().join("candidate.json");
    std::fs::write(&candidate_path, candidate.to_canonical_json()).unwrap();

    let service_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    rustix::fs::flock(&service_lock, rustix::fs::FlockOperation::LockShared).unwrap();

    for error in [
        activate_command_with_lock_path(
            ReleaseActivateArgs {
                dir: store_path.clone(),
                record: candidate_path,
            },
            &lock_path,
        )
        .unwrap_err(),
        rollback_command_with_lock_path_and_binary(
            ReleaseRollbackArgs {
                dir: store_path.clone(),
            },
            &lock_path,
            &installed_binary,
            &older.build.debian_version,
        )
        .unwrap_err(),
    ] {
        assert!(error.to_string().contains("package transaction lock"));
        assert!(error.to_string().contains("is busy"));
        assert_eq!(
            store.active_tag().unwrap().as_deref(),
            Some(newer_key.as_str())
        );
    }

    drop(service_lock);
    rollback_command_with_lock_path_and_binary(
        ReleaseRollbackArgs { dir: store_path },
        &lock_path,
        &installed_binary,
        &older.build.debian_version,
    )
    .unwrap();
    assert_eq!(
        store.active_tag().unwrap().as_deref(),
        Some("0.1.120~preview.25+8d49319")
    );
}

#[test]
fn service_identity_gate_skips_development_but_verifies_preview_packages() {
    let identity = |kind: &str, source_sha: &str| EmbeddedIdentity {
        source_sha: source_sha.to_owned(),
        tag: "test".into(),
        kind: kind.to_owned(),
        crate_version: "0.1.277".into(),
    };

    assert!(
        !service_identity_requires_installed_tuple(&identity("development", "development"))
            .unwrap()
    );
    let source_sha = "a".repeat(40);
    assert!(service_identity_requires_installed_tuple(&identity("release", &source_sha)).unwrap());
    assert!(service_identity_requires_installed_tuple(&identity("preview", &source_sha)).unwrap());
    assert!(
        service_identity_requires_installed_tuple(&identity("development", &source_sha)).is_err()
    );
    assert!(service_identity_requires_installed_tuple(&identity("unknown", &source_sha)).is_err());
}

#[test]
fn service_identity_must_match_active_record_and_compiled_manifest() {
    let release = ActiveRecord::Release(valid_record());
    let release_record = match &release {
        ActiveRecord::Release(record) => record,
        ActiveRecord::Package(_) => unreachable!(),
    };
    let identity = EmbeddedIdentity {
        source_sha: release_record.build.commit.to_string(),
        tag: release_record.build.tag.clone(),
        kind: "release".into(),
        crate_version: release_record.build.crate_version.clone(),
    };
    let manifest_hash = release.manifest_sha256().clone();
    verify_embedded_identity_matches_record(
        &identity,
        &release,
        release.manifest_version(),
        manifest_hash.clone(),
    )
    .unwrap();

    for stale_identity in [
        EmbeddedIdentity {
            source_sha: source_sha("other").to_string(),
            ..identity.clone()
        },
        EmbeddedIdentity {
            tag: "v0.1.120".into(),
            ..identity.clone()
        },
        EmbeddedIdentity {
            kind: "preview".into(),
            ..identity.clone()
        },
        EmbeddedIdentity {
            crate_version: "0.1.120".into(),
            ..identity.clone()
        },
    ] {
        assert!(verify_embedded_identity_matches_record(
            &stale_identity,
            &release,
            release.manifest_version(),
            manifest_hash.clone(),
        )
        .is_err());
    }
    assert!(verify_embedded_identity_matches_record(
        &identity,
        &release,
        release.manifest_version().saturating_add(1),
        manifest_hash.clone(),
    )
    .is_err());
    assert!(verify_embedded_identity_matches_record(
        &identity,
        &release,
        release.manifest_version(),
        digest_of("different-manifest"),
    )
    .is_err());

    let preview = ActiveRecord::Package(package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64));
    let preview_record = match &preview {
        ActiveRecord::Package(record) => record,
        ActiveRecord::Release(_) => unreachable!(),
    };
    let preview_identity = EmbeddedIdentity {
        source_sha: preview_record.build.commit.to_string(),
        tag: "preview".into(),
        kind: PACKAGE_KIND_PREVIEW.into(),
        crate_version: preview_record.build.crate_version.clone(),
    };
    verify_embedded_identity_matches_record(
        &preview_identity,
        &preview,
        preview.manifest_version(),
        preview.manifest_sha256().clone(),
    )
    .unwrap();
    assert!(verify_embedded_identity_matches_record(
        &EmbeddedIdentity {
            tag: "stale-preview".into(),
            ..preview_identity.clone()
        },
        &preview,
        preview.manifest_version(),
        preview.manifest_sha256().clone(),
    )
    .is_err());

    let stable_package =
        ActiveRecord::Package(package_record_for(PACKAGE_KIND_STABLE, Arch::Amd64));
    assert!(verify_embedded_identity_matches_record(
        &EmbeddedIdentity {
            kind: PACKAGE_KIND_STABLE.into(),
            ..preview_identity
        },
        &stable_package,
        stable_package.manifest_version(),
        stable_package.manifest_sha256().clone(),
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn service_verifier_binds_active_pointer_to_record_store_key() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("active-record-key");
    let record = ActiveRecord::Package(package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64));
    let active = dir.path().join("active");
    let expected = Path::new("records").join(record.store_key());
    symlink(&expected, &active).unwrap();
    verify_active_record_key_at(&active, &record).unwrap();

    std::fs::remove_file(&active).unwrap();
    symlink(Path::new("records/wrong-key"), &active).unwrap();
    assert!(verify_active_record_key_at(&active, &record).is_err());
}

#[test]
fn running_runner_image_must_match_active_record_bytes() {
    let dir = TempDir::new("running-runner-image");
    let mut package = package_record_for(PACKAGE_KIND_PREVIEW, Arch::host().unwrap());
    package.architecture.binary_sha256 = Sha256Hex::of_bytes(b"mapped-runner");
    let record = ActiveRecord::Package(package);
    let executable = dir.path().join("renamed-worker");
    std::fs::write(&executable, b"mapped-runner").unwrap();
    verify_running_executable_image_at(&record, PackageExecutionBinary::Runner, &executable)
        .unwrap();

    std::fs::write(&executable, b"stale-mapped-runner").unwrap();
    assert!(verify_running_executable_image_at(
        &record,
        PackageExecutionBinary::Runner,
        &executable,
    )
    .is_err());
}

#[test]
fn packaged_execution_guard_verifies_under_shared_lock() {
    use std::cell::Cell;

    let dir = TempDir::new("execution-guard");
    let lock_path = dir.path().join("package-transaction.lock");
    std::fs::write(&lock_path, b"").unwrap();
    let source_sha = "a".repeat(40);
    let identity = EmbeddedIdentity {
        source_sha,
        tag: "test".into(),
        kind: "preview".into(),
        crate_version: "0.1.277".into(),
    };
    let verified = Cell::new(false);

    let guard = package_execution_guard_for(&identity, &lock_path, || {
        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)?;
        assert!(matches!(
            rustix::fs::flock(
                &writer,
                rustix::fs::FlockOperation::NonBlockingLockExclusive
            ),
            Err(rustix::io::Errno::WOULDBLOCK)
        ));
        verified.set(true);
        Ok(())
    })
    .unwrap();
    assert!(
        verified.get(),
        "tuple verification must run after lock acquire"
    );

    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert!(matches!(
        rustix::fs::flock(
            &writer,
            rustix::fs::FlockOperation::NonBlockingLockExclusive
        ),
        Err(rustix::io::Errno::WOULDBLOCK)
    ));
    drop(guard);
    rustix::fs::flock(
        &writer,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .unwrap();
}

#[test]
fn development_execution_guard_does_not_open_package_lock_or_verify() {
    let dir = TempDir::new("execution-guard-dev");
    let missing_lock = dir.path().join("missing-package-transaction.lock");
    let identity = EmbeddedIdentity {
        source_sha: "development".into(),
        tag: "development".into(),
        kind: "development".into(),
        crate_version: "0.1.277".into(),
    };

    let _guard = package_execution_guard_for(&identity, &missing_lock, || {
        anyhow::bail!("development guard must skip tuple verification")
    })
    .unwrap();
    assert!(!missing_lock.exists());
}

#[test]
fn store_record_refuses_to_clobber_divergent_bytes() {
    let dir = TempDir::new("clobber");
    let store = ReleaseStore::new(dir.path());
    let transaction_lock = transaction_lock_for(&dir);
    let record = valid_record();
    store
        .store_record(&ActiveRecord::Release(record.clone()), &transaction_lock)
        .unwrap();

    // Same tag, different content -> must not overwrite.
    let mut tampered = record.clone();
    tampered.oci_index_digest = oci_of("tampered-index");
    tampered.oci_image_ref = format!(
        "ghcr.io/tailrocks/velnor-job-ubuntu@{}",
        tampered.oci_index_digest
    );
    let err = store
        .store_record(&ActiveRecord::Release(tampered), &transaction_lock)
        .unwrap_err();
    assert!(err.to_string().contains("refusing to clobber"));

    // Exact re-store of identical bytes is an idempotent success.
    store
        .store_record(&ActiveRecord::Release(record.clone()), &transaction_lock)
        .unwrap();
}

#[test]
fn live_store_record_emission_serializes_on_package_lock() {
    let dir = TempDir::new("emit-live-lock");
    let lock_path = dir.path().join("package-transaction.lock");
    std::fs::write(&lock_path, b"").unwrap();
    let store = ReleaseStore::new(dir.path().join("store"));
    let record = ActiveRecord::Release(valid_record());
    let key = record.store_key();
    let record_path = store.record_path(key);
    let checksum_path = store
        .root
        .join("records")
        .join(format!("{key}.json.sha256"));

    let service_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    rustix::fs::flock(&service_lock, rustix::fs::FlockOperation::LockShared).unwrap();

    let error = store_live_record_with_lock_path(&store, &record, &lock_path).unwrap_err();
    assert!(error.to_string().contains("package transaction lock"));
    assert!(error.to_string().contains("is busy"));
    assert!(!record_path.exists());
    assert!(!checksum_path.exists());

    drop(service_lock);
    let digest = store_live_record_with_lock_path(&store, &record, &lock_path).unwrap();
    assert_eq!(store.read_record(key).unwrap(), record);
    assert_eq!(
        std::fs::read_to_string(checksum_path).unwrap(),
        format!("{digest}  {key}.json\n")
    );
}

#[test]
fn activate_requires_a_stored_record() {
    let dir = TempDir::new("noactivate");
    let store = ReleaseStore::new(dir.path());
    let transaction_lock = transaction_lock_for(&dir);
    let record = valid_record();
    let mut deployed = deployed_for(&record, Arch::host().unwrap());
    deployed.record_sha256 = digest_of("wrong-record");
    assert!(store
        .activate(
            &ActiveRecord::Release(record.clone()),
            &deployed,
            &transaction_lock,
            &record.build.debian_version,
        )
        .is_err());
}

#[test]
fn rollback_requires_a_previous_tuple() {
    let dir = TempDir::new("norollback");
    let store = ReleaseStore::new(dir.path());
    let transaction_lock = transaction_lock_for(&dir);
    assert!(store
        .rollback(
            &transaction_lock,
            Path::new("/missing/runner"),
            "0.1.121",
            |_| Ok(()),
        )
        .is_err());
}

// --- sha256_file over fixed bytes ------------------------------------------

#[test]
fn sha256_file_matches_in_memory_digest() {
    let dir = TempDir::new("hashfile");
    let path = dir.path().join("artifact.bin");
    std::fs::write(&path, b"velnor-runner-bytes").unwrap();
    assert_eq!(
        sha256_file(&path).unwrap(),
        Sha256Hex::of_bytes(b"velnor-runner-bytes")
    );
}

// --- package records (issue #673) ------------------------------------------

/// The version the preview workflow builds and publishes: the crate version is
/// prefixed by `~` so dpkg ranks the preview strictly below its own release.
fn preview_debian_version(crate_version: &str) -> String {
    format!("{crate_version}~preview.25+8d49319")
}

fn package_record_for(kind: &str, arch: Arch) -> PackageRecord {
    let debian_version = match kind {
        PACKAGE_KIND_PREVIEW => preview_debian_version("0.1.121"),
        _ => "0.1.121".to_string(),
    };
    PackageRecord {
        schema: PACKAGE_RECORD_SCHEMA.to_string(),
        build: PackageBuildIdentity {
            repository: SOURCE_REPOSITORY.to_string(),
            kind: kind.to_string(),
            commit: source_sha("commit-seed"),
            crate_version: "0.1.121".to_string(),
            debian_version,
            manifest_version: crate::manifest::MANIFEST_VERSION,
            manifest_sha256: digest_of("manifest"),
        },
        architecture: PackageArchitectureIdentity {
            arch,
            target: arch.target().to_string(),
            binary_sha256: digest_of(&format!("bin-{}", arch.as_str())),
        },
    }
}

fn deployed_for_package(record: &PackageRecord) -> DeployedIdentity {
    DeployedIdentity {
        schema: DEPLOYED_IDENTITY_SCHEMA.to_string(),
        package_version: record.build.debian_version.clone(),
        crate_version: record.build.crate_version.clone(),
        source_commit: record.build.commit.clone(),
        binary_sha256: record.architecture.binary_sha256.clone(),
        manifest_version: record.build.manifest_version,
        manifest_sha256: record.build.manifest_sha256.clone(),
        // A package ships no image, so its deployed identity names none.
        oci_image_digest: None,
        record_sha256: record.digest(),
    }
}

#[test]
fn preview_and_stable_package_records_verify() {
    assert_eq!(
        package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64).verify(),
        Ok(())
    );
    assert_eq!(
        package_record_for(PACKAGE_KIND_STABLE, Arch::Arm64).verify(),
        Ok(())
    );
}

#[test]
fn installed_package_version_query_requires_a_fully_installed_deb() {
    assert_eq!(
        parse_installed_debian_package_version(
            "install ok installed\t0.1.121~preview.25+8d49319\n"
        )
        .unwrap(),
        "0.1.121~preview.25+8d49319"
    );
    assert_eq!(
        parse_installed_debian_package_version("hold ok installed\t0.1.121").unwrap(),
        "0.1.121"
    );
    assert!(
        parse_installed_debian_package_version("install reinstreq installed\t0.1.121").is_err()
    );
    assert!(parse_installed_debian_package_version(
        "install ok half-configured\t0.1.121~preview.25+8d49319"
    )
    .is_err());
    assert!(parse_installed_debian_package_version("install ok installed\t").is_err());
}

#[test]
fn activation_binds_same_preview_bytes_to_the_installed_package_version() {
    let dir = TempDir::new("activate-preview-package-version");
    let store = ReleaseStore::new(dir.path());
    let transaction_lock = transaction_lock_for(&dir);
    let host = Arch::host().unwrap();
    let installed = package_record_for(PACKAGE_KIND_PREVIEW, host);
    let mut candidate = installed.clone();
    candidate.build.debian_version = "0.1.121~preview.26+8d49319".into();

    assert_eq!(installed.verify(), Ok(()));
    assert_eq!(candidate.verify(), Ok(()));
    assert_eq!(installed.build.commit, candidate.build.commit);
    assert_eq!(
        installed.architecture.binary_sha256,
        candidate.architecture.binary_sha256
    );
    assert_ne!(installed.digest(), candidate.digest());

    let installed_record = ActiveRecord::Package(installed.clone());
    store
        .activate(
            &installed_record,
            &deployed_for_package(&installed),
            &transaction_lock,
            &installed.build.debian_version,
        )
        .unwrap();

    let error = store
        .activate(
            &ActiveRecord::Package(candidate.clone()),
            &deployed_for_package(&candidate),
            &transaction_lock,
            &installed.build.debian_version,
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("installed Debian package version does not match"));
    assert_eq!(
        store.active_tag().unwrap().as_deref(),
        Some(installed.build.debian_version.as_str())
    );
    assert_eq!(store.previous_tag().unwrap(), None);
}

#[test]
fn rollback_requires_reinstalling_the_older_preview_package_version() {
    let dir = TempDir::new("rollback-preview-package-version");
    let store = ReleaseStore::new(dir.path());
    let transaction_lock = transaction_lock_for(&dir);
    let host = Arch::host().unwrap();
    let mut older = package_record_for(PACKAGE_KIND_PREVIEW, host);
    older.build.debian_version = "0.1.121~preview.25+8d49319".into();
    let mut newer = older.clone();
    newer.build.debian_version = "0.1.121~preview.26+8d49319".into();
    assert_eq!(older.verify(), Ok(()));
    assert_eq!(newer.verify(), Ok(()));
    assert_eq!(older.build.commit, newer.build.commit);
    assert_eq!(
        older.architecture.binary_sha256,
        newer.architecture.binary_sha256
    );

    store
        .activate(
            &ActiveRecord::Package(older.clone()),
            &deployed_for_package(&older),
            &transaction_lock,
            &older.build.debian_version,
        )
        .unwrap();
    store
        .activate(
            &ActiveRecord::Package(newer.clone()),
            &deployed_for_package(&newer),
            &transaction_lock,
            &newer.build.debian_version,
        )
        .unwrap();
    let installed_binary = dir.path().join("velnor-runner");
    write_fixture_runner_binary(&ActiveRecord::Package(older.clone()), &installed_binary);

    let error = store
        .rollback(
            &transaction_lock,
            &installed_binary,
            &newer.build.debian_version,
            |_| Ok(()),
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("installed Debian package version does not match"));
    assert_eq!(
        store.active_tag().unwrap().as_deref(),
        Some(newer.build.debian_version.as_str())
    );
    assert_eq!(
        store.previous_tag().unwrap().as_deref(),
        Some(older.build.debian_version.as_str())
    );
}

/// Kind and Debian version validate each other, so a record can never be
/// re-labelled across channels without breaking the version contract.
#[test]
fn package_record_kind_must_satisfy_its_version_contract() {
    let cases: Vec<(String, String, CoherenceError)> = vec![
        // A preview record whose Debian version equals its crate version would
        // win an upgrade race against the stable release: never.
        (
            PACKAGE_KIND_PREVIEW.into(),
            "0.1.121".into(),
            CoherenceError::PackageVersion,
        ),
        // A preview record without the `~preview.<run>+<short>` shape.
        (
            PACKAGE_KIND_PREVIEW.into(),
            "0.1.999".into(),
            CoherenceError::PackageVersion,
        ),
        (
            PACKAGE_KIND_PREVIEW.into(),
            "0.1.121~preview.".into(),
            CoherenceError::PackageVersion,
        ),
        (
            PACKAGE_KIND_PREVIEW.into(),
            "0.1.121~preview.25".into(),
            CoherenceError::PackageVersion,
        ),
        (
            PACKAGE_KIND_PREVIEW.into(),
            "0.1.121~preview.25+8d4931".into(),
            CoherenceError::PackageVersion,
        ),
        (
            PACKAGE_KIND_PREVIEW.into(),
            "0.1.121~preview.25+8D49319".into(),
            CoherenceError::PackageVersion,
        ),
        (
            PACKAGE_KIND_PREVIEW.into(),
            "0.1.121~preview.x+8d49319".into(),
            CoherenceError::PackageVersion,
        ),
        // A stable record must keep the exact release version, never a preview
        // suffix: the stable chain's coherence is unchanged.
        (
            PACKAGE_KIND_STABLE.into(),
            "0.1.121~preview.25+8d49319".into(),
            CoherenceError::PackageVersion,
        ),
        // No third kind exists.
        (
            "development".into(),
            preview_debian_version("0.1.121"),
            CoherenceError::Kind,
        ),
    ];
    for (kind, debian_version, expected) in cases {
        let mut record = package_record_for(&kind, Arch::Amd64);
        record.build.debian_version = debian_version;
        assert_eq!(record.verify(), Err(expected), "kind {kind}");
    }
}

#[test]
fn package_record_is_deterministic_and_acyclic() {
    let record = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64);
    assert_eq!(record.to_canonical_json(), record.to_canonical_json());
    let bytes = record.to_canonical_json();
    // Same acyclicity rule as the release record: no self digest inside.
    assert!(!bytes.contains(record.digest().as_str()));
    // The record names its binary digest exactly once (the architecture entry).
    assert_eq!(
        bytes
            .matches(record.architecture.binary_sha256.as_str())
            .count(),
        1
    );
}

#[test]
fn package_record_and_release_record_describe_one_source() {
    // Both record kinds are derived from the same source identity, so a host can
    // hold either tuple for the same commit and manifest.
    let release = valid_record();
    let package = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64);
    assert_eq!(release.build.commit, package.build.commit);
    assert_eq!(release.build.manifest_sha256, package.build.manifest_sha256);
    assert_eq!(release.build.crate_version, package.build.crate_version);
}

#[test]
fn package_record_rejects_unknown_fields_and_wrong_schema() {
    let mut value =
        serde_json::to_value(package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64)).unwrap();
    value["apt_token"] = serde_json::json!("ghp_secretsecretsecret");
    assert!(serde_json::from_value::<PackageRecord>(value).is_err());

    let mut wrong_schema = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64);
    wrong_schema.schema = "velnor.package-record/v2".into();
    assert_eq!(
        wrong_schema.verify(),
        Err(CoherenceError::Schema {
            want: PACKAGE_RECORD_SCHEMA,
        })
    );

    let mut wrong_repo = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64);
    wrong_repo.build.repository = "evil/fork".into();
    assert_eq!(wrong_repo.verify(), Err(CoherenceError::Repository));

    let mut wrong_target = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Arm64);
    wrong_target.architecture.target = "wrong-triple".into();
    assert_eq!(wrong_target.verify(), Err(CoherenceError::ArchTarget));
}

#[test]
fn verify_package_record_bytes_enforces_the_same_contract() {
    let record = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64);
    let bytes = record.to_canonical_json();
    let checksum = Sha256Hex::of_bytes(bytes.as_bytes());
    assert_eq!(
        verify_package_record_bytes(bytes.as_bytes(), &checksum).map(|parsed| parsed.digest()),
        Ok(record.digest())
    );

    let wrong = digest_of("wrong");
    assert_eq!(
        verify_package_record_bytes(bytes.as_bytes(), &wrong),
        Err(CoherenceError::RecordChecksum)
    );

    let mut padded = bytes.clone().into_bytes();
    padded.extend_from_slice(b"   \n");
    assert_eq!(
        verify_package_record_bytes(&padded, &Sha256Hex::of_bytes(&padded)),
        Err(CoherenceError::NonCanonical)
    );

    assert_eq!(
        verify_package_record_bytes(b"{not json", &Sha256Hex::of_bytes(b"{not json")),
        Err(CoherenceError::Malformed)
    );
}

#[test]
fn active_record_parses_either_schema_and_refuses_others() {
    let package = ActiveRecord::parse(
        package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64)
            .to_canonical_json()
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(
        package,
        ActiveRecord::Package(package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64))
    );
    assert_eq!(package.store_key(), "0.1.121~preview.25+8d49319");
    assert_eq!(package.label(), "preview 0.1.121~preview.25+8d49319");
    assert!(package.oci_index_digest().is_none());

    let release = ActiveRecord::parse(valid_record().to_canonical_json().as_bytes()).unwrap();
    assert_eq!(release.store_key(), "v0.1.121");
    assert_eq!(release.label(), "v0.1.121");
    assert!(release.oci_index_digest().is_some());

    let unknown = br#"{"schema":"velnor.release-record/v9"}"#;
    assert_eq!(
        ActiveRecord::parse(unknown),
        Err(CoherenceError::RecordSchema)
    );
    assert_eq!(
        ActiveRecord::parse(b"{not json"),
        Err(CoherenceError::Malformed)
    );
}

#[test]
fn emit_package_record_refuses_development_identity() {
    let identity = embedded();
    assert!(identity.is_development());
    let record = package_record_for(PACKAGE_KIND_PREVIEW, Arch::host().unwrap());
    let dir = TempDir::new("emit-pkg-binary");
    let binary = dir.path().join("velnor-runner");
    std::fs::write(&binary, b"runner-bytes").unwrap();
    let err = emit_package_record(PackageRecordEmission {
        identity: &identity,
        record: &record,
        binary: &binary,
    })
    .unwrap_err();
    assert!(err.to_string().contains("development"));
}

#[test]
fn emit_package_record_accepts_a_matching_preview_identity() {
    let mut record = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64);
    // Name exactly the bytes the packaging lane stages.
    record.architecture.binary_sha256 = Sha256Hex::of_bytes(b"exact-runner-bytes");
    let identity = EmbeddedIdentity {
        source_sha: record.build.commit.as_str().to_string(),
        tag: "preview".into(),
        kind: PACKAGE_KIND_PREVIEW.into(),
        crate_version: record.build.crate_version.clone(),
    };
    let dir = TempDir::new("emit-pkg-ok");
    let binary = dir.path().join("velnor-runner");
    std::fs::write(&binary, b"exact-runner-bytes").unwrap();
    emit_package_record(PackageRecordEmission {
        identity: &identity,
        record: &record,
        binary: &binary,
    })
    .unwrap();

    // The staged record is byte-identical to the canonical form and re-verifies
    // against the digest the packaging lane publishes beside it.
    let bytes = record.to_canonical_json();
    verify_package_record_bytes(bytes.as_bytes(), &Sha256Hex::of_bytes(bytes.as_bytes())).unwrap();
}

/// A record is only ever staged against the bytes it names, from a binary whose
/// own embedded identity agrees with it.
#[test]
fn emit_package_record_rejects_every_binding_drift() {
    let mut record = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64);
    record.architecture.binary_sha256 = Sha256Hex::of_bytes(b"exact-runner-bytes");
    let dir = TempDir::new("emit-pkg-drift");
    let binary = dir.path().join("velnor-runner");
    std::fs::write(&binary, b"exact-runner-bytes").unwrap();
    let emission = |identity: &EmbeddedIdentity, record: &PackageRecord| {
        emit_package_record(PackageRecordEmission {
            identity,
            record,
            binary: &binary,
        })
    };

    let matching = EmbeddedIdentity {
        source_sha: record.build.commit.as_str().to_string(),
        tag: "preview".into(),
        kind: PACKAGE_KIND_PREVIEW.into(),
        crate_version: record.build.crate_version.clone(),
    };
    // Sanity: the matching identity emits cleanly, so each rejection below is
    // caused by the single field it drifts.
    emission(&matching, &record).unwrap();

    let mut other_commit = matching.clone();
    other_commit.source_sha = source_sha("other-commit").as_str().to_string();
    assert!(emission(&other_commit, &record)
        .unwrap_err()
        .to_string()
        .contains("source commit"));

    let mut other_version = matching.clone();
    other_version.crate_version = "0.1.999".into();
    assert!(emission(&other_version, &record)
        .unwrap_err()
        .to_string()
        .contains("crate version"));

    // Kind mismatch: a preview record cannot come from a release-build binary.
    let mut stable_identity = matching.clone();
    stable_identity.kind = PACKAGE_KIND_STABLE.into();
    assert!(emission(&stable_identity, &record)
        .unwrap_err()
        .to_string()
        .contains("kind"));

    // Binary drift: the record must never ship against bytes it does not name.
    let tampered = dir.path().join("tampered");
    std::fs::write(&tampered, b"tampered-runner-bytes").unwrap();
    assert!(emit_package_record(PackageRecordEmission {
        identity: &matching,
        record: &record,
        binary: &tampered,
    })
    .unwrap_err()
    .to_string()
    .contains("binary digest disagrees"));
}

#[test]
fn verify_installed_accepts_a_preview_package_tuple() {
    let record = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64);
    let deployed = deployed_for_package(&record);
    assert_eq!(
        verify_installed(
            &deployed,
            &ActiveRecord::Package(record.clone()),
            Arch::Amd64,
            &deployed.binary_sha256
        ),
        Ok(())
    );
}

#[test]
fn verify_installed_catches_drift_in_a_preview_package_tuple() {
    let record = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64);
    let active = ActiveRecord::Package(record.clone());

    // A package tuple must name no image: the preview lane builds none.
    let mut with_image = deployed_for_package(&record);
    with_image.oci_image_digest = Some(oci_of("some-index"));
    assert_eq!(
        verify_installed(&with_image, &active, Arch::Amd64, &with_image.binary_sha256),
        Err(CoherenceError::InstalledOci)
    );

    let mut wrong_binary = deployed_for_package(&record);
    wrong_binary.binary_sha256 = digest_of("tampered-binary");
    assert_eq!(
        verify_installed(
            &wrong_binary,
            &active,
            Arch::Amd64,
            &wrong_binary.binary_sha256
        ),
        Err(CoherenceError::InstalledBinary)
    );

    // Installed bytes disagreeing with the record fail even with a coherent
    // deployed identity.
    let deployed = deployed_for_package(&record);
    assert_eq!(
        verify_installed(
            &deployed,
            &active,
            Arch::Amd64,
            &digest_of("tampered-binary")
        ),
        Err(CoherenceError::InstalledBinary)
    );

    let mut wrong_source = deployed_for_package(&record);
    wrong_source.source_commit = source_sha("other");
    assert_eq!(
        verify_installed(
            &wrong_source,
            &active,
            Arch::Amd64,
            &wrong_source.binary_sha256
        ),
        Err(CoherenceError::InstalledSource)
    );

    let mut wrong_pointer = deployed_for_package(&record);
    wrong_pointer.record_sha256 = digest_of("x");
    assert_eq!(
        verify_installed(
            &wrong_pointer,
            &active,
            Arch::Amd64,
            &wrong_pointer.binary_sha256
        ),
        Err(CoherenceError::InstalledRecordPointer)
    );

    // The record carries exactly one architecture.
    assert_eq!(
        verify_installed(&deployed, &active, Arch::Arm64, &deployed.binary_sha256),
        Err(CoherenceError::InstalledArchMissing)
    );
}

#[test]
fn verify_installed_command_reads_a_preview_package_tuple() {
    let dir = TempDir::new("verify-installed-preview");
    let mut record = package_record_for(PACKAGE_KIND_PREVIEW, Arch::host().unwrap());
    record.architecture.binary_sha256 = Sha256Hex::of_bytes(b"installed-bytes");
    let deployed = deployed_for_package(&record);
    let record_path = dir.path().join("record.json");
    let deployed_path = dir.path().join("deployed.json");
    let binary_path = dir.path().join("velnor-runner");
    std::fs::write(&record_path, record.to_canonical_json()).unwrap();
    std::fs::write(&deployed_path, serde_json::to_vec(&deployed).unwrap()).unwrap();
    std::fs::write(&binary_path, b"installed-bytes").unwrap();

    verify_installed_command_with_package_version(
        ReleaseVerifyInstalledArgs {
            record: record_path,
            deployed: deployed_path,
            binary: binary_path,
            arch: None,
        },
        &record.build.debian_version,
    )
    .unwrap();

    let mut other_published_version = record.clone();
    other_published_version.build.debian_version = "0.1.121~preview.26+8d49319".into();
    assert_eq!(other_published_version.verify(), Ok(()));
    let version_error = verify_installed_command_with_package_version(
        ReleaseVerifyInstalledArgs {
            record: dir.path().join("record.json"),
            deployed: dir.path().join("deployed.json"),
            binary: dir.path().join("velnor-runner"),
            arch: None,
        },
        &other_published_version.build.debian_version,
    )
    .unwrap_err();
    assert!(version_error
        .to_string()
        .contains("installed Debian package version does not match"));

    // And an installed binary that disagrees fails closed.
    let tampered = dir.path().join("tampered");
    std::fs::write(&tampered, b"tampered-bytes").unwrap();
    assert!(verify_installed_command_with_package_version(
        ReleaseVerifyInstalledArgs {
            record: dir.path().join("record.json"),
            deployed: dir.path().join("deployed.json"),
            binary: tampered,
            arch: None,
        },
        &record.build.debian_version,
    )
    .is_err());
}

#[test]
fn store_activates_and_rolls_back_preview_package_tuples() {
    let dir = TempDir::new("store-preview");
    let store = ReleaseStore::new(dir.path());
    let transaction_lock = transaction_lock_for(&dir);
    let host = Arch::host().unwrap();

    let mut older = package_record_for(PACKAGE_KIND_PREVIEW, host);
    older.build.crate_version = "0.1.120".into();
    older.build.debian_version = preview_debian_version("0.1.120");
    assert_eq!(older.verify(), Ok(()));

    let newer = package_record_for(PACKAGE_KIND_PREVIEW, host);
    let old_record = ActiveRecord::Package(older.clone());
    let new_record = ActiveRecord::Package(newer.clone());

    store
        .activate(
            &old_record,
            &deployed_for_package(&older),
            &transaction_lock,
            &older.build.debian_version,
        )
        .unwrap();
    store
        .activate(
            &new_record,
            &deployed_for_package(&newer),
            &transaction_lock,
            &newer.build.debian_version,
        )
        .unwrap();

    assert_eq!(
        store.active_tag().unwrap().as_deref(),
        Some(newer.build.debian_version.as_str())
    );
    assert_eq!(
        store.previous_tag().unwrap().as_deref(),
        Some(older.build.debian_version.as_str())
    );
    assert_eq!(
        std::fs::read(dir.path().join("active/record.json")).unwrap(),
        new_record.to_canonical_json().as_bytes()
    );
    // A stored package record re-reads as a package record under its Debian
    // version key.
    assert_eq!(
        store
            .read_record(newer.build.debian_version.as_str())
            .unwrap(),
        new_record
    );

    let installed_binary = dir.path().join("velnor-runner");
    write_fixture_runner_binary(&old_record, &installed_binary);
    let restored = store
        .rollback(
            &transaction_lock,
            &installed_binary,
            &older.build.debian_version,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(restored, older.build.debian_version);
    assert_eq!(
        store.read_record(restored.as_str()).unwrap(),
        ActiveRecord::Package(older)
    );
}

/// Activation of a stable package record is refused before any byte is hashed:
/// the stable chain activates from its out-of-band release record, never from
/// the deb carrying an informational package record.
#[test]
fn activate_command_refuses_a_stable_package_record() {
    let dir = TempDir::new("activate-stable-package");
    let record = package_record_for(PACKAGE_KIND_STABLE, Arch::host().unwrap());
    let record_path = dir.path().join("record.json");
    std::fs::write(&record_path, record.to_canonical_json()).unwrap();

    let transaction_lock = transaction_lock_for(&dir);
    let error = activate_command_with_lock_and_package_version(
        ReleaseActivateArgs {
            dir: dir.path().join("store"),
            record: record_path,
        },
        &transaction_lock,
        &record.build.debian_version,
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("a stable package activates from its out-of-band release record"));
}

#[test]
fn activate_command_refuses_a_package_for_another_host_architecture() {
    let dir = TempDir::new("activate-package-arch");
    let other = match Arch::host().unwrap() {
        Arch::Amd64 => Arch::Arm64,
        Arch::Arm64 => Arch::Amd64,
    };
    let record = package_record_for(PACKAGE_KIND_PREVIEW, other);
    let record_path = dir.path().join("record.json");
    std::fs::write(&record_path, record.to_canonical_json()).unwrap();

    let transaction_lock = transaction_lock_for(&dir);
    let error = activate_command_with_lock_and_package_version(
        ReleaseActivateArgs {
            dir: dir.path().join("store"),
            record: record_path,
        },
        &transaction_lock,
        &record.build.debian_version,
    )
    .unwrap_err();
    assert!(error.to_string().contains("but this host is"));
}

#[test]
fn emit_command_requires_a_binary_for_a_package_record() {
    let dir = TempDir::new("emit-command-package");
    let record_path = dir.path().join("package-record.json");
    std::fs::write(
        &record_path,
        package_record_for(PACKAGE_KIND_PREVIEW, Arch::host().unwrap()).to_canonical_json(),
    )
    .unwrap();

    // No --binary: refused before anything is staged.
    let error = emit_command(ReleaseEmitArgs {
        record: record_path.clone(),
        out_dir: dir.path().join("store"),
        out: None,
        binary: None,
    })
    .unwrap_err();
    assert!(error.to_string().contains("requires --binary"));

    // With --binary the development identity of the test binary still refuses:
    // no provenance-less bytes can produce a package record.
    let binary = dir.path().join("velnor-runner");
    std::fs::write(&binary, b"runner-bytes").unwrap();
    let error = emit_command(ReleaseEmitArgs {
        record: record_path,
        out_dir: dir.path().join("store"),
        out: None,
        binary: Some(binary),
    })
    .unwrap_err();
    assert!(error.to_string().contains("development"));
}

#[test]
fn verify_record_command_verifies_and_guards_a_package_record() {
    let dir = TempDir::new("verify-record-package");
    let record = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64);
    let record_path = dir.path().join("package-record.json");
    let checksum_path = dir.path().join("package-record.json.sha256");
    let bytes = record.to_canonical_json();
    let digest = Sha256Hex::of_bytes(bytes.as_bytes());
    std::fs::write(&record_path, &bytes).unwrap();
    std::fs::write(&checksum_path, format!("{digest}  package-record.json\n")).unwrap();

    verify_record_command(ReleaseVerifyRecordArgs {
        record: record_path.clone(),
        checksum: Some(checksum_path),
        sha256: None,
        publication: None,
        expected_apt_metadata: None,
        served_apt_metadata: None,
    })
    .unwrap();

    // A package record has no publication: the stable chain's APT claims do not
    // apply to it.
    let publication_path = dir.path().join("publication.json");
    std::fs::write(
        &publication_path,
        serde_json::to_vec(&publication_for(&valid_record())).unwrap(),
    )
    .unwrap();
    let error = verify_record_command(ReleaseVerifyRecordArgs {
        record: record_path,
        checksum: Some(dir.path().join("package-record.json.sha256")),
        sha256: None,
        publication: Some(publication_path),
        expected_apt_metadata: None,
        served_apt_metadata: None,
    })
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("a package record has no publication to check"));
}

#[test]
fn verify_record_command_accepts_sha256_for_a_package_record() {
    let dir = TempDir::new("verify-record-package-hex");
    let record = package_record_for(PACKAGE_KIND_PREVIEW, Arch::Amd64);
    let record_path = dir.path().join("package-record.json");
    let bytes = record.to_canonical_json();
    std::fs::write(&record_path, &bytes).unwrap();

    verify_record_command(ReleaseVerifyRecordArgs {
        record: record_path,
        checksum: None,
        sha256: Some(Sha256Hex::of_bytes(bytes.as_bytes()).as_str().to_string()),
        publication: None,
        expected_apt_metadata: None,
        served_apt_metadata: None,
    })
    .unwrap();
}

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
    assert!(postrm.contains("'velnor-daemon@*.service'"));
    assert!(postrm.contains("systemctl disable \"$unit\""));
}

#[test]
fn debian_preinst_supports_scoped_canary_drains() {
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
    assert!(preinst.contains("VELNOR_DRAINED_UNITS"));
    assert!(preinst.contains("scoped_units_drained"));
    assert!(preinst.contains("invalid scoped unit name"));
}

#[cfg(target_os = "linux")]
#[test]
fn maintainer_lock_proof_rejects_marker_and_shared_lock_spoofs() {
    use std::{
        fs,
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
    let script_source = include_str!("../../debian/preinst")
        .replace(
            "PACKAGE_TRANSACTION_LOCK=/run/velnor/package-transaction.lock",
            &format!("PACKAGE_TRANSACTION_LOCK={}", lock.display()),
        )
        .replace(
            "if [ -d /run/systemd/system ]; then",
            "if [ -d /__velnor-lock-test-no-systemd ]; then",
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

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn shipped_velnor_services_hold_shared_package_lock_across_exec() {
    let debian_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("debian");
    let expected = "/usr/bin/flock --shared --no-fork /run/velnor/package-transaction.lock";
    let mut service_count = 0;

    for entry in std::fs::read_dir(&debian_dir).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("service") {
            continue;
        }
        service_count += 1;
        let unit = std::fs::read_to_string(entry.path()).unwrap();
        for line in unit.lines().filter(|line| line.starts_with("ExecStart")) {
            assert!(
                line.contains(expected),
                "{} has an unguarded Velnor command: {line}",
                entry.path().display()
            );
        }
    }

    assert_eq!(
        service_count, 8,
        "all shipped Velnor service units must be audited"
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
            "../../fleet/release-refs.toml",
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
        include_str!("../../../../fleet/release-refs.toml").contains("schema_version = 1"),
        "packaged fleet ledger must be the repository's authoritative schema-v1 ledger"
    );
}

#[test]
fn debian_package_declares_flock_provider() {
    let manifest = include_str!("../../Cargo.toml");
    assert!(manifest.contains("util-linux (>= 2.37.2)"));
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
        oci_image_digest: record.oci_index_digest.clone(),
        record_sha256: record.digest(),
    }
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
fn record_verify_requires_exact_canonical_oci_identity() {
    let record = valid_record();
    let digest = record.oci_index_digest.as_str();
    let invalid_refs = [
        format!("ghcr.io/other/velnor-job-ubuntu@{digest}"),
        "ghcr.io/tailrocks/velnor-job-ubuntu:latest".to_string(),
        format!("ghcr.io/tailrocks/velnor-job-ubuntu@{digest}-suffix"),
        format!("ghcr.io/tailrocks/velnor-job-ubuntu@prefix-{digest}"),
    ];

    for oci_image_ref in invalid_refs {
        let mut candidate = record.clone();
        candidate.oci_image_ref = oci_image_ref;
        assert_eq!(candidate.verify(), Err(CoherenceError::OciRef));
    }
}

#[test]
fn release_store_rejects_unsafe_tags_before_any_write() {
    let dir = TempDir::new("unsafe-tag");
    let store = ReleaseStore::new(dir.path());
    let outside = dir.path().parent().unwrap().join(format!(
        "{}-outside",
        dir.path().file_name().unwrap().to_string_lossy()
    ));

    for tag in ["", ".", "..", "v0/1", r"v0\1", "v0\0"] {
        assert!(store.record_path(tag).is_err());
        assert!(store.deployed_path(tag).is_err());
    }

    let mut record = valid_record();
    record.build.tag = "../../outside".into();
    let error = store.store_record(&record).unwrap_err();
    assert!(error.to_string().contains("safe path component"));
    assert!(!outside.exists());
    assert!(!dir.path().join("records").exists());
}

#[test]
fn release_store_rejects_unsafe_symlink_derived_tags() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("unsafe-pointer");
    symlink("records/../escape", dir.path().join("active")).unwrap();
    let error = ReleaseStore::new(dir.path()).active_tag().unwrap_err();
    assert!(error.to_string().contains("release pointer target"));
}

#[test]
fn release_store_rejects_symlinked_records_and_tag_directories() {
    use std::os::unix::fs::symlink;

    let records_root = TempDir::new("symlink-records");
    let outside = records_root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink("outside", records_root.path().join("records")).unwrap();
    let store = ReleaseStore::new(records_root.path());
    assert!(store.store_record(&valid_record()).is_err());
    assert!(!outside.join("v0.1.121").exists());

    let tag_root = TempDir::new("symlink-tag");
    let outside = tag_root.path().join("outside");
    std::fs::create_dir_all(tag_root.path().join("records")).unwrap();
    std::fs::create_dir(&outside).unwrap();
    symlink("../outside", tag_root.path().join("records/v0.1.121")).unwrap();
    let store = ReleaseStore::new(tag_root.path());
    assert!(store.store_record(&valid_record()).is_err());
    assert!(!outside.join("record.json").exists());
}

#[test]
fn release_store_rejects_dangling_active_before_demoting_it() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("dangling-active");
    std::fs::create_dir(dir.path().join("records")).unwrap();
    symlink("records/v0.1.121", dir.path().join("active")).unwrap();
    let store = ReleaseStore::new(dir.path());
    let next = valid_record();
    let next_deployed = deployed_for(&next, Arch::host().unwrap());
    assert!(store.activate(&next, &next_deployed).is_err());
    assert_eq!(
        std::fs::read_link(dir.path().join("active")).unwrap(),
        Path::new("records/v0.1.121")
    );
    assert!(!dir.path().join("previous").exists());
}

#[test]
fn release_store_rejects_mismatched_active_tuple_without_pointer_mutation() {
    let dir = TempDir::new("mismatched-active");
    let store = ReleaseStore::new(dir.path());
    let first = valid_record();
    let host = Arch::host().unwrap();
    store.activate(&first, &deployed_for(&first, host)).unwrap();
    let deployed_path = store.deployed_path(first.build.tag.as_str()).unwrap();
    let mut tampered = deployed_for(&first, host);
    tampered.record_sha256 = digest_of("wrong-record");
    std::fs::write(
        &deployed_path,
        serde_json::to_vec_pretty(&tampered).unwrap(),
    )
    .unwrap();

    let next = {
        let mut record = valid_record();
        record.build.tag = "v0.1.122".into();
        record.build.crate_version = "0.1.122".into();
        record.build.debian_version = "0.1.122".into();
        record.oci_labels.version = "0.1.122".into();
        record
    };
    assert!(store.activate(&next, &deployed_for(&next, host)).is_err());
    assert!(store.active_tag().is_err());
    assert!(!dir.path().join("previous").exists());
}

#[test]
fn release_store_rejects_symlinked_tuple_files() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("symlink-tuple-files");
    let store = ReleaseStore::new(dir.path());
    let record = valid_record();
    let host = Arch::host().unwrap();
    store
        .activate(&record, &deployed_for(&record, host))
        .unwrap();

    let outside = dir.path().join("outside");
    std::fs::write(&outside, b"outside").unwrap();
    let record_path = store.record_path(record.build.tag.as_str()).unwrap();
    std::fs::remove_file(&record_path).unwrap();
    symlink("../../outside", &record_path).unwrap();
    assert!(store.active_tag().is_err());
    std::fs::remove_file(&record_path).unwrap();
    std::fs::write(&record_path, record.to_canonical_json()).unwrap();

    let deployed_path = store.deployed_path(record.build.tag.as_str()).unwrap();
    std::fs::remove_file(&deployed_path).unwrap();
    symlink("../../outside", &deployed_path).unwrap();
    assert!(store.active_tag().is_err());
    std::fs::remove_file(&deployed_path).unwrap();
    std::fs::write(
        &deployed_path,
        serde_json::to_vec_pretty(&deployed_for(&record, host)).unwrap(),
    )
    .unwrap();

    let checksum_path = dir
        .path()
        .join("records")
        .join(format!("{}.json.sha256", record.build.tag));
    std::fs::remove_file(&checksum_path).unwrap();
    symlink("../../outside", &checksum_path).unwrap();
    assert!(store.active_tag().is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
}

#[test]
fn release_store_rejects_symlinked_root() {
    use std::os::unix::fs::symlink;

    let actual = TempDir::new("root-target");
    let link = actual.path().parent().unwrap().join(format!(
        "{}-link",
        actual.path().file_name().unwrap().to_string_lossy()
    ));
    symlink(actual.path(), &link).unwrap();
    let store = ReleaseStore::new(&link);
    assert!(store.store_record(&valid_record()).is_err());
    assert!(!actual.path().join("records").exists());
}

#[test]
fn release_store_rejects_symlinked_root_ancestor() {
    use std::os::unix::fs::symlink;

    let outside = TempDir::new("ancestor-target");
    let parent = TempDir::new("ancestor-parent");
    let parent_link = parent.path().join("link");
    symlink(outside.path(), &parent_link).unwrap();
    let store = ReleaseStore::new(parent_link.join("release"));
    assert!(store.store_record(&valid_record()).is_err());
    assert!(!outside.path().join("release").exists());
}

#[test]
fn release_store_recovers_a_partially_published_transition() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("transition-recovery");
    let store = ReleaseStore::new(dir.path());
    let host = Arch::host().unwrap();
    let first = valid_record();
    store.activate(&first, &deployed_for(&first, host)).unwrap();

    let second = {
        let mut record = valid_record();
        record.build.tag = "v0.1.122".into();
        record.build.crate_version = "0.1.122".into();
        record.build.debian_version = "0.1.122".into();
        record.oci_labels.version = "0.1.122".into();
        record
    };
    store.store_record(&second).unwrap();
    std::fs::write(
        store.deployed_path(second.build.tag.as_str()).unwrap(),
        serde_json::to_vec_pretty(&deployed_for(&second, host)).unwrap(),
    )
    .unwrap();

    let transition = ReleaseTransition {
        from_active: Some(first.build.tag.clone()),
        from_previous: None,
        to_active: Some(second.build.tag.clone()),
        to_previous: Some(first.build.tag.clone()),
    };
    write_atomic(
        &dir.path().join(RELEASE_TRANSITION_FILE_NAME),
        &serde_json::to_vec(&transition).unwrap(),
    )
    .unwrap();
    symlink(
        format!("records/{}", first.build.tag),
        dir.path().join("previous"),
    )
    .unwrap();

    assert_eq!(
        store.active_tag().unwrap().as_deref(),
        Some(second.build.tag.as_str())
    );
    assert_eq!(
        store.previous_tag().unwrap().as_deref(),
        Some(first.build.tag.as_str())
    );
    assert!(!dir.path().join(RELEASE_TRANSITION_FILE_NAME).exists());
}

#[test]
fn release_store_rejects_incoherent_previous_on_rollback() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("incoherent-previous");
    let store = ReleaseStore::new(dir.path());
    let first = valid_record();
    let host = Arch::host().unwrap();
    store.activate(&first, &deployed_for(&first, host)).unwrap();
    let second = {
        let mut record = valid_record();
        record.build.tag = "v0.1.122".into();
        record.build.crate_version = "0.1.122".into();
        record.build.debian_version = "0.1.122".into();
        record.oci_labels.version = "0.1.122".into();
        record
    };
    store
        .activate(&second, &deployed_for(&second, host))
        .unwrap();
    std::fs::remove_file(store.deployed_path(first.build.tag.as_str()).unwrap()).unwrap();
    let active_before = std::fs::read_link(dir.path().join("active")).unwrap();
    let previous_before = std::fs::read_link(dir.path().join("previous")).unwrap();
    assert!(store.rollback().is_err());
    assert_eq!(
        std::fs::read_link(dir.path().join("active")).unwrap(),
        active_before
    );
    assert_eq!(
        std::fs::read_link(dir.path().join("previous")).unwrap(),
        previous_before
    );

    std::fs::remove_file(dir.path().join("previous")).unwrap();
    symlink("records/v0.1.122", dir.path().join("previous")).unwrap();
    assert!(store.rollback().is_err());
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
        verify_installed(&deployed, &record, host, &deployed.binary_sha256),
        Ok(())
    );
}

#[test]
fn verify_installed_catches_each_field() {
    let record = valid_record();
    let host = Arch::Amd64;

    let mut wrong_pointer = deployed_for(&record, host);
    wrong_pointer.record_sha256 = digest_of("x");
    assert_eq!(
        verify_installed(&wrong_pointer, &record, host, &wrong_pointer.binary_sha256),
        Err(CoherenceError::InstalledRecordPointer)
    );

    let mut wrong_source = deployed_for(&record, host);
    wrong_source.source_commit = source_sha("other");
    wrong_source.record_sha256 = record.digest();
    assert_eq!(
        verify_installed(&wrong_source, &record, host, &wrong_source.binary_sha256),
        Err(CoherenceError::InstalledSource)
    );

    let mut wrong_pkg = deployed_for(&record, host);
    wrong_pkg.package_version = "0.1.58".into();
    assert_eq!(
        verify_installed(&wrong_pkg, &record, host, &wrong_pkg.binary_sha256),
        Err(CoherenceError::InstalledPackageVersion)
    );

    let mut wrong_oci = deployed_for(&record, host);
    wrong_oci.oci_image_digest = oci_of("other-index");
    assert_eq!(
        verify_installed(&wrong_oci, &record, host, &wrong_oci.binary_sha256),
        Err(CoherenceError::InstalledOci)
    );

    // Installed binary on disk disagrees with the recorded digest.
    let deployed = deployed_for(&record, host);
    assert_eq!(
        verify_installed(&deployed, &record, host, &digest_of("tampered-binary")),
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
        oci_image_digest: record.oci_index_digest.clone(),
        record_sha256: record.digest(),
    };
    assert_eq!(
        verify_installed(&deployed, &record, host, &deployed.binary_sha256),
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

#[test]
fn assemble_command_requires_complete_binary_and_deb_sidecars() {
    let root = TempDir::new("assemble-sidecars");
    let (record, artifacts) = write_assemble_fixture(root.path());
    assemble_command(ReleaseAssembleArgs {
        record,
        artifacts,
        out: None,
    })
    .unwrap();
}

#[test]
fn assemble_command_rejects_missing_deb_sidecar() {
    let root = TempDir::new("assemble-missing-deb-sidecar");
    let (record, artifacts) = write_assemble_fixture(root.path());
    std::fs::remove_file(artifacts.join("velnor-runner-0.1.121-arm64.deb.sha256")).unwrap();
    let error = assemble_command(ReleaseAssembleArgs {
        record,
        artifacts,
        out: None,
    })
    .unwrap_err();
    assert!(error.to_string().contains("read deb checksum"));
}

#[test]
fn assemble_command_rejects_oversized_checksum_sidecar() {
    let root = TempDir::new("assemble-oversized-checksum");
    let (record, artifacts) = write_assemble_fixture(root.path());
    std::fs::write(
        artifacts.join("velnor-runner-amd64.bin.sha256"),
        vec![b'0'; MAX_RELEASE_CHECKSUM_BYTES + 1],
    )
    .unwrap();
    let error = assemble_command(ReleaseAssembleArgs {
        record,
        artifacts,
        out: None,
    })
    .unwrap_err();
    assert!(error.to_string().contains("exceeds 4096 bytes"));
}

#[test]
fn verify_record_command_rejects_oversized_checksum_sidecar() {
    let dir = TempDir::new("verify-record-oversized-checksum");
    let record = valid_record();
    let record_path = dir.path().join("record.json");
    let checksum_path = dir.path().join("record.json.sha256");
    std::fs::write(&record_path, record.to_canonical_json()).unwrap();
    std::fs::write(&checksum_path, vec![b'0'; MAX_RELEASE_CHECKSUM_BYTES + 1]).unwrap();

    let error = verify_record_command(ReleaseVerifyRecordArgs {
        record: record_path,
        checksum: Some(checksum_path),
        sha256: None,
        publication: None,
        expected_apt_metadata: None,
        served_apt_metadata: None,
    })
    .unwrap_err();
    assert!(error.to_string().contains("exceeds 4096 bytes"));
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
    let error = assemble_command(ReleaseAssembleArgs {
        record,
        artifacts,
        out: None,
    })
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("artifact checksum file must contain only one checksum"));
}

#[test]
fn assemble_command_rejects_mismatched_deb_sidecar() {
    let root = TempDir::new("assemble-mismatched-deb-sidecar");
    let (record, artifacts) = write_assemble_fixture(root.path());
    std::fs::write(
        artifacts.join("velnor-runner-0.1.121-amd64.deb.sha256"),
        format!("{}\n", digest_of("tampered-deb")),
    )
    .unwrap();
    let error = assemble_command(ReleaseAssembleArgs {
        record,
        artifacts,
        out: None,
    })
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("assembled deb checksum for amd64 disagrees with the artifact"));
}

#[test]
fn assemble_command_rejects_tampered_deb_with_forged_sidecar() {
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
    let error = assemble_command(ReleaseAssembleArgs {
        record,
        artifacts,
        out: None,
    })
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("assembled deb artifact digest for amd64 disagrees with the record"));
}

#[test]
fn assemble_command_rejects_unsafe_version_path_component() {
    let root = TempDir::new("assemble-unsafe-version");
    let mut record = valid_record();
    record.build.debian_version = "/../".into();
    let record_path = root.path().join("record.json");
    let artifacts = root.path().join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    std::fs::write(&record_path, record.to_canonical_json()).unwrap();

    let error = assemble_command(ReleaseAssembleArgs {
        record: record_path,
        artifacts,
        out: None,
    })
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "release version is not a safe artifact path component"
    );
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
fn store_activate_and_rollback_restore_exact_tuple() {
    let dir = TempDir::new("store");
    let store = ReleaseStore::new(dir.path());

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

    store.activate(&v120, &deployed120).unwrap();
    store.activate(&v121, &deployed121).unwrap();

    assert_eq!(store.active_tag().unwrap().as_deref(), Some("v0.1.121"));
    assert_eq!(store.previous_tag().unwrap().as_deref(), Some("v0.1.120"));
    assert_eq!(
        std::fs::read(dir.path().join("active/record.json")).unwrap(),
        v121.to_canonical_json().as_bytes()
    );
    assert!(dir.path().join("active/deployed.json").is_file());

    let restored = store.rollback().unwrap();
    assert_eq!(restored, "v0.1.120");
    assert_eq!(store.active_tag().unwrap().as_deref(), Some("v0.1.120"));
}

#[test]
fn store_record_refuses_to_clobber_divergent_bytes() {
    let dir = TempDir::new("clobber");
    let store = ReleaseStore::new(dir.path());
    let record = valid_record();
    store.store_record(&record).unwrap();

    // Same tag, different content -> must not overwrite.
    let mut tampered = record.clone();
    tampered.oci_index_digest = oci_of("tampered-index");
    tampered.oci_image_ref = format!(
        "ghcr.io/tailrocks/velnor-job-ubuntu@{}",
        tampered.oci_index_digest
    );
    let err = store.store_record(&tampered).unwrap_err();
    assert!(err.to_string().contains("refusing to clobber"));

    // Exact re-store of identical bytes is an idempotent success.
    store.store_record(&record).unwrap();
}

#[test]
fn activate_requires_a_stored_record() {
    let dir = TempDir::new("noactivate");
    let store = ReleaseStore::new(dir.path());
    let record = valid_record();
    let mut deployed = deployed_for(&record, Arch::host().unwrap());
    deployed.record_sha256 = digest_of("wrong-record");
    assert!(store.activate(&record, &deployed).is_err());
}

#[test]
fn rollback_requires_a_previous_tuple() {
    let dir = TempDir::new("norollback");
    let store = ReleaseStore::new(dir.path());
    assert!(store.rollback().is_err());
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

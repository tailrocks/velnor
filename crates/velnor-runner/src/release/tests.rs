//! Plan 010 release-model tests. Every fixture is built from fixed seed bytes so
//! the digests are reproducible and no test depends on a real tagged build, live
//! OCI registry, or host state (the `release-build` feature is off here).

use std::path::{Path, PathBuf};

use super::*;

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
        signer_fingerprint: "261EDAC957DEB801".to_string(),
        previous: Some(PreviousPointer {
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
        packages: vec![PackagesIndex {
            arch: Arch::Amd64,
            sha256: digest_of("packages-amd64"),
        }],
        signer_fingerprint: "261EDAC957DEB801".to_string(),
        previous: Some(PreviousPointer {
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
    self_previous.previous = Some(PreviousPointer {
        tag: record.build.tag.clone(),
        source_record_sha256: record.digest(),
    });
    assert_eq!(
        verify_publication_binds(&self_previous, &record),
        Err(CoherenceError::PublicationPrevious)
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

    store.store_record(&v120).unwrap();
    store.store_record(&v121).unwrap();
    store.activate("v0.1.120").unwrap();
    store.activate("v0.1.121").unwrap();

    assert_eq!(store.active_tag().unwrap().as_deref(), Some("v0.1.121"));
    assert_eq!(store.previous_tag().unwrap().as_deref(), Some("v0.1.120"));

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
    assert!(store.activate("v0.1.121").is_err());
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

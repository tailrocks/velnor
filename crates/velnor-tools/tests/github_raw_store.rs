#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "these tests turn fixture setup failures into explicit test failures"
)]

mod github_acquisition {
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};

    #[derive(Clone, PartialEq, Eq)]
    pub struct RawObject {
        pub raw_id: String,
        pub request_id: String,
        pub object_kind: String,
        pub canonicalization: String,
        pub media_type: String,
        pub bytes: Vec<u8>,
        pub original_bytes: Vec<u8>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct RawObjectRef {
        pub raw_id: String,
        pub request_id: String,
        pub object_kind: String,
        pub canonicalization: String,
        pub sha256: String,
        pub byte_length: u64,
        pub original_sha256: String,
        pub original_byte_length: u64,
        pub bytes_base64: String,
        pub media_type: String,
        pub storage_ref: String,
        pub original_storage_ref: String,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RawStorageError {
        Unavailable,
        Refused,
        Unbound,
    }

    pub trait RawObjectStore {
        fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError>;
        fn verify(&self, reference: &RawObjectRef) -> Result<(), RawStorageError>;
    }

    pub fn sha256_digest(bytes: &[u8]) -> String {
        let hex = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("sha256:{hex}")
    }

    pub fn content_addressed_storage_ref(digest: &str) -> String {
        let digest = digest.strip_prefix("sha256:").unwrap_or(digest);
        format!("sha256://{digest}")
    }
}

#[path = "../src/github_raw_store.rs"]
mod github_raw_store;

use github_acquisition::{RawObject, RawObjectRef, RawObjectStore};
use github_raw_store::RawObjectFileStore;
use std::fmt::Debug;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn fixture(name: &str) -> PathBuf {
    let number = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let base = std::env::current_dir()
        .unwrap_or_else(|error| panic!("find test working directory: {error}"))
        .join(".github-raw-store-fixtures");
    let path = base.join(format!(
        "velnor-github-raw-store-{}-{number}-{name}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap_or_else(|error| panic!("create fixture: {error}"));
    path
}

fn remove_fixture(path: &Path) {
    let _ = fs::remove_dir_all(path);
    #[cfg(unix)]
    {
        let Some(name) = path.file_name() else {
            return;
        };
        let mut marker = b".velnor-raw-anchor-".to_vec();
        for byte in name.as_bytes() {
            marker.extend(format!("{byte:02x}").bytes());
        }
        let marker = path
            .parent()
            .unwrap_or_else(|| panic!("fixture has parent"))
            .join(
                String::from_utf8(marker).unwrap_or_else(|error| panic!("marker UTF-8: {error}")),
            );
        let _ = fs::remove_file(marker);
    }
}

fn must<T, E: Debug>(result: Result<T, E>, context: &str) -> T {
    result.unwrap_or_else(|error| panic!("{context}: {error:?}"))
}

fn capture(raw_id: &str, source: &[u8], safe_bytes: &[u8]) -> RawObject {
    RawObject {
        raw_id: raw_id.to_owned(),
        request_id: format!("request-{raw_id}"),
        object_kind: "rest-response".to_owned(),
        canonicalization: "json-canonical-v1".to_owned(),
        media_type: "application/json".to_owned(),
        bytes: safe_bytes.to_vec(),
        original_bytes: source.to_vec(),
    }
}

fn object_path(root: &Path, reference: &RawObjectRef) -> PathBuf {
    let digest = reference
        .sha256
        .strip_prefix("sha256:")
        .unwrap_or_else(|| panic!("safe digest has sha256 prefix"));
    root.join("sha256").join(digest)
}

fn original_object_path(root: &Path, reference: &RawObjectRef) -> PathBuf {
    let digest = reference
        .original_sha256
        .strip_prefix("sha256:")
        .unwrap_or_else(|| panic!("original digest has sha256 prefix"));
    root.join("original").join(digest)
}

fn sidecar_path(root: &Path, reference: &RawObjectRef) -> PathBuf {
    root.join("refs").join(format!("{}.json", reference.raw_id))
}

#[cfg(unix)]
fn transaction_journal_bytes(reference: &RawObjectRef) -> Vec<u8> {
    must(
        serde_json::to_vec(&serde_json::json!({
            "raw_id": reference.raw_id,
            "request_id": reference.request_id,
            "object_kind": reference.object_kind,
            "canonicalization": reference.canonicalization,
            "sha256": reference.sha256,
            "byte_length": reference.byte_length,
            "original_sha256": reference.original_sha256,
            "original_byte_length": reference.original_byte_length,
            "media_type": reference.media_type,
            "storage_ref": reference.storage_ref,
            "original_storage_ref": reference.original_storage_ref,
        })),
        "serialize transaction journal",
    )
}

#[test]
fn stores_safe_bytes_with_distinct_original_provenance() {
    let root = fixture("provenance");
    let source = br#"{"authorization":"github_pat_original"}"#;
    let safe = br#"{"authorization":"[REDACTED]"}"#;
    let mut store = must(RawObjectFileStore::new(&root), "open store");
    assert_eq!(store.root(), root.as_path());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let original_directory = root.join("original");
        for directory in [
            root.as_path(),
            root.join("sha256").as_path(),
            original_directory.as_path(),
            root.join("refs").as_path(),
        ] {
            assert_eq!(
                must(fs::metadata(directory), "read raw-store directory mode")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    let reference = must(
        store.store(capture("provenance-1", source, safe)),
        "store capture",
    );
    assert_eq!(
        reference.original_sha256,
        github_acquisition::sha256_digest(source)
    );
    assert_eq!(reference.original_byte_length, source.len() as u64);
    assert_eq!(reference.sha256, github_acquisition::sha256_digest(safe));
    assert_eq!(reference.byte_length, safe.len() as u64);
    assert_eq!(
        reference.storage_ref,
        format!(
            "sha256://{}",
            reference
                .sha256
                .strip_prefix("sha256:")
                .unwrap_or_else(|| panic!("safe digest has sha256 prefix"))
        )
    );
    assert_eq!(
        reference.original_storage_ref,
        format!(
            "sha256://{}",
            reference
                .original_sha256
                .strip_prefix("sha256:")
                .unwrap_or_else(|| panic!("original digest has sha256 prefix"))
        )
    );
    assert_ne!(reference.original_sha256, reference.sha256);
    assert_ne!(reference.original_byte_length, reference.byte_length);
    assert_eq!(
        must(
            fs::read(original_object_path(&root, &reference)),
            "read original proof object",
        ),
        source
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            must(
                fs::metadata(original_object_path(&root, &reference)),
                "read original proof mode",
            )
            .permissions()
            .mode()
                & 0o777,
            0o400
        );
    }
    let sidecar = must(
        fs::read(sidecar_path(&root, &reference)),
        "read provenance sidecar",
    );
    assert!(!sidecar
        .windows(b"github_pat_original".len())
        .any(|window| { window == b"github_pat_original" }));
    must(store.verify(&reference), "verify capture");
    let reopened = must(RawObjectFileStore::new(&root), "reopen anchored raw store");
    must(reopened.verify(&reference), "verify reopened capture");

    let original_path = original_object_path(&root, &reference);
    must(
        fs::remove_file(&original_path),
        "remove original proof object",
    );
    must(
        fs::write(&original_path, b"forged-original"),
        "write forged original",
    );
    assert!(store.verify(&reference).is_err());

    let mut forged = reference.clone();
    forged.original_sha256 = github_acquisition::sha256_digest(b"unrelated-source");
    assert!(store.verify(&forged).is_err());
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn recovers_complete_transaction_and_sweeps_incomplete_bundle() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("transaction-recovery");
    let mut store = must(RawObjectFileStore::new(&root), "open transaction store");
    let reference = must(
        store.store(capture("recover-complete", b"source", b"safe")),
        "store recoverable bundle",
    );
    let sidecar = sidecar_path(&root, &reference);
    let transaction = root.join("refs").join("recover-complete.txn");
    must(
        fs::remove_file(&sidecar),
        "remove complete sidecar before journaling",
    );
    must(
        fs::write(&transaction, transaction_journal_bytes(&reference)),
        "write compact durable transaction journal",
    );
    must(
        fs::set_permissions(&transaction, fs::Permissions::from_mode(0o400)),
        "restrict complete transaction journal",
    );
    drop(store);

    let reopened = must(
        RawObjectFileStore::new(&root),
        "recover complete transaction journal",
    );
    assert!(sidecar.exists());
    assert!(!transaction.exists());
    assert_eq!(
        must(
            fs::read_dir(root.join(".velnor-raw-quarantine")),
            "read successful recovery retention",
        )
        .flatten()
        .count(),
        0,
        "completed transaction recovery must not retain its journal",
    );
    must(reopened.verify(&reference), "verify recovered bundle");
    drop(reopened);

    let mut incomplete = must(
        RawObjectFileStore::new(&root),
        "reopen for incomplete transaction",
    );
    let incomplete_reference = must(
        incomplete.store(capture("recover-incomplete", b"source-2", b"safe-2")),
        "store incomplete bundle",
    );
    let incomplete_sidecar = sidecar_path(&root, &incomplete_reference);
    let incomplete_transaction = root.join("refs").join("recover-incomplete.txn");
    must(
        fs::remove_file(&incomplete_sidecar),
        "remove incomplete sidecar before journaling",
    );
    must(
        fs::write(
            &incomplete_transaction,
            transaction_journal_bytes(&incomplete_reference),
        ),
        "write compact incomplete transaction journal",
    );
    must(
        fs::set_permissions(&incomplete_transaction, fs::Permissions::from_mode(0o400)),
        "restrict incomplete transaction journal",
    );
    must(
        fs::remove_file(object_path(&root, &incomplete_reference)),
        "remove incomplete safe object",
    );
    drop(incomplete);

    let recovered = must(
        RawObjectFileStore::new(&root),
        "discard incomplete transaction journal",
    );
    assert!(!incomplete_transaction.exists());
    assert!(!incomplete_sidecar.exists());
    assert!(!original_object_path(&root, &incomplete_reference).exists());
    assert!(!object_path(&root, &incomplete_reference).exists());
    must(
        recovered.verify(&reference),
        "verify surviving complete bundle",
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn recovers_max_size_valid_compact_transaction_without_payload_duplicate() {
    use std::os::unix::fs::PermissionsExt;

    const MAX_OBJECT_BYTES: usize = 64 * 1024 * 1024;
    let root = fixture("transaction-recovery-max");
    let mut store = must(RawObjectFileStore::new(&root), "open max transaction store");
    let reference = must(
        store.store(capture(
            "recover-max",
            b"small-original",
            &vec![b's'; MAX_OBJECT_BYTES],
        )),
        "store max recoverable bundle",
    );
    let sidecar = sidecar_path(&root, &reference);
    let transaction = root.join("refs").join("recover-max.txn");
    must(
        fs::remove_file(&sidecar),
        "remove max sidecar before journaling",
    );
    must(
        fs::write(&transaction, transaction_journal_bytes(&reference)),
        "write max compact transaction journal",
    );
    must(
        fs::set_permissions(&transaction, fs::Permissions::from_mode(0o400)),
        "restrict max transaction journal",
    );
    drop(store);

    let reopened = must(
        RawObjectFileStore::new(&root),
        "recover max compact transaction journal",
    );
    assert!(sidecar.exists());
    assert!(!transaction.exists());
    must(reopened.verify(&reference), "verify max recovered bundle");
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn successful_transaction_cleanup_releases_retention_capacity() {
    use std::os::unix::fs::PermissionsExt;

    const SUCCESSFUL_CAPTURES: usize = 130;
    const RETAINED_FAILURES: usize = 63;
    let root = fixture("successful-transaction-retention");
    let store = must(
        RawObjectFileStore::new(&root),
        "open successful-transaction retention store",
    );
    drop(store);

    // Keep a capture-sized set of real failure/quarantine records.
    // Successful transaction journals must not displace this evidence while
    // the bounded store handles a capture-sized run.
    for index in 0..RETAINED_FAILURES {
        let failure_quarantine = root
            .join("sha256")
            .join(format!(".velnor-raw-quarantine-900-{index}-0"));
        must(
            fs::create_dir(&failure_quarantine),
            "create failure quarantine",
        );
        must(
            fs::set_permissions(&failure_quarantine, fs::Permissions::from_mode(0o700)),
            "restrict failure quarantine",
        );
        let failure_entry = failure_quarantine.join("entry");
        let evidence = format!("failure-evidence-{index}");
        must(
            fs::write(&failure_entry, evidence.as_bytes()),
            "write failure evidence",
        );
        must(
            fs::set_permissions(&failure_entry, fs::Permissions::from_mode(0o400)),
            "restrict failure evidence",
        );
    }
    let reopened = must(
        RawObjectFileStore::new(&root),
        "materialize failure evidence",
    );
    drop(reopened);
    assert_eq!(
        must(
            fs::read_dir(root.join(".velnor-raw-quarantine")),
            "read materialized failure evidence",
        )
        .flatten()
        .count(),
        RETAINED_FAILURES,
    );

    let mut store = must(
        RawObjectFileStore::new(&root),
        "reopen successful-transaction retention store",
    );
    for index in 0..SUCCESSFUL_CAPTURES {
        let raw_id = format!("successful-{index}");
        store
            .store(capture(&raw_id, b"capture-source", b"capture-safe"))
            .unwrap_or_else(|error| panic!("store successful capture {index}: {error:?}"));
    }
    drop(store);

    let retention = root.join(".velnor-raw-quarantine");
    assert_eq!(
        must(fs::read_dir(&retention), "read retained evidence")
            .flatten()
            .count(),
        RETAINED_FAILURES,
        "successful transaction journals must not become retained records",
    );
    assert_eq!(
        must(fs::read_dir(root.join("refs")), "read refs after captures")
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "txn"))
            .count(),
        0,
        "successful transaction journals must be removed",
    );
    assert_eq!(
        must(
            fs::read_dir(root.join("refs")),
            "read refs quarantine state"
        )
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".velnor-raw-quarantine-")
        })
        .count(),
        0,
        "successful cleanup must not leave source quarantine directories",
    );

    let reopened = must(
        RawObjectFileStore::new(&root),
        "reopen after successful capture run",
    );
    drop(reopened);
    assert_eq!(
        must(fs::read_dir(&retention), "reread retained evidence")
            .flatten()
            .count(),
        RETAINED_FAILURES,
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn successful_transaction_cleanup_retains_replacement_evidence() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("successful-transaction-replacement");
    let mut store = must(
        RawObjectFileStore::new(&root),
        "open successful-transaction replacement store",
    );
    let refs = root.join("refs");
    let refs_for_hook = refs.clone();
    github_raw_store::set_test_successful_cleanup_hook(Box::new(move || {
        let quarantine = fs::read_dir(&refs_for_hook)
            .unwrap_or_else(|error| panic!("read cleanup quarantine: {error}"))
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with(".velnor-raw-quarantine-")
                })
            })
            .unwrap_or_else(|| panic!("successful cleanup quarantine missing"));
        let entry = quarantine.join("entry");
        fs::remove_file(&entry)
            .unwrap_or_else(|error| panic!("remove cleanup journal for replacement: {error}"));
        fs::write(&entry, b"attacker-replacement")
            .unwrap_or_else(|error| panic!("write cleanup replacement: {error}"));
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o400))
            .unwrap_or_else(|error| panic!("restrict cleanup replacement: {error}"));
    }));

    assert!(
        store
            .store(capture("cleanup-race", b"source", b"safe"))
            .is_err(),
        "replacement of a successful journal must fail closed",
    );
    let quarantine = must(fs::read_dir(&refs), "read retained cleanup quarantine")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with(".velnor-raw-quarantine-")
            })
        })
        .unwrap_or_else(|| panic!("cleanup replacement quarantine was removed"));
    assert_eq!(
        must(
            fs::read(quarantine.join("entry")),
            "read retained replacement"
        ),
        b"attacker-replacement",
    );
    assert_eq!(
        must(
            fs::read_dir(root.join(".velnor-raw-quarantine")),
            "read retained cleanup record",
        )
        .flatten()
        .count(),
        1,
    );
    drop(store);
    let reopened = must(
        RawObjectFileStore::new(&root),
        "reopen retained cleanup replacement",
    );
    drop(reopened);
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn recovers_private_temporary_files_but_leaves_replaced_public_entry() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("temporary-recovery");
    let store = must(RawObjectFileStore::new(&root), "open temporary store");
    drop(store);
    for (index, directory) in ["sha256", "original", "refs"].into_iter().enumerate() {
        let temporary = root
            .join(directory)
            .join(format!(".velnor-raw-7-{index}-0.tmp"));
        must(
            fs::write(&temporary, b"stale-private-temp"),
            "write stale temp",
        );
        must(
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o400)),
            "restrict stale temp",
        );
    }
    let replaced = root.join("refs").join("operator-owned-entry.tmp");
    must(
        fs::write(&replaced, b"operator-owned-entry"),
        "write replaced temp",
    );

    let reopened = must(RawObjectFileStore::new(&root), "reconcile temporary files");
    drop(reopened);
    for directory in ["sha256", "original"] {
        let entries = must(
            fs::read_dir(root.join(directory)),
            "read reconciled object directory",
        )
        .flatten()
        .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert!(entries[0]
            .file_name()
            .to_string_lossy()
            .starts_with(".velnor-raw-quarantine-"));
    }
    assert_eq!(
        must(fs::read(&replaced), "read replaced temp"),
        b"operator-owned-entry"
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn reconciles_crash_left_quarantine_into_store_retention() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("crash-left-quarantine");
    let store = must(
        RawObjectFileStore::new(&root),
        "open crash-quarantine store",
    );
    drop(store);

    let source_quarantine = root.join("sha256").join(".velnor-raw-quarantine-1-0-0");
    must(
        fs::create_dir(&source_quarantine),
        "create crash-left quarantine",
    );
    must(
        fs::set_permissions(&source_quarantine, fs::Permissions::from_mode(0o700)),
        "restrict crash-left quarantine",
    );
    let entry = source_quarantine.join("entry");
    must(
        fs::write(&entry, b"crash-left-bytes"),
        "write crash-left entry",
    );
    must(
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o400)),
        "restrict crash-left entry",
    );

    let reopened = must(
        RawObjectFileStore::new(&root),
        "reconcile crash-left quarantine",
    );
    drop(reopened);
    assert!(source_quarantine.is_dir());
    assert!(source_quarantine.join("entry").is_file());
    assert!(!source_quarantine.join("manifest.json").exists());
    let retention = root.join(".velnor-raw-quarantine");
    let retained = must(fs::read_dir(&retention), "read retained quarantines")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".velnor-raw-retained-"))
        })
        .unwrap_or_else(|| panic!("crash-left quarantine was not retained"));
    let retained_manifest = String::from_utf8(must(
        fs::read(retained.join("manifest.json")),
        "read retained crash manifest",
    ))
    .unwrap_or_else(|error| panic!("retained crash manifest UTF-8: {error}"));
    assert!(retained_manifest.contains("\"source_name\":\".velnor-raw-quarantine-1-0-0\""));
    assert!(retained_manifest.contains("\"byte_length\":16"));
    assert!(retained_manifest.contains(&github_acquisition::sha256_digest(b"crash-left-bytes")));
    assert!(!retained.join("entry").exists());

    let reopened_again = must(
        RawObjectFileStore::new(&root),
        "reopen retained crash quarantine",
    );
    drop(reopened_again);
    assert_eq!(
        must(fs::read_dir(&retention), "read retained quarantines again")
            .flatten()
            .count(),
        1
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn retention_replay_rejects_replaced_source_entry_identity() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("retention-source-replay");
    let store = must(RawObjectFileStore::new(&root), "open source-replay store");
    drop(store);
    let source_quarantine = root.join("sha256").join(".velnor-raw-quarantine-1-0-0");
    must(
        fs::create_dir(&source_quarantine),
        "create source-replay quarantine",
    );
    must(
        fs::set_permissions(&source_quarantine, fs::Permissions::from_mode(0o700)),
        "restrict source-replay quarantine",
    );
    let entry = source_quarantine.join("entry");
    must(
        fs::write(&entry, b"authenticated-source"),
        "write source-replay entry",
    );
    must(
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o400)),
        "restrict source-replay entry",
    );
    let reopened = must(
        RawObjectFileStore::new(&root),
        "materialize source-replay record",
    );
    drop(reopened);

    must(fs::remove_file(&entry), "replace source-replay entry");
    must(
        fs::write(&entry, b"attacker-replacement"),
        "write replacement entry",
    );
    must(
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o400)),
        "restrict replacement entry",
    );
    assert!(RawObjectFileStore::new(&root).is_err());
    assert_eq!(
        must(fs::read(&entry), "read replacement source entry"),
        b"attacker-replacement"
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn reconciliation_fails_closed_on_fifo_symlink_and_unknown_entries() {
    use std::os::unix::fs::{symlink, FileTypeExt, PermissionsExt};

    let root = fixture("hostile-temporary-entries");
    let store = must(RawObjectFileStore::new(&root), "open hostile-entry store");
    drop(store);

    let object_directory = root.join("sha256");
    let fifo = object_directory.join(".velnor-raw-hostile-fifo.tmp");
    let fifo_name = must(
        std::ffi::CString::new(fifo.as_os_str().as_bytes()),
        "encode hostile FIFO path",
    );
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);

    let outside = root.join("hostile-outside");
    must(
        fs::write(&outside, b"outside-bytes"),
        "write hostile target",
    );
    let symlink_path = object_directory.join(".velnor-raw-hostile-link.tmp");
    must(
        symlink(&outside, &symlink_path),
        "write hostile temporary symlink",
    );
    let quarantine_symlink = object_directory.join(".velnor-raw-quarantine-hostile-link");
    must(
        symlink(&outside, &quarantine_symlink),
        "write hostile quarantine symlink",
    );

    let unknown_directory = object_directory.join(".velnor-raw-hostile-directory.tmp");
    must(
        fs::create_dir(&unknown_directory),
        "write hostile temporary directory",
    );
    let public_file = object_directory.join(".velnor-raw-hostile-public.tmp");
    must(
        fs::write(&public_file, b"operator-bytes"),
        "write operator temporary file",
    );
    must(
        fs::set_permissions(&public_file, fs::Permissions::from_mode(0o644)),
        "make operator temporary file public",
    );

    assert!(RawObjectFileStore::new(&root).is_err());
    assert!(fs::symlink_metadata(&fifo)
        .unwrap_or_else(|error| panic!("stat hostile FIFO: {error}"))
        .file_type()
        .is_fifo());
    assert_eq!(
        must(fs::read_link(&symlink_path), "read hostile symlink"),
        outside
    );
    assert_eq!(
        must(
            fs::read_link(&quarantine_symlink),
            "read hostile quarantine symlink",
        ),
        outside
    );
    assert!(unknown_directory.is_dir());
    assert_eq!(
        must(fs::read(&public_file), "read operator temporary file"),
        b"operator-bytes"
    );
    remove_fixture(&root);
}

#[test]
fn rejects_path_like_raw_id_before_publishing_an_object() {
    let root = fixture("invalid-raw-id");
    let mut store = must(RawObjectFileStore::new(&root), "open invalid-id store");
    assert!(store
        .store(capture("../escape", b"source", b"safe"))
        .is_err());
    let entries = must(fs::read_dir(root.join("sha256")), "read object directory");
    assert_eq!(entries.count(), 0);
    remove_fixture(&root);
}

#[test]
fn valid_raw_id_collision_does_not_publish_an_orphan_object() {
    let root = fixture("raw-id-collision");
    let mut store = must(RawObjectFileStore::new(&root), "open collision store");
    let first = must(
        store.store(capture("same-raw-id", b"source-one", b"safe-one")),
        "store first collision object",
    );
    assert_eq!(
        must(
            fs::read_dir(root.join("sha256")),
            "read first object directory"
        )
        .count(),
        1
    );
    assert_eq!(
        must(
            fs::read_dir(root.join("original")),
            "read first original directory",
        )
        .count(),
        1
    );
    assert!(store
        .store(capture("same-raw-id", b"source-two", b"safe-two"))
        .is_err());
    assert_eq!(
        must(
            fs::read_dir(root.join("sha256")),
            "read collision object directory"
        )
        .count(),
        1
    );
    assert_eq!(
        must(
            fs::read_dir(root.join("original")),
            "read collision original directory",
        )
        .count(),
        1
    );
    must(store.verify(&first), "verify original collision object");
    remove_fixture(&root);
}

#[test]
fn accepts_exact_max_payload_but_rejects_max_plus_one_without_publish() {
    const MAX_OBJECT_BYTES: usize = 64 * 1024 * 1024;
    let root = fixture("max-payload");
    let mut store = must(RawObjectFileStore::new(&root), "open max-payload store");
    let exact = vec![b'x'; MAX_OBJECT_BYTES];
    let reference = must(
        store.store(capture("max-payload", b"original", &exact)),
        "store exact max payload",
    );
    assert_eq!(reference.byte_length, MAX_OBJECT_BYTES as u64);

    let too_large = vec![b'y'; MAX_OBJECT_BYTES + 1];
    assert!(store
        .store(capture("max-plus-one", b"original", &too_large))
        .is_err());
    assert_eq!(
        must(
            fs::read_dir(root.join("sha256")),
            "read max object directory"
        )
        .count(),
        1
    );
    assert_eq!(
        must(
            fs::read_dir(root.join("original")),
            "read max original directory",
        )
        .count(),
        1
    );
    remove_fixture(&root);
}

#[test]
fn rejects_oversized_sidecar_fields_before_serializing_them() {
    let root = fixture("sidecar-bound");
    let mut store = must(RawObjectFileStore::new(&root), "open sidecar-bound store");
    let reference = must(
        store.store(capture("sidecar-bound", b"original", b"safe")),
        "seed sidecar-bound object",
    );
    let mut oversized = reference.clone();
    oversized.request_id = "r".repeat(24 * 1024 * 1024);
    let started = Instant::now();
    assert!(store.verify(&oversized).is_err());
    assert!(started.elapsed().as_secs() < 2);
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn refuses_root_ancestor_and_child_symlinks() {
    use std::os::unix::fs::symlink;

    let base = fixture("symlinks");
    let outside = base.join("outside");
    must(fs::create_dir_all(&outside), "create outside");

    let root_link = base.join("root-link");
    must(symlink(&outside, &root_link), "link root");
    assert!(RawObjectFileStore::new(&root_link).is_err());
    assert!(!outside.join("sha256").exists());

    let ancestor_target = base.join("ancestor-target");
    must(
        fs::create_dir_all(&ancestor_target),
        "create ancestor target",
    );
    let ancestor_link = base.join("ancestor-link");
    must(symlink(&ancestor_target, &ancestor_link), "link ancestor");
    assert!(RawObjectFileStore::new(ancestor_link.join("nested-root")).is_err());
    assert!(!ancestor_target.join("nested-root").exists());

    let explicit_root = base.join("explicit-root");
    must(fs::create_dir_all(&explicit_root), "create explicit root");
    let child_target = base.join("child-target");
    must(fs::create_dir_all(&child_target), "create child target");
    must(
        symlink(&child_target, explicit_root.join("sha256")),
        "link child",
    );
    assert!(RawObjectFileStore::new(&explicit_root).is_err());
    assert!(!child_target.join("refs").exists());

    let parent_component_root = base.join("new-component").join("../target");
    assert!(RawObjectFileStore::new(&parent_component_root).is_err());
    assert!(!base.join("new-component").exists());
    assert!(!base.join("target").exists());
    remove_fixture(&base);
}

#[cfg(unix)]
#[test]
fn rejects_refs_namespace_replacement_and_new_store_anchor_mismatch() {
    let root = fixture("namespace-replacement");
    let mut store = must(RawObjectFileStore::new(&root), "open namespace store");
    let reference = must(
        store.store(capture("namespace-replacement", b"source", b"safe")),
        "seed namespace store",
    );
    let refs = root.join("refs");
    let moved = root.join("refs-moved");
    must(fs::rename(&refs, &moved), "move refs namespace");
    must(fs::create_dir(&refs), "replace refs namespace");
    assert!(store
        .store(capture("namespace-replacement-2", b"source-2", b"safe-2"))
        .is_err());
    assert!(RawObjectFileStore::new(&root).is_err());
    must(fs::remove_dir(&refs), "remove replacement refs");
    must(fs::rename(&moved, &refs), "restore refs namespace");
    must(store.verify(&reference), "verify original namespace");
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn refuses_hardlinked_objects_and_sidecars() {
    let root = fixture("hardlinks");
    let safe = br#"{"ok":true}"#;
    let mut store = must(RawObjectFileStore::new(&root), "open object store");
    let reference = must(
        store.store(capture("hardlink-object", b"source-object", safe)),
        "store object",
    );
    let object = object_path(&root, &reference);
    let outside_object = root.join("outside-object");
    must(
        fs::write(&outside_object, b"attacker-bytes"),
        "write outside object",
    );
    must(fs::remove_file(&object), "remove object before hardlink");
    must(fs::hard_link(&outside_object, &object), "hardlink object");
    assert!(store.verify(&reference).is_err());
    let started = Instant::now();
    assert!(store
        .store(capture("hardlink-object", b"source-object", safe))
        .is_err());
    assert!(started.elapsed().as_secs() < 5);
    must(fs::remove_file(&object), "remove object hardlink");
    must(fs::remove_file(&outside_object), "remove outside object");

    let second_root = fixture("hardlinks-sidecar");
    let mut second_store = must(RawObjectFileStore::new(&second_root), "open sidecar store");
    let second_reference = must(
        second_store.store(capture("hardlink-sidecar", b"source-sidecar", safe)),
        "store sidecar",
    );
    let sidecar = sidecar_path(&second_root, &second_reference);
    let outside_sidecar = second_root.join("outside-sidecar");
    must(fs::copy(&sidecar, &outside_sidecar), "copy sidecar");
    must(fs::remove_file(&sidecar), "remove sidecar before hardlink");
    must(
        fs::hard_link(&outside_sidecar, &sidecar),
        "hardlink sidecar",
    );
    assert!(second_store.verify(&second_reference).is_err());
    remove_fixture(&root);
    remove_fixture(&second_root);
}

#[cfg(unix)]
#[test]
fn refuses_capture_when_original_proof_path_is_hardlinked() {
    let root = fixture("original-proof-hardlink");
    let source = b"sensitive-source-bytes";
    let safe = b"redacted-safe-bytes";
    let mut store = must(RawObjectFileStore::new(&root), "open original-proof store");
    let original_digest = github_acquisition::sha256_digest(source);
    let safe_digest = github_acquisition::sha256_digest(safe);
    let original_path = root.join("original").join(
        original_digest
            .strip_prefix("sha256:")
            .unwrap_or_else(|| panic!("original digest has prefix")),
    );
    let safe_path = root.join("sha256").join(
        safe_digest
            .strip_prefix("sha256:")
            .unwrap_or_else(|| panic!("safe digest has prefix")),
    );
    let outside = root.join("outside-original-proof");
    must(
        fs::write(&outside, b"attacker-original"),
        "write outside proof",
    );
    must(
        fs::hard_link(&outside, &original_path),
        "hardlink original proof",
    );

    assert!(store
        .store(capture("original-proof-hardlink", source, safe))
        .is_err());
    assert!(!safe_path.exists());
    assert_eq!(
        must(fs::read(&outside), "read outside proof"),
        b"attacker-original"
    );
    remove_fixture(&root);
}

#[test]
fn concurrent_publish_is_no_clobber() {
    let root = fixture("concurrent");
    let workers = 16;
    let barrier = Arc::new(Barrier::new(workers));
    let safe = br#"{"same":"payload"}"#;
    let mut joins = Vec::with_capacity(workers);
    for _ in 0..workers {
        let root = root.clone();
        let barrier = Arc::clone(&barrier);
        joins.push(thread::spawn(move || {
            barrier.wait();
            let mut store = match RawObjectFileStore::new(&root) {
                Ok(store) => store,
                Err(error) => return Err(format!("open store: {error}")),
            };
            store
                .store(capture("same-object", b"same-source", safe))
                .map_err(|error| format!("store object: {error:?}"))
        }));
    }

    let mut references = Vec::with_capacity(workers);
    for join in joins {
        let result = join
            .join()
            .unwrap_or_else(|_| panic!("concurrent publisher panicked"));
        references.push(result.unwrap_or_else(|error| panic!("concurrent publish: {error}")));
    }
    for reference in references.iter().skip(1) {
        assert_eq!(reference, &references[0]);
    }
    let verifier = must(RawObjectFileStore::new(&root), "reopen concurrent store");
    must(verifier.verify(&references[0]), "verify concurrent object");
    remove_fixture(&root);
}

#[test]
fn concurrent_different_payloads_same_raw_id_publish_one_bundle() {
    let root = fixture("concurrent-collision");
    let workers = 16;
    let barrier = Arc::new(Barrier::new(workers));
    let mut joins = Vec::with_capacity(workers);
    for worker in 0..workers {
        let root = root.clone();
        let barrier = Arc::clone(&barrier);
        joins.push(thread::spawn(move || {
            barrier.wait();
            let mut store = RawObjectFileStore::new(&root)
                .unwrap_or_else(|error| panic!("open concurrent collision store: {error}"));
            let source = format!("source-{worker}");
            let safe = format!("safe-{worker}");
            store.store(capture("same-raw-id", source.as_bytes(), safe.as_bytes()))
        }));
    }

    let mut successes = Vec::new();
    for join in joins {
        let result = join
            .join()
            .unwrap_or_else(|_| panic!("concurrent collision publisher panicked"));
        if let Ok(reference) = result {
            successes.push(reference);
        }
    }
    assert_eq!(successes.len(), 1);
    assert_eq!(
        must(
            fs::read_dir(root.join("sha256")),
            "read concurrent collision objects",
        )
        .count(),
        1
    );
    assert_eq!(
        must(
            fs::read_dir(root.join("original")),
            "read concurrent collision originals",
        )
        .count(),
        1
    );
    assert_eq!(
        must(
            fs::read_dir(root.join("refs")),
            "read concurrent collision sidecars",
        )
        .flatten()
        .filter(|entry| entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json"))
        .count(),
        1
    );
    let verifier = must(
        RawObjectFileStore::new(&root),
        "reopen concurrent collision store",
    );
    must(
        verifier.verify(&successes[0]),
        "verify winning collision bundle",
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn verification_survives_symlink_and_hardlink_replacement_race() {
    use std::os::unix::fs::symlink;

    let root = fixture("replacement-race");
    let safe = br#"{"safe":true}"#;
    let mut store = must(RawObjectFileStore::new(&root), "open race store");
    let reference = must(
        store.store(capture("race-object", b"source", safe)),
        "store race object",
    );
    let object = object_path(&root, &reference);
    let moved = root.join("sha256").join("moved-object");
    let outside = root.join("outside-race");
    must(
        fs::write(&outside, b"attacker-bytes"),
        "write race attacker file",
    );

    let stop = Arc::new(AtomicBool::new(false));
    let replacements = Arc::new(AtomicUsize::new(0));
    let attacker_stop = Arc::clone(&stop);
    let attacker_replacements = Arc::clone(&replacements);
    let attacker_object = object.clone();
    let attacker_moved = moved.clone();
    let attacker_outside = outside.clone();
    let attacker = thread::spawn(move || {
        while !attacker_stop.load(Ordering::Relaxed) {
            let _ = fs::remove_file(&attacker_moved);
            if fs::rename(&attacker_object, &attacker_moved).is_ok() {
                if symlink(&attacker_outside, &attacker_object).is_ok() {
                    let _ = fs::remove_file(&attacker_object);
                    attacker_replacements.fetch_add(1, Ordering::Relaxed);
                }
                let _ = fs::rename(&attacker_moved, &attacker_object);
            }
            if fs::rename(&attacker_object, &attacker_moved).is_ok() {
                if fs::hard_link(&attacker_outside, &attacker_object).is_ok() {
                    let _ = fs::remove_file(&attacker_object);
                    attacker_replacements.fetch_add(1, Ordering::Relaxed);
                }
                let _ = fs::rename(&attacker_moved, &attacker_object);
            }
        }
        let _ = fs::remove_file(&attacker_moved);
    });

    let mut rejected = 0_usize;
    for _ in 0..2_000 {
        if store.verify(&reference).is_err() {
            rejected += 1;
        }
        thread::yield_now();
    }
    stop.store(true, Ordering::Relaxed);
    attacker
        .join()
        .unwrap_or_else(|_| panic!("replacement attacker panicked"));
    assert!(replacements.load(Ordering::Relaxed) > 0);
    assert!(rejected > 0);
    assert_eq!(
        must(fs::read(&outside), "read attacker file"),
        b"attacker-bytes"
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn cleanup_preserves_preexisting_replacement_as_retained_evidence() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("cleanup-replacement");
    let store = must(RawObjectFileStore::new(&root), "open cleanup store");
    drop(store);
    let temporary = root
        .join("sha256")
        .join(format!(".velnor-raw-{}-0-0.tmp", std::process::id()));
    must(
        fs::write(&temporary, b"attacker-temporary-file"),
        "write preexisting replacement",
    );
    must(
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o400)),
        "restrict preexisting replacement",
    );

    assert!(RawObjectFileStore::new(&root).is_ok());
    assert!(!temporary.exists());
    let entry = must(fs::read_dir(root.join("sha256")), "read retained sources")
        .flatten()
        .map(|entry| entry.path().join("entry"))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("replacement was not retained"));
    assert_eq!(
        must(fs::read(entry), "read retained replacement"),
        b"attacker-temporary-file"
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn rejects_fifo_sidecar_without_blocking() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let root = fixture("fifo-sidecar");
    let mut store = must(RawObjectFileStore::new(&root), "open FIFO store");
    let reference = must(
        store.store(capture("fifo-sidecar", b"source", b"safe")),
        "seed FIFO object",
    );
    let sidecar = sidecar_path(&root, &reference);
    must(fs::remove_file(&sidecar), "remove sidecar for FIFO");
    let sidecar_c = must(
        CString::new(sidecar.as_os_str().as_bytes()),
        "encode FIFO path",
    );
    let result = unsafe { libc::mkfifo(sidecar_c.as_ptr(), 0o600) };
    assert_eq!(result, 0);
    let started = Instant::now();
    assert!(store.verify(&reference).is_err());
    assert!(started.elapsed().as_secs() < 2);
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn retention_unknown_child_fails_closed_without_reclaim() {
    let root = fixture("retention-unknown-child");
    let store = must(RawObjectFileStore::new(&root), "open retention store");
    drop(store);
    let unknown = root.join(".velnor-raw-quarantine").join("operator-entry");
    must(
        fs::write(&unknown, b"operator-owned"),
        "write unknown retention child",
    );

    assert!(RawObjectFileStore::new(&root).is_err());
    assert_eq!(
        must(fs::read(&unknown), "read unknown retention child"),
        b"operator-owned"
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn retention_admission_is_bounded_and_reopens_fail_closed() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("retention-quota");
    let store = must(RawObjectFileStore::new(&root), "open retention quota store");
    drop(store);
    for index in 0..128_u32 {
        let quarantine = root
            .join("sha256")
            .join(format!(".velnor-raw-quarantine-{index}-0-0"));
        must(fs::create_dir(&quarantine), "create quota quarantine");
        must(
            fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)),
            "restrict quota quarantine",
        );
        let entry = quarantine.join("entry");
        must(fs::write(&entry, index.to_le_bytes()), "write quota entry");
        must(
            fs::set_permissions(&entry, fs::Permissions::from_mode(0o400)),
            "restrict quota entry",
        );
    }

    let reopened = must(
        RawObjectFileStore::new(&root),
        "materialize bounded retention records",
    );
    drop(reopened);
    assert_eq!(
        must(
            fs::read_dir(root.join(".velnor-raw-quarantine")),
            "read bounded retention records",
        )
        .flatten()
        .count(),
        128
    );

    let quarantine = root.join("sha256").join(".velnor-raw-quarantine-128-0-0");
    must(fs::create_dir(&quarantine), "create over-quota quarantine");
    must(
        fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)),
        "restrict over-quota quarantine",
    );
    let entry = quarantine.join("entry");
    must(
        fs::write(&entry, 128_u32.to_le_bytes()),
        "write over-quota entry",
    );
    must(
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o400)),
        "restrict over-quota entry",
    );

    assert!(RawObjectFileStore::new(&root).is_err());
    assert_eq!(
        must(
            fs::read_dir(root.join("sha256")),
            "read pending quota quarantines"
        )
        .flatten()
        .count(),
        129
    );
    assert!(RawObjectFileStore::new(&root).is_err());
    assert_eq!(
        must(
            fs::read_dir(root.join(".velnor-raw-quarantine")),
            "reread bounded retention records",
        )
        .flatten()
        .count(),
        128
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn quota_rejects_max_transaction_before_quarantine_move() {
    use std::os::unix::fs::PermissionsExt;

    const MAX_SIDECAR_BYTES: usize = 2 * 64 * 1024 * 1024 + 4096;

    let root = fixture("quota-max-transaction");
    let store = must(RawObjectFileStore::new(&root), "open max-transaction store");
    drop(store);
    let transaction = root.join("refs").join("raw-max-transaction.txn");
    must(
        fs::write(&transaction, vec![b't'; MAX_SIDECAR_BYTES]),
        "write max transaction",
    );
    must(
        fs::set_permissions(&transaction, fs::Permissions::from_mode(0o400)),
        "restrict max transaction",
    );

    assert!(RawObjectFileStore::new(&root).is_err());
    assert_eq!(
        must(fs::metadata(&transaction), "stat rejected transaction").len(),
        MAX_SIDECAR_BYTES as u64
    );
    assert_eq!(
        must(
            fs::read_dir(root.join("refs")),
            "read refs after rejected transaction"
        )
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".velnor-raw-quarantine-")
        })
        .count(),
        0
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn quota_rejects_orphan_at_boundary_before_move() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    const SOURCE_BYTES: usize = 120 * 1024 * 1024;
    const ORPHAN_BYTES: usize = 8 * 1024 * 1024;

    let root = fixture("quota-orphan-boundary");
    let store = must(RawObjectFileStore::new(&root), "open orphan-boundary store");
    drop(store);
    let source_quarantine = root.join("sha256").join(".velnor-raw-quarantine-0-0-0");
    must(
        fs::create_dir(&source_quarantine),
        "create source quarantine",
    );
    must(
        fs::set_permissions(&source_quarantine, fs::Permissions::from_mode(0o700)),
        "restrict source quarantine",
    );
    let source_entry = source_quarantine.join("entry");
    must(
        fs::write(&source_entry, vec![b's'; SOURCE_BYTES]),
        "write source quarantine entry",
    );
    must(
        fs::set_permissions(&source_entry, fs::Permissions::from_mode(0o400)),
        "restrict source quarantine entry",
    );
    let orphan_name = "a".repeat(64);
    let orphan = root.join("sha256").join(&orphan_name);
    must(
        fs::write(&orphan, vec![b'o'; ORPHAN_BYTES]),
        "write orphan object",
    );
    must(
        fs::set_permissions(&orphan, fs::Permissions::from_mode(0o400)),
        "restrict orphan object",
    );
    assert!(
        must(fs::metadata(&orphan), "stat orphan object").blocks() * 512 >= ORPHAN_BYTES as u64
    );

    assert!(RawObjectFileStore::new(&root).is_err());
    assert_eq!(
        must(fs::metadata(&orphan), "stat preserved orphan object").len(),
        ORPHAN_BYTES as u64
    );
    assert_eq!(
        must(
            fs::read_dir(root.join("sha256")),
            "read objects after rejected orphan"
        )
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".velnor-raw-quarantine-")
        })
        .count(),
        1
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn concurrent_orphans_reject_before_either_move() {
    use std::os::unix::fs::PermissionsExt;

    const ORPHAN_BYTES: usize = 64 * 1024 * 1024;

    let root = fixture("quota-concurrent-orphans");
    let store = must(
        RawObjectFileStore::new(&root),
        "open concurrent-orphan store",
    );
    drop(store);
    let names = ["b".repeat(64), "c".repeat(64)];
    for name in &names {
        let path = root.join("sha256").join(name);
        must(
            fs::write(&path, vec![b'x'; ORPHAN_BYTES]),
            "write concurrent orphan",
        );
        must(
            fs::set_permissions(&path, fs::Permissions::from_mode(0o400)),
            "restrict concurrent orphan",
        );
    }
    let first_root = root.clone();
    let second_root = root.clone();
    let first = thread::spawn(move || RawObjectFileStore::new(&first_root).is_err());
    let second = thread::spawn(move || RawObjectFileStore::new(&second_root).is_err());
    assert!(first
        .join()
        .unwrap_or_else(|_| panic!("first opener panicked")));
    assert!(second
        .join()
        .unwrap_or_else(|_| panic!("second opener panicked")));
    for name in &names {
        assert!(root.join("sha256").join(name).is_file());
    }
    assert_eq!(
        must(
            fs::read_dir(root.join("sha256")),
            "read objects after concurrent rejection"
        )
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".velnor-raw-quarantine-")
        })
        .count(),
        0
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn complete_staged_retention_record_is_published_on_reopen() {
    let root = fixture("retention-staged-recovery");
    let store = must(RawObjectFileStore::new(&root), "open staged store");
    drop(store);
    let quarantine = root.join("sha256").join(".velnor-raw-quarantine-11-0-0");
    must(
        fs::create_dir(&quarantine),
        "create staged source quarantine",
    );
    must(
        fs::write(quarantine.join("entry"), b"staged-source"),
        "write staged source entry",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        must(
            fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)),
            "restrict staged source quarantine",
        );
        must(
            fs::set_permissions(quarantine.join("entry"), fs::Permissions::from_mode(0o400)),
            "restrict staged source entry",
        );
    }
    let reopened = must(
        RawObjectFileStore::new(&root),
        "materialize staged retention record",
    );
    drop(reopened);
    let retention = root.join(".velnor-raw-quarantine");
    let final_record = must(fs::read_dir(&retention), "read staged retention records")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".velnor-raw-retained-"))
                && !path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with(".velnor-raw-retained-pending-")
                })
        })
        .unwrap_or_else(|| panic!("materialized retention record missing"));
    let pending = retention.join(
        final_record
            .file_name()
            .unwrap_or_else(|| panic!("staged record has name"))
            .to_string_lossy()
            .replacen(".velnor-raw-retained-", ".velnor-raw-retained-pending-", 1),
    );
    must(
        fs::rename(&final_record, &pending),
        "move complete record into staging name",
    );
    assert!(RawObjectFileStore::new(&root).is_ok());
    assert!(final_record.is_dir());
    assert!(!pending.exists());
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn pending_recovery_rejects_replaced_valid_record() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("retention-staged-replacement");
    let store = must(RawObjectFileStore::new(&root), "open replacement store");
    drop(store);
    for (name, bytes) in [
        (".velnor-raw-quarantine-21-0-0", b"first-source".as_slice()),
        (".velnor-raw-quarantine-22-0-0", b"second-source".as_slice()),
    ] {
        let quarantine = root.join("sha256").join(name);
        must(
            fs::create_dir(&quarantine),
            "create replacement source quarantine",
        );
        must(
            fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)),
            "restrict replacement source quarantine",
        );
        let entry = quarantine.join("entry");
        must(fs::write(&entry, bytes), "write replacement source entry");
        must(
            fs::set_permissions(&entry, fs::Permissions::from_mode(0o400)),
            "restrict replacement source entry",
        );
    }
    let reopened = must(
        RawObjectFileStore::new(&root),
        "materialize replacement source records",
    );
    drop(reopened);

    let retention = root.join(".velnor-raw-quarantine");
    let mut records = must(fs::read_dir(&retention), "read replacement records")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".velnor-raw-retained-"))
                && !path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with(".velnor-raw-retained-pending-")
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    records.sort();
    let target = records.remove(0);
    let wrong = records.remove(0);
    let target_name = target
        .file_name()
        .unwrap_or_else(|| panic!("target record name"))
        .to_owned();
    let pending = retention.join(target_name.to_string_lossy().replacen(
        ".velnor-raw-retained-",
        ".velnor-raw-retained-pending-",
        1,
    ));
    must(fs::rename(&target, &pending), "stage target record");
    let pending_for_hook = pending.clone();
    let wrong_for_hook = wrong.clone();
    github_raw_store::set_test_pending_rename_hook(Box::new(move || {
        fs::remove_dir_all(&pending_for_hook)
            .unwrap_or_else(|error| panic!("remove staged target for replacement: {error}"));
        fs::rename(&wrong_for_hook, &pending_for_hook)
            .unwrap_or_else(|error| panic!("install valid wrong staged record: {error}"));
    }));

    assert!(RawObjectFileStore::new(&root).is_err());
    assert!(!retention.join(&target_name).exists());
    assert!(!pending.exists());
    assert!(must(fs::read_dir(&retention), "read rejected record")
        .flatten()
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".velnor-raw-retained-rejected-")
        }));
    assert!(
        RawObjectFileStore::new(&root).is_ok(),
        "typed rejected record must survive a subsequent reopen"
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn pending_recovery_rejects_manifest_name_alias() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("retention-staged-alias");
    let store = must(RawObjectFileStore::new(&root), "open alias store");
    drop(store);
    let quarantine = root.join("sha256").join(".velnor-raw-quarantine-41-0-0");
    must(
        fs::create_dir(&quarantine),
        "create alias source quarantine",
    );
    must(
        fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)),
        "restrict alias source quarantine",
    );
    must(
        fs::write(quarantine.join("entry"), b"alias-source"),
        "write alias source entry",
    );
    must(
        fs::set_permissions(quarantine.join("entry"), fs::Permissions::from_mode(0o400)),
        "restrict alias source entry",
    );
    let reopened = must(RawObjectFileStore::new(&root), "materialize alias source");
    drop(reopened);

    let retention = root.join(".velnor-raw-quarantine");
    let final_record = must(fs::read_dir(&retention), "read alias records")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".velnor-raw-retained-"))
                && !path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with(".velnor-raw-retained-pending-")
                })
        })
        .unwrap_or_else(|| panic!("alias final record missing"));
    let alias = retention.join(".velnor-raw-retained-pending-999-0-0");
    must(
        fs::rename(&final_record, &alias),
        "rename record to mismatched pending alias",
    );

    assert!(RawObjectFileStore::new(&root).is_err());
    assert!(
        alias.is_dir(),
        "mismatched alias must remain for inspection"
    );
    assert!(!final_record.exists());
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn fresh_materialization_rejects_replaced_record_and_reopens() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("retention-fresh-replacement");
    let store = must(
        RawObjectFileStore::new(&root),
        "open fresh replacement store",
    );
    drop(store);

    let wrong_quarantine = root.join("sha256").join(".velnor-raw-quarantine-51-0-0");
    must(
        fs::create_dir(&wrong_quarantine),
        "create wrong source quarantine",
    );
    must(
        fs::set_permissions(&wrong_quarantine, fs::Permissions::from_mode(0o700)),
        "restrict wrong source quarantine",
    );
    let wrong_entry = wrong_quarantine.join("entry");
    must(
        fs::write(&wrong_entry, b"wrong-source"),
        "write wrong source entry",
    );
    must(
        fs::set_permissions(&wrong_entry, fs::Permissions::from_mode(0o400)),
        "restrict wrong source entry",
    );
    let reopened = must(
        RawObjectFileStore::new(&root),
        "materialize wrong source record",
    );
    drop(reopened);

    let retention = root.join(".velnor-raw-quarantine");
    let wrong_record = must(fs::read_dir(&retention), "read wrong retention record")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".velnor-raw-retained-"))
                && !path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with(".velnor-raw-retained-pending-")
                })
        })
        .unwrap_or_else(|| panic!("wrong retention record missing"));
    let wrong_name = wrong_record
        .file_name()
        .unwrap_or_else(|| panic!("wrong retention record name"))
        .to_owned();

    let target_quarantine = root.join("sha256").join(".velnor-raw-quarantine-52-0-0");
    must(
        fs::create_dir(&target_quarantine),
        "create target source quarantine",
    );
    must(
        fs::set_permissions(&target_quarantine, fs::Permissions::from_mode(0o700)),
        "restrict target source quarantine",
    );
    let target_entry = target_quarantine.join("entry");
    must(
        fs::write(&target_entry, b"target-source"),
        "write target source entry",
    );
    must(
        fs::set_permissions(&target_entry, fs::Permissions::from_mode(0o400)),
        "restrict target source entry",
    );

    let retention_for_hook = retention.clone();
    let wrong_record_for_hook = wrong_record.clone();
    let wrong_name_for_hook = wrong_name.clone();
    github_raw_store::set_test_materialize_rename_hook(Box::new(move || {
        let fresh = fs::read_dir(&retention_for_hook)
            .unwrap_or_else(|error| panic!("read fresh records: {error}"))
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".velnor-raw-retained-"))
                    && path.file_name() != Some(wrong_name_for_hook.as_os_str())
                    && !path.file_name().is_some_and(|name| {
                        name.to_string_lossy()
                            .starts_with(".velnor-raw-retained-pending-")
                    })
            })
            .unwrap_or_else(|| panic!("fresh record missing in replacement hook"));
        fs::remove_dir_all(&fresh)
            .unwrap_or_else(|error| panic!("remove fresh record for replacement: {error}"));
        fs::rename(&wrong_record_for_hook, &fresh)
            .unwrap_or_else(|error| panic!("install wrong fresh record: {error}"));
    }));

    assert!(RawObjectFileStore::new(&root).is_err());
    assert!(must(fs::read_dir(&retention), "read fresh rejected record")
        .flatten()
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".velnor-raw-retained-rejected-")
        }));
    assert!(
        RawObjectFileStore::new(&root).is_ok(),
        "fresh identity rejection must be restart-safe"
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn pending_recovery_rejects_replaced_symlink() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let root = fixture("retention-staged-symlink");
    let store = must(
        RawObjectFileStore::new(&root),
        "open symlink replacement store",
    );
    drop(store);
    let quarantine = root.join("sha256").join(".velnor-raw-quarantine-31-0-0");
    must(
        fs::create_dir(&quarantine),
        "create symlink source quarantine",
    );
    must(
        fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)),
        "restrict symlink source quarantine",
    );
    let entry = quarantine.join("entry");
    must(
        fs::write(&entry, b"symlink-source"),
        "write symlink source entry",
    );
    must(
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o400)),
        "restrict symlink source entry",
    );
    let reopened = must(
        RawObjectFileStore::new(&root),
        "materialize symlink source record",
    );
    drop(reopened);

    let retention = root.join(".velnor-raw-quarantine");
    let target = must(fs::read_dir(&retention), "read symlink retention");
    let target = target
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".velnor-raw-retained-"))
        })
        .unwrap_or_else(|| panic!("symlink target record missing"));
    let target_name = target
        .file_name()
        .unwrap_or_else(|| panic!("symlink target name"))
        .to_owned();
    let pending = retention.join(target_name.to_string_lossy().replacen(
        ".velnor-raw-retained-",
        ".velnor-raw-retained-pending-",
        1,
    ));
    must(fs::rename(&target, &pending), "stage symlink target");
    let outside = root.join("outside-pending-target");
    must(fs::create_dir(&outside), "create symlink outside target");
    let pending_for_hook = pending.clone();
    let outside_for_hook = outside.clone();
    github_raw_store::set_test_pending_rename_hook(Box::new(move || {
        fs::remove_dir_all(&pending_for_hook)
            .unwrap_or_else(|error| panic!("remove staged symlink target: {error}"));
        symlink(&outside_for_hook, &pending_for_hook)
            .unwrap_or_else(|error| panic!("install staged symlink replacement: {error}"));
    }));

    assert!(RawObjectFileStore::new(&root).is_err());
    assert!(!retention.join(&target_name).exists());
    assert!(!pending.exists());
    assert!(must(fs::read_dir(&retention), "read rejected symlink")
        .flatten()
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".velnor-raw-retained-rejected-")
        }));
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn partial_staged_retention_record_fails_closed_without_reclaim() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("retention-partial-staged-record");
    let store = must(RawObjectFileStore::new(&root), "open partial-staged store");
    drop(store);
    let record = root
        .join(".velnor-raw-quarantine")
        .join(".velnor-raw-retained-pending-1-0-0");
    must(fs::create_dir(&record), "create partial staged record");
    must(
        fs::set_permissions(&record, fs::Permissions::from_mode(0o700)),
        "restrict partial staged record",
    );
    let manifest = record.join("manifest.json");
    must(
        fs::write(&manifest, b"{\"schema\":3"),
        "write partial staged manifest",
    );
    must(
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o400)),
        "restrict partial staged manifest",
    );

    assert!(RawObjectFileStore::new(&root).is_err());
    assert_eq!(
        must(
            fs::read(&manifest),
            "read preserved partial staged manifest"
        ),
        b"{\"schema\":3"
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn partial_retention_record_fails_closed_without_reclaim() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("retention-partial-record");
    let store = must(RawObjectFileStore::new(&root), "open partial-record store");
    drop(store);

    let record = root
        .join(".velnor-raw-quarantine")
        .join(".velnor-raw-retained-1-0-0");
    must(fs::create_dir(&record), "create partial retention record");
    must(
        fs::set_permissions(&record, fs::Permissions::from_mode(0o700)),
        "restrict partial retention record",
    );
    let manifest = record.join("manifest.json");
    must(
        fs::write(&manifest, b"{\"schema\":1"),
        "write partial manifest",
    );
    must(
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o400)),
        "restrict partial manifest",
    );

    assert!(RawObjectFileStore::new(&root).is_err());
    assert_eq!(
        must(fs::read(&manifest), "read partial manifest"),
        b"{\"schema\":1"
    );
    remove_fixture(&root);
}

#[cfg(unix)]
#[test]
fn manifest_fault_boundaries_leave_only_bounded_staging_state() {
    use github_raw_store::TestManifestFault;
    use std::os::unix::fs::PermissionsExt;

    let faults = [
        TestManifestFault::Write,
        TestManifestFault::FileSync,
        TestManifestFault::Chmod,
        TestManifestFault::Readback,
        TestManifestFault::DirectorySync,
        TestManifestFault::PublishRename,
    ];
    for (index, fault) in faults.into_iter().enumerate() {
        let root = fixture(&format!("retention-fault-{index}"));
        let store = must(RawObjectFileStore::new(&root), "open fault store");
        drop(store);
        let quarantine = root
            .join("sha256")
            .join(format!(".velnor-raw-quarantine-{index}-0-0"));
        must(
            fs::create_dir(&quarantine),
            "create fault source quarantine",
        );
        must(
            fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)),
            "restrict fault source quarantine",
        );
        let entry = quarantine.join("entry");
        must(
            fs::write(&entry, b"fault-source"),
            "write fault source entry",
        );
        must(
            fs::set_permissions(&entry, fs::Permissions::from_mode(0o400)),
            "restrict fault source entry",
        );

        github_raw_store::set_test_manifest_fault(fault);
        assert!(RawObjectFileStore::new(&root).is_err());
        let retention = root.join(".velnor-raw-quarantine");
        let pending = must(fs::read_dir(&retention), "read fault staging")
            .flatten()
            .any(|record| {
                record
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".velnor-raw-retained-pending-")
            });
        assert!(pending, "fault {fault:?} lost its bounded staging state");
        assert!(quarantine.is_dir());
        if fault == TestManifestFault::PublishRename
            || fault == TestManifestFault::DirectorySync
            || fault == TestManifestFault::Chmod
            || fault == TestManifestFault::Readback
            || fault == TestManifestFault::FileSync
        {
            assert!(RawObjectFileStore::new(&root).is_ok());
        } else {
            assert!(RawObjectFileStore::new(&root).is_err());
        }
        remove_fixture(&root);
    }
}

#[cfg(unix)]
#[test]
fn retention_disk_usage_does_not_duplicate_source_bytes() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    const PAYLOAD_BYTES: usize = 60 * 1024 * 1024;
    const RETENTION_QUOTA_BYTES: u64 = 128 * 1024 * 1024;
    const NAMESPACE_OVERHEAD_BYTES: u64 = 4 * 64 * 1024;
    const SOURCE_OVERHEAD_BYTES: u64 = 2 * 64 * 1024;
    const RECORD_OVERHEAD_BYTES: u64 = 2 * 64 * 1024;

    let root = fixture("retention-disk-usage");
    let store = must(RawObjectFileStore::new(&root), "open disk-usage store");
    drop(store);
    let payload = vec![b'q'; PAYLOAD_BYTES];
    for index in 0..2_u32 {
        let quarantine = root
            .join("sha256")
            .join(format!(".velnor-raw-quarantine-{index}-0-0"));
        must(fs::create_dir(&quarantine), "create disk-usage quarantine");
        must(
            fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)),
            "restrict disk-usage quarantine",
        );
        let entry = quarantine.join("entry");
        must(fs::write(&entry, &payload), "write disk-usage entry");
        must(
            fs::set_permissions(&entry, fs::Permissions::from_mode(0o400)),
            "restrict disk-usage entry",
        );
    }

    let reopened = must(
        RawObjectFileStore::new(&root),
        "reopen two 60MiB retained sources",
    );
    drop(reopened);

    let retention = root.join(".velnor-raw-quarantine");
    let mut manifest_bytes = 0_u64;
    let mut duplicate_entry_bytes = 0_u64;
    for record in must(fs::read_dir(&retention), "read disk-usage records").flatten() {
        let record = record.path();
        manifest_bytes += must(
            fs::metadata(record.join("manifest.json")),
            "stat disk-usage manifest",
        )
        .len();
        if record.join("entry").is_file() {
            duplicate_entry_bytes += must(
                fs::metadata(record.join("entry")),
                "stat duplicate disk-usage entry",
            )
            .len();
        }
    }
    let mut source_bytes = 0_u64;
    for quarantine in must(fs::read_dir(root.join("sha256")), "read source qdirs").flatten() {
        let path = quarantine.path();
        if path.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with(".velnor-raw-quarantine-")
        }) {
            source_bytes += must(fs::metadata(path.join("entry")), "stat source entry").len();
        }
    }
    assert_eq!(source_bytes, (2 * PAYLOAD_BYTES) as u64);
    assert_eq!(duplicate_entry_bytes, 0);
    assert!(
        source_bytes
            + manifest_bytes
            + NAMESPACE_OVERHEAD_BYTES
            + SOURCE_OVERHEAD_BYTES
            + RECORD_OVERHEAD_BYTES
            <= RETENTION_QUOTA_BYTES
    );
    fn allocated_bytes(path: &Path) -> u64 {
        let metadata = must(fs::symlink_metadata(path), "stat allocated tree entry");
        let self_bytes = metadata.blocks() * 512;
        if metadata.file_type().is_dir() {
            self_bytes
                + must(fs::read_dir(path), "read allocated tree directory")
                    .flatten()
                    .map(|entry| allocated_bytes(&entry.path()))
                    .sum::<u64>()
        } else {
            self_bytes
        }
    }
    assert!(
        allocated_bytes(&root) <= RETENTION_QUOTA_BYTES,
        "measured filesystem allocation exceeds retention quota"
    );
    remove_fixture(&root);
}

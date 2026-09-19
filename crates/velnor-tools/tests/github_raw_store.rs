#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "these tests turn fixture setup failures into explicit test failures"
)]

mod github_acquisition {
    use sha2::{Digest, Sha256};

    #[derive(Clone, PartialEq, Eq)]
    pub struct RawObject {
        pub raw_id: String,
        pub request_id: String,
        pub object_kind: String,
        pub canonicalization: String,
        pub media_type: String,
        pub bytes: Vec<u8>,
        pub original_sha256: String,
        pub original_byte_length: u64,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

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
        original_sha256: github_acquisition::sha256_digest(source),
        original_byte_length: source.len() as u64,
    }
}

fn object_path(root: &Path, reference: &RawObjectRef) -> PathBuf {
    let digest = reference
        .sha256
        .strip_prefix("sha256:")
        .unwrap_or_else(|| panic!("safe digest has sha256 prefix"));
    root.join("sha256").join(digest)
}

fn sidecar_path(root: &Path, reference: &RawObjectRef) -> PathBuf {
    root.join("refs").join(format!("{}.json", reference.raw_id))
}

#[test]
fn stores_safe_bytes_with_distinct_original_provenance() {
    let root = fixture("provenance");
    let source = br#"{"authorization":"github_pat_original"}"#;
    let safe = br#"{"authorization":"[REDACTED]"}"#;
    let mut store = must(RawObjectFileStore::new(&root), "open store");
    assert_eq!(store.root(), root.as_path());

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
    assert_ne!(reference.original_sha256, reference.sha256);
    assert_ne!(reference.original_byte_length, reference.byte_length);
    must(store.verify(&reference), "verify capture");

    let mut forged = reference.clone();
    forged.original_sha256 = github_acquisition::sha256_digest(b"unrelated-source");
    assert!(store.verify(&forged).is_err());
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
    remove_fixture(&base);
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

#[test]
fn concurrent_publish_is_no_clobber() {
    let root = fixture("concurrent");
    let workers = 8;
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

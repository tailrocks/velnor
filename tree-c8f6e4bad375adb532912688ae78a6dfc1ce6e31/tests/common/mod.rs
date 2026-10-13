//! Shared test helpers.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// Absolute path to the canonical repository root (two levels above the crate).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root resolves")
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A unique, freshly created temporary directory under the system temp dir.
pub fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "velnor-actions-test-{tag}-{}-{n}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Recursively copy `src` into `dst` (which is created if absent).
pub fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create dst");
    for entry in std::fs::read_dir(src).expect("read src") {
        let entry = entry.expect("dir entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy file");
        }
    }
}

/// Build a full fleet fixture in a fresh temp dir: copy `fleet/` and `actions/`,
/// bind `fleet/block-sha` to `block_sha`, then generate every template and
/// callable workflow. Returns the fixture root.
pub fn bound_fixture(block_sha: &str) -> PathBuf {
    let root = repo_root();
    let dir = temp_dir("fixture");
    copy_dir(&root.join("fleet"), &dir.join("fleet"));
    copy_dir(&root.join("actions"), &dir.join("actions"));
    std::fs::create_dir_all(dir.join("templates")).unwrap();
    std::fs::write(
        dir.join("fleet").join("block-sha"),
        format!("{block_sha}\n"),
    )
    .unwrap();
    velnor_actions_generator::generate(&dir).expect("generate fixture");
    dir
}

/// Overwrite one byte in the file at `path` and leave the rest intact.
pub fn tamper_one_byte(path: &Path) {
    let mut bytes = std::fs::read(path).expect("read file to tamper");
    assert!(!bytes.is_empty(), "cannot tamper an empty file");
    // Flip a byte near the middle to a distinct printable value.
    let idx = bytes.len() / 2;
    bytes[idx] = if bytes[idx] == b'X' { b'Y' } else { b'X' };
    std::fs::write(path, bytes).expect("write tampered file");
}

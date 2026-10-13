//! Source identity embedding for `velnor-workflow`.
//!
//! Two values are stamped into every binary:
//!
//! * `VELNOR_WORKFLOW_SOURCE_SHA`: the exact 40-hex `HEAD` of the crate's
//!   checkout (`velnor-workflow --revision`). Provenance metadata: which
//!   commit the binary was built from.
//! * `VELNOR_WORKFLOW_CLOSURE_DIGEST`: the source-closure digest of the
//!   checkout's `HEAD` tree (`velnor-workflow --closure`, see
//!   `src/closure.rs`). Product identity: binaries built from different
//!   commits with the same closure are interchangeable renderers.
//!
//! The closure duplicates the canonicalization in `src/closure.rs` (a build
//! script cannot import the crate it builds): `git ls-tree -r HEAD` over the
//! closure paths, re-sorted in byte order, plus the footer, hashed with
//! SHA-256. The footer version and path list are pinned by unit tests against
//! `src/closure.rs`, so the two implementations cannot drift silently: any
//! drift fails closed (digests mismatch, no product is accepted).
//!
//! A tree without git (or a git failure) stamps `unknown` for both values,
//! which no pinned-revision or closure probe can ever match.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// Closure paths, mirroring `closure::CLOSURE_PATHS`.
const CLOSURE_PATHS: &[&str] = &[
    "crates/velnor-workflow",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rust-toolchain",
    ".cargo",
];

/// Closure algorithm version, mirroring `closure::CLOSURE_VERSION`.
const CLOSURE_VERSION: u8 = 1;

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_else(|| ".".into()));
    // Re-run on every build. Cargo only re-runs a build script when a file it
    // named last time changed, and the fingerprint (and therefore that list of
    // files) is keyed by package, not by checkout: a target directory shared by
    // two checkouts would otherwise keep the first checkout's `HEAD` paths and
    // hand the second checkout a binary stamped with the first one's commit —
    // exactly the false proof the D19 guard must never be able to produce.
    // Naming a path that never exists marks the script stale unconditionally;
    // the cost is one `git rev-parse`, and the crate itself only recompiles
    // when the stamped value actually changes.
    if let Some(out_dir) = std::env::var_os("OUT_DIR") {
        let sentinel = PathBuf::from(out_dir).join("velnor-workflow-source-sha.always-rerun");
        println!("cargo:rerun-if-changed={}", sentinel.display());
    }
    let sha = git(&manifest_dir, &["rev-parse", "HEAD"])
        .filter(|value| is_full_sha(value))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=VELNOR_WORKFLOW_SOURCE_SHA={sha}");
    println!(
        "cargo:rustc-env=VELNOR_WORKFLOW_CLOSURE_DIGEST={}",
        self_closure(&manifest_dir).unwrap_or_else(|| "unknown".to_owned())
    );
    println!(
        "cargo:rustc-env=VELNOR_WORKFLOW_FEATURES={}",
        cargo_features()
    );
}

/// Closure digest of the checkout's `HEAD` tree, or `None` when it cannot be
/// proven (no git, unknown `HEAD`, or no closure inputs tracked).
fn self_closure(manifest_dir: &Path) -> Option<String> {
    let root = git(manifest_dir, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root);
    let mut arguments = vec!["ls-tree", "-r", "HEAD", "--"];
    arguments.extend_from_slice(CLOSURE_PATHS);
    let listing = git(&root, &arguments)?;
    let mut lines: Vec<&str> = listing.lines().collect();
    if lines.is_empty() {
        return None;
    }
    lines.sort_unstable();
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(
        format!(
            "closure-version:{CLOSURE_VERSION}\nfeatures:{}\nprofile:{}\n",
            cargo_features(),
            std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned())
        )
        .as_bytes(),
    );
    let digest = Sha256::digest(&bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    Some(output)
}

/// Enabled Cargo features as a sorted comma list (`CARGO_FEATURE_*` is set
/// per enabled feature, uppercased with `-` mapped to `_`).
fn cargo_features() -> String {
    let mut features: Vec<String> = std::env::vars()
        .filter_map(|(name, _)| {
            name.strip_prefix("CARGO_FEATURE_")
                .map(str::to_ascii_lowercase)
        })
        .collect();
    features.sort();
    features.join(",")
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

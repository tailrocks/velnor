//! Source identity embedding for `velnor-workflow`.
//!
//! Four values are stamped into every binary:
//!
//! * `VELNOR_WORKFLOW_SOURCE_SHA`: the exact 40-hex `HEAD` of the crate's
//!   checkout (`velnor-workflow --revision`). Provenance metadata: which
//!   commit the binary was built from.
//! * `VELNOR_WORKFLOW_CLOSURE_DIGEST`: the source-closure digest of the
//!   checkout's `HEAD` tree (`velnor-workflow --closure`, see
//!   `src/closure.rs`). Product identity: binaries built from different
//!   commits with the same closure are interchangeable renderers.
//! * `VELNOR_WORKFLOW_FEATURES`: the sorted enabled-feature list the closure
//!   footer hashed (see `cargo_features`).
//! * `VELNOR_WORKFLOW_PROFILE`: the Cargo profile the closure footer hashed.
//!   Together with the feature list it lets `promote` recompute the stamped
//!   pin's closure under exactly the running binary's own build identity, so
//!   the render-with-X-stamp-X binding holds for release products and
//!   development builds alike.
//!
//! The closure uses the shared canonicalization in `src/identity.rs` (a build
//! script cannot import the crate it builds). Clean trees retain the exact
//! `git ls-tree -r HEAD` v1 lines; dirty trees hash the current bytes, modes,
//! symlink targets, additions, and deletions before the same footer. The
//! footer version and path list remain pinned by closure tests, so a drift
//! fails closed (digests mismatch, no product is accepted).
//!
//! A tree without git (or a git failure) stamps `unknown` for both values,
//! which no pinned-revision or closure probe can ever match.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/identity.rs"]
mod identity;

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
    println!(
        "cargo:rustc-env=VELNOR_WORKFLOW_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned())
    );
}

/// Closure digest of the checkout's `HEAD` baseline and current worktree, or
/// `None` when it cannot be proven (no git, unknown `HEAD`, no closure inputs,
/// or an unsupported worktree path).
fn self_closure(manifest_dir: &Path) -> Option<String> {
    let root = git(manifest_dir, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root);
    identity::worktree_digest(
        &root,
        "HEAD",
        CLOSURE_PATHS,
        CLOSURE_VERSION,
        &cargo_features(),
        &std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned()),
    )
    .ok()
}

/// Enabled Cargo features as a sorted comma list (`CARGO_FEATURE_*` is set
/// per enabled feature, uppercased with `-` mapped to `_`). The synthetic
/// `default` marker is filtered out: it names no code, and the canonical
/// closure form (`closure::DEV_FEATURES`) spells the default set without it,
/// so keeping it would make no default build ever match its own closure.
fn cargo_features() -> String {
    let mut features: Vec<String> = std::env::vars()
        .filter_map(|(name, _)| {
            name.strip_prefix("CARGO_FEATURE_")
                .map(str::to_ascii_lowercase)
        })
        .filter(|feature| feature != "default")
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

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

use serde_json::json;
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
    println!(
        "cargo:rustc-env=VELNOR_WORKFLOW_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned())
    );
    println!(
        "cargo:rustc-env=VELNOR_WORKFLOW_BUILD_IDENTITY={}",
        build_identity(&sha)
    );
}

/// The fields that can alter a runtime binary but are not represented by the
/// source closure. This JSON is embedded in the binary and copied into the
/// immutable release manifest by the trusted publisher; consumers therefore
/// compare the manifest to bytes produced by the compiler itself rather than
/// trusting a build step's self-reported environment.
fn build_identity(source_revision: &str) -> String {
    let target = env_value("TARGET").unwrap_or_default();
    let host = env_value("HOST").unwrap_or_default();
    let rustc = command_output("rustc", &["-Vv"]).unwrap_or_default();
    let toolchain = env_value("VELNOR_WORKFLOW_BUILD_TOOLCHAIN")
        .or_else(|| env_value("RUSTUP_TOOLCHAIN"))
        .or_else(|| command_output("rustup", &["show", "active-toolchain"]))
        .unwrap_or_else(|| rustc.clone());
    let linker = target_linker(&target)
        .or_else(|| env_value("RUSTC_LINKER"))
        .unwrap_or_default();
    let platform =
        env_value("VELNOR_WORKFLOW_BUILD_PLATFORM").unwrap_or_else(|| platform_for_target(&target));
    let identity = json!({
        "schema": "velnor-workflow.runtime-build-identity.v1",
        "source_revision": source_revision,
        "toolchain": toolchain,
        "rustc": rustc,
        "target": target,
        "host": host,
        "platform": platform,
        "profile": std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned()),
        "features": cargo_features(),
        "rustflags": std::env::var("RUSTFLAGS").unwrap_or_default(),
        "cargo_encoded_rustflags": std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default(),
        "linker": linker,
        "cc": std::env::var("CC").unwrap_or_default(),
        "cflags": std::env::var("CFLAGS").unwrap_or_default(),
    });
    serde_json::to_string(&identity).unwrap_or_else(|_| "{}".to_owned())
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn command_output(command: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(command).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn target_linker(target: &str) -> Option<String> {
    let suffix = target.replace('-', "_").to_ascii_uppercase();
    env_value(&format!("CARGO_TARGET_{suffix}_LINKER"))
}

fn platform_for_target(target: &str) -> String {
    if target.starts_with("x86_64-") {
        "Linux-X64".to_owned()
    } else if target.starts_with("aarch64-apple-") {
        "macOS-ARM64".to_owned()
    } else if target.starts_with("aarch64-") {
        "Linux-ARM64".to_owned()
    } else {
        target.to_owned()
    }
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

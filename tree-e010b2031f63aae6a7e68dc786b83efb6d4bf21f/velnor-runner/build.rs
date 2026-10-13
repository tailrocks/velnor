//! Release identity embedding for `velnor-runner`.
//!
//! Plan 010 (§1, "Define a non-circular identity model"): a publishable release
//! record must bind one source commit through crate version, binary/deb digests,
//! OCI digest, compiled-manifest hash, APT coordinate, and the deployed export.
//! The *root* of that chain is the exact source SHA and tag the binary was built
//! from — embedded here at compile time so a running binary can prove its own
//! provenance without trusting a mutable label.
//!
//! Two modes, selected by the `release-build` cargo feature:
//!
//! * **default (development)** — no git introspection. The binary is stamped
//!   `development`/`development`/`development` and `release::embedded()` reports a
//!   development build that *cannot* emit a publishable record. This is the only
//!   path exercised by the normal build/test pipeline, so it must stay cheap and
//!   never fail.
//! * **`release-build`** — the exact 40-hex `HEAD` commit and the single `v*` tag
//!   pointing at `HEAD` are derived from git. The build FAILS (panics) unless the
//!   tree is clean, exactly one release tag points at `HEAD`, and
//!   `tag == v<crate version> == Cargo.lock version`. This is the empty-slack gate
//!   that makes a mismatched package/manifest structurally impossible to ship.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Re-run whenever the crate version, the lockfile, or the checked-out ref
    // changes — all three feed the release-build coherence gate.
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_RELEASE_BUILD");
    let crate_version = env("CARGO_PKG_VERSION");

    if std::env::var_os("CARGO_FEATURE_RELEASE_BUILD").is_none() {
        emit("VELNOR_SOURCE_SHA", "development");
        emit("VELNOR_SOURCE_TAG", "development");
        emit("VELNOR_BUILD_KIND", "development");
        return;
    }

    let (sha, tag) = derive_release_identity(&crate_version);
    emit("VELNOR_SOURCE_SHA", &sha);
    emit("VELNOR_SOURCE_TAG", &tag);
    emit("VELNOR_BUILD_KIND", "release");
}

fn emit(key: &str, value: &str) {
    println!("cargo:rustc-env={key}={value}");
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("build.rs: {key} is not set by cargo"))
}

/// Derive `(commit_sha, tag)` from git and prove the coherence identity. Any
/// failure here is a hard build failure: a release binary must never be produced
/// from an ambiguous, dirty, or version-drifted tree.
fn derive_release_identity(crate_version: &str) -> (String, String) {
    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../../Cargo.lock").display()
    );

    let head = git(&manifest_dir, &["rev-parse", "HEAD"]);
    if !is_full_sha(&head) {
        panic!("release-build: `git rev-parse HEAD` did not return a 40-hex commit: {head:?}");
    }

    let status = git(&manifest_dir, &["status", "--porcelain"]);
    if !status.is_empty() {
        panic!(
            "release-build: refusing to embed identity from a dirty tree ({} changed path(s)); \
             commit or stash before a release build",
            status.lines().count()
        );
    }

    // Exactly one `v*` tag must point at HEAD. Zero (non-tag build) or many
    // (ambiguous lineage) both fail closed.
    let points_at = git(&manifest_dir, &["tag", "--points-at", "HEAD"]);
    let release_tags: Vec<&str> = points_at
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('v') && !line.is_empty())
        .collect();
    let tag = match release_tags.as_slice() {
        [single] => (*single).to_string(),
        [] => panic!(
            "release-build: no `v*` tag points at HEAD ({head}); release binaries build only from a tagged commit"
        ),
        many => panic!("release-build: {} `v*` tags point at HEAD; lineage is ambiguous: {many:?}", many.len()),
    };

    let tag_version = tag.strip_prefix('v').unwrap_or(&tag);
    if tag_version != crate_version {
        panic!(
            "release-build: tag {tag} (version {tag_version}) does not match crate version {crate_version}"
        );
    }

    let lock_version = cargo_lock_version(&manifest_dir.join("../../Cargo.lock"));
    if lock_version.as_deref() != Some(crate_version) {
        panic!(
            "release-build: Cargo.lock records velnor-runner {lock_version:?}, crate version is {crate_version}"
        );
    }

    (head, tag)
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|err| {
            panic!(
                "release-build: failed to run `git {}`: {err}",
                args.join(" ")
            )
        });
    if !output.status.success() {
        panic!(
            "release-build: `git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Extract the `velnor-runner` version from `Cargo.lock`. Hand-scanned (no toml
/// build-dependency) because this only runs on the release-build path and must
/// never add cost to the default build.
fn cargo_lock_version(lock_path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(lock_path)
        .unwrap_or_else(|err| panic!("release-build: cannot read {}: {err}", lock_path.display()));
    let mut in_target = false;
    for line in contents.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            in_target = false;
            continue;
        }
        if line == "name = \"velnor-runner\"" {
            in_target = true;
            continue;
        }
        if in_target {
            if let Some(rest) = line.strip_prefix("version = \"") {
                return rest.strip_suffix('"').map(str::to_string);
            }
        }
    }
    None
}

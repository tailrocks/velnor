//! Release identity embedding for `velnor-runner`.
//!
//! Plan 010 (§1, "Define a non-circular identity model"): a publishable release
//! record must bind one source commit through crate version, binary/deb digests,
//! OCI digest, compiled-manifest hash, APT coordinate, and the deployed export.
//! The *root* of that chain is the exact source SHA and tag the binary was built
//! from — embedded here at compile time so a running binary can prove its own
//! provenance without trusting a mutable label.
//!
//! Two modes, selected by the `release-build` cargo feature plus the
//! `VELNOR_RELEASE_BUILD=1` environment, which only the release workflow
//! exports:
//!
//! * **default (development)** — no git introspection. The binary is stamped
//!   `development`/`development`/`development` and `release::embedded()` reports a
//!   development build that *cannot* emit a publishable record. This is the only
//!   path exercised by the normal build/test pipeline, so it must stay cheap and
//!   never fail.
//! * **feature on, environment unset (inert)** — the scanner-driven CI enables
//!   `--all-features`, which turns this feature on outside any release. Without
//!   `VELNOR_RELEASE_BUILD=1` the feature is inert (development stamp plus a
//!   build warning) so normal `--all-features` builds stay green. A binary
//!   built this way still cannot emit a publishable record.
//! * **`release-build`** — with `VELNOR_RELEASE_BUILD=1`, the exact 40-hex
//!   `HEAD` commit and the single `v*` tag pointing at `HEAD` are derived from
//!   git. The build FAILS (panics) unless the tree is clean, exactly one
//!   release tag points at `HEAD`, and `tag == v<crate version> == Cargo.lock
//!   version`. This is the empty-slack gate that makes a mismatched
//!   package/manifest structurally impossible to ship.

// Fail-closed by design: every `panic!` below aborts a release or preview
// build rather than embedding an ambiguous, dirty, or version-drifted
// identity. A build script cannot return a recoverable error for these
// gates — panicking (failing the build) is the only sound outcome — so the
// whole module keeps `panic!` under one allow instead of per-site noise.
#![allow(
    clippy::panic,
    reason = "fail-closed release identity gates must abort the build"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Re-run whenever the crate version, the lockfile, or the checked-out ref
    // changes — all three feed the release-build coherence gate.
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_RELEASE_BUILD");
    println!("cargo:rerun-if-env-changed=VELNOR_RELEASE_BUILD");
    println!("cargo:rerun-if-env-changed=VELNOR_PREVIEW_SOURCE_SHA");
    println!("cargo:rerun-if-env-changed=VELNOR_PREVIEW_BUILD_VERSION");
    let crate_version = env("CARGO_PKG_VERSION");

    if std::env::var_os("CARGO_FEATURE_RELEASE_BUILD").is_none() {
        emit("VELNOR_SOURCE_SHA", "development");
        emit("VELNOR_SOURCE_TAG", "development");
        emit("VELNOR_BUILD_KIND", "development");
        emit("VELNOR_BUILD_VERSION", &crate_version);
        return;
    }
    if std::env::var("VELNOR_RELEASE_BUILD").as_deref() != Ok("1") {
        println!(
            "cargo:warning=release-build feature is inert without VELNOR_RELEASE_BUILD=1; \
             embedding development identity"
        );
        emit("VELNOR_SOURCE_SHA", "development");
        emit("VELNOR_SOURCE_TAG", "development");
        emit("VELNOR_BUILD_KIND", "development");
        emit("VELNOR_BUILD_VERSION", &crate_version);
        return;
    }

    // Rolling preview builds are set by the generated preview workflow: an
    // untagged main commit can never satisfy the release tag gate below, yet the
    // shipped package identity must still name the exact source commit.
    if std::env::var_os("VELNOR_PREVIEW_SOURCE_SHA").is_some() {
        let (sha, tag) = derive_preview_identity(&crate_version);
        emit("VELNOR_SOURCE_SHA", &sha);
        emit("VELNOR_SOURCE_TAG", &tag);
        emit("VELNOR_BUILD_KIND", "preview");
        let display_version = preview_build_version(&crate_version, &sha);
        emit("VELNOR_BUILD_VERSION", &display_version);
        return;
    }

    let (sha, tag) = derive_release_identity(&crate_version);
    emit("VELNOR_SOURCE_SHA", &sha);
    emit("VELNOR_SOURCE_TAG", &tag);
    emit("VELNOR_BUILD_KIND", "release");
    emit("VELNOR_BUILD_VERSION", &crate_version);
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

/// Derive `(commit_sha, tag)` for a rolling preview build. A source checkout
/// proves identity through exact Git HEAD and a clean tree. A Homebrew source
/// archive has no `.git`, so its root marker must contain the exact admitted
/// SHA. Both modes retain the lockfile/version check. The preview tag remains
/// `preview`, and preview builds cannot emit a stable release record.
fn derive_preview_identity(crate_version: &str) -> (String, String) {
    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../../Cargo.lock").display()
    );

    let sha = std::env::var("VELNOR_PREVIEW_SOURCE_SHA")
        .unwrap_or_else(|err| panic!("build.rs: VELNOR_PREVIEW_SOURCE_SHA is not set: {err}"));
    if !is_full_sha(&sha) {
        panic!("preview-build: VELNOR_PREVIEW_SOURCE_SHA must be a 40-hex commit, got {sha:?}");
    }

    if git_checkout_exists(&manifest_dir) {
        let head = git(&manifest_dir, &["rev-parse", "HEAD"]);
        if head != sha {
            panic!(
                "preview-build: VELNOR_PREVIEW_SOURCE_SHA {sha} does not name the checked-out HEAD {head}"
            );
        }

        let status = git(&manifest_dir, &["status", "--porcelain"]);
        if !status.is_empty() {
            panic!(
                "preview-build: refusing to embed identity from a dirty tree ({} changed path(s))",
                status.lines().count()
            );
        }
    } else {
        let marker = manifest_dir.join("../../homebrew-preview-source.sha");
        let metadata = std::fs::symlink_metadata(&marker).unwrap_or_else(|err| {
            panic!("preview-build: source archive identity marker is unavailable: {err}")
        });
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            panic!("preview-build: source archive identity marker must be a regular file");
        }
        let contents = std::fs::read(&marker).unwrap_or_else(|err| {
            panic!("preview-build: cannot read source archive identity marker: {err}")
        });
        if contents != format!("{sha}\n").as_bytes() {
            panic!("preview-build: source archive marker does not match VELNOR_PREVIEW_SOURCE_SHA");
        }
        println!("cargo:rerun-if-changed={}", marker.display());
    }

    // Same lockfile gate as a release build: a preview of crate version X must
    // be built from a tree whose lockfile also records X.
    let lock_version = cargo_lock_version(&manifest_dir.join("../../Cargo.lock"));
    if lock_version.as_deref() != Some(crate_version) {
        panic!(
            "preview-build: Cargo.lock records velnor-runner {lock_version:?}, crate version is {crate_version}"
        );
    }

    (sha, "preview".to_string())
}

/// Apply the source-derived manifest version to the binary's human-facing
/// identity. A source archive has no Git metadata, so the formula must pass its
/// verified manifest version. Git-backed native/APT preview builds keep their
/// existing crate-version behavior unless they explicitly provide a version.
fn preview_build_version(crate_version: &str, source_sha: &str) -> String {
    let version = match std::env::var("VELNOR_PREVIEW_BUILD_VERSION") {
        Ok(version) => version,
        Err(std::env::VarError::NotPresent) => {
            let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
            if git_checkout_exists(&manifest_dir) {
                return crate_version.to_string();
            }
            panic!("preview-build: VELNOR_PREVIEW_BUILD_VERSION is required for source archives");
        }
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("preview-build: VELNOR_PREVIEW_BUILD_VERSION is not valid Unicode");
        }
    };
    let Some(rest) = version.strip_prefix(&format!("{crate_version}-preview.")) else {
        panic!("preview-build: VELNOR_PREVIEW_BUILD_VERSION has the wrong crate-version prefix");
    };
    let Some((count, short_sha)) = rest.split_once('+') else {
        panic!("preview-build: VELNOR_PREVIEW_BUILD_VERSION must include source revision and SHA");
    };
    let parsed_count = count.parse::<u64>();
    if parsed_count.is_err()
        || parsed_count.is_ok_and(|value| value == 0 || value.to_string() != count)
        || short_sha != &source_sha[..7]
    {
        panic!("preview-build: VELNOR_PREVIEW_BUILD_VERSION does not bind the source SHA");
    }
    version
}

fn git_checkout_exists(dir: &Path) -> bool {
    Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .is_ok_and(|output| output.status.success())
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
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
        if in_target && let Some(rest) = line.strip_prefix("version = \"") {
            return rest.strip_suffix('"').map(str::to_string);
        }
    }
    None
}

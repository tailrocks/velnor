//! Source identity embedding for `velnor-workflow`.
//!
//! The D19 coherence guard needs a `velnor-workflow` binary built at the
//! pinned policy revision and must be able to *prove* which revision a
//! candidate binary is, instead of trusting the directory it was found in.
//! This mirrors `crates/velnor-runner/build.rs`: the exact 40-hex `HEAD` of
//! the crate's checkout is read from git at build time and stamped into the
//! binary as `VELNOR_WORKFLOW_SOURCE_SHA` (`velnor-workflow --revision`,
//! `velnor-workflow version --json`).
//!
//! Unlike the runner there is no release gate here: `cargo install --git …
//! --rev <sha>` builds inside cargo's git checkout, whose detached `HEAD` is
//! exactly `<sha>`, and a workspace build reports the checked-out commit. A
//! tree without git (or a git failure) stamps `unknown`, which no
//! pinned-revision probe can ever match — the guard then fails closed rather
//! than accept an unprovable binary.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_else(|| ".".into()));
    // Rebuild when HEAD moves so a workspace binary never reports a stale
    // commit. `--absolute-git-dir` resolves cargo's checkout `.git` files too.
    if let Some(git_dir) = git(&manifest_dir, &["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/packed-refs");
        if let Some(reference) = git(&manifest_dir, &["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={git_dir}/{reference}");
        }
    }
    let sha = git(&manifest_dir, &["rev-parse", "HEAD"])
        .filter(|value| is_full_sha(value))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=VELNOR_WORKFLOW_SOURCE_SHA={sha}");
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

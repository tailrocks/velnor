//! Source-closure identity for `velnor-workflow` products.
//!
//! A product is identified by the digest of everything that can affect the
//! binary, not by the repository commit it happened to be built from. Two
//! commits with the same closure name the same product, so an unrelated
//! change elsewhere in the monorepo never invalidates the runtime.
//!
//! # Closure inputs
//!
//! * the `crates/velnor-workflow` subtree (all sources, `build.rs`, the
//!   crate manifest, and embedded templates — captured as a file set, so a
//!   newly added file can never escape the digest);
//! * the workspace root `Cargo.toml` (profiles, lints, workspace settings);
//! * `Cargo.lock` (every dependency version, including git revisions);
//! * the toolchain pins (`rust-toolchain.toml`, `rust-toolchain`);
//! * tracked `.cargo` configuration;
//! * the enabled Cargo features (sorted);
//! * the Cargo profile (`release` or `debug`).
//!
//! Machine-local inputs are excluded by contract, not by hashing: producers
//! build clean checkouts (`git status --porcelain` must be empty) with an
//! isolated `CARGO_HOME`, scrubbed `RUSTFLAGS`/`CARGO_ENCODED_RUSTFLAGS`
//! (asserted empty), and no `RUSTC_WRAPPER`. Anything that violates that
//! contract fails the producer before a product exists.
//!
//! # Canonical form
//!
//! The digest input is the byte stream `git ls-tree -r <rev> -- <paths>`
//! emits (one `<mode> SP <type> SP <sha> TAB <path> LF` line per entry),
//! re-sorted in byte order so producers never depend on git's native tree
//! order, followed by the footer:
//!
//! ```text
//! closure-version:1
//! features:<sorted-comma-list-or-empty>
//! profile:<release|debug>
//! ```
//!
//! The digest is the lowercase hex SHA-256 of those bytes. `setup-velnor-workflow`
//! recomputes the identical byte stream in shell (`git ls-tree … | LC_ALL=C sort`,
//! or the recursive-trees API filtered to the same paths when the pin is not
//! in the local history) — the formats agree by construction and the fixture
//! test below pins the byte layout.
//!
//! Product tags name `velnor-workflow-runtime-v1-<digest16>` (one release per
//! closure, one asset per platform). The tag is a locator only: acceptance
//! always compares the full 64-hex digest the binary reports (`--closure`)
//! and the manifest records.

use std::path::Path;
use std::process::Command;

use super::GeneratorError;

use crate::closure::worktree_identity;

/// Closure algorithm version. Bump when the inputs or canonical form change;
/// digests minted under different versions never compare equal because the
/// version is part of the hashed footer.
pub(crate) const CLOSURE_VERSION: u8 = 1;

/// Cargo profile of Stage-0 release products.
pub(crate) const PROFILE_RELEASE: &str = "release";
/// Cargo profile of candidate products (verified against the unit job's test
/// build before any reuse).
pub(crate) const PROFILE_DEBUG: &str = "debug";

/// Feature set CI products are built with: none (`--no-default-features`).
/// Local development builds enable `tui` and therefore never share a digest
/// with a CI product.
pub(crate) const CI_FEATURES: &str = "";

/// Feature set the candidate build stamps (default features): the
/// candidate product. Pinned by test against the crate manifest. Unit jobs
/// may build with wider features, so reuse is verified, never assumed.
pub(crate) const DEV_FEATURES: &str = "tui";

/// Release tag prefix for runtime products.
pub(crate) const PRODUCT_TAG_PREFIX: &str = "velnor-workflow-runtime-v1-";

/// Paths (files or subtrees) whose tracked content is the source closure.
/// Git pathspec semantics: a directory names its whole subtree, so future
/// files inside these directories are covered without updating this list.
pub(crate) const CLOSURE_PATHS: &[&str] = &[
    "crates/velnor-workflow",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rust-toolchain",
    ".cargo",
];

/// Whether `value` is a full 64-hex closure digest.
pub(crate) fn is_full_closure(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The immutable release tag locating the product built from `closure`.
/// The tag carries the digest prefix as a locator; acceptance always uses
/// the full digest from the manifest and the binary's own report.
pub(crate) fn product_tag(closure: &str) -> String {
    debug_assert!(is_full_closure(closure));
    format!("{PRODUCT_TAG_PREFIX}{}", &closure[..16])
}

/// Canonical digest over `ls_tree_lines` (raw `git ls-tree -r` output lines,
/// without trailing newlines) with the given feature set and profile.
pub(crate) fn canonical_digest(ls_tree_lines: &[String], features: &str, profile: &str) -> String {
    worktree_identity::canonical_digest(ls_tree_lines, CLOSURE_VERSION, features, profile)
}

/// Digest of the closure inputs at `rev` in the repository at `repo`.
/// Fails when `rev` is not a commit of that checkout.
pub(crate) fn closure_of_tree(
    repo: &Path,
    rev: &str,
    features: &str,
    profile: &str,
) -> Result<String, GeneratorError> {
    let mut arguments = vec!["ls-tree", "-r", rev, "--"];
    arguments.extend_from_slice(CLOSURE_PATHS);
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(&arguments)
        .output()
        .map_err(|error| {
            GeneratorError::usage(format!("run git {}: {error}", arguments.join(" ")))
        })?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "revision {rev} is not a commit of {}",
            repo.display()
        )));
    }
    let lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    if lines.is_empty() {
        return Err(GeneratorError::usage(format!(
            "revision {rev} has no closure inputs in {}",
            repo.display()
        )));
    }
    Ok(canonical_digest(&lines, features, profile))
}

/// Digest identifying the candidate product for `rev`: the debug binary the
/// Rust unit job builds with default features. The unit job (publisher) and
/// the policy job (consumer) both name the candidate artifact through
/// `velnor-workflow closure --rev <sha> --candidate`, so the two can never
/// disagree about which product a revision's candidate is.
pub(crate) fn candidate_closure_of_tree(repo: &Path, rev: &str) -> Result<String, GeneratorError> {
    closure_of_tree(repo, rev, DEV_FEATURES, PROFILE_DEBUG)
}

/// Digest the candidate closure from the bytes and modes currently in the
/// checkout.  This is the local `--check` binding; CI artifacts still use the
/// external manifest contract and the clean Git-tree function above.
pub(crate) fn candidate_closure_of_worktree(
    repo: &Path,
    rev: &str,
) -> Result<String, GeneratorError> {
    worktree_identity::worktree_digest(
        repo,
        rev,
        CLOSURE_PATHS,
        CLOSURE_VERSION,
        DEV_FEATURES,
        PROFILE_DEBUG,
    )
    .map_err(|error| GeneratorError::usage(format!("compute worktree closure: {error}")))
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]

    use super::*;

    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn must_some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("{context}: missing value"),
        }
    }

    fn write_closure_fixture(root: &std::path::Path) {
        must(
            std::fs::create_dir_all(root.join("crates/velnor-workflow/src")),
            "fixture dirs",
        );
        for (name, content) in [
            ("crates/velnor-workflow/src/Zebra.rs", "zebra\n"),
            ("crates/velnor-workflow/src/apple.rs", "apple\n"),
            ("Cargo.toml", "[workspace]\n"),
            ("Cargo.lock", "# lock\n"),
            ("UNRELATED.md", "unrelated\n"),
        ] {
            must(std::fs::write(root.join(name), content), "write");
        }
    }

    fn git_in(root: &std::path::Path, arguments: &[&str]) {
        let status = must(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(arguments)
                .status(),
            "git present",
        );
        assert!(status.success());
    }

    fn fixture_lines() -> Vec<String> {
        vec![
            "100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tcrates/velnor-workflow/src/lib.rs"
                .to_owned(),
            "100644 blob bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\tCargo.toml".to_owned(),
            "100644 blob cccccccccccccccccccccccccccccccccccccccc\tCargo.lock".to_owned(),
        ]
    }

    #[test]
    fn canonical_digest_is_order_independent() {
        let mut shuffled = fixture_lines();
        shuffled.reverse();
        assert_eq!(
            canonical_digest(&fixture_lines(), "", PROFILE_RELEASE),
            canonical_digest(&shuffled, "", PROFILE_RELEASE)
        );
    }

    #[test]
    fn canonical_digest_separates_features_profile_and_content() {
        let base = canonical_digest(&fixture_lines(), "", PROFILE_RELEASE);
        assert!(is_full_closure(&base));
        assert_ne!(
            base,
            canonical_digest(&fixture_lines(), "tui", PROFILE_RELEASE)
        );
        assert_ne!(base, canonical_digest(&fixture_lines(), "", PROFILE_DEBUG));
        let mut changed = fixture_lines();
        changed[0] = changed[0].replace('a', "d");
        assert_ne!(base, canonical_digest(&changed, "", PROFILE_RELEASE));
    }

    #[test]
    fn dev_features_pin_the_candidate_build() {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let content = must(
            std::fs::read_to_string(&manifest),
            "read crate manifest for feature pin",
        );
        assert!(
            content.contains("default = [\"tui\"]"),
            "DEV_FEATURES names the default feature set the candidate build stamps: {DEV_FEATURES}"
        );
        assert_eq!(DEV_FEATURES, "tui");
        assert_ne!(
            canonical_digest(&fixture_lines(), DEV_FEATURES, PROFILE_DEBUG),
            canonical_digest(&fixture_lines(), CI_FEATURES, PROFILE_DEBUG),
            "the candidate identity never collides with the lean debug product"
        );
    }

    #[test]
    fn stamped_features_match_dev_features() {
        // The candidate gate compares a binary's stamped features closure
        // against `candidate_closure_of_tree`, so a default build must stamp
        // exactly DEV_FEATURES: if `build.rs` spelled the set any other way
        // (for example by keeping cargo's synthetic `default` marker), no
        // default build would ever match its own closure and every candidate
        // publish would fail closed.
        assert_eq!(
            env!("VELNOR_WORKFLOW_FEATURES"),
            DEV_FEATURES,
            "build.rs and the canonical closure form must spell the default feature set identically"
        );
    }

    fn git_output(root: &std::path::Path, arguments: &[&str]) -> String {
        let output = must(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(arguments)
                .output(),
            "git output",
        );
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[test]
    fn candidate_closure_of_merge_matches_head_unless_main_touches_closure_paths() {
        // Pins the candidate fast-path condition: the publisher reuses the
        // merge-tree build iff the merge and head candidate closures agree,
        // which holds iff main's side of the merge avoids `CLOSURE_PATHS`.
        let root =
            std::env::temp_dir().join(format!("velnor-closure-merge-{}", crate::unique_suffix()));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        git_in(&root, &["init", "--quiet", "-b", "main"]);
        git_in(&root, &["config", "user.email", "closure@test"]);
        git_in(&root, &["config", "user.name", "closure"]);
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "--quiet", "--message", "base"]);
        git_in(&root, &["checkout", "--quiet", "-b", "pr"]);
        must(
            std::fs::write(
                root.join("crates/velnor-workflow/src/apple.rs"),
                "apple-changed\n",
            ),
            "change a closure path on the head side",
        );
        git_in(&root, &["commit", "--quiet", "--all", "--message", "head"]);
        let head = git_output(&root, &["rev-parse", "HEAD"]);
        let head_closure = must(candidate_closure_of_tree(&root, &head), "head closure");
        // Main advances outside the closure paths: the merge tree carries
        // the head's closure inputs, so the fast path applies.
        git_in(&root, &["checkout", "--quiet", "main"]);
        must(
            std::fs::write(root.join("UNRELATED.md"), "unrelated-changed\n"),
            "change a non-closure path on main",
        );
        git_in(&root, &["commit", "--quiet", "--all", "--message", "main"]);
        git_in(&root, &["merge", "--quiet", "--no-edit", "pr"]);
        let merge = git_output(&root, &["rev-parse", "HEAD"]);
        assert_eq!(
            must(candidate_closure_of_tree(&root, &merge), "merge closure"),
            head_closure,
            "a merge whose other parent avoids closure paths shares the head closure"
        );
        // Main advances inside the closure paths: the merge tree differs, so
        // the publisher must build the head.
        git_in(
            &root,
            &["checkout", "--quiet", "-b", "main-lock", "main@{1}"],
        );
        must(
            std::fs::write(root.join("Cargo.lock"), "# lock-changed\n"),
            "change a closure path on main",
        );
        git_in(
            &root,
            &["commit", "--quiet", "--all", "--message", "main-lock"],
        );
        git_in(&root, &["merge", "--quiet", "--no-edit", "pr"]);
        let merge = git_output(&root, &["rev-parse", "HEAD"]);
        assert_ne!(
            must(candidate_closure_of_tree(&root, &merge), "merge closure"),
            head_closure,
            "a merge whose other parent touches closure paths diverges from head"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn product_tag_carries_a_digest_locator() {
        let digest = canonical_digest(&fixture_lines(), "", PROFILE_RELEASE);
        assert_eq!(
            product_tag(&digest),
            format!("velnor-workflow-runtime-v1-{}", &digest[..16])
        );
    }

    #[test]
    fn closure_of_tree_agrees_with_git_on_a_fixture_repository() {
        use std::io::Write as _;

        let root =
            std::env::temp_dir().join(format!("velnor-closure-fixture-{}", crate::unique_suffix()));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        git_in(&root, &["init", "--quiet"]);
        git_in(&root, &["add", "-A"]);
        git_in(
            &root,
            &[
                "-c",
                "user.email=closure@test",
                "-c",
                "user.name=closure",
                "commit",
                "--quiet",
                "--message",
                "fixture",
            ],
        );
        let head = String::from_utf8_lossy(
            &must(
                Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(["rev-parse", "HEAD"])
                    .output(),
                "rev-parse",
            )
            .stdout,
        )
        .trim()
        .to_owned();
        // Byte-sort the real `git ls-tree` output exactly like the shell
        // consumer does (`LC_ALL=C sort`) and hash it with the system tool,
        // proving the Rust canonicalizer agrees byte-for-byte.
        let ls_tree = must(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args([
                    "ls-tree",
                    "-r",
                    "HEAD",
                    "--",
                    "crates/velnor-workflow",
                    "Cargo.toml",
                    "Cargo.lock",
                    "rust-toolchain.toml",
                    "rust-toolchain",
                    ".cargo",
                ])
                .output(),
            "ls-tree",
        );
        assert!(ls_tree.status.success());
        let mut child = must(
            Command::new("sh")
                .arg("-c")
                .arg("LC_ALL=C sort")
                .current_dir(&root)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn(),
            "spawn sort",
        );
        must(
            must_some(child.stdin.as_mut(), "sort stdin").write_all(&ls_tree.stdout),
            "write",
        );
        let sorted = must(child.wait_with_output(), "sort").stdout;
        let mut canonical = sorted;
        canonical.extend_from_slice("closure-version:1\nfeatures:\nprofile:release\n".as_bytes());
        let mut digest_child = must(
            Command::new("sh")
                .arg("-c")
                .arg("sha256sum | awk '{print $1}'")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn(),
            "spawn sha256sum",
        );
        must(
            must_some(digest_child.stdin.as_mut(), "digest stdin").write_all(&canonical),
            "write",
        );
        let shell_digest =
            String::from_utf8_lossy(&must(digest_child.wait_with_output(), "digest").stdout)
                .trim()
                .to_owned();
        let rust_digest = must(
            closure_of_tree(&root, &head, "", PROFILE_RELEASE),
            "closure of fixture",
        );
        assert_eq!(rust_digest, shell_digest);
        // An unrelated file never enters the digest: committing one more
        // unrelated file under a new commit keeps no input, and removing it
        // from the listing is covered by construction (the pathspec).
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_script_mirrors_the_closure_spec() {
        // `build.rs` cannot import this module, so its duplicate constants
        // are pinned here: any drift fails the build's own test suite
        // instead of minting products no verifier accepts.
        let build = include_str!("../../build.rs");
        assert!(
            build.contains(&format!("const CLOSURE_VERSION: u8 = {CLOSURE_VERSION};")),
            "build.rs footer version drifted"
        );
        for path in CLOSURE_PATHS {
            assert!(
                build.contains(&format!("\"{path}\"")),
                "build.rs pathspec is missing {path}"
            );
        }
        assert!(
            build.contains("VELNOR_WORKFLOW_CLOSURE_DIGEST"),
            "build.rs must stamp the closure digest"
        );
    }
}

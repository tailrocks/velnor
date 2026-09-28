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

use sha2::{Digest, Sha256};

use super::GeneratorError;
use crate::closure_inputs;
#[cfg(test)]
use crate::closure_inputs::BASE_CLOSURE_PATHS;

/// Closure algorithm version. Bump when the inputs or canonical form change;
/// digests minted under different versions never compare equal because the
/// version is part of the hashed footer.
pub(crate) const CLOSURE_VERSION: u8 = 1;

/// Cargo profile of Stage-0 release products.
pub(crate) const PROFILE_RELEASE: &str = "release";
const LEGACY_DOCS_SYMLINK_REVISION: &str = "9567d50ca2b404e64d818dca845beec747518565";
const LEGACY_DOCS_SYMLINK_BLOB: &str = "47dc3e3d863cfb5727b87d785d09abf9743c0a72";
const LEGACY_DOCS_SYMLINK_PATH: &str = "crates/velnor-workflow/CLAUDE.md";
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
/// The legacy v1 input set. Optional local dependency inputs are selected by
/// `closure_inputs::closure_paths` for the revision being measured.
#[cfg(test)]
pub(crate) const CLOSURE_PATHS: &[&str] = BASE_CLOSURE_PATHS;

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
    let mut lines: Vec<&str> = ls_tree_lines.iter().map(String::as_str).collect();
    lines.sort_unstable();
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(
        format!("closure-version:{CLOSURE_VERSION}\nfeatures:{features}\nprofile:{profile}\n")
            .as_bytes(),
    );
    hex_digest(&bytes)
}

/// Lowercase hex SHA-256 of `bytes`.
fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// Digest of the closure inputs at `rev` in the repository at `repo`.
/// Fails when `rev` is not a commit of that checkout.
pub(crate) fn closure_of_tree(
    repo: &Path,
    rev: &str,
    features: &str,
    profile: &str,
) -> Result<String, GeneratorError> {
    let workflow_manifest = git_show(repo, rev, "crates/velnor-workflow/Cargo.toml")?;
    let workspace_manifest = if git_path_exists(repo, rev, "Cargo.toml")? {
        git_show(repo, rev, "Cargo.toml")?
    } else {
        "[workspace]\n".to_owned()
    };
    let paths = closure_inputs::closure_paths(&workflow_manifest, &workspace_manifest)
        .map_err(GeneratorError::usage)?;
    for dependency_root in paths.iter().skip(closure_inputs::BASE_CLOSURE_PATHS.len()) {
        reject_transitive_path_dependencies(repo, rev, dependency_root, &workspace_manifest)?;
    }
    let mut arguments = vec![
        "ls-tree".to_owned(),
        "-r".to_owned(),
        rev.to_owned(),
        "--".to_owned(),
    ];
    arguments.extend(paths.iter().enumerate().map(|(index, path)| {
        if index < closure_inputs::BASE_CLOSURE_PATHS.len() {
            path.clone()
        } else {
            format!(":(literal){path}")
        }
    }));
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
    if lines
        .iter()
        .any(|line| is_unsafe_closure_symlink(line, rev))
    {
        return Err(GeneratorError::usage(format!(
            "revision {rev} contains a Cargo/runtime symlink in the source closure"
        )));
    }
    if lines.is_empty() {
        return Err(GeneratorError::usage(format!(
            "revision {rev} has no closure inputs in {}",
            repo.display()
        )));
    }
    Ok(canonical_digest(&lines, features, profile))
}

fn is_unsafe_closure_symlink(line: &str, revision: &str) -> bool {
    if !line.starts_with("120000 ") {
        return false;
    }
    let Some((header, path)) = line.split_once('\t') else {
        return true;
    };
    !(revision == LEGACY_DOCS_SYMLINK_REVISION
        && header == format!("120000 blob {LEGACY_DOCS_SYMLINK_BLOB}")
        && path == LEGACY_DOCS_SYMLINK_PATH)
}

fn reject_transitive_path_dependencies(
    repo: &Path,
    rev: &str,
    dependency_root: &str,
    workspace_manifest: &str,
) -> Result<(), GeneratorError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-tree", "-r", "--name-only", rev, "--"])
        .arg(format!(":(literal){dependency_root}"))
        .output()
        .map_err(|error| {
            GeneratorError::usage(format!(
                "list local dependency tree {dependency_root}: {error}"
            ))
        })?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "list local dependency tree {dependency_root} at {rev} failed"
        )));
    }
    let paths = String::from_utf8_lossy(&output.stdout);
    if paths.trim().is_empty() {
        return Err(GeneratorError::usage(format!(
            "local Cargo dependency tree {dependency_root} is missing at {rev}"
        )));
    }
    for manifest_path in paths.lines().filter(|path| path.ends_with("Cargo.toml")) {
        let manifest = git_show(repo, rev, manifest_path)?;
        if closure_inputs::manifest_has_local_dependency(
            &manifest,
            workspace_manifest,
            dependency_root,
            manifest_path,
        )
        .map_err(GeneratorError::usage)?
        {
            return Err(GeneratorError::usage(format!(
                "local dependency tree {dependency_root} contains transitive Cargo path dependency in {manifest_path}"
            )));
        }
    }
    Ok(())
}

fn git_path_exists(repo: &Path, rev: &str, path: &str) -> Result<bool, GeneratorError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-e", &format!("{rev}:{path}")])
        .output()
        .map_err(|error| GeneratorError::usage(format!("check {path} at {rev}: {error}")))?;
    if output.status.success() {
        Ok(true)
    } else if git_commit_exists(repo, rev)? {
        Ok(false)
    } else {
        Err(GeneratorError::usage(format!(
            "revision {rev} is not a commit of {}",
            repo.display()
        )))
    }
}

fn git_commit_exists(repo: &Path, rev: &str) -> Result<bool, GeneratorError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-e", &format!("{rev}^{{commit}}")])
        .output()
        .map_err(|error| GeneratorError::usage(format!("check revision {rev}: {error}")))?;
    Ok(output.status.success())
}

fn git_show(repo: &Path, rev: &str, path: &str) -> Result<String, GeneratorError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &format!("{rev}:{path}")])
        .output()
        .map_err(|error| GeneratorError::usage(format!("read {path} at {rev}: {error}")))?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "revision {rev} has no tracked {path} in {}",
            repo.display()
        )));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| GeneratorError::usage(format!("{path} at {rev} is not UTF-8: {error}")))
}

/// Digest identifying the candidate product for `rev`: the debug binary the
/// Rust unit job builds with default features. The unit job (publisher) and
/// the policy job (consumer) both name the candidate artifact through
/// `velnor-workflow closure --rev <sha> --candidate`, so the two can never
/// disagree about which product a revision's candidate is.
pub(crate) fn candidate_closure_of_tree(repo: &Path, rev: &str) -> Result<String, GeneratorError> {
    closure_of_tree(repo, rev, DEV_FEATURES, PROFILE_DEBUG)
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
        must(
            std::fs::create_dir_all(root.join("crates/velnor-model/src")),
            "fixture model dirs",
        );
        for (name, content) in [
            ("crates/velnor-workflow/src/Zebra.rs", "zebra\n"),
            ("crates/velnor-workflow/src/apple.rs", "apple\n"),
            (
                "crates/velnor-workflow/Cargo.toml",
                "[package]\nname = \"velnor-workflow\"\n[dependencies]\nserde = \"1\"\n",
            ),
            (
                "crates/velnor-model/src/lib.rs",
                "pub fn value() -> u8 { 1 }\n",
            ),
            (
                "crates/velnor-model/Cargo.toml",
                "[package]\nname = \"velnor-model\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/velnor-workflow\", \"crates/velnor-model\"]\n",
            ),
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
    fn closure_allows_only_the_legacy_docs_symlink() {
        let allowed = format!("120000 blob {LEGACY_DOCS_SYMLINK_BLOB}\t{LEGACY_DOCS_SYMLINK_PATH}");
        assert!(!is_unsafe_closure_symlink(
            &allowed,
            LEGACY_DOCS_SYMLINK_REVISION
        ));
        assert!(is_unsafe_closure_symlink(
            &allowed,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(is_unsafe_closure_symlink(
            "120000 blob bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\tcrates/velnor-workflow/OTHER.md",
            LEGACY_DOCS_SYMLINK_REVISION
        ));
        assert!(is_unsafe_closure_symlink(
            "120000 blob cccccccccccccccccccccccccccccccccccccccc\tcrates/velnor-workflow/CLAUDE.md/child",
            LEGACY_DOCS_SYMLINK_REVISION
        ));
        assert!(is_unsafe_closure_symlink(
            "120000 blob cccccccccccccccccccccccccccccccccccccccc\tcrates/velnor-workflow/CLAUDE.md\tother",
            LEGACY_DOCS_SYMLINK_REVISION
        ));
    }

    #[cfg(unix)]
    #[expect(clippy::expect_used, reason = "symlink fixture failures need context")]
    #[test]
    fn closure_rejects_docs_symlinks_outside_the_legacy_pinned_revision() {
        let root = std::env::temp_dir().join(format!(
            "velnor-s2-closure-base-symlink-{}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        std::fs::write(root.join("CLAUDE.md"), "legacy instructions\n").expect("root instructions");
        std::os::unix::fs::symlink(
            "../../CLAUDE.md",
            root.join("crates/velnor-workflow/CLAUDE.md"),
        )
        .expect("known workflow instructions symlink");
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
                "legacy CLAUDE symlink",
            ],
        );
        let rev = git_output(&root, &["rev-parse", "HEAD"]);
        let error = closure_of_tree(&root, &rev, "", PROFILE_RELEASE)
            .expect_err("docs symlink outside legacy pin must fail closed");
        assert!(error.to_string().contains("symlink"), "{error}");
        let _ = std::fs::remove_dir_all(&root);
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
    fn stamped_features_match_compiled_features() {
        // The stamp must describe this compilation's enabled features. Derive
        // the expectation from Cargo's feature cfgs so the no-default-features
        // build correctly expects an empty set instead of the default set.
        let compiled_features = if cfg!(feature = "tui") { "tui" } else { "" };
        assert_eq!(
            env!("VELNOR_WORKFLOW_FEATURES"),
            compiled_features,
            "build.rs must stamp the feature set enabled for this compilation"
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

    fn commit_fixture(root: &std::path::Path, message: &str) {
        git_in(root, &["add", "-A"]);
        git_in(
            root,
            &[
                "-c",
                "user.email=closure@test",
                "-c",
                "user.name=closure",
                "commit",
                "--quiet",
                "--message",
                message,
            ],
        );
    }

    #[test]
    fn closure_of_tree_requires_revision_manifest() {
        let root = std::env::temp_dir().join(format!(
            "velnor-closure-no-manifest-{}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        git_in(&root, &["init", "--quiet"]);
        commit_fixture(&root, "fixture with manifest");
        let original = git_output(&root, &["rev-parse", "HEAD"]);
        must(
            std::fs::remove_file(root.join("crates/velnor-workflow/Cargo.toml")),
            "remove workflow manifest",
        );
        commit_fixture(&root, "fixture without manifest");
        let missing = git_output(&root, &["rev-parse", "HEAD"]);

        let error = must_some(
            closure_of_tree(&root, &missing, "", PROFILE_RELEASE).err(),
            "missing workflow manifest must fail closed",
        );
        assert!(error
            .to_string()
            .contains("crates/velnor-workflow/Cargo.toml"));
        assert!(
            closure_of_tree(&root, "missing-revision", "", PROFILE_RELEASE).is_err(),
            "git show failure for an unreadable revision must fail closed"
        );
        assert!(
            closure_of_tree(&root, &original, "", PROFILE_RELEASE).is_ok(),
            "revision with a manifest remains readable"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn closure_of_tree_keeps_old_no_model_pins_and_tracks_current_model_dependency() {
        let root = std::env::temp_dir().join(format!(
            "velnor-closure-model-dependency-{}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        git_in(&root, &["init", "--quiet"]);
        commit_fixture(&root, "old pin without model dependency");
        let old_rev = git_output(&root, &["rev-parse", "HEAD"]);
        let old_closure = must(
            closure_of_tree(&root, &old_rev, "", PROFILE_RELEASE),
            "old no-model closure",
        );

        must(
            std::fs::write(
                root.join("crates/velnor-model/src/lib.rs"),
                "pub fn value() -> u8 { 2 }\n",
            ),
            "change unreferenced model",
        );
        commit_fixture(&root, "change model outside old closure");
        let old_unchanged_rev = git_output(&root, &["rev-parse", "HEAD"]);
        assert_eq!(
            must(
                closure_of_tree(&root, &old_unchanged_rev, "", PROFILE_RELEASE),
                "old no-model closure after model change"
            ),
            old_closure,
            "manifest without local model dependency keeps the legacy pathset"
        );

        must(
            std::fs::write(
                root.join("crates/velnor-workflow/Cargo.toml"),
                "[package]\nname = \"velnor-workflow\"\n[dependencies]\nvelnor-model = { path = \"../velnor-model\" }\n",
            ),
            "declare current model dependency",
        );
        commit_fixture(&root, "current manifest uses model");
        let current_rev = git_output(&root, &["rev-parse", "HEAD"]);
        assert_ne!(
            must(
                closure_of_tree(&root, &current_rev, "", PROFILE_RELEASE),
                "current model closure"
            ),
            old_closure,
            "current manifest includes the model dependency tree"
        );
        let _ = std::fs::remove_dir_all(&root);
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

    #[cfg(unix)]
    #[expect(
        clippy::expect_used,
        reason = "symlink fixture setup failures must be explicit"
    )]
    #[test]
    fn closure_rejects_symlinks_in_local_path_dependency_trees() {
        let root = std::env::temp_dir().join(format!(
            "velnor-s2-closure-symlink-{}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        std::fs::create_dir_all(root.join("crates/helper/src")).expect("helper directories");
        std::fs::write(
            root.join("crates/helper/Cargo.toml"),
            "[package]\nname = \"helper\"\nversion = \"0.1.0\"\n",
        )
        .expect("helper manifest");
        std::fs::write(root.join("crates/helper/src/lib.rs"), "mod linked;\n")
            .expect("helper source");
        std::os::unix::fs::symlink(
            "../../../UNRELATED.md",
            root.join("crates/helper/src/linked.rs"),
        )
        .expect("dependency source symlink");
        std::fs::write(
            root.join("crates/velnor-workflow/Cargo.toml"),
            "[package]\nname = \"velnor-workflow\"\n[dependencies.helper]\npath = \"../helper\"\n",
        )
        .expect("workflow dependency manifest");
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
                "symlink fixture",
            ],
        );
        let rev = git_output(&root, &["rev-parse", "HEAD"]);
        let error = closure_of_tree(&root, &rev, "", PROFILE_RELEASE)
            .expect_err("dependency symlink cannot be hidden by a blob digest");
        assert!(error.to_string().contains("symlink"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[expect(
        clippy::expect_used,
        reason = "the malformed dependency fixture must name its rejection reason"
    )]
    #[test]
    fn closure_rejects_transitive_path_dependencies_in_optional_tree() {
        let root = std::env::temp_dir().join(format!(
            "velnor-s2-transitive-path-{}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        must(
            std::fs::create_dir_all(root.join("crates/model-helper/src")),
            "nested helper dirs",
        );
        must(
            std::fs::write(
                root.join("crates/model-helper/Cargo.toml"),
                "[package]\nname = \"model-helper\"\nversion = \"0.1.0\"\n",
            ),
            "nested helper manifest",
        );
        must(
            std::fs::write(
                root.join("crates/velnor-model/Cargo.toml"),
                "[package]\nname = \"velnor-model\"\nversion = \"0.1.0\"\n[dependencies.model-helper]\npath = \"../model-helper\"\n",
            ),
            "model manifest with transitive path dependency",
        );
        must(
            std::fs::write(
                root.join("crates/model-helper/src/lib.rs"),
                "pub fn helper() {}\n",
            ),
            "nested helper source",
        );
        must(
            std::fs::write(
                root.join("crates/velnor-workflow/Cargo.toml"),
                "[package]\nname = \"velnor-workflow\"\n[dependencies.velnor-model]\npath = \"../velnor-model\"\n",
            ),
            "workflow manifest with optional model dependency",
        );
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
                "transitive model path dependency",
            ],
        );
        let rev = git_output(&root, &["rev-parse", "HEAD"]);
        let error = closure_of_tree(&root, &rev, "", PROFILE_RELEASE)
            .expect_err("optional model tree rejects transitive path dependencies");
        assert!(
            error
                .to_string()
                .contains("transitive Cargo path dependency"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[expect(
        clippy::expect_used,
        reason = "missing path dependency fixture must state its fail-closed result"
    )]
    #[test]
    fn closure_rejects_missing_local_dependency_tree() {
        let root = std::env::temp_dir().join(format!(
            "velnor-s2-missing-dependency-{}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        must(
            std::fs::write(
                root.join("crates/velnor-workflow/Cargo.toml"),
                "[package]\nname = \"velnor-workflow\"\n[dependencies.helper]\npath = \"../missing-helper\"\n",
            ),
            "workflow manifest with missing local dependency",
        );
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
                "missing local dependency",
            ],
        );
        let rev = git_output(&root, &["rev-parse", "HEAD"]);
        let error = closure_of_tree(&root, &rev, "", PROFILE_RELEASE)
            .expect_err("missing local dependency must fail closed");
        assert!(error.to_string().contains("is missing"), "{error}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dependency_aware_resolver_preserves_old_tree_identity() {
        let root = std::env::temp_dir().join(format!(
            "velnor-closure-bridge-s2-{}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        git_in(&root, &["init", "--quiet", "-b", "main"]);
        git_in(&root, &["config", "user.email", "closure@test"]);
        git_in(&root, &["config", "user.name", "closure"]);
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "--quiet", "--message", "legacy tree"]);
        let old_rev = git_output(&root, &["rev-parse", "HEAD"]);
        let old_digest = must(
            closure_of_tree(&root, &old_rev, "", PROFILE_RELEASE),
            "legacy closure",
        );

        must(
            std::fs::write(
                root.join("crates/velnor-model/src/lib.rs"),
                "pub fn value() -> u8 { 2 }\n",
            ),
            "change unrelated model source",
        );
        git_in(
            &root,
            &["commit", "--quiet", "-a", "--message", "unrelated model"],
        );
        let old_model_rev = git_output(&root, &["rev-parse", "HEAD"]);
        assert_eq!(
            must(
                closure_of_tree(&root, &old_model_rev, "", PROFILE_RELEASE),
                "legacy closure after model edit"
            ),
            old_digest,
            "model source stays outside legacy product identity"
        );

        must(
            std::fs::write(
                root.join("crates/velnor-workflow/Cargo.toml"),
                "[package]\nname = \"velnor-workflow\"\n[dependencies]\nvelnor-model = { path = \"../velnor-model\" }\n",
            ),
            "declare local model dependency",
        );
        git_in(
            &root,
            &["commit", "--quiet", "-a", "--message", "model dependency"],
        );
        let dependent_rev = git_output(&root, &["rev-parse", "HEAD"]);
        let dependent_digest = must(
            closure_of_tree(&root, &dependent_rev, "", PROFILE_RELEASE),
            "dependent closure",
        );
        assert_ne!(dependent_digest, old_digest);
        must(
            std::fs::write(
                root.join("crates/velnor-model/src/lib.rs"),
                "pub fn value() -> u8 { 3 }\n",
            ),
            "change depended-on model source",
        );
        git_in(
            &root,
            &["commit", "--quiet", "-a", "--message", "model source"],
        );
        let changed_model_rev = git_output(&root, &["rev-parse", "HEAD"]);
        assert_ne!(
            must(
                closure_of_tree(&root, &changed_model_rev, "", PROFILE_RELEASE),
                "closure after model source edit"
            ),
            dependent_digest,
            "model source edits invalidate dependent product identity"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_workflow_manifest_fails_closed() {
        let root = std::env::temp_dir().join(format!(
            "velnor-closure-bridge-s2-missing-manifest-{}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        git_in(&root, &["init", "--quiet", "-b", "main"]);
        git_in(&root, &["config", "user.email", "closure@test"]);
        git_in(&root, &["config", "user.name", "closure"]);
        let workflow_manifest = root.join("crates/velnor-workflow/Cargo.toml");
        must(
            std::fs::remove_file(&workflow_manifest),
            "remove workflow manifest",
        );
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "--quiet", "--message", "historic tree"]);
        let revision = git_output(&root, &["rev-parse", "HEAD"]);
        let error = must_some(
            closure_of_tree(&root, &revision, "", PROFILE_RELEASE).err(),
            "missing manifest must fail closed",
        );
        assert!(error
            .to_string()
            .contains("crates/velnor-workflow/Cargo.toml"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_script_mirrors_the_closure_spec() {
        // `build.rs` compiles the same selector module, so the only duplicated
        // value left here is the closure format version.
        let build = include_str!("../../build.rs");
        assert!(
            build.contains(&format!("const CLOSURE_VERSION: u8 = {CLOSURE_VERSION};")),
            "build.rs footer version drifted"
        );
        for path in CLOSURE_PATHS {
            assert!(
                crate::closure_inputs::BASE_CLOSURE_PATHS.contains(path),
                "shared closure path list is missing {path}"
            );
        }
        assert!(build.contains("build_closure_paths"));
        assert!(
            build.contains("VELNOR_WORKFLOW_CLOSURE_DIGEST"),
            "build.rs must stamp the closure digest"
        );
    }
}

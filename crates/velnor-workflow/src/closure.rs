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
    let paths = if git_path_exists(repo, rev, "crates/velnor-workflow/Cargo.toml")? {
        let workflow_manifest = git_show(repo, rev, "crates/velnor-workflow/Cargo.toml")?;
        let workspace_manifest = if git_path_exists(repo, rev, "Cargo.toml")? {
            git_show(repo, rev, "Cargo.toml")?
        } else {
            "[workspace]\n".to_owned()
        };
        closure_inputs::closure_paths(&workflow_manifest, &workspace_manifest)
            .map_err(GeneratorError::usage)?
    } else {
        // Trees from before the workflow crate manifest existed cannot declare
        // its optional model dependency. Retain their published v1 pathset.
        closure_inputs::BASE_CLOSURE_PATHS
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>()
    };
    let workspace_manifest = if git_path_exists(repo, rev, "Cargo.toml")? {
        git_show(repo, rev, "Cargo.toml")?
    } else {
        "[workspace]\n".to_owned()
    };
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
    if lines.iter().any(|line| is_unsafe_closure_symlink(line)) {
        return Err(GeneratorError::usage(format!(
            "revision {rev} contains a symlink in the source closure"
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

fn is_unsafe_closure_symlink(line: &str) -> bool {
    line.starts_with("120000 ")
        && line
            .split_once('\t')
            .is_some_and(|(_, path)| path != "crates/velnor-workflow/CLAUDE.md")
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
        .map_err(|error| GeneratorError::usage(format!("list dependency tree: {error}")))?;
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

    #[cfg(unix)]
    #[expect(
        clippy::expect_used,
        reason = "symlink fixture setup failures must be explicit"
    )]
    #[test]
    fn closure_rejects_symlinks_in_local_path_dependency_trees() {
        let root =
            std::env::temp_dir().join(format!("velnor-closure-symlink-{}", crate::unique_suffix()));
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

    #[cfg(unix)]
    #[expect(clippy::expect_used, reason = "symlink fixture failures need context")]
    #[test]
    fn closure_allows_only_the_known_non_runtime_claude_symlink_in_base_paths() {
        let root = std::env::temp_dir().join(format!(
            "velnor-closure-base-symlink-{}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        std::os::unix::fs::symlink(
            "../../AGENTS.md",
            root.join("crates/velnor-workflow/CLAUDE.md"),
        )
        .expect("known Claude compatibility link");
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
                "known base symlink",
            ],
        );
        let rev = git_output(&root, &["rev-parse", "HEAD"]);
        must(
            closure_of_tree(&root, &rev, "", PROFILE_RELEASE),
            "known Claude symlink remains hash-compatible",
        );
        std::os::unix::fs::symlink(
            "../../../../UNRELATED.md",
            root.join("crates/velnor-workflow/src/linked.rs"),
        )
        .expect("unsafe source symlink");
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
                "unsafe base symlink",
            ],
        );
        let rev = git_output(&root, &["rev-parse", "HEAD"]);
        assert!(closure_of_tree(&root, &rev, "", PROFILE_RELEASE)
            .expect_err("source closure rejects unsafe base symlink")
            .to_string()
            .contains("symlink"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the parity test keeps old/new revision proof adjacent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "the nested manifest fixture needs setup and rejection context"
    )]
    #[test]
    fn old_and_dependency_trees_resolve_without_closure_drift() {
        let root =
            std::env::temp_dir().join(format!("velnor-closure-bridge-{}", crate::unique_suffix()));
        let _ = std::fs::remove_dir_all(&root);
        write_closure_fixture(&root);
        git_in(&root, &["init", "--quiet", "-b", "main"]);
        git_in(&root, &["config", "user.email", "closure@test"]);
        git_in(&root, &["config", "user.name", "closure"]);
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "--quiet", "--message", "legacy tree"]);
        let legacy = git_output(&root, &["rev-parse", "HEAD"]);
        let legacy_digest = must(
            closure_of_tree(&root, &legacy, "", PROFILE_RELEASE),
            "legacy closure",
        );
        let legacy_listing = must(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args([
                    "ls-tree",
                    "-r",
                    &legacy,
                    "--",
                    "crates/velnor-workflow",
                    "Cargo.toml",
                    "Cargo.lock",
                    "rust-toolchain.toml",
                    "rust-toolchain",
                    ".cargo",
                ])
                .output(),
            "legacy pathset oracle",
        );
        let legacy_lines = String::from_utf8_lossy(&legacy_listing.stdout)
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(
            legacy_digest,
            canonical_digest(&legacy_lines, "", PROFILE_RELEASE),
            "a revision without the model dependency retains the published v1 pathset"
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
            &[
                "commit",
                "--quiet",
                "-a",
                "--message",
                "unrelated model change",
            ],
        );
        let legacy_after_model_change = git_output(&root, &["rev-parse", "HEAD"]);
        assert_eq!(
            must(
                closure_of_tree(&root, &legacy_after_model_change, "", PROFILE_RELEASE),
                "legacy closure after unrelated model change"
            ),
            legacy_digest,
            "unreferenced model changes do not invalidate old product identities"
        );

        must(
            std::fs::write(
                root.join("crates/velnor-workflow/Cargo.toml"),
                "[package]\nname = \"velnor-workflow\"\n[dependencies.velnor-model]\npath = \"../velnor-model\"\n",
            ),
            "add local model dependency",
        );
        git_in(
            &root,
            &["commit", "--quiet", "-a", "--message", "model dependency"],
        );
        let dependent = git_output(&root, &["rev-parse", "HEAD"]);
        let dependent_digest = must(
            closure_of_tree(&root, &dependent, "", PROFILE_RELEASE),
            "dependent closure",
        );
        assert_ne!(dependent_digest, legacy_digest);
        must(
            std::fs::write(
                root.join("crates/velnor-model/src/lib.rs"),
                "pub fn value() -> u8 { 3 }\n",
            ),
            "change depended-on model source",
        );
        git_in(
            &root,
            &[
                "commit",
                "--quiet",
                "-a",
                "--message",
                "model source change",
            ],
        );
        let dependent_after_model_change = git_output(&root, &["rev-parse", "HEAD"]);
        assert_ne!(
            must(
                closure_of_tree(&root, &dependent_after_model_change, "", PROFILE_RELEASE),
                "dependent closure after model source change"
            ),
            dependent_digest,
            "model source edits invalidate dependent workflow products"
        );
        std::fs::create_dir_all(root.join("crates/model-helper/src")).expect("nested helper dirs");
        std::fs::write(
            root.join("crates/model-helper/Cargo.toml"),
            "[package]\nname = \"model-helper\"\nversion = \"0.1.0\"\n",
        )
        .expect("nested helper manifest");
        std::fs::write(
            root.join("crates/model-helper/src/lib.rs"),
            "pub fn helper() {}\n",
        )
        .expect("nested helper source");
        std::fs::write(
            root.join("crates/velnor-model/Cargo.toml"),
            "[package]\nname = \"velnor-model\"\nversion = \"0.1.0\"\n[dependencies.model-helper]\npath = \"../model-helper\"\n",
        )
        .expect("model manifest with transitive path dependency");
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
                "transitive path dependency",
            ],
        );
        let transitive = git_output(&root, &["rev-parse", "HEAD"]);
        let error = closure_of_tree(&root, &transitive, "", PROFILE_RELEASE)
            .expect_err("optional dependency tree rejects transitive Cargo paths");
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
            "velnor-closure-missing-dependency-{}",
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
    fn revision_without_workflow_manifest_keeps_the_published_pathset() {
        let root = std::env::temp_dir().join(format!(
            "velnor-closure-legacy-manifest-{}",
            crate::unique_suffix()
        ));
        write_closure_fixture(&root);
        must(
            std::fs::remove_file(root.join("crates/velnor-workflow/Cargo.toml")),
            "remove legacy-missing manifest",
        );
        git_in(&root, &["init", "--quiet", "-b", "main"]);
        git_in(&root, &["config", "user.email", "closure@test"]);
        git_in(&root, &["config", "user.name", "closure"]);
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "--quiet", "--message", "legacy tree"]);
        let revision = git_output(&root, &["rev-parse", "HEAD"]);
        let digest = must(
            closure_of_tree(&root, &revision, "", PROFILE_RELEASE),
            "legacy closure without workflow manifest",
        );
        let output = must(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args([
                    "ls-tree",
                    "-r",
                    &revision,
                    "--",
                    "crates/velnor-workflow",
                    "Cargo.toml",
                    "Cargo.lock",
                    "rust-toolchain.toml",
                    "rust-toolchain",
                    ".cargo",
                ])
                .output(),
            "legacy pathset listing",
        );
        let lines = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(digest, canonical_digest(&lines, "", PROFILE_RELEASE));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Every `[dependencies]`-shaped section of the crate manifest that can
    /// feed the shipped binary. Dev-dependencies are excluded by
    /// construction: they never enter release products, so the dev-only
    /// `velnor-runner` path dependency (contract tests proving emitted
    /// `install_args` pass the runner's own lock gate) needs no closure
    /// coverage.
    fn binary_dependency_sections(manifest: &str) -> Vec<(String, Vec<String>)> {
        let mut sections = Vec::new();
        let mut current: Option<(String, Vec<String>)> = None;
        for line in manifest.lines() {
            let trimmed = line.trim();
            if let Some(header) = trimmed.strip_prefix('[') {
                if let Some(section) = current.take() {
                    sections.push(section);
                }
                let name = header
                    .split(']')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                current = Some((name, Vec::new()));
            } else if let Some((_, body)) = current.as_mut() {
                body.push(trimmed.to_owned());
            }
        }
        if let Some(section) = current.take() {
            sections.push(section);
        }
        sections
            .into_iter()
            .filter(|(name, _)| {
                let segments: Vec<&str> = name.split('.').map(str::trim).collect();
                !segments.contains(&"dev-dependencies")
                    && segments.iter().any(|segment| {
                        *segment == "dependencies" || *segment == "build-dependencies"
                    })
            })
            .collect()
    }

    /// Whether a manifest code line (comments stripped) names a local `path`
    /// dependency. Matches the TOML key only, so a crate whose name merely
    /// contains "path" never trips the guard.
    fn names_local_path(code: &str) -> bool {
        for (index, _) in code.match_indices("path") {
            let boundary = code[..index]
                .chars()
                .next_back()
                .is_none_or(|before| !(before.is_alphanumeric() || before == '_' || before == '-'));
            if boundary && code[index + "path".len()..].trim_start().starts_with('=') {
                return true;
            }
        }
        false
    }

    #[test]
    fn crate_inputs_stay_closure_complete() {
        // A local path dependency outside the closure paths would silently
        // escape the digest: two trees with different dependency bytes would
        // mint the same product tag. Any non-dev `path =` dependency fails
        // here until its tree is folded into `CLOSURE_PATHS` (or the
        // dependency goes away).
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let manifest = must(
            std::fs::read_to_string(manifest_dir.join("Cargo.toml")),
            "read crate manifest for closure completeness",
        );
        for (section, lines) in binary_dependency_sections(&manifest) {
            for line in &lines {
                let code = line.split('#').next().unwrap_or_default();
                assert!(
                    !names_local_path(code),
                    "section [{section}] names a local path dependency that the source closure does not cover: {line}"
                );
            }
        }
        assert!(
            names_local_path("velnor-runner = { path = \"../velnor-runner\" }"),
            "the guard matches a real path dependency"
        );
        for innocent in [
            "xpath = \"1.0\"",
            "some-path = { version = \"1\" }",
            "my_path = { git = \"https://example.invalid/x\" }",
        ] {
            assert!(
                !names_local_path(innocent),
                "the guard ignores a name that merely contains path: {innocent}"
            );
        }
        // Workspace-inherited dependencies resolve in the workspace root:
        // a `path` there would escape the same way.
        let workspace = must(
            manifest_dir
                .ancestors()
                .nth(2)
                .ok_or_else(|| "crate manifest has no workspace root".to_owned()),
            "locate the workspace root",
        );
        let root_manifest = must(
            std::fs::read_to_string(workspace.join("Cargo.toml")),
            "read workspace manifest for closure completeness",
        );
        let mut in_workspace_dependencies = false;
        for line in root_manifest.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_workspace_dependencies = trimmed == "[workspace.dependencies]";
                continue;
            }
            if in_workspace_dependencies {
                let code = trimmed.split('#').next().unwrap_or_default();
                assert!(
                    !names_local_path(code),
                    "workspace dependencies name a local path the source closure does not cover: {line}"
                );
            }
        }
    }

    #[test]
    fn build_script_mirrors_the_closure_spec() {
        // `build.rs` cannot import this module, so its duplicate constants
        // are pinned here: any drift fails the build's own test suite
        // instead of minting products no verifier accepts.
        let build = include_str!("../build.rs");
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
        assert!(
            build.contains("build_closure_paths"),
            "build.rs must use the fail-closed dependency detector"
        );
        assert!(
            build.contains("VELNOR_WORKFLOW_CLOSURE_DIGEST"),
            "build.rs must stamp the closure digest"
        );
        for stamp in ["VELNOR_WORKFLOW_FEATURES", "VELNOR_WORKFLOW_PROFILE"] {
            assert!(
                build.contains(stamp),
                "build.rs must stamp {stamp}: promote recomputes the pin closure under the running binary's own build identity"
            );
        }
    }
}

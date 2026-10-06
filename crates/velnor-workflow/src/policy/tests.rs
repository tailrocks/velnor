#![expect(
    clippy::panic,
    reason = "tests need setup failures to name their root cause"
)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

use super::*;
use crate::{PolicyJobSpec, ProjectConfig, RunnerMode};

const PIN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PIN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const CLOSURE_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CLOSURE_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error}"),
    }
}

fn must_fail<T, E>(result: Result<T, E>, context: &str) -> E {
    match result {
        Ok(_) => panic!("{context}: expected an error"),
        Err(error) => error,
    }
}

fn temporary_directory(name: &str) -> PathBuf {
    let root = env::temp_dir().join(format!(
        "velnor-workflow-policy-{name}-{}",
        crate::unique_suffix()
    ));
    must(fs::create_dir_all(&root), "create test directory");
    root
}

#[cfg(unix)]
fn restore_writable_tree(root: &Path) {
    let metadata = must(fs::symlink_metadata(root), "inspect tree entry");
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.is_dir() {
        for entry in must(fs::read_dir(root), "read tree directory") {
            let entry = must(entry, "read tree entry");
            restore_writable_tree(&entry.path());
        }
        must(
            fs::set_permissions(root, fs::Permissions::from_mode(0o755)),
            "restore directory permissions",
        );
    }
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        must(fs::create_dir_all(parent), "create parent directory");
    }
    must(fs::write(path, content), "write file");
}

fn fake_velnor_workflow(directory: &Path, revision: &str, closure: &str) -> PathBuf {
    let binary = directory.join("velnor-workflow");
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --revision ]; then echo {revision}; exit 0; fi\nif [ \"$1\" = --closure ]; then echo {closure}; exit 0; fi\nexit 2\n"
            ),
        ),
        "write fake velnor-workflow",
    );
    #[cfg(unix)]
    {
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark fake velnor-workflow executable",
        );
    }
    binary
}

fn fake_legacy_velnor_workflow(directory: &Path, revision: &str) -> PathBuf {
    let binary = directory.join("velnor-workflow");
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --revision ]; then echo {revision}; exit 0; fi\nif [ \"$1\" = --closure ]; then echo unknown; exit 0; fi\nexit 2\n"
            ),
        ),
        "write fake legacy velnor-workflow",
    );
    #[cfg(unix)]
    {
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark fake legacy velnor-workflow executable",
        );
    }
    binary
}

fn lookup(
    pinned_binary: Option<PathBuf>,
    search_path: Option<std::ffi::OsString>,
    install_root: PathBuf,
) -> PinnedBinaryLookup {
    PinnedBinaryLookup {
        pinned_binary,
        candidate_binary: None,
        search_path,
        install_root,
        build_forbidden: true,
        candidate_manifest: None,
    }
}

fn lookup_with_manifest(
    candidate_binary: Option<PathBuf>,
    search_path: Option<std::ffi::OsString>,
    install_root: PathBuf,
    candidate_manifest: PathBuf,
) -> PinnedBinaryLookup {
    PinnedBinaryLookup {
        pinned_binary: None,
        candidate_binary,
        search_path,
        install_root,
        build_forbidden: true,
        candidate_manifest: Some(candidate_manifest),
    }
}

fn candidate_lookup(
    candidate_binary: Option<PathBuf>,
    search_path: Option<std::ffi::OsString>,
    install_root: PathBuf,
) -> PinnedBinaryLookup {
    PinnedBinaryLookup {
        pinned_binary: None,
        candidate_binary,
        search_path,
        install_root,
        build_forbidden: true,
        candidate_manifest: None,
    }
}

fn checkout_source(root: &Path) -> PinSource {
    PinSource::Checkout(root.to_path_buf())
}

// ---------------------------------------------------------------------------
// Pinned binary resolution
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn pinned_binary_env_is_used_only_when_it_proves_the_pin() {
    let root = temporary_directory("pinned-env");
    let pinned = fake_velnor_workflow(&root, PIN_A, CLOSURE_A);
    let lookup = lookup(Some(pinned.clone()), None, root.join("install"));
    let expected = [CLOSURE_A.to_owned()];
    assert_eq!(
        must(
            resolve_pinned_binary(PIN_A, Some(&expected), &lookup, &checkout_source(&root)),
            "env binary at the pin"
        ),
        pinned
    );
    let other = [CLOSURE_B.to_owned()];
    let error = must_fail(
        resolve_pinned_binary(PIN_A, Some(&other), &lookup, &checkout_source(&root)),
        "env binary at another closure",
    )
    .to_string();
    assert!(error.contains(VELNOR_WORKFLOW_PINNED_BINARY_ENV), "{error}");
    assert!(
        error.contains(&format!("reports closure {CLOSURE_A}")),
        "an explicit pointer at the wrong closure is refused, not skipped: {error}"
    );
    assert!(error.contains(PIN_A), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn path_binary_is_used_when_its_reported_closure_is_the_pin() {
    let root = temporary_directory("pinned-path");
    let stale = root.join("stale");
    let current = root.join("current");
    must(fs::create_dir_all(&stale), "stale dir");
    must(fs::create_dir_all(&current), "current dir");
    fake_velnor_workflow(&stale, PIN_B, CLOSURE_B);
    let wanted = fake_velnor_workflow(&current, PIN_A, CLOSURE_A);
    let lookup = lookup(
        None,
        env::join_paths([&stale, &current]).ok(),
        root.join("install"),
    );
    let expected = [CLOSURE_A.to_owned()];
    assert_eq!(
        must(
            resolve_pinned_binary(PIN_A, Some(&expected), &lookup, &checkout_source(&root)),
            "PATH search"
        ),
        wanted,
        "the first PATH entry reporting the pin wins; stale entries are skipped"
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn forbidden_build_fails_closed_listing_every_candidate() {
    let root = temporary_directory("pinned-offline");
    let stale_dir = root.join("stale");
    must(fs::create_dir_all(&stale_dir), "stale dir");
    let stale = fake_velnor_workflow(&stale_dir, PIN_B, CLOSURE_B);
    let install_root = root.join("install");
    must(fs::create_dir_all(install_root.join("bin")), "install bin");
    let installed = fake_velnor_workflow(&install_root.join("bin"), PIN_B, CLOSURE_B);
    let lookup = lookup(None, env::join_paths([&stale_dir]).ok(), install_root);
    let expected = [CLOSURE_A.to_owned()];
    let error = must_fail(
        resolve_pinned_binary(PIN_A, Some(&expected), &lookup, &checkout_source(&root)),
        "forbidden build miss",
    )
    .to_string();
    assert!(error.contains("building one is forbidden here"), "{error}");
    assert!(error.contains(PIN_A), "{error}");
    assert!(
        error.contains("--pin-build"),
        "the fail-closed message names the local-development escape hatch: {error}"
    );
    assert!(
        error.contains(&format!("{}: reports closure {CLOSURE_B}", stale.display())),
        "{error}"
    );
    assert!(
        error.contains(&format!(
            "{}: reports closure {CLOSURE_B}",
            installed.display()
        )),
        "a previously installed binary is proven by closure, not by its directory name: {error}"
    );
    assert!(error.contains(VELNOR_WORKFLOW_PINNED_BINARY_ENV), "{error}");
    assert!(!error.contains("cargo install"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn revision_fallback_requires_the_pin_and_a_closure_report() {
    let root = temporary_directory("pinned-fallback");
    for name in ["matching", "wrong", "legacy"] {
        must(fs::create_dir_all(root.join(name)), "fallback dir");
    }
    let matching = fake_velnor_workflow(&root.join("matching"), PIN_A, CLOSURE_A);
    let matching_lookup = lookup(Some(matching.clone()), None, root.join("install"));
    assert_eq!(
        must(
            resolve_pinned_binary(PIN_A, None, &matching_lookup, &checkout_source(&root)),
            "env binary at the pin without history"
        ),
        matching
    );
    let wrong = fake_velnor_workflow(&root.join("wrong"), PIN_B, CLOSURE_B);
    let wrong_lookup = lookup(Some(wrong), None, root.join("install"));
    let error = must_fail(
        resolve_pinned_binary(PIN_A, None, &wrong_lookup, &checkout_source(&root)),
        "env binary at another revision without history",
    )
    .to_string();
    assert!(
        error.contains(&format!("reports revision {PIN_B}")),
        "an explicit pointer at the wrong revision is refused, not skipped: {error}"
    );
    let legacy = fake_legacy_velnor_workflow(&root.join("legacy"), PIN_A);
    let legacy_lookup = lookup(Some(legacy), None, root.join("install"));
    let error = must_fail(
        resolve_pinned_binary(PIN_A, None, &legacy_lookup, &checkout_source(&root)),
        "binary without a closure report",
    )
    .to_string();
    assert!(error.contains("source-closure digest"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_running_binary_is_the_pin_when_built_at_it() {
    let root = temporary_directory("pinned-self");
    let lookup = lookup(None, None, root.join("install"));
    let expected = [SOURCE_CLOSURE.to_owned()];
    let resolved = must(
        resolve_pinned_binary(
            SOURCE_REVISION,
            Some(&expected),
            &lookup,
            &checkout_source(&root),
        ),
        "the running binary at its own closure",
    );
    assert_eq!(resolved, must(env::current_exe(), "current exe"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn pin_source_follows_the_declared_repository() {
    let root = Path::new("/tree");
    assert_eq!(
        pin_source(root, Some(crate::workflow_setup_action_repository())),
        PinSource::Checkout(root.to_path_buf())
    );
    assert_eq!(
        pin_source(root, Some("example/consumer")),
        PinSource::Remote(crate::VELNOR_WORKFLOW_INSTALL_GIT_URL.to_owned())
    );
    assert_eq!(
        pin_source(root, None),
        PinSource::Remote(crate::VELNOR_WORKFLOW_INSTALL_GIT_URL.to_owned())
    );
}

// ---------------------------------------------------------------------------
// Pin ancestry
// ---------------------------------------------------------------------------

fn git_ok(root: &Path, arguments: &[&str]) -> String {
    let output = must(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output(),
        "run git",
    );
    assert!(
        output.status.success(),
        "git {}: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit(root: &Path, message: &str) -> String {
    git_ok(root, &["add", "-A"]);
    git_ok(root, &["commit", "-q", "--allow-empty", "-m", message]);
    git_ok(root, &["rev-parse", "HEAD"])
}

fn generation_config(revision: &str) -> String {
    format!(
        "schema = 1\n\n[generator]\nrepository = \"{}\"\nrevision = \"{revision}\"\n",
        crate::workflow_setup_action_repository()
    )
}

fn ownership_state_with_outputs(outputs: &[&str]) -> String {
    let mut state = String::from(
        "# Generated ownership state; do not edit.\nschema = 2\n[inputs]\nconfig\t0000000000000000\nscan\t0000000000000000\ngenerator\t66\n[outputs]\n",
    );
    for output in outputs {
        state.push_str(output);
        state.push_str("\t0000000000000000\n");
    }
    state
}

#[cfg(unix)]
fn removed_static_row_fixture(name: &str) -> (PathBuf, String, String) {
    let root = temporary_directory(name);
    git_ok(&root, &["init", "-q", "-b", "main"]);
    write(
        &root.join(GENERATION_CONFIG),
        "schema = 1\n\n[workflow]\nfiles = [\"ci-policy.yml\"]\n\n[[static_files]]\nfile = \".github/workflows/ci-static.yml\"\nsource = \".github-gen/ci-static.yml\"\n",
    );
    write(
        &root.join(".github-gen/ci-static.yml"),
        "name: Static workflow\non: workflow_dispatch\njobs: {}\n",
    );
    write(
        &root.join(".github/workflows/ci-static.yml"),
        "name: Former generated workflow\non: workflow_dispatch\njobs: {}\n",
    );
    write(
        &root.join(".github/ci/.github-actions-generator-state"),
        &ownership_state_with_outputs(&[".github/workflows/ci-static.yml"]),
    );
    let base = commit(&root, "base owns static workflow");

    write(
        &root.join(GENERATION_CONFIG),
        "schema = 1\n\n[workflow]\nfiles = [\"ci-policy.yml\"]\n\n[policy]\nexclude_workflows = [\"ci-ſtatic.yml\"]\n",
    );
    write(
        &root.join(".github/ci/.github-actions-generator-state"),
        &ownership_state_with_outputs(&[]),
    );
    let head = commit(&root, "remove static row and add obsolete exclusion");
    (root, base, head)
}

/// History: `validator` (the base branch's pin) → `pin` → `head`. The pin
/// descends from the validator, so the chain advances.
#[test]
fn pin_rules_pass_when_the_declared_pin_descends_from_the_base_validator() {
    let root = temporary_directory("ancestry-forward");
    git_ok(&root, &["init", "-q", "-b", "main"]);
    write(&root.join("README.md"), "one\n");
    let validator = commit(&root, "validator");
    write(&root.join("README.md"), "two\n");
    let pin = commit(&root, "pin");
    write(&root.join(GENERATION_CONFIG), &generation_config(&pin));
    let head = commit(&root, "head");

    let reachable = pin_reachable(&root, &pin, &head, Some(&validator));
    assert!(reachable.passed, "{}", reachable.reason);
    assert!(
        reachable.reason.contains("introduced by this change"),
        "{}",
        reachable.reason
    );
    let monotonic = pin_monotonic(&root, &pin, &validator, &head, Some(&validator));
    assert!(monotonic.passed, "{}", monotonic.reason);
    assert!(
        monotonic
            .reason
            .contains("descends from the base validator"),
        "{}",
        monotonic.reason
    );
    let same = pin_monotonic(&root, &validator, &validator, &head, Some(&validator));
    assert!(same.passed, "{}", same.reason);
    let _ = fs::remove_dir_all(root);
}

/// A pin that is not in the head's history is refused: the tree must be
/// rendered by a commit it contains.
#[test]
fn pin_reachable_fails_for_a_pin_outside_the_head_history() {
    let root = temporary_directory("ancestry-unreachable");
    git_ok(&root, &["init", "-q", "-b", "main"]);
    write(&root.join("README.md"), "one\n");
    let head = commit(&root, "head");
    let missing = pin_reachable(&root, PIN_A, &head, None);
    assert!(!missing.passed);
    assert!(
        missing.reason.contains(&format!(
            "pin {PIN_A} is not a commit in this full-history checkout"
        )),
        "{}",
        missing.reason
    );
    assert!(
        !missing.reason.contains(&format!("head {head}")),
        "{}",
        missing.reason
    );
    assert!(!missing.reason.contains("shallow"), "{}", missing.reason);
    git_ok(&root, &["checkout", "-q", "-b", "side"]);
    write(&root.join("README.md"), "side\n");
    let side = commit(&root, "side");
    let sideways = pin_reachable(&root, &side, &head, None);
    assert!(!sideways.passed);
    assert!(
        sideways.reason.contains("is not an ancestor of head"),
        "{}",
        sideways.reason
    );
    let _ = fs::remove_dir_all(root);
}

/// A shallow checkout (`actions/checkout` at its default `fetch-depth: 1`)
/// holds the head but not the pin it descends from. The rule names the
/// shallow clone as the cause and the full-history checkout as the fix,
/// instead of reporting the pin as foreign to the repository.
#[test]
fn pin_reachable_names_a_shallow_checkout_as_the_cause() {
    let origin = temporary_directory("ancestry-shallow-origin");
    git_ok(&origin, &["init", "-q", "-b", "main"]);
    write(&origin.join("README.md"), "one\n");
    let pin = commit(&origin, "pin");
    write(&origin.join(GENERATION_CONFIG), &generation_config(&pin));
    let head = commit(&origin, "head");

    let shallow = temporary_directory("ancestry-shallow-clone");
    let _ = fs::remove_dir_all(&shallow);
    let origin_url = format!("file://{}", origin.display());
    git_ok(
        &origin,
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &origin_url,
            &shallow.display().to_string(),
        ],
    );
    assert_eq!(git_ok(&shallow, &["rev-parse", "HEAD"]), head);

    let report = pin_reachable(&shallow, &pin, &head, None);
    assert!(!report.passed);
    assert!(
        report.reason.contains(&format!(
            "pin {pin} is not a commit in this shallow checkout"
        )),
        "{}",
        report.reason
    );
    assert!(
        report.reason.contains("fetch-depth: 0"),
        "{}",
        report.reason
    );
    assert!(
        !report.reason.contains(&format!("head {head}")),
        "{}",
        report.reason
    );

    // The same pin from the full clone is simply reachable.
    let full = pin_reachable(&origin, &pin, &head, None);
    assert!(full.passed, "{}", full.reason);
    let _ = fs::remove_dir_all(origin);
    let _ = fs::remove_dir_all(shallow);
}

// ---------------------------------------------------------------------------
// Pin self-fetch (stale-rev)
// ---------------------------------------------------------------------------

/// A pinned renderer stand-in: proves the pin through `--closure` and renders
/// nothing (exit 0), so the comparison passes on an empty tree.
#[cfg(unix)]
fn fake_pin_renderer(directory: &Path, revision: &str, closure: &str) -> PathBuf {
    let binary = directory.join("velnor-workflow");
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --revision ]; then echo {revision}; exit 0; fi\nif [ \"$1\" = --closure ]; then echo {closure}; exit 0; fi\nexit 0\n"
            ),
        ),
        "write fake pinned renderer",
    );
    #[cfg(unix)]
    {
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark fake pinned renderer executable",
        );
    }
    binary
}

/// Two-commit origin, depth-1 clone of the tip, declared pin = parent: the
/// stale-rev shape. Returns the origin, the shallow clone, the pin, the head,
/// and the pin's closures computed from the full history.
fn shallow_pin_fixture(name: &str) -> (PathBuf, PathBuf, String, String, Vec<String>) {
    let origin = temporary_directory(&format!("{name}-origin"));
    git_ok(&origin, &["init", "-q", "-b", "main"]);
    write(&origin.join("Cargo.toml"), "[workspace]\n");
    write(
        &origin.join("crates/velnor-workflow/lib.rs"),
        "pub fn f() {}\n",
    );
    write(
        &origin.join("crates/velnor-workflow/Cargo.toml"),
        "[package]\nname = \"velnor-workflow\"\n[dependencies]\n",
    );
    let pin = commit(&origin, "pin");
    write(&origin.join(GENERATION_CONFIG), &generation_config(&pin));
    let head = commit(&origin, "head");
    let expected = must(expected_closures(&origin, &pin), "pin closures");

    let shallow = temporary_directory(&format!("{name}-shallow"));
    let _ = fs::remove_dir_all(&shallow);
    git_ok(
        &origin,
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &format!("file://{}", origin.display()),
            &shallow.display().to_string(),
        ],
    );
    assert_eq!(git_ok(&shallow, &["rev-parse", "HEAD"]), head);
    (origin, shallow, pin, head, expected)
}

/// The stale-rev shape: a depth-1 unit checkout holds the head but not the
/// declared pin (its parent). The Checkout arm fetches the pin itself, so
/// regeneration compares against the pin instead of failing closed on the
/// missing commit — no lane YAML involved.
#[cfg(unix)]
#[test]
fn checkout_arm_self_fetches_the_pin_in_a_shallow_clone() {
    let (origin, shallow, pin, _, expected) = shallow_pin_fixture("pin-fetch");
    assert!(
        !commit_exists(&shallow, &pin),
        "the pin starts absent from the shallow clone"
    );
    let bin = temporary_directory("pin-fetch-bin");
    let renderer = fake_pin_renderer(&bin, &pin, &expected[0]);
    let lookup = lookup(Some(renderer), None, bin.join("install"));
    match must(
        regenerate_and_compare(
            &shallow,
            &shallow,
            &pin,
            "main",
            &lookup,
            &checkout_source(&shallow),
        ),
        "regenerate after self-fetch",
    ) {
        TreeComparison::Pin => {}
        TreeComparison::Candidate(_) => panic!("the render matches the pin, not a candidate"),
        TreeComparison::Differences(_) => panic!("the empty render matches the empty tree"),
    }
    assert!(
        commit_exists(&shallow, &pin),
        "the self-fetch materialized the pin"
    );
    let _ = fs::remove_dir_all(origin);
    let _ = fs::remove_dir_all(shallow);
    let _ = fs::remove_dir_all(bin);
}

#[cfg(unix)]
#[test]
fn removed_static_row_with_obsolete_exclusion_is_rejected_before_rendering() {
    let (root, base, head) = removed_static_row_fixture("removed-static-exclusion");
    let candidate_config = must(
        fs::read_to_string(root.join(GENERATION_CONFIG)),
        "read candidate config",
    );
    assert!(!candidate_config.contains("[[static_files]]"));
    assert!(candidate_config.contains("exclude_workflows = [\"ci-ſtatic.yml\"]"));
    assert!(root.join(".github/workflows/ci-static.yml").is_file());
    let error = must_fail(
        DeclaredTree::read(&root),
        "reject obsolete exclusion before rendering",
    );
    let message = error.to_string();
    assert!(message.contains("exclude_workflows"), "{message}");
    assert_eq!(git_ok(&root, &["rev-parse", "HEAD"]), head);
    assert_ne!(base, head);
    let _ = fs::remove_dir_all(root);
}

/// A pin the remote cannot provide fails closed naming the pin, the checkout's
/// shallow state, and the remediation — never through the revision fallback.
#[test]
fn pin_self_fetch_fails_loud_when_the_remote_lacks_the_pin() {
    let (origin, shallow, _, _, _) = shallow_pin_fixture("pin-fetch-missing");
    let shallow_error = must_fail(ensure_pin_present(&shallow, PIN_A), "unknown pin, shallow");
    let shallow_message = shallow_error.to_string();
    assert!(
        shallow_message.contains(PIN_A),
        "the failure names the pin: {shallow_message}"
    );
    assert!(
        shallow_message.contains("shallow"),
        "the failure names the shallow state: {shallow_message}"
    );
    assert!(
        shallow_message.contains("fetch-depth: 0"),
        "the failure names the remediation: {shallow_message}"
    );
    let full_error = must_fail(ensure_pin_present(&origin, PIN_A), "unknown pin, full");
    let full_message = full_error.to_string();
    assert!(
        full_message.contains(PIN_A),
        "the failure names the pin: {full_message}"
    );
    assert!(
        full_message.contains("full-history"),
        "the failure names the full history: {full_message}"
    );
    assert!(
        full_message.contains("re-pin"),
        "the failure names the remediation: {full_message}"
    );
    let _ = fs::remove_dir_all(origin);
    let _ = fs::remove_dir_all(shallow);
}

/// The base branch advanced its validator after this branch forked. A branch
/// that left the pin exactly as the merge base declared it passes (the merge
/// keeps the base's pin); a branch that re-pinned to something older fails.
#[test]
fn pin_monotonic_admits_an_unchanged_inherited_pin_and_refuses_a_regression() {
    let root = temporary_directory("ancestry-inherited");
    git_ok(&root, &["init", "-q", "-b", "main"]);
    write(&root.join("README.md"), "one\n");
    let old_pin = commit(&root, "old generator");
    write(&root.join(GENERATION_CONFIG), &generation_config(&old_pin));
    let fork_point = commit(&root, "old pin bump");
    // The base branch moves on: a new validator and its pin bump.
    write(&root.join("README.md"), "two\n");
    let new_validator = commit(&root, "new generator");
    write(
        &root.join(GENERATION_CONFIG),
        &generation_config(&new_validator),
    );
    let base = commit(&root, "new pin bump");
    // A feature branch forked before that, pin untouched.
    git_ok(&root, &["checkout", "-q", "-b", "feature", &fork_point]);
    write(&root.join("feature.txt"), "work\n");
    let feature_head = commit(&root, "feature work");
    let inherited = pin_monotonic(&root, &old_pin, &new_validator, &feature_head, Some(&base));
    assert!(inherited.passed, "{}", inherited.reason);
    assert!(
        inherited.reason.contains("unchanged since the merge base"),
        "{}",
        inherited.reason
    );
    // Without a base commit the inheritance cannot be proven.
    let unknown = pin_monotonic(&root, &old_pin, &new_validator, &feature_head, None);
    assert!(!unknown.passed, "{}", unknown.reason);
    // A branch that deliberately re-pins to an older commit regresses.
    git_ok(&root, &["checkout", "-q", "-b", "regress", &base]);
    write(&root.join(GENERATION_CONFIG), &generation_config(&old_pin));
    let regress_head = commit(&root, "downgrade pin");
    let regression = pin_monotonic(&root, &old_pin, &new_validator, &regress_head, Some(&base));
    assert!(!regression.passed, "{}", regression.reason);
    assert!(
        regression
            .reason
            .contains("does not descend from the base validator"),
        "{}",
        regression.reason
    );
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// The entrypoint
// ---------------------------------------------------------------------------

fn hosted_entrypoint(revision: &str) -> String {
    let fixture = temporary_directory("entrypoint-fixture");
    write(
        &fixture.join("Cargo.toml"),
        "[package]\nname = \"example\"\nversion = \"0.1.0\"\n",
    );
    write(
        &fixture.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.91.1\"\n",
    );
    let shape = must(
        crate::scan::scan_shape(&fixture, RunnerMode::Both, "main", &[]),
        "scan entrypoint fixture",
    );
    let _ = fs::remove_dir_all(fixture);
    let mut config = ProjectConfig::from(shape);
    revision.clone_into(&mut config.workflow_revision);
    crate::render_policy_entrypoint(&config)
}

fn entrypoint_tree(name: &str, entrypoint: &str) -> PathBuf {
    let root = temporary_directory(name);
    write(&root.join(POLICY_ENTRYPOINT), entrypoint);
    root
}

/// The generated entrypoint holds exactly the privileges the trust argument
/// in `ci-policy.yml` states: workflow `contents: read`; the policy job may
/// use `contents: read` or the exact final set `actions: read`, `contents: read`,
/// and `pull-requests: read`; no secrets,
/// no persisted credentials, no deployment environment, one hosted job, and
/// only the reviewed triggers.
#[test]
fn generated_entrypoint_satisfies_the_privilege_and_trigger_invariants() {
    let entrypoint = hosted_entrypoint(PIN_A);
    let root = entrypoint_tree("entrypoint-clean", &entrypoint);
    let audit = must(
        audit_policy_entrypoint(&root, &VelnorPolicyContract::default()),
        "audit generated entrypoint",
    );
    assert!(audit.trigger.is_empty(), "{:?}", audit.trigger);
    assert!(audit.privileges.is_empty(), "{:?}", audit.privileges);
    assert!(
        entrypoint.contains("with no secret references"),
        "the trust invariant states the absence honestly: {entrypoint}"
    );
    assert!(!entrypoint.contains("secrets."), "{entrypoint}");
    assert_eq!(
        entrypoint.matches("permissions:\n").count(),
        2,
        "workflow and job level: {entrypoint}"
    );
    assert_eq!(entrypoint.matches("contents: read\n").count(), 2);
    assert_eq!(entrypoint.matches("${{ github.token }}").count(), 1);
    assert!(entrypoint.contains("GH_TOKEN: ${{ github.token }}"));
    assert!(entrypoint.contains("  workflow_dispatch:\n"));
    assert!(entrypoint.contains("# Trust invariant:"), "{entrypoint}");
    let pin = entrypoint_pin(&root, PIN_A);
    assert!(pin.passed, "{}", pin.reason);
    let drift = entrypoint_pin(&root, PIN_B);
    assert!(!drift.passed);
    assert!(
        drift
            .details
            .iter()
            .any(|detail| detail.contains(BASE_REVISION_ENV) && detail.contains(PIN_A)),
        "{:?}",
        drift.details
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn entrypoint_audit_accepts_legacy_or_final_read_permissions_on_policy_job() {
    let clean = hosted_entrypoint(PIN_A);
    let legacy = clean.replacen(
        "    permissions:\n      actions: read\n      contents: read\n      pull-requests: read\n",
        "    permissions:\n      contents: read\n",
        1,
    );
    let legacy_root = entrypoint_tree("entrypoint-legacy-contents-only", &legacy);
    let legacy_audit = must(
        audit_policy_entrypoint(&legacy_root, &VelnorPolicyContract::default()),
        "audit legacy entrypoint",
    );
    assert!(
        legacy_audit.trigger.is_empty(),
        "{:?}",
        legacy_audit.trigger
    );
    assert!(
        legacy_audit.privileges.is_empty(),
        "{:?}",
        legacy_audit.privileges
    );
    let _ = fs::remove_dir_all(legacy_root);

    let root = entrypoint_tree("entrypoint-final-read", &clean);
    let audit = must(
        audit_policy_entrypoint(&root, &VelnorPolicyContract::default()),
        "audit final-read entrypoint",
    );
    assert!(audit.trigger.is_empty(), "{:?}", audit.trigger);
    assert!(audit.privileges.is_empty(), "{:?}", audit.privileges);
    assert!(clean.contains("actions: read\n      contents: read\n      pull-requests: read"));
    let _ = fs::remove_dir_all(root);

    let with_two_permissions = clean.replacen(
        "    permissions:\n      actions: read\n      contents: read\n      pull-requests: read\n",
        "    permissions:\n      actions: read\n      contents: read\n",
        1,
    );
    let root = entrypoint_tree(
        "entrypoint-actions-and-contents-only",
        &with_two_permissions,
    );
    let audit = must(
        audit_policy_entrypoint(&root, &VelnorPolicyContract::default()),
        "audit incomplete final permissions entrypoint",
    );
    assert!(
        audit
            .privileges
            .iter()
            .any(|finding| finding.contains("pull-requests: read")),
        "two-key permissions must remain rejected: {:?}",
        audit.privileges
    );
    let _ = fs::remove_dir_all(root);
}

/// The owner policy job acquires products (no `--rev <sha>` install) and its
/// candidate step passes shell variables to `closure --rev=`: those variable
/// references are not pin literals, so the pin rule still passes on the
/// exported revision and still names it on drift.
#[test]
fn owner_entrypoint_pin_ignores_variable_references() {
    let job = crate::policy_job(&PolicyJobSpec {
        candidate_artifact_wiring: true,
        name: "Policy",
        revision: PIN_A,
        runner: "ubuntu-24.04",
        repository: crate::workflow_setup_action_repository(),
        cache_backend: "github",
        trusted_gate: None,
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,DCO,Policy",
    });
    assert!(job.contains("--rev=\"$pin\""), "{job}");
    assert!(
        !job.contains("--rev "),
        "rendered templates never use the space form a pre-product base validator scans for: {job}"
    );
    assert!(
        job.contains(&format!(
            "--candidate-manifest \"${{{VELNOR_WORKFLOW_CANDIDATE_MANIFEST_ENV}:-}}\""
        )),
        "the Enforce step binds the candidate manifest through the unset-safe fallback: {job}"
    );
    let root = entrypoint_tree("entrypoint-owner-pin", &job);
    let pin = entrypoint_pin(&root, PIN_A);
    assert!(pin.passed, "{:?}", pin.details);
    let drift = entrypoint_pin(&root, PIN_B);
    assert!(!drift.passed);
    assert!(
        drift
            .details
            .iter()
            .any(|detail| detail.contains(BASE_REVISION_ENV) && detail.contains(PIN_A)),
        "{:?}",
        drift.details
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn owner_policy_resolves_squash_merge_push_to_pr_head() {
    let job = crate::policy_job(&PolicyJobSpec {
        candidate_artifact_wiring: true,
        name: "Policy",
        revision: PIN_A,
        runner: "ubuntu-24.04",
        repository: crate::workflow_setup_action_repository(),
        cache_backend: "github",
        trusted_gate: None,
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,DCO,Policy",
    });
    assert!(
        job.contains("EVENT_NAME: ${{ github.event_name }}")
            && job.contains("MERGE_SHA: ${{ github.sha }}"),
        "candidate lookup distinguishes the push event and its squash commit: {job}"
    );
    let Some(candidate) = job
        .split("      - name: Acquire candidate generator product")
        .nth(1)
        .and_then(|tail| {
            tail.split("      - name: Resolve required status checks")
                .next()
        })
    else {
        panic!("owner policy candidate step")
    };
    assert!(
        candidate.contains("PR_HEAD_SHA: ${{ github.event.pull_request.head.sha }}")
            && candidate.contains(
                "PR_HEAD_REPOSITORY: ${{ github.event.pull_request.head.repo.full_name }}"
            )
            && !candidate.contains("github.event.pull_request.head.sha || github.sha"),
        "pull request lookup has no workflow_dispatch fallback: {job}"
    );
    assert!(
        job.contains("actions: read\n      contents: read\n      pull-requests: read"),
        "candidate API access is limited to the exact read-only Actions, contents, and pull-requests permissions: {job}"
    );
    assert!(
        job.contains("repos/$GITHUB_REPOSITORY/commits/$MERGE_SHA/pulls"),
        "a main push resolves its associated pull request through the commit API: {job}"
    );
    for clause in [
        ".merge_commit_sha == $merge_sha",
        ".merged_at != null",
        ".base.ref == $default_branch",
        ".base.repo.full_name == $repository",
        ".head.repo.full_name == $repository",
    ] {
        assert!(
            job.contains(clause),
            "squash merge resolution requires {clause}: {job}"
        );
    }
    assert!(
        job.contains("CANDIDATE_SHA=\"$(jq -er '.[0].head_sha' <<<\"$merged_pulls\")\""),
        "candidate lookup switches to the merged PR head SHA: {job}"
    );
    assert!(
        job.contains("HEAD_REPOSITORY=\"$(jq -er '.[0].head_repository' <<<\"$merged_pulls\")\""),
        "candidate lookup carries the merged PR head repository: {job}"
    );
    assert!(
        job.contains(".revision == $revision"),
        "the candidate manifest revision must match the resolved PR head: {job}"
    );
}

#[test]
fn owner_policy_fails_closed_for_direct_push_without_merged_pr() {
    let job = crate::policy_job(&PolicyJobSpec {
        candidate_artifact_wiring: true,
        name: "Policy",
        revision: PIN_A,
        runner: "ubuntu-24.04",
        repository: crate::workflow_setup_action_repository(),
        cache_backend: "github",
        trusted_gate: None,
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,DCO,Policy",
    });
    assert!(
        job.contains(
            "push $MERGE_SHA is not associated with exactly one merged same-repository pull request"
        ),
        "a direct push has no trusted candidate source: {job}"
    );
    assert!(
        job.contains("direct pushes and ambiguous squash merges fail closed")
            && job.contains("exit 1"),
        "direct push resolution exits before artifact polling: {job}"
    );
    assert!(
        job.contains(
            "candidate lookup requires pull_request_target or a squash-merge push, got $EVENT_NAME"
        ),
        "unsupported manual events also fail closed: {job}"
    );
}

#[test]
fn owner_policy_rejects_merge_sha_wrong_head_repository_and_revision() {
    let job = crate::policy_job(&PolicyJobSpec {
        candidate_artifact_wiring: true,
        name: "Policy",
        revision: PIN_A,
        runner: "ubuntu-24.04",
        repository: crate::workflow_setup_action_repository(),
        cache_backend: "github",
        trusted_gate: None,
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,DCO,Policy",
    });
    assert!(
        job.contains("actions/workflows/ci-pr.yml/runs?head_sha=$CANDIDATE_SHA"),
        "candidate polling is keyed by the resolved PR head: {job}"
    );
    assert!(
        !job.contains("actions/workflows/ci-pr.yml/runs?head_sha=$MERGE_SHA"),
        "a squash merge SHA is never used as the candidate head: {job}"
    );
    assert!(
        job.contains("HEAD_REPOSITORY=\"$(jq -er '.[0].head_repository' <<<\"$merged_pulls\")\"")
            && job.contains("$HEAD_REPOSITORY\" == \"$GITHUB_REPOSITORY\""),
        "a wrong head repository fails closed: {job}"
    );
    assert!(
        job.contains(".repository == $repo")
            && job.contains(".revision == $revision")
            && job.contains(".build_revision | test"),
        "wrong repository, resolved head revision, and build revision fail closed: {job}"
    );
}

#[test]
fn entrypoint_audit_names_each_escalation() {
    let clean = hosted_entrypoint(PIN_A);
    let cases: [(&str, &str, &str, &str); 6] = [
        (
            "write",
            "permissions:\n  contents: read\n\njobs:",
            "permissions:\n  contents: write\n\njobs:",
            "workflow permissions must be exactly `contents: read`",
        ),
        (
            "job-permissions",
            "    permissions:\n      actions: read\n      contents: read\n      pull-requests: read\n",
            "    permissions:\n      actions: read\n      contents: read\n      pull-requests: read\n      id-token: write\n",
            "permissions must be exactly `contents: read` or `actions: read, contents: read, pull-requests: read`",
        ),
        (
            "secret",
            "GH_TOKEN: ${{ github.token }}",
            "GH_TOKEN: ${{ secrets.ADMIN_TOKEN }}",
            "must not reference the GitHub `secrets` context",
        ),
        (
            "credentials",
            "persist-credentials: false",
            "persist-credentials: true",
            "checkout must set `persist-credentials: false`",
        ),
        (
            "trigger",
            "  workflow_dispatch:\n",
            "  workflow_dispatch:\n  push:\n",
            "trigger `push` is not admitted",
        ),
        (
            "types",
            "types: [opened, synchronize, reopened]",
            "types: [opened, synchronize, reopened, labeled]",
            "pull_request_target types must be",
        ),
    ];
    for (name, from, to, expected) in cases {
        assert!(clean.contains(from), "{name}: fixture lacks {from:?}");
        let mutated = clean.replacen(from, to, 1);
        let root = entrypoint_tree(&format!("entrypoint-{name}"), &mutated);
        let audit = must(
            audit_policy_entrypoint(&root, &VelnorPolicyContract::default()),
            "audit mutated entrypoint",
        );
        let findings = [audit.trigger, audit.privileges].concat();
        assert!(
            findings.iter().any(|finding| finding.contains(expected)),
            "{name}: expected a finding containing {expected:?}, got {findings:?}"
        );
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn entrypoint_audit_rejects_obfuscated_github_token_access() {
    let clean = hosted_entrypoint(PIN_A);
    let cases = [
        (
            "bracket-single",
            "GH_TOKEN: ${{ github.token }}",
            "GH_TOKEN: ${{ github['token'] }}",
        ),
        (
            "bracket-double",
            "GH_TOKEN: ${{ github.token }}",
            "GH_TOKEN: ${{ github[\"token\"] }}",
        ),
        (
            "bracket-indirect",
            "GH_TOKEN: ${{ github.token }}",
            "GH_TOKEN: ${{ github[format('token')] }}",
        ),
        (
            "serialized-context",
            "          WORKFLOW_ROOT: ${{ github.workspace }}/policy-checkout\n",
            "          WORKFLOW_ROOT: ${{ github.workspace }}/policy-checkout\n          LEAK: ${{ toJSON(github) }}\n",
        ),
        (
            "indirect-context",
            "          WORKFLOW_ROOT: ${{ github.workspace }}/policy-checkout\n",
            "          WORKFLOW_ROOT: ${{ github.workspace }}/policy-checkout\n          LEAK: ${{ format('{0}', github) }}\n",
        ),
    ];
    for (name, from, to) in cases {
        assert!(clean.contains(from), "{name}: fixture lacks {from:?}");
        let mutated = clean.replacen(from, to, 1);
        let root = entrypoint_tree(&format!("entrypoint-token-{name}"), &mutated);
        let audit = must(
            audit_policy_entrypoint(&root, &VelnorPolicyContract::default()),
            "audit obfuscated token entrypoint",
        );
        assert!(
            audit.privileges.iter().any(|finding| {
                finding.contains("`github.token`") && finding.contains("only be bound as GH_TOKEN")
            }),
            "{name}: token access bypass must fail closed: {:?}",
            audit.privileges
        );
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn entrypoint_audit_rejects_every_secrets_context_shape() {
    let clean = hosted_entrypoint(PIN_A);
    let cases = [
        ("bracket", "${{ secrets['TOKEN'] }}"),
        ("whitespace", "${{ secrets  .  TOKEN }}"),
        ("case", "${{ SeCrEtS.TOKEN }}"),
        ("serialized", "${{ toJSON(secrets) }}"),
        ("indirect", "${{ format('{0}', secrets) }}"),
    ];
    for (name, expression) in cases {
        let marker = "          WORKFLOW_ROOT: ${{ github.workspace }}/policy-checkout\n";
        assert!(
            clean.contains(marker),
            "{name}: fixture lacks workflow marker"
        );
        let mutated = clean.replacen(
            marker,
            &format!("{marker}          LEAK: {expression}\n"),
            1,
        );
        let root = entrypoint_tree(&format!("entrypoint-secrets-{name}"), &mutated);
        let audit = must(
            audit_policy_entrypoint(&root, &VelnorPolicyContract::default()),
            "audit secrets-context entrypoint",
        );
        assert!(
            audit
                .privileges
                .iter()
                .any(|finding| finding.contains("must not reference the GitHub `secrets` context")),
            "{name}: secrets context bypass must fail closed: {:?}",
            audit.privileges
        );
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn entrypoint_audit_requires_unique_canonical_api_token_steps() {
    let clean = hosted_entrypoint(PIN_A);
    let marker = "      - name: Resolve required status checks\n";
    assert!(clean.contains(marker), "fixture lacks ruleset API step");
    let cases = [
        (
            "duplicate",
            format!("{marker}{marker}"),
            "must appear exactly once",
        ),
        (
            "renamed",
            "      - name: Resolve required status checks (copy)\n".to_owned(),
            "canonical Acquire/Ruleset API steps",
        ),
        (
            "bad-body",
            "      - name: Resolve required status checks\n".to_owned(),
            "canonical full structure and body",
        ),
    ];
    for (name, replacement, expected) in cases {
        let mut mutated = clean.replacen(marker, &replacement, 1);
        if name == "bad-body" {
            let body = "          echo \"RULESET_CONTEXTS=$contexts\" >> \"$GITHUB_ENV\"\n";
            assert!(mutated.contains(body), "fixture lacks canonical API body");
            mutated = mutated.replacen(
                body,
                "          echo \"$GH_TOKEN\" >&2\n          echo \"RULESET_CONTEXTS=$contexts\" >> \"$GITHUB_ENV\"\n",
                1,
            );
        }
        let root = entrypoint_tree(&format!("entrypoint-api-step-{name}"), &mutated);
        let audit = must(
            audit_policy_entrypoint(&root, &VelnorPolicyContract::default()),
            "audit API-step entrypoint",
        );
        assert!(
            audit
                .privileges
                .iter()
                .any(|finding| finding.contains(expected)),
            "{name}: canonical token-step bypass must fail closed: {:?}",
            audit.privileges
        );
        let _ = fs::remove_dir_all(root);
    }
}

/// A Velnor-mode entrypoint runs on the approved self-hosted runner behind the
/// trusted-event gate and never builds pull-request code.
#[test]
fn velnor_entrypoint_is_gated_and_never_builds_the_pin() {
    let job = crate::policy_job(&PolicyJobSpec {
        candidate_artifact_wiring: true,
        name: "Policy",
        revision: PIN_A,
        runner: "[self-hosted, velnor]",
        repository: crate::regen_repository_marker(),
        cache_backend: "local",
        trusted_gate: Some(&crate::control_plane_trusted_gate("main")),
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,DCO,Policy",
    });
    assert!(!job.contains("--pin-build"), "{job}");
    assert!(job.contains("    if: ${{ github.event_name == 'pull_request_target' ||"));
    assert!(!job.contains("--ruleset-contexts"), "{job}");
}

// ---------------------------------------------------------------------------
// Semantic rules over a synthetic tree
// ---------------------------------------------------------------------------

/// The approved Velnor labels as a TOML array literal.
fn approved_labels_toml() -> String {
    crate::estate::approved_velnor_runner_labels()
        .iter()
        .map(|label| format!("\"{label}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn velnor_tree(name: &str, pr_workflow: &str) -> PathBuf {
    let root = temporary_directory(name);
    let labels = approved_labels_toml();
    write(
        &root.join(GENERATION_CONFIG),
        &format!(
            "schema = 1\n\n[generator]\nrepository = \"example/consumer\"\n\n[workflow]\nrunners = \"velnor\"\nautomatic = \"velnor\"\ndefault_branch = \"main\"\nvelnor_labels = [{labels}]\nvelnor_trusted_label = \"{TRUSTED_LABEL}\"\n"
        ),
    );
    write(
        &root.join(RUNTIME_CONFIG),
        &format!(
            "runners = \"velnor\"\n\n[workflow]\ndefault_branch = \"main\"\nvelnor_labels = [{labels}]\n"
        ),
    );
    write(&root.join(PULL_REQUEST_AGGREGATE), pr_workflow);
    write(&root.join(POLICY_ENTRYPOINT), &hosted_entrypoint(PIN_A));
    root
}

/// A trust label of the tree's own choosing; the estate vocabulary is not
/// the point of these tests, the gate is.
const TRUSTED_LABEL: &str = "example-trusted-hosts";

/// A pull-request aggregate with a hosted required job and one Velnor job on
/// the approved labels plus the trust label, gated on the default-branch
/// trusted events.
fn gated_trusted_job() -> String {
    let labels = crate::estate::approved_velnor_runner_labels().join(", ");
    format!(
        "name: CI / PR\non:\n  pull_request:\njobs:\n  ci-required:\n    name: ci-required\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ok\n  velnor-docker:\n    name: Docker\n    if: ${{{{ github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') }}}}\n    runs-on: [{labels}, {TRUSTED_LABEL}]\n    steps:\n      - run: echo trusted\n"
    )
}

fn required_artifact_tree(name: &str, workflow: &str) -> PathBuf {
    required_artifact_tree_with_settings(name, workflow, "github", "github", false)
}

fn required_artifact_tree_with_runner(name: &str, workflow: &str, runner: &str) -> PathBuf {
    let workflow_runners = if runner == "velnor" {
        "velnor"
    } else {
        "github"
    };
    required_artifact_tree_with_settings(name, workflow, runner, workflow_runners, false)
}

fn required_artifact_findings_with_macos_runner(
    name: &str,
    workflow: &str,
    macos_runner: Option<&str>,
) -> Vec<String> {
    let root = required_artifact_tree_with_runner(name, workflow, "macos");
    if let Some(macos_runner) = macos_runner {
        let config_path = root.join(GENERATION_CONFIG);
        let generation = must(
            fs::read_to_string(&config_path),
            "read required-artifact config for custom macOS selector",
        )
        .replace(
            "runners = \"github\"\ndefault_branch = \"main\"",
            &format!(
                "runners = \"github\"\nmacos_runner = \"{macos_runner}\"\ndefault_branch = \"main\""
            ),
        );
        write(&config_path, &generation);
    }
    let findings = must(
        audit_workflows(&root),
        "audit required-artifact macOS workflow",
    )
    .structure;
    let _ = fs::remove_dir_all(root);
    findings
}

fn required_artifact_tree_with_settings(
    name: &str,
    workflow: &str,
    profile_runner: &str,
    workflow_runners: &str,
    lanes_input: bool,
) -> PathBuf {
    let root = temporary_directory(name);
    let lanes_input_arg = if lanes_input {
        "lanes_input = true\n"
    } else {
        ""
    };
    write(
        &root.join(GENERATION_CONFIG),
        &format!(
            "schema = 1\n\n[generator]\nrepository = \"example/consumer\"\n\n[workflow]\nrunners = \"{workflow_runners}\"\ndefault_branch = \"main\"\nvelnor_labels = [\"self-hosted\", \"velnor\"]\n\n[[check_profile]]\nid = \"producer\"\nname = \"Produce evidence\"\nschedule = \"0 4 * * *\"\nrunner = \"{profile_runner}\"\ntasks = [\"check\"]\ntimeout_minutes = 15\nartifacts_required = true\nartifacts = [\"target/evidence.json\"]\n\n[[check_profile]]\nid = \"consumer\"\nname = \"Consume evidence\"\nschedule = \"0 4 * * *\"\nrunner = \"{profile_runner}\"\ntasks = [\"consume\"]\nneeds = [\"producer\"]\n\n[[declare]]\nprimitive = \"scheduled-checks\"\nfile = \"checks.yml\"\n[declare.args]\nprofiles = [\"producer\", \"consumer\"]\n{lanes_input_arg}"
        ),
    );
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    );
    write_required_artifact_mise_tasks(&root, &["check", "consume"]);
    write(
        &root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.91.1\"\n",
    );
    write(&root.join("src/lib.rs"), "// fixture\n");
    write(&root.join(".github/workflows/checks.yml"), workflow);
    root
}

fn write_required_artifact_mise_tasks(root: &Path, tasks: &[&str]) {
    let declarations = tasks
        .iter()
        .map(|task| format!("[tasks.{task}]\nrun = \"true\"\n"))
        .collect::<Vec<_>>()
        .join("\n");
    write(&root.join("mise.toml"), &declarations);
}

fn append_required_artifact_mise_tasks(root: &Path, tasks: &[&str]) {
    let path = root.join("mise.toml");
    let mut declarations = must(
        fs::read_to_string(&path),
        "read required-artifact mise tasks",
    );
    declarations.push('\n');
    for task in tasks {
        declarations.push_str("[tasks.");
        declarations.push_str(task);
        declarations.push_str("]\nrun = \"true\"\n\n");
    }
    write(&path, &declarations);
}

fn render_required_artifact_workflow(root: &Path) -> Result<String, String> {
    render_required_artifact_workflows(root)?
        .into_iter()
        .find(|(path, _)| path == &PathBuf::from(".github/workflows/checks.yml"))
        .map(|(_, content)| content)
        .ok_or_else(|| "protected renderer omitted checks.yml".to_owned())
}

fn render_required_artifact_workflows(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let scanned = super::super::scan_target(root, RunnerMode::Github, "main")
        .map_err(|error| format!("scan required-artifact fixture: {error}"))?;
    let surface = super::super::primitives::generate(
        root,
        &scanned.shape,
        &scanned.config,
        scanned.generation.as_ref(),
    )
    .map_err(|error| format!("render required-artifact fixture: {error}"))?;
    Ok(surface
        .files
        .into_iter()
        .filter(|(path, _)| {
            path.starts_with(".github/workflows")
                && matches!(
                    path.extension().and_then(|value| value.to_str()),
                    Some("yml" | "yaml")
                )
        })
        .collect())
}

fn required_artifact_transitive_tree(name: &str, workflow: &str) -> PathBuf {
    let root = required_artifact_tree(name, workflow);
    append_required_artifact_mise_tasks(&root, &["prep", "build", "tail"]);
    let config_path = root.join(GENERATION_CONFIG);
    let generation = must(
        fs::read_to_string(&config_path),
        "read required-artifact config for transitive dependency fixture",
    )
    .replace(
        "[[check_profile]]\nid = \"producer\"",
        "[[check_profile]]\nid = \"prep\"\nname = \"Prep\"\nschedule = \"0 4 * * *\"\nrunner = \"github\"\ntasks = [\"prep\"]\nartifacts = [\"target/prep.json\"]\ntimeout_minutes = 15\nstatus = \"advisory\"\n\n[[check_profile]]\nid = \"build\"\nname = \"Build\"\nschedule = \"0 4 * * *\"\nrunner = \"github\"\ntasks = [\"build\"]\nneeds = [\"prep\"]\ntimeout_minutes = 15\n\n[[check_profile]]\nid = \"producer\"",
    )
    .replace(
        "tasks = [\"check\"]\ntimeout_minutes = 15\nartifacts_required = true",
        "tasks = [\"check\"]\nneeds = [\"build\"]\ntimeout_minutes = 15\nartifacts_required = true",
    )
    .replace(
        "[[declare]]\nprimitive = \"scheduled-checks\"",
        "[[check_profile]]\nid = \"tail\"\nname = \"Tail\"\nschedule = \"0 4 * * *\"\nrunner = \"github\"\ntasks = [\"tail\"]\nneeds = [\"consumer\"]\ntimeout_minutes = 15\n\n[[declare]]\nprimitive = \"scheduled-checks\"",
    )
    .replace(
        "profiles = [\"producer\", \"consumer\"]",
        "profiles = [\"prep\", \"build\", \"producer\", \"consumer\", \"tail\"]",
    );
    write(&config_path, &generation);
    root
}

fn required_artifact_optional_dependency_tree(name: &str, workflow: &str) -> PathBuf {
    let root = required_artifact_tree(name, workflow);
    append_required_artifact_mise_tasks(&root, &["optional"]);
    let config_path = root.join(GENERATION_CONFIG);
    let generation = must(
        fs::read_to_string(&config_path),
        "read required-artifact config for optional dependency fixture",
    )
    .replace(
        "[[check_profile]]\nid = \"producer\"",
        "[[check_profile]]\nid = \"optional\"\nname = \"Optional check\"\nschedule = \"0 4 * * *\"\nrunner = \"github\"\ntasks = [\"optional\"]\ntimeout_minutes = 15\nstatus = \"advisory\"\n\n[[check_profile]]\nid = \"producer\"",
    )
    .replace(
        "tasks = [\"check\"]\ntimeout_minutes = 15\nartifacts_required = true",
        "tasks = [\"check\"]\nneeds = [\"optional\"]\ntimeout_minutes = 15\nartifacts_required = true",
    )
    .replace(
        "profiles = [\"producer\", \"consumer\"]",
        "profiles = [\"optional\", \"producer\", \"consumer\"]",
    );
    write(&config_path, &generation);
    root
}

fn required_artifact_split_workflow_tree(name: &str) -> PathBuf {
    let root = required_artifact_tree(name, "");
    let config_path = root.join(GENERATION_CONFIG);
    let generation = must(
        fs::read_to_string(&config_path),
        "read config for split required-artifact workflows",
    )
    .replace(
        "[[declare]]\nprimitive = \"scheduled-checks\"\nfile = \"checks.yml\"\n[declare.args]\nprofiles = [\"producer\", \"consumer\"]",
        "[[declare]]\nprimitive = \"scheduled-checks\"\nfile = \"producer-only.yml\"\n[declare.args]\nprofiles = [\"producer\"]\n\n[[declare]]\nprimitive = \"scheduled-checks\"\nfile = \"checks.yml\"\n[declare.args]\nprofiles = [\"consumer\"]",
    )
    .replace(
        "tasks = [\"consume\"]\nneeds = [\"producer\"]",
        "tasks = [\"consume\"]",
    );
    write(&config_path, &generation);
    for (path, content) in must(
        render_required_artifact_workflows(&root),
        "render split required-artifact workflow files",
    ) {
        write(&root.join(path), &content);
    }
    root
}

fn canonical_required_artifact_workflow() -> String {
    let root = required_artifact_tree("required-artifact-rendered-canonical", "");
    let workflow = must(
        render_required_artifact_workflow(&root),
        "render canonical required-artifact workflow with protected S1 renderer",
    );
    let _ = fs::remove_dir_all(root);
    workflow
}

fn canonical_required_artifact_transitive_workflow() -> String {
    let root = required_artifact_transitive_tree("required-artifact-transitive-rendered", "");
    let workflow = must(
        render_required_artifact_workflow(&root),
        "render canonical required-artifact transitive workflow",
    );
    let _ = fs::remove_dir_all(root);
    workflow
}

fn canonical_required_artifact_optional_dependency_workflow() -> String {
    let root =
        required_artifact_optional_dependency_tree("required-artifact-optional-rendered", "");
    let workflow = must(
        render_required_artifact_workflow(&root),
        "render canonical required-artifact optional dependency workflow",
    );
    let _ = fs::remove_dir_all(root);
    workflow
}

fn mutate_required_artifact_workflow(workflow: &str, mutate: impl FnOnce(&mut Value)) -> String {
    let mut value: Value = must(serde_yaml::from_str(workflow), "parse mutation fixture");
    mutate(&mut value);
    must(serde_yaml::to_string(&value), "serialize mutation fixture")
}

fn required_artifact_job_mut<'a>(workflow: &'a mut Value, id: &str) -> &'a mut Mapping {
    workflow
        .as_mapping_mut()
        .and_then(|mapping| mapping.get_mut("jobs"))
        .and_then(Value::as_mapping_mut)
        .and_then(|jobs| jobs.get_mut(id))
        .and_then(Value::as_mapping_mut)
        .unwrap_or_else(|| panic!("required-artifact job `{id}` exists"))
}

fn required_artifact_step_mut<'a>(
    workflow: &'a mut Value,
    job_id: &str,
    step_name: &str,
) -> &'a mut Mapping {
    required_artifact_job_mut(workflow, job_id)
        .get_mut("steps")
        .and_then(Value::as_sequence_mut)
        .and_then(|steps| {
            steps.iter_mut().find_map(|step| {
                let mapping = step.as_mapping_mut()?;
                (mapping.get("name").and_then(Value::as_str) == Some(step_name)).then_some(mapping)
            })
        })
        .unwrap_or_else(|| panic!("required-artifact step `{step_name}` exists in `{job_id}`"))
}

fn required_artifact_lanes_workflow(profile_runner: &str) -> Result<String, String> {
    if !matches!(profile_runner, "github" | "velnor") {
        return Err(format!("unsupported fixture runner `{profile_runner}`"));
    }
    let root = required_artifact_tree_with_settings(
        "required-artifact-rendered-lanes",
        "",
        profile_runner,
        "both",
        true,
    );
    let workflow = render_required_artifact_workflow(&root);
    let _ = fs::remove_dir_all(root);
    workflow
}

fn required_artifact_static_velnor_workflow() -> String {
    let root = required_artifact_tree_with_settings(
        "required-artifact-rendered-velnor",
        "",
        "velnor",
        "velnor",
        false,
    );
    let workflow = must(
        render_required_artifact_workflow(&root),
        "render static Velnor required-artifact workflow",
    );
    let _ = fs::remove_dir_all(root);
    workflow
}

fn required_artifact_mutation_findings(name: &str, workflow: &str) -> Vec<String> {
    required_artifact_findings_with_runner(name, workflow, "github")
}

fn required_artifact_findings_with_runner(name: &str, workflow: &str, runner: &str) -> Vec<String> {
    let root = required_artifact_tree_with_runner(name, workflow, runner);
    let findings = must(audit_workflows(&root), "audit required-artifact workflow").structure;
    let _ = fs::remove_dir_all(root);
    findings
}

fn required_artifact_transitive_findings(name: &str, workflow: &str) -> Vec<String> {
    let root = required_artifact_transitive_tree(name, workflow);
    let findings = must(
        audit_workflows(&root),
        "audit required-artifact transitive workflow",
    )
    .structure;
    let _ = fs::remove_dir_all(root);
    findings
}

fn required_artifact_lanes_findings(name: &str, workflow: &str, runner: &str) -> Vec<String> {
    let root = required_artifact_tree_with_settings(name, workflow, runner, "both", true);
    let audit = must(
        audit_workflows(&root),
        "audit required-artifact lanes workflow",
    );
    let mut findings = audit.structure;
    findings.extend(audit.runners);
    let _ = fs::remove_dir_all(root);
    findings
}

fn required_artifact_runner_profile_contracts(canonical: &str) {
    let findings = required_artifact_mutation_findings("required-artifact-canonical", canonical);
    assert!(
        findings.is_empty(),
        "canonical verifier was rejected: {findings:?}"
    );
    let macos = canonical.replace("ubuntu-24.04", "macos-15");
    assert!(
        required_artifact_findings_with_macos_runner("required-artifact-macos", &macos, None)
            .is_empty(),
        "valid macOS artifact profile was rejected"
    );
    let wrong_macos_producer = mutate_required_artifact_workflow(&macos, |workflow| {
        required_artifact_job_mut(workflow, "producer")
            .insert("runs-on".to_owned(), Value::String("macos-26".to_owned()));
    });
    let findings = required_artifact_findings_with_macos_runner(
        "required-artifact-macos-producer-runner-mutation",
        &wrong_macos_producer,
        None,
    );
    assert!(
        findings.iter().any(|finding| {
            finding
                .contains("producer `producer` runs-on must match its configured profile selector")
        }),
        "accepted altered fixed macOS producer selector: {findings:?}"
    );
    let custom_macos = canonical.replace("ubuntu-24.04", "macos-custom");
    assert!(
        required_artifact_findings_with_macos_runner(
            "required-artifact-custom-macos",
            &custom_macos,
            Some("macos-custom"),
        )
        .is_empty(),
        "valid configured custom macOS selector was rejected"
    );

    let velnor = required_artifact_static_velnor_workflow();
    let root = required_artifact_tree_with_settings(
        "required-artifact-velnor",
        &velnor,
        "velnor",
        "velnor",
        false,
    );
    let audit = must(
        audit_workflows(&root),
        "audit Velnor required-artifact workflow",
    );
    assert!(audit.structure.is_empty(), "{:?}", audit.structure);
    assert!(audit.runners.is_empty(), "{:?}", audit.runners);
    let _ = fs::remove_dir_all(root);
    let wrong_velnor_producer = velnor.replacen(
        "    runs-on: [self-hosted, velnor]\n",
        "    runs-on: [self-hosted, other]\n",
        1,
    );
    let findings = required_artifact_findings_with_runner(
        "required-artifact-velnor-producer-runner-mutation",
        &wrong_velnor_producer,
        "velnor",
    );
    assert!(
        findings.iter().any(|finding| {
            finding
                .contains("producer `producer` runs-on must match its configured profile selector")
        }),
        "accepted altered static Velnor producer selector: {findings:?}"
    );
}

fn required_artifact_lanes_input_contract() {
    for (runner, wrong_selector, replacement_selector, runs_on) in [
        (
            "github",
            "inputs.lanes != 'velnor'",
            "inputs.lanes == 'velnor'",
            "${{ (github.ref == 'refs/heads/main' && github.event_name == 'workflow_dispatch' && inputs.lanes == 'velnor') && fromJSON('[\"self-hosted\",\"velnor\"]') || \"ubuntu-24.04\" }}",
        ),
        (
            "velnor",
            "inputs.lanes == 'github'",
            "inputs.lanes == 'velnor'",
            "${{ (github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') && \"ubuntu-24.04\" || fromJSON('[\"self-hosted\",\"velnor\"]') }}",
        ),
    ] {
        let workflow = must(
            required_artifact_lanes_workflow(runner),
            "render required-artifact lanes workflow",
        );
        let findings =
            required_artifact_lanes_findings("required-artifact-lanes-valid", &workflow, runner);
        assert!(
            findings.is_empty(),
            "valid {runner}-default lanes_input verifier was rejected: {findings:?}"
        );
        let mutated = workflow.replacen(wrong_selector, replacement_selector, 1);
        let findings =
            required_artifact_lanes_findings("required-artifact-lanes-mutation", &mutated, runner);
        assert!(
            findings
                .iter()
                .any(|finding| finding.contains("config-derived admission gate")),
            "accepted lane-selection mutation for {runner}: {findings:?}"
        );

        let mutated_runs_on =
            workflow.replace(runs_on, &runs_on.replace("ubuntu-24.04", "ubuntu-22.04"));
        let findings = required_artifact_lanes_findings(
            "required-artifact-lanes-runs-on-mutation",
            &mutated_runs_on,
            runner,
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.contains("config-derived lane selector")),
            "accepted lane runs-on mutation for {runner}: {findings:?}"
        );

        let labels_mutation = workflow.replace(
            "[\"self-hosted\",\"velnor\"]",
            "[\"self-hosted\",\"unconfigured\"]",
        );
        let findings = required_artifact_lanes_findings(
            "required-artifact-lanes-labels-mutation",
            &labels_mutation,
            runner,
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.contains("config-derived lane selector")),
            "accepted lane labels mutation for {runner}: {findings:?}"
        );
    }

    let grouped_root = required_artifact_tree_with_settings(
        "required-artifact-lanes-runner-group",
        &must(
            required_artifact_lanes_workflow("github"),
            "render grouped GitHub lanes workflow",
        ),
        "github",
        "both",
        true,
    );
    let generation_path = grouped_root.join(GENERATION_CONFIG);
    let generation = must(
        fs::read_to_string(&generation_path),
        "read lanes_input generation config",
    )
    .replace(
        "velnor_labels = [\"self-hosted\", \"velnor\"]\n",
        "velnor_labels = [\"self-hosted\", \"velnor\"]\nvelnor_runner_group = \"fleet\"\n",
    );
    write(&generation_path, &generation);
    let audit = must(
        audit_workflows(&grouped_root),
        "audit lanes_input runner group",
    );
    let mut findings = audit.structure;
    findings.extend(audit.runners);
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("cannot route a configured Velnor runner group")),
        "accepted lanes_input with a runner group: {findings:?}"
    );
    let _ = fs::remove_dir_all(grouped_root);
}

fn assert_required_artifact_mutation_cases<const N: usize>(
    canonical: &str,
    mutations: [(&str, String, &str); N],
) {
    for (name, workflow, expected_finding) in mutations {
        assert_ne!(
            workflow, canonical,
            "mutation fixture did not alter generated S1 YAML: {name}"
        );
        let findings =
            required_artifact_mutation_findings(&format!("required-artifact-{name}"), &workflow);
        assert!(
            findings
                .iter()
                .any(|finding| finding.contains(expected_finding)),
            "accepted hostile mutation or unrelated rejection: {name}: {findings:?}"
        );
    }
}

fn required_artifact_verifier_job_mutations(canonical: &str) {
    assert_required_artifact_mutation_cases(
        canonical,
        [
        (
            "missing-or-renamed-verifier",
            canonical.replace("verify-producer-artifacts:", "renamed-verifier:"),
            "requires verifier job `verify-producer-artifacts`",
        ),
        (
            "extra-checkout",
            canonical.replace(
                "      - name: Clear verifier workspace",
                "      - uses: actions/checkout@v4\n      - name: Clear verifier workspace",
            ),
            "verifier job `verify-producer-artifacts` must match its protected S1 renderer output",
        ),
        (
            "extra-run",
            canonical.replace(
                "      - name: Clear verifier workspace",
                "      - name: Extra verifier script\n        run: echo hostile\n      - name: Clear verifier workspace",
            ),
            "verifier job `verify-producer-artifacts` must match its protected S1 renderer output",
        ),
        (
            "permission-widening",
            canonical.replace("      actions: read\n", "      actions: write\n"),
            "verifier job `verify-producer-artifacts` must match its protected S1 renderer output",
        ),
        (
            "altered-artifact-id",
            canonical.replace(
                "artifact-ids: ${{ needs.producer.outputs.artifact_id }}",
                "artifact-ids: ${{ github.run_id }}",
            ),
            "verifier job `verify-producer-artifacts` must match its protected S1 renderer output",
        ),
        (
            "altered-runner",
            canonical.replace("    runs-on: ubuntu-latest\n", "    runs-on: self-hosted\n"),
            "verifier job `verify-producer-artifacts` must match its protected S1 renderer output",
        ),
        (
            "altered-hosted-guard",
            canonical
                .replace(
                    "VERIFIER_RUNNER_ENVIRONMENT: ${{ runner.environment }}",
                    "VERIFIER_RUNNER_ENVIRONMENT: untrusted",
                )
                .replace("!= \"github-hosted\"", "!= \"self-hosted\""),
            "verifier job `verify-producer-artifacts` must match its protected S1 renderer output",
        ),
        ],
    );
}

fn required_artifact_producer_upload_mutations(canonical: &str) {
    assert_required_artifact_mutation_cases(
        canonical,
        [
        (
            "producer-skips-artifact-upload",
            canonical.replace(
                "  producer:\n    name: \"Produce evidence\"\n",
                "  producer:\n    name: \"Produce evidence\"\n    if: false\n",
            ),
            "producer `producer` if must match its canonical lane admission",
        ),
        (
            "producer-runner-mutation",
            mutate_required_artifact_workflow(canonical, |workflow| {
                required_artifact_job_mut(workflow, "producer").insert(
                    "runs-on".to_owned(),
                    Value::String("ubuntu-22.04".to_owned()),
                );
            }),
            "runs-on must match its configured profile selector",
        ),
        (
            "producer-task-step-continue-on-error",
            canonical.replace(
                "      - name: Run check\n        run: mise run check",
                "      - name: Run check\n        continue-on-error: true\n        run: mise run check",
            ),
            "producer `producer` must preserve configured dependencies and bind artifact_id",
        ),
        (
            "producer-output-redirection",
            canonical.replace(
                "artifact_id: ${{ steps.upload_artifact.outputs.artifact-id }}",
                "artifact_id: ${{ needs.evil.outputs.artifact_id }}",
            ),
            "must preserve configured dependencies and bind artifact_id to its canonical pinned upload step",
        ),
        (
            "producer-continue-on-error",
            canonical.replace(
                "  producer:\n    name: \"Produce evidence\"\n",
                "  producer:\n    name: \"Produce evidence\"\n    continue-on-error: true\n",
            ),
            "must preserve configured dependencies and bind artifact_id to its canonical pinned upload step",
        ),
        (
            "producer-upload-id-mutation",
            canonical.replace("id: upload_artifact", "id: other_upload"),
            "must preserve configured dependencies and bind artifact_id to its canonical pinned upload step",
        ),
        (
            "producer-upload-path-mutation",
            canonical.replace(
                "path: ${{ runner.temp }}/velnor-required-artifacts-${{ github.run_id }}-producer\n          if-no-files-found: error",
                "path: ${{ runner.temp }}/unrelated\n          if-no-files-found: error",
            ),
            "must preserve configured dependencies and bind artifact_id to its canonical pinned upload step",
        ),
        (
            "producer-upload-error-policy-mutation",
            canonical.replace("if-no-files-found: error", "if-no-files-found: warn"),
            "must preserve configured dependencies and bind artifact_id to its canonical pinned upload step",
        ),
        (
            "producer-upload-pin-mutation",
            canonical.replace(
                "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1",
                "actions/upload-artifact@0000000000000000000000000000000000000000",
            ),
            "must preserve configured dependencies and bind artifact_id to its canonical pinned upload step",
        ),
        ],
    );
}

fn required_artifact_consumer_job_mutations(canonical: &str) {
    assert_required_artifact_mutation_cases(
        canonical,
        [
        (
            "consumer-skips-verifier",
            canonical.replace("needs: [verify-producer-artifacts]", "needs: [producer]"),
            "consumer profile `consumer` needs must match its protected-renderer dependencies",
        ),
        (
            "consumer-runner-mutation",
            canonical.replace(
                "  consumer:\n    name: \"Consume evidence\"\n    needs: [verify-producer-artifacts]\n    runs-on: ubuntu-24.04\n",
                "  consumer:\n    name: \"Consume evidence\"\n    needs: [verify-producer-artifacts]\n    runs-on: ubuntu-22.04\n",
            ),
            "consumer profile `consumer` runs-on must match its configured profile selector",
        ),
        (
            "consumer-continue-on-error",
            canonical.replace(
                "  consumer:\n    name: \"Consume evidence\"\n",
                "  consumer:\n    name: \"Consume evidence\"\n    continue-on-error: true\n",
            ),
            "consumer profile `consumer` continue-on-error must match its configured advisory status",
        ),
        (
            "consumer-always-runs",
            canonical.replace(
                "  consumer:\n    name: \"Consume evidence\"\n",
                "  consumer:\n    name: \"Consume evidence\"\n    if: ${{ always() }}\n",
            ),
            "if must preserve verifier success propagation",
        ),
        (
            "consumer-runs-on-failure",
            canonical.replace(
                "  consumer:\n    name: \"Consume evidence\"\n",
                "  consumer:\n    name: \"Consume evidence\"\n    if: ${{ failure() }}\n",
            ),
            "if must preserve verifier success propagation",
        ),
        (
            "consumer-runs-on-cancelled",
            canonical.replace(
                "  consumer:\n    name: \"Consume evidence\"\n",
                "  consumer:\n    name: \"Consume evidence\"\n    if: ${{ cancelled() }}\n",
            ),
            "if must preserve verifier success propagation",
        ),
        ],
    );
}

fn required_artifact_optional_dependency_mutations() {
    let optional_workflow = canonical_required_artifact_optional_dependency_workflow();
    let root = required_artifact_optional_dependency_tree(
        "required-artifact-optional-dependency-canonical",
        &optional_workflow,
    );
    let audit = must(
        audit_workflows(&root),
        "audit generated optional dependency workflow",
    );
    assert!(
        audit.structure.is_empty(),
        "canonical optional dependency was rejected: {:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);

    let skipped_optional = mutate_required_artifact_workflow(&optional_workflow, |workflow| {
        let optional = required_artifact_job_mut(workflow, "optional");
        optional.insert(
            "if".to_owned(),
            Value::String(concat!("$", "{{ false }}").to_owned()),
        );
    });
    let root = required_artifact_optional_dependency_tree(
        "required-artifact-optional-dependency-skipped",
        &skipped_optional,
    );
    let audit = must(
        audit_workflows(&root),
        "audit required producer with skipped generated optional ancestor",
    );
    assert!(
        audit.structure.iter().any(|finding| {
            finding.contains(
                "required-artifact chain profile `optional` job must match the complete protected S1 renderer output",
            )
        }),
        "accepted producer depending on a skipped generated optional job: {:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);
}

fn required_artifact_transitive_canonical_and_prep_skip(transitive_workflow: &str) {
    let root = required_artifact_transitive_tree(
        "required-artifact-transitive-canonical",
        transitive_workflow,
    );
    let audit = must(
        audit_workflows(&root),
        "audit canonical required-artifact transitive dependencies",
    );
    assert!(
        audit.structure.is_empty(),
        "canonical transitive dependencies were rejected: {:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);

    let prep_skipped = mutate_required_artifact_workflow(transitive_workflow, |workflow| {
        let prep = required_artifact_job_mut(workflow, "prep");
        prep.insert(
            "if".to_owned(),
            Value::String(concat!("$", "{{ false }}").to_owned()),
        );
    });
    let root =
        required_artifact_transitive_tree("required-artifact-transitive-prep-skip", &prep_skipped);
    let audit = must(
        audit_workflows(&root),
        "audit required producer with skipped configured prep ancestor",
    );
    assert!(
        audit.structure.iter().any(|finding| {
            finding.contains(
                "required-artifact chain profile `prep` job must match the complete protected S1 renderer output",
            )
        }),
        "accepted required producer with skipped configured prep ancestor: {:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);
}

fn required_artifact_transitive_status_mutations(transitive_workflow: &str) {
    let missing_advisory_continue =
        mutate_required_artifact_workflow(transitive_workflow, |workflow| {
            required_artifact_job_mut(workflow, "prep").remove("continue-on-error");
        });
    let root = required_artifact_transitive_tree(
        "required-artifact-transitive-advisory-continue-missing",
        &missing_advisory_continue,
    );
    let audit = must(
        audit_workflows(&root),
        "audit advisory ancestor missing continue-on-error",
    );
    assert!(
        audit.structure.iter().any(|finding| {
            finding.contains(
                "required-artifact ancestor `prep` continue-on-error must match its configured advisory status",
            )
        }),
        "accepted advisory ancestor without renderer-emitted continue-on-error: {:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);

    let required_ancestor_continue =
        mutate_required_artifact_workflow(transitive_workflow, |workflow| {
            required_artifact_job_mut(workflow, "build")
                .insert("continue-on-error".to_owned(), Value::Bool(true));
        });
    let root = required_artifact_transitive_tree(
        "required-artifact-transitive-required-continue",
        &required_ancestor_continue,
    );
    let audit = must(
        audit_workflows(&root),
        "audit required ancestor with unexpected continue-on-error",
    );
    assert!(
        audit.structure.iter().any(|finding| {
            finding.contains(
                "required-artifact ancestor `build` continue-on-error must match its configured advisory status",
            )
        }),
        "accepted required ancestor with unexpected continue-on-error: {:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);

    let build_step_continue = mutate_required_artifact_workflow(transitive_workflow, |workflow| {
        required_artifact_step_mut(workflow, "build", "Run build")
            .insert("continue-on-error".to_owned(), Value::Bool(true));
    });
    let root = required_artifact_transitive_tree(
        "required-artifact-transitive-step-continue",
        &build_step_continue,
    );
    let audit = must(
        audit_workflows(&root),
        "audit required ancestor task step with continue-on-error",
    );
    assert!(
        audit.structure.iter().any(|finding| {
            finding
                .contains("required-artifact ancestor `build` steps must not set continue-on-error")
        }),
        "accepted required ancestor task step with continue-on-error: {:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);
}

fn required_artifact_transitive_runner_and_ancestor_mutations(transitive_workflow: &str) {
    let wrong_ancestor_runner =
        mutate_required_artifact_workflow(transitive_workflow, |workflow| {
            required_artifact_job_mut(workflow, "prep").insert(
                "runs-on".to_owned(),
                Value::String("ubuntu-22.04".to_owned()),
            );
        });
    let root = required_artifact_transitive_tree(
        "required-artifact-transitive-runner-mutation",
        &wrong_ancestor_runner,
    );
    let audit = must(
        audit_workflows(&root),
        "audit required-artifact ancestor runner selector",
    );
    assert!(
        audit.structure.iter().any(|finding| {
            finding.contains(
                "required-artifact ancestor `prep` runs-on must match its configured profile selector",
            )
        }),
        "accepted altered ancestor runner selector: {:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);

    for (name, mutate) in [
        (
            "ancestor-task-command",
            Box::new(|workflow: &mut Value| {
                required_artifact_step_mut(workflow, "build", "Run build").insert(
                    "run".to_owned(),
                    Value::String("mise run unrelated".to_owned()),
                );
            }) as Box<dyn Fn(&mut Value)>,
        ),
        (
            "ancestor-task-if",
            Box::new(|workflow: &mut Value| {
                required_artifact_step_mut(workflow, "build", "Run build").insert(
                    "if".to_owned(),
                    Value::String(concat!("$", "{{ false }}").to_owned()),
                );
            }),
        ),
        (
            "ancestor-prelude",
            Box::new(|workflow: &mut Value| {
                required_artifact_step_mut(workflow, "build", "Set up Mise").insert(
                    "run".to_owned(),
                    Value::String("echo hostile PATH".to_owned()),
                );
            }),
        ),
        (
            "ancestor-container",
            Box::new(|workflow: &mut Value| {
                required_artifact_job_mut(workflow, "build").insert(
                    "container".to_owned(),
                    Value::String("node:latest".to_owned()),
                );
            }),
        ),
        (
            "ancestor-job-bash-env",
            Box::new(|workflow: &mut Value| {
                let mut env = Mapping::new();
                env.insert(
                    "BASH_ENV".to_owned(),
                    Value::String("/tmp/attacker.sh".to_owned()),
                );
                required_artifact_job_mut(workflow, "build")
                    .insert("env".to_owned(), Value::Mapping(env));
            }),
        ),
    ] {
        let mutated = mutate_required_artifact_workflow(transitive_workflow, |workflow| {
            mutate(workflow);
        });
        let findings =
            required_artifact_transitive_findings(&format!("required-artifact-{name}"), &mutated);
        assert!(
            findings.iter().any(|finding| {
                finding.contains(
                    "required-artifact chain profile `build` job must match the complete protected S1 renderer output",
                )
            }),
            "accepted transitive ancestor {name} mutation: {findings:?}"
        );
    }
}

fn required_artifact_transitive_optional_artifact_tail(transitive_workflow: &str) {
    let prep_optional_tail = mutate_required_artifact_workflow(transitive_workflow, |workflow| {
        let prep = required_artifact_job_mut(workflow, "prep");
        let steps = prep
            .get_mut("steps")
            .and_then(Value::as_sequence_mut)
            .unwrap_or_else(|| panic!("prep steps exist"));
        let before = steps.len();
        steps.retain(|step| {
            step.as_mapping()
                .and_then(|mapping| mapping.get("name"))
                .and_then(Value::as_str)
                != Some("Upload prep artifacts")
        });
        assert_eq!(
            steps.len() + 1,
            before,
            "prep optional artifact tail exists"
        );
    });
    let findings = required_artifact_transitive_findings(
        "required-artifact-transitive-optional-tail",
        &prep_optional_tail,
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains("complete protected S1 renderer output")
                && finding.contains("chain profile")
                && finding.contains("prep")
        }),
        "accepted ancestor optional-artifact tail mutation: {findings:?}"
    );
}

fn required_artifact_producer_task_mutations(canonical: &str) {
    let producer_task_command = mutate_required_artifact_workflow(canonical, |workflow| {
        required_artifact_step_mut(workflow, "producer", "Run check").insert(
            "run".to_owned(),
            Value::String("mise run unrelated".to_owned()),
        );
    });
    let findings = required_artifact_mutation_findings(
        "required-artifact-producer-task-command",
        &producer_task_command,
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains(
                "required-artifact chain profile `producer` job must match the complete protected S1 renderer output",
            )
        }),
        "accepted producer task command mutation: {findings:?}"
    );

    let producer_task_if = mutate_required_artifact_workflow(canonical, |workflow| {
        required_artifact_step_mut(workflow, "producer", "Run check").insert(
            "if".to_owned(),
            Value::String(concat!("$", "{{ false }}").to_owned()),
        );
    });
    let findings = required_artifact_mutation_findings(
        "required-artifact-producer-task-if",
        &producer_task_if,
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains(
                "required-artifact chain profile `producer` job must match the complete protected S1 renderer output",
            )
        }),
        "accepted producer task condition mutation: {findings:?}"
    );

    let producer_extra_step = mutate_required_artifact_workflow(canonical, |workflow| {
        let producer = required_artifact_job_mut(workflow, "producer");
        producer
            .get_mut("steps")
            .and_then(Value::as_sequence_mut)
            .unwrap_or_else(|| panic!("producer steps exist"))
            .insert(2, Value::Mapping(Mapping::new()));
    });
    let findings = required_artifact_mutation_findings(
        "required-artifact-producer-extra-step",
        &producer_extra_step,
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains(
                "required-artifact chain profile `producer` job must match the complete protected S1 renderer output",
            )
        }),
        "accepted producer extra step: {findings:?}"
    );

    let producer_checkout = mutate_required_artifact_workflow(canonical, |workflow| {
        required_artifact_step_mut(workflow, "producer", "Checkout repository").insert(
            "uses".to_owned(),
            Value::String("actions/checkout@v4".to_owned()),
        );
    });
    let findings = required_artifact_mutation_findings(
        "required-artifact-producer-checkout-mutation",
        &producer_checkout,
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains(
                "required-artifact chain profile `producer` job must match the complete protected S1 renderer output",
            )
        }),
        "accepted producer checkout mutation: {findings:?}"
    );
}

fn required_artifact_producer_setup_and_staging_mutations(canonical: &str) {
    let producer_setup = mutate_required_artifact_workflow(canonical, |workflow| {
        required_artifact_step_mut(workflow, "producer", "Set up Mise").insert(
            "run".to_owned(),
            Value::String("echo hostile PATH".to_owned()),
        );
    });
    let findings = required_artifact_mutation_findings(
        "required-artifact-producer-setup-mutation",
        &producer_setup,
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains(
                "required-artifact chain profile `producer` job must match the complete protected S1 renderer output",
            )
        }),
        "accepted producer setup mutation: {findings:?}"
    );

    let producer_preflight = mutate_required_artifact_workflow(canonical, |workflow| {
        required_artifact_step_mut(workflow, "producer", "Verify producer artifacts").insert(
            "run".to_owned(),
            Value::String("set -euo pipefail\ntrue".to_owned()),
        );
    });
    let findings = required_artifact_mutation_findings(
        "required-artifact-producer-preflight-mutation",
        &producer_preflight,
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains("complete protected S1 renderer output")
                && finding.contains("chain profile")
                && finding.contains("producer")
        }),
        "accepted producer artifact preflight mutation: {findings:?}"
    );

    let producer_staging = mutate_required_artifact_workflow(canonical, |workflow| {
        required_artifact_step_mut(workflow, "producer", "Stage producer artifacts").insert(
            "run".to_owned(),
            Value::String("echo partial stage".to_owned()),
        );
    });
    let findings = required_artifact_mutation_findings(
        "required-artifact-producer-staging-mutation",
        &producer_staging,
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains(
                "required-artifact chain profile `producer` job must match the complete protected S1 renderer output",
            )
        }),
        "accepted producer staging mutation: {findings:?}"
    );
}

fn required_artifact_producer_job_environment_mutations(canonical: &str) {
    for (name, mutate) in [
        (
            "container",
            Box::new(|job: &mut Mapping| {
                job.insert(
                    "container".to_owned(),
                    Value::String("node:latest".to_owned()),
                );
            }) as Box<dyn Fn(&mut Mapping)>,
        ),
        (
            "defaults-run",
            Box::new(|job: &mut Mapping| {
                let mut defaults = Mapping::new();
                let mut run = Mapping::new();
                run.insert("shell".to_owned(), Value::String("bash".to_owned()));
                defaults.insert("run".to_owned(), Value::Mapping(run));
                job.insert("defaults".to_owned(), Value::Mapping(defaults));
            }),
        ),
        (
            "job-bash-env",
            Box::new(|job: &mut Mapping| {
                let mut env = Mapping::new();
                env.insert(
                    "BASH_ENV".to_owned(),
                    Value::String("/tmp/attacker.sh".to_owned()),
                );
                job.insert("env".to_owned(), Value::Mapping(env));
            }),
        ),
        (
            "strategy",
            Box::new(|job: &mut Mapping| {
                job.insert("strategy".to_owned(), Value::Mapping(Mapping::new()));
            }),
        ),
    ] {
        let mutated = mutate_required_artifact_workflow(canonical, |workflow| {
            mutate(required_artifact_job_mut(workflow, "producer"));
        });
        let findings = required_artifact_mutation_findings(
            &format!("required-artifact-producer-{name}"),
            &mutated,
        );
        assert!(
            findings.iter().any(|finding| {
                finding.contains(
                    "required-artifact chain profile `producer` job must match the complete protected S1 renderer output",
                )
            }),
            "accepted producer {name} mutation: {findings:?}"
        );
    }
}

fn required_artifact_transitive_tail_and_consumer_step_mutations(
    canonical: &str,
    transitive_workflow: &str,
) {
    for (name, condition) in [
        ("always", "${{ always() }}"),
        ("failure", "${{ failure() }}"),
    ] {
        let mutated = mutate_required_artifact_workflow(transitive_workflow, |workflow| {
            required_artifact_job_mut(workflow, "tail")
                .insert("if".to_owned(), Value::String(condition.to_owned()));
        });
        let root = required_artifact_transitive_tree(
            &format!("required-artifact-transitive-tail-{name}"),
            &mutated,
        );
        let audit = must(
            audit_workflows(&root),
            "audit consumer descendant condition mutation",
        );
        assert!(
            audit.structure.iter().any(|finding| {
                finding.contains(
                    "required-artifact chain profile `tail` job must match the complete protected S1 renderer output",
                )
            }),
            "accepted transitive consumer {name} condition: {:?}",
            audit.structure
        );
        let _ = fs::remove_dir_all(root);
    }

    let consumer_task_continue = mutate_required_artifact_workflow(canonical, |workflow| {
        required_artifact_step_mut(workflow, "consumer", "Run consume")
            .insert("continue-on-error".to_owned(), Value::Bool(true));
    });
    let findings = required_artifact_mutation_findings(
        "required-artifact-consumer-task-continue-on-error",
        &consumer_task_continue,
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains(
                "required-artifact chain profile `consumer` job must match the complete protected S1 renderer output",
            )
        }),
        "accepted consumer task continue-on-error: {findings:?}"
    );
}

fn required_artifact_optional_artifact_profile() {
    let root = required_artifact_tree("required-artifact-optional-artifacts", "");
    let config_path = root.join(GENERATION_CONFIG);
    let generation = must(
        fs::read_to_string(&config_path),
        "read optional artifact config",
    );
    write(
        &config_path,
        &generation.replace("artifacts_required = true\n", ""),
    );
    let rendered = must(
        render_required_artifact_workflow(&root),
        "render optional artifact profile",
    );
    write(&root.join(".github/workflows/checks.yml"), &rendered);
    let audit = must(audit_workflows(&root), "audit optional artifact profile");
    assert!(
        audit.structure.is_empty(),
        "optional artifacts were treated as required: {:?}",
        audit.structure
    );
    assert!(
        !rendered.contains("verify-producer-artifacts:"),
        "renderer created a verifier for optional artifacts"
    );
    let _ = fs::remove_dir_all(root);
}

fn assert_required_artifact_workflow_level_mutation(
    canonical: &str,
    name: &str,
    mutate: impl FnOnce(&mut Value),
) {
    let mutated = mutate_required_artifact_workflow(canonical, mutate);
    let findings = required_artifact_mutation_findings(
        &format!("required-artifact-workflow-{name}"),
        &mutated,
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains(
                "required-artifact workflow must match its complete protected S1 renderer output",
            )
        }),
        "accepted workflow-level {name} bypass: {findings:?}"
    );
}

fn required_artifact_workflow_level_mutations(canonical: &str) {
    assert_required_artifact_workflow_level_mutation(
        canonical,
        "extra-output-consumer",
        |workflow: &mut Value| {
            let mapping = workflow
                .as_mapping_mut()
                .unwrap_or_else(|| panic!("workflow mapping exists"));
            let jobs = mapping
                .get_mut("jobs")
                .and_then(Value::as_mapping_mut)
                .unwrap_or_else(|| panic!("workflow jobs exist"));
            let mut rogue = Mapping::new();
            rogue.insert(
                "needs".to_owned(),
                Value::Sequence(vec![Value::String("producer".to_owned())]),
            );
            rogue.insert(
                "runs-on".to_owned(),
                Value::String("ubuntu-latest".to_owned()),
            );
            let mut outputs = Mapping::new();
            outputs.insert(
                "artifact_id".to_owned(),
                Value::String(concat!("$", "{{ needs.producer.outputs.artifact_id }}").to_owned()),
            );
            rogue.insert("outputs".to_owned(), Value::Mapping(outputs));
            let mut env = Mapping::new();
            env.insert(
                "ARTIFACT_ID".to_owned(),
                Value::String(concat!("$", "{{ needs.producer.outputs.artifact_id }}").to_owned()),
            );
            rogue.insert("env".to_owned(), Value::Mapping(env));
            let mut step = Mapping::new();
            step.insert(
                "run".to_owned(),
                Value::String("echo \"$ARTIFACT_ID\"".to_owned()),
            );
            rogue.insert(
                "steps".to_owned(),
                Value::Sequence(vec![Value::Mapping(step)]),
            );
            jobs.insert("unreviewed-consumer".to_owned(), Value::Mapping(rogue));
        },
    );

    for (name, mutate) in [
        (
            "workflow-env",
            Box::new(|workflow: &mut Value| {
                let mut env = Mapping::new();
                env.insert(
                    "BASH_ENV".to_owned(),
                    Value::String("/tmp/attacker.sh".to_owned()),
                );
                workflow
                    .as_mapping_mut()
                    .unwrap_or_else(|| panic!("workflow mapping exists"))
                    .insert("env".to_owned(), Value::Mapping(env));
            }) as Box<dyn Fn(&mut Value)>,
        ),
        (
            "workflow-permissions",
            Box::new(|workflow: &mut Value| {
                let mut permissions = Mapping::new();
                permissions.insert("actions".to_owned(), Value::String("write".to_owned()));
                workflow
                    .as_mapping_mut()
                    .unwrap_or_else(|| panic!("workflow mapping exists"))
                    .insert("permissions".to_owned(), Value::Mapping(permissions));
            }),
        ),
        (
            "workflow-defaults",
            Box::new(|workflow: &mut Value| {
                let mut defaults = Mapping::new();
                let mut run = Mapping::new();
                run.insert("shell".to_owned(), Value::String("bash {0}".to_owned()));
                defaults.insert("run".to_owned(), Value::Mapping(run));
                workflow
                    .as_mapping_mut()
                    .unwrap_or_else(|| panic!("workflow mapping exists"))
                    .insert("defaults".to_owned(), Value::Mapping(defaults));
            }),
        ),
    ] {
        assert_required_artifact_workflow_level_mutation(canonical, name, mutate);
    }
}

fn required_artifact_duplicate_key_and_path_mutations(canonical: &str) {
    let duplicate_key = canonical.replace(
        "    name: Verify producer artifacts\n",
        "    name: Verify producer artifacts\n    name: Duplicate verifier name\n",
    );
    let findings =
        required_artifact_mutation_findings("required-artifact-duplicate-key", &duplicate_key);
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("parse workflow")),
        "duplicate YAML key escaped required-artifact parsing: {findings:?}"
    );

    let root = required_artifact_tree(
        "required-artifact-trailing-dot-path",
        &canonical.replace("target/evidence.json", "target/report."),
    );
    let config_path = root.join(GENERATION_CONFIG);
    let generation = must(
        fs::read_to_string(&config_path),
        "read required-artifact config for trailing-dot mutation",
    )
    .replace("target/evidence.json", "target/report.");
    write(&config_path, &generation);
    let audit = must(
        audit_workflows(&root),
        "audit required-artifact trailing-dot path",
    );
    assert!(
        audit.structure.iter().any(|finding| {
            finding.contains("cannot reconstruct required-artifact jobs with the protected S1 renderer")
                && finding.contains(
                    "must be one non-empty relative literal file path without traversal, globs, or shell syntax",
                )
        }),
        "accepted trailing-dot required-artifact path: {:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn required_artifact_verifier_contract_rejects_hostile_job_mutations() {
    let canonical = canonical_required_artifact_workflow();
    required_artifact_runner_profile_contracts(&canonical);
    required_artifact_lanes_input_contract();
    required_artifact_verifier_job_mutations(&canonical);
    required_artifact_producer_upload_mutations(&canonical);
    required_artifact_consumer_job_mutations(&canonical);
    required_artifact_optional_dependency_mutations();

    let transitive_workflow = canonical_required_artifact_transitive_workflow();
    required_artifact_transitive_canonical_and_prep_skip(&transitive_workflow);
    required_artifact_transitive_status_mutations(&transitive_workflow);
    required_artifact_transitive_runner_and_ancestor_mutations(&transitive_workflow);
    required_artifact_transitive_optional_artifact_tail(&transitive_workflow);
    required_artifact_producer_task_mutations(&canonical);
    required_artifact_producer_setup_and_staging_mutations(&canonical);
    required_artifact_producer_job_environment_mutations(&canonical);
    required_artifact_transitive_tail_and_consumer_step_mutations(&canonical, &transitive_workflow);
    required_artifact_optional_artifact_profile();
    required_artifact_workflow_level_mutations(&canonical);
    required_artifact_duplicate_key_and_path_mutations(&canonical);
}

#[test]
fn required_artifact_split_workflows_are_audited_per_file() {
    let root = required_artifact_split_workflow_tree("required-artifact-split-workflows");
    let audit = must(
        audit_workflows(&root),
        "audit generated split required-artifact workflows",
    );
    assert!(
        audit.structure.is_empty(),
        "valid producer-only and producer-plus-consumer files were rejected: {:?}",
        audit.structure
    );

    let producer_only_path = root.join(".github/workflows/producer-only.yml");
    let content = must(
        fs::read_to_string(&producer_only_path),
        "read producer-only workflow",
    );
    let mut workflow: Value = must(
        serde_yaml::from_str(&content),
        "parse producer-only workflow",
    );
    let jobs = workflow
        .as_mapping_mut()
        .and_then(|mapping| mapping.get_mut("jobs"))
        .and_then(Value::as_mapping_mut)
        .unwrap_or_else(|| panic!("producer-only workflow jobs exist"));
    let producer = jobs
        .remove("producer")
        .unwrap_or_else(|| panic!("producer job exists"));
    let verifier = jobs
        .remove("verify-producer-artifacts")
        .unwrap_or_else(|| panic!("producer verifier exists"));
    write(
        &producer_only_path,
        &must(
            serde_yaml::to_string(&workflow),
            "serialize producer-only workflow without producer/verifier",
        ),
    );

    let checks_path = root.join(".github/workflows/checks.yml");
    let content = must(fs::read_to_string(&checks_path), "read checks workflow");
    let mut checks: Value = must(serde_yaml::from_str(&content), "parse checks workflow");
    let checks_jobs = checks
        .as_mapping_mut()
        .and_then(|mapping| mapping.get_mut("jobs"))
        .and_then(Value::as_mapping_mut)
        .unwrap_or_else(|| panic!("checks workflow jobs exist"));
    assert!(
        checks_jobs
            .insert("producer".to_owned(), producer)
            .is_none(),
        "checks workflow has no producer before mutation"
    );
    assert!(
        checks_jobs
            .insert("verify-producer-artifacts".to_owned(), verifier)
            .is_none(),
        "checks workflow has no producer verifier before mutation"
    );
    write(
        &checks_path,
        &must(
            serde_yaml::to_string(&checks),
            "serialize checks workflow with moved producer/verifier",
        ),
    );
    let audit = must(
        audit_workflows(&root),
        "audit split workflow with producer/verifier moved to another file",
    );
    assert!(
        audit.structure.iter().any(|finding| {
            finding.contains("producer-only.yml")
                && (finding.contains("protected-renderer workflow")
                    || finding.contains("protected S1 renderer output"))
        }),
        "another file's producer masked missing producer/verifier in producer-only.yml: {:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);
}

/// The semantic rules pass on a tree whose trusted Velnor job carries the
/// default-branch trusted-event gate, and fail — naming the job — when the
/// gate is removed. This is the ungated synthetic tree the design demands.
#[test]
fn ungated_trusted_velnor_job_fails_the_trusted_runners_rule() {
    let gated_job = gated_trusted_job();
    let gated = velnor_tree("semantic-gated", &gated_job);
    let audit = must(audit_workflows(&gated), "audit gated tree");
    assert!(audit.runners.is_empty(), "{:?}", audit.runners);
    assert!(
        audit.pull_request_target.is_empty(),
        "{:?}",
        audit.pull_request_target
    );
    let _ = fs::remove_dir_all(gated);

    let ungated_workflow = gated_job
        .lines()
        .filter(|line| !line.trim_start().starts_with("if: "))
        .fold(String::new(), |mut workflow, line| {
            workflow.push_str(line);
            workflow.push('\n');
            workflow
        });
    let ungated = velnor_tree("semantic-ungated", &ungated_workflow);
    let options = PolicyOptions {
        root: ungated.clone(),
        head_sha: None,
        base_sha: None,
        base_revision: PIN_A.to_owned(),
        ruleset_contexts: Some(vec!["ci-required".to_owned(), "DCO".to_owned()]),
        build_pin: false,
        candidate_manifest: None,
    };
    let report = must(evaluate(&options), "evaluate ungated tree");
    let runners = must_some(report.rule("trusted-runners"), "trusted-runners rule");
    assert!(!runners.passed);
    assert!(
        runners.details.iter().any(|detail| {
            detail.contains("velnor-docker")
                && detail.contains("self-hosted jobs require a default-branch trusted-event gate")
        }),
        "{:?}",
        runners.details
    );
    let rendered = report.render();
    assert!(rendered.contains("FAIL trusted-runners"), "{rendered}");
    let required = must_some(report.rule("required-checks"), "required-checks rule");
    assert!(!required.passed, "{}", required.reason);
    assert!(
        required
            .details
            .iter()
            .any(|detail| detail.contains("ruleset requires `DCO`")),
        "the live ruleset context DCO is undeclared: {:?}",
        required.details
    );
    assert!(
        required.details.iter().any(|detail| {
            detail.contains("does not require the policy entrypoint context `Policy`")
        }),
        "the ruleset must require the entrypoint's own job: {:?}",
        required.details
    );
    assert!(
        rendered.contains("PASS pull-request-target"),
        "the entrypoint is the only pull_request_target workflow: {rendered}"
    );
    assert!(!report.passed());
    let _ = fs::remove_dir_all(ungated);
}

#[test]
fn static_and_policy_workflows_are_audited_with_all_other_workflows() {
    let root = velnor_tree("workflow-audit", &gated_trusted_job());
    let generation_config = root.join(GENERATION_CONFIG);
    let base = must(
        fs::read_to_string(&generation_config),
        "read generation config",
    );
    write(
        &generation_config,
        &format!(
            "{base}\n\n[[static_files]]\nfile = \".github/workflows/ci-static.yml\"\nsource = \".github-gen/ci-static.yml\"\n"
        ),
    );
    let unpinned = "name: Workflow\non:\n  workflow_dispatch:\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v4\n";
    write(&root.join(".github-gen/ci-static.yml"), unpinned);
    write(&root.join(".github/workflows/ci-static.yml"), unpinned);
    write(
        &root.join(".github/workflows/ci-old.yml"),
        "name: Old workflow\non:\n  pull_request_target:\n    types: [opened]\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v4\n",
    );
    write(
        &root.join(".github/workflows/ci-invalid.yml"),
        "name: Invalid workflow\non: pull_request_target\non: workflow_dispatch\n",
    );
    write(
        &root.join(POLICY_ENTRYPOINT),
        "name: Policy\non:\n  pull_request_target:\n    types: [opened, synchronize, reopened]\n  workflow_dispatch:\npermissions:\n  contents: read\njobs:\n  policy:\n    runs-on: ubuntu-24.04\n    permissions:\n      contents: read\n    steps:\n      - uses: actions/checkout@v4\n",
    );

    let audit = must(audit_workflows(&root), "audit workflow tree");
    for workflow in ["ci-static.yml", "ci-policy.yml"] {
        assert!(
            audit.actions.iter().any(|finding| {
                finding.contains(workflow) && finding.contains("not a full SHA pin")
            }),
            "semantic audit must include {workflow}: {:?}",
            audit.actions
        );
    }
    assert_eq!(
        audit.pull_request_target.len(),
        1,
        "{:?}",
        audit.pull_request_target
    );
    assert!(
        audit
            .pull_request_target
            .iter()
            .any(|finding| finding.contains("ci-old.yml")),
        "all workflows must be checked for pull_request_target: {:?}",
        audit.pull_request_target
    );
    assert!(
        audit.structure.iter().any(|finding| {
            finding.contains("ci-invalid.yml") && finding.contains("parse workflow")
        }),
        "an invalid workflow must fail closed: {:?}",
        audit.structure
    );
    assert!(
        !audit
            .pull_request_target
            .iter()
            .any(|finding| finding.contains("ci-policy.yml")),
        "the policy entrypoint alone is admitted to pull_request_target"
    );
    assert!(
        audit
            .actions
            .iter()
            .any(|finding| finding.contains("ci-old.yml") && finding.contains("not a full SHA pin")),
        "all workflows must receive action-pin checks: {:?}",
        audit.actions
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn workflow_audit_rejects_symlinked_roots_and_files() {
    use std::os::unix::fs::symlink;

    let root = velnor_tree("workflow-audit-symlink-github", &gated_trusted_job());
    let target = temporary_directory("workflow-audit-symlink-github-target");
    must(
        fs::create_dir_all(target.join("workflows")),
        "create symlinked workflow target",
    );
    must(
        fs::remove_dir_all(root.join(".github")),
        "remove workflow root",
    );
    must(
        symlink(&target, root.join(".github")),
        "create symlinked workflow root",
    );
    let error = must_fail(
        audit_workflows(&root),
        "audit must reject a symlinked .github root",
    )
    .to_string();
    assert!(error.contains("symlinked workflow root"), "{error}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(target);

    let root = velnor_tree("workflow-audit-symlink-workflows", &gated_trusted_job());
    let target = temporary_directory("workflow-audit-symlink-workflows-target");
    must(
        fs::remove_dir_all(root.join(".github/workflows")),
        "remove workflow directory",
    );
    must(
        symlink(&target, root.join(".github/workflows")),
        "create symlinked workflow directory",
    );
    let error = must_fail(
        audit_workflows(&root),
        "audit must reject a symlinked workflows directory",
    )
    .to_string();
    assert!(error.contains("symlinked workflow directory"), "{error}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(target);

    let root = velnor_tree("workflow-audit-symlink-file", &gated_trusted_job());
    let target = temporary_directory("workflow-audit-symlink-file-target");
    write(
        &target.join("external.yml"),
        "name: External\non:\n  workflow_dispatch:\n",
    );
    must(
        symlink(
            target.join("external.yml"),
            root.join(".github/workflows/aaa-external.yml"),
        ),
        "create symlinked workflow file",
    );
    let error = must_fail(
        audit_workflows(&root),
        "audit must reject a symlinked workflow file",
    )
    .to_string();
    assert!(error.contains("symlink"), "{error}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(target);
}

#[cfg(unix)]
#[test]
fn declared_pin_rejects_a_symlinked_policy_entrypoint() {
    use std::os::unix::fs::symlink;

    let root = velnor_tree("declared-pin-symlink-entrypoint", &gated_trusted_job());
    let target = temporary_directory("declared-pin-symlink-entrypoint-target");
    write(&target.join("ci-policy.yml"), &hosted_entrypoint(PIN_B));
    must(
        fs::remove_file(root.join(POLICY_ENTRYPOINT)),
        "remove policy entrypoint",
    );
    must(
        symlink(target.join("ci-policy.yml"), root.join(POLICY_ENTRYPOINT)),
        "create symlinked policy entrypoint",
    );
    let error = must_fail(
        DeclaredTree::read(&root),
        "declared pin must reject a symlinked policy entrypoint",
    )
    .to_string();
    assert!(error.contains("symlink"), "{error}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(target);
}

#[cfg(unix)]
#[test]
fn policy_rejects_a_symlinked_or_special_generation_config_before_reading_it() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    let root = velnor_tree("policy-generation-config-symlink", &gated_trusted_job());
    let target = temporary_directory("policy-generation-config-symlink-target");
    write(&target.join("velnor-workflow.toml"), "this is not TOML\n");
    let config_path = root.join(GENERATION_CONFIG);
    must(fs::remove_file(&config_path), "remove generation config");
    must(
        symlink(target.join("velnor-workflow.toml"), &config_path),
        "create symlinked generation config",
    );
    let error = must_fail(
        DeclaredTree::read(&root),
        "policy must reject an escaping generation-config symlink",
    )
    .to_string();
    assert!(error.contains("escapes the repository"), "{error}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(target);

    let root = PathBuf::from("/tmp").join(format!(
        "vw-pgen-special-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    must(
        fs::create_dir_all(root.join(".github-gen")),
        "create short generation-config root",
    );
    let config_path = root.join(GENERATION_CONFIG);
    let listener = must(
        UnixListener::bind(&config_path),
        "create special generation config",
    );
    let error = must_fail(
        DeclaredTree::read(&root),
        "policy must reject a special generation config",
    )
    .to_string();
    assert!(error.contains("special file"), "{error}");
    drop(listener);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn evaluate_rejects_a_symlinked_checkout_before_policy_reads() {
    use std::os::unix::fs::symlink;

    let target = velnor_tree("evaluate-symlinked-root-target", &gated_trusted_job());
    let link = temporary_directory("evaluate-symlinked-root-link");
    must(fs::remove_dir_all(&link), "remove link placeholder");
    must(symlink(&target, &link), "create symlinked checkout root");
    let options = PolicyOptions {
        root: link.clone(),
        head_sha: None,
        base_sha: None,
        base_revision: PIN_A.to_owned(),
        ruleset_contexts: None,
        build_pin: false,
        candidate_manifest: None,
    };
    let error = must_fail(
        evaluate(&options),
        "policy must reject a symlinked checkout before config discovery",
    )
    .to_string();
    assert!(error.contains("symlinked repository root"), "{error}");
    must(fs::remove_file(&link), "remove symlinked checkout root");
    let _ = fs::remove_dir_all(target);
}

#[test]
fn policy_preflight_rejects_a_missing_or_aliased_policy_entrypoint() {
    let root = velnor_tree("policy-entrypoint-preflight", &gated_trusted_job());
    let config_path = root.join(GENERATION_CONFIG);
    let base = must(
        fs::read_to_string(&config_path),
        "read generation config for policy preflight",
    );

    write(
        &config_path,
        &base.replace("[workflow]\n", "[workflow]\nfiles = [\"ci-custom.yml\"]\n"),
    );
    let error = must_fail(
        DeclaredTree::read(&root),
        "policy preflight must reject an omitted entrypoint",
    )
    .to_string();
    assert!(error.contains("must include `ci-policy.yml`"), "{error}");

    write(
        &config_path,
        &format!(
            "{base}\n[[static_files]]\nfile = \".github/workflowſ/ci-policy.yml\"\nsource = \".github-gen/policy.yml\"\n"
        ),
    );
    let error = must_fail(
        DeclaredTree::read(&root),
        "policy preflight must reject a static alias of its entrypoint",
    )
    .to_string();
    assert!(error.contains("generator owns this path"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn policy_preflight_rejects_static_replacements_of_ci_pr_and_runtime_products() {
    let root = velnor_tree(
        "policy-owned-workflow-static-overrides",
        &gated_trusted_job(),
    );
    let config_path = root.join(GENERATION_CONFIG);
    let base = must(
        fs::read_to_string(&config_path),
        "read generation config for owned workflow policy preflight",
    );
    let base = base.replace(
        "repository = \"example/consumer\"",
        &format!(
            "repository = \"{}\"",
            crate::workflow_setup_action_repository()
        ),
    );

    for path in [crate::CI_PR_WORKFLOW, crate::CI_RUNTIME_PRODUCTS_WORKFLOW] {
        write(
            &config_path,
            &format!(
                "{base}\n[[static_files]]\nfile = \"{path}\"\nsource = \".github-gen/static.yml\"\n"
            ),
        );
        let error = must_fail(
            DeclaredTree::read(&root),
            "policy preflight must reject a static replacement of an owned workflow",
        )
        .to_string();
        assert!(error.contains("generator owns this path"), "{error}");
        assert!(error.contains(path), "{error}");
    }

    let _ = fs::remove_dir_all(root);
}

/// The dual-lane Velnor gate only admits dispatch from the configured
/// default branch; any-ref dispatch shapes are rejected.
#[test]
fn velnor_pr_gate_requires_default_branch_dispatch() {
    let automatic = "github.event_name == 'pull_request' && \
        github.event.pull_request.head.repo.full_name == github.repository || \
        (github.ref == 'refs/heads/main' && (github.event_name == 'push' || \
        github.event_name == 'schedule'))";
    for dispatch in [
        "(github.ref == 'refs/heads/main' && (github.event_name == 'workflow_dispatch' && \
            (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both')))",
        "(github.ref == 'refs/heads/main' && (github.event_name == 'workflow_dispatch' && \
            (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both' || \
            github.event.inputs.runner == '')))",
    ] {
        let gate = format!("{}{{{{ ({automatic} || {dispatch}) }}}}", "$");
        assert!(
            is_generated_velnor_pr_gate(&gate, "main"),
            "default-branch dispatch shape admitted: {dispatch}"
        );
    }
    for dispatch in [
        "(github.event_name == 'workflow_dispatch' && \
            (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both'))",
        "(github.event_name == 'workflow_dispatch' && \
            (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both' || \
            github.event.inputs.runner == ''))",
    ] {
        let gate = format!("{}{{{{ ({automatic} || {dispatch}) }}}}", "$");
        assert!(
            !is_generated_velnor_pr_gate(&gate, "main"),
            "dispatch outside the default branch must not be admitted: {dispatch}"
        );
    }
    let github_only = format!(
        "{}{{{{ ({automatic} || (github.event_name == 'workflow_dispatch' && \
            (github.event.inputs.runner == 'github'))) }}}}",
        "$"
    );
    assert!(
        !is_generated_velnor_pr_gate(&github_only, "main"),
        "a dispatch selecting only the hosted lane is not a Velnor gate",
    );
}
#[test]
fn a_second_pull_request_target_workflow_is_refused() {
    let root = velnor_tree("semantic-prt", &gated_trusted_job());
    write(
        &root.join(".github/workflows/rogue.yml"),
        "name: Rogue\non:\n  pull_request_target:\njobs:\n  run:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo rogue\n",
    );
    let audit = must(audit_workflows(&root), "audit tree with a rogue entrypoint");
    assert!(
        audit
            .pull_request_target
            .iter()
            .any(|finding| finding.contains("rogue.yml")),
        "{:?}",
        audit.pull_request_target
    );
    let _ = fs::remove_dir_all(root);
}

/// Job-level `env:` allows the documented contexts (`github, needs, strategy,
/// matrix, vars, secrets, inputs`), and step-level `env:` allows `runner` and
/// `steps`. A workflow using only those passes `workflow-structure`.
#[test]
fn job_level_env_with_allowed_contexts_passes() {
    let root = velnor_tree("semantic-job-env-allowed", &gated_trusted_job());
    write(
        &root.join(".github/workflows/allowed.yml"),
        "name: Allowed\non: push\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    strategy:\n      matrix:\n        target: [a, b]\n    env:\n      TARGET: ${{ matrix.target }}\n      PARALLEL: ${{ strategy.job-index }}\n      VERSION: ${{ needs.identity.outputs.version }}\n      REPO: ${{ github.repository }}\n      SECRET: ${{ secrets.MY_SECRET }}\n      CONFIG: ${{ vars.MY_VAR }}\n      INPUT: ${{ inputs.my_input }}\n      SELECTOR: ${{ github.event.inputs.runner }}\n    steps:\n      - id: first\n        run: echo ok\n      - run: echo ok\n        env:\n          TMP: ${{ runner.temp }}\n          PREV: ${{ steps.first.outputs.value }}\n",
    );
    let audit = must(audit_workflows(&root), "audit tree with allowed job env");
    assert!(audit.structure.is_empty(), "{:?}", audit.structure);
    let _ = fs::remove_dir_all(root);
}

/// The `runner` context is unavailable in job-level `env:` (GitHub rejects the
/// workflow at compile time with `Unrecognized named-value: 'runner'`). Both
/// property and index syntax fail with a finding naming file, job, and
/// expression.
#[test]
fn job_level_env_with_runner_context_fails() {
    let root = velnor_tree("semantic-job-env-runner", &gated_trusted_job());
    write(
        &root.join(".github/workflows/bad-runner.yml"),
        "name: Bad\non: push\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    env:\n      CARGO_HOME: ${{ runner.temp }}/velnor-producer-cargo-home\n      INDEXED: ${{ runner['temp'] }}/x\n    steps:\n      - run: echo ok\n",
    );
    let audit = must(audit_workflows(&root), "audit tree with runner in job env");
    assert_eq!(audit.structure.len(), 2, "{:?}", audit.structure);
    for (key, expression) in [("CARGO_HOME", "runner.temp"), ("INDEXED", "runner['temp']")] {
        assert!(
            audit.structure.iter().any(|finding| {
                finding.contains("bad-runner.yml")
                    && finding.contains("job build")
                    && finding.contains(key)
                    && finding.contains("runner")
                    && finding.contains(expression)
            }),
            "missing finding for {key} ({expression}): {:?}",
            audit.structure
        );
    }
    let _ = fs::remove_dir_all(root);
}

/// The `steps` context is available only from steps, never in job-level `env:`.
#[test]
fn job_level_env_with_steps_context_fails() {
    let root = velnor_tree("semantic-job-env-steps", &gated_trusted_job());
    write(
        &root.join(".github/workflows/bad-steps.yml"),
        "name: Bad\non: push\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    env:\n      PREV: ${{ steps.first.outputs.value }}\n    steps:\n      - id: first\n        run: echo ok\n",
    );
    let audit = must(audit_workflows(&root), "audit tree with steps in job env");
    assert_eq!(audit.structure.len(), 1, "{:?}", audit.structure);
    assert!(
        audit.structure[0].contains("bad-steps.yml")
            && audit.structure[0].contains("job build")
            && audit.structure[0].contains("PREV")
            && audit.structure[0].contains("steps")
            && audit.structure[0].contains("steps.first.outputs.value"),
        "{:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);
}

fn must_some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(value) => value,
        None => panic!("{context}: expected a value"),
    }
}

// ---------------------------------------------------------------------------
// CLI parsing
// ---------------------------------------------------------------------------

#[test]
fn cli_requires_the_base_validator_revision() {
    let root = temporary_directory("cli-no-base");
    // Hermetic: the empty lookup observes no ambient environment, so the
    // missing-option error fires even when the job exports
    // `VELNOR_WORKFLOW_POLICY_REVISION` (Preview exports it job-wide).
    let error = must_fail(
        run_cli_with_env(
            &[
                std::ffi::OsString::from("--workflow-root"),
                root.as_os_str().to_owned(),
            ],
            &|_| None,
        ),
        "policy without a base revision",
    )
    .to_string();
    assert!(error.contains("--base-revision"), "{error}");
    assert!(error.contains(BASE_REVISION_ENV), "{error}");
    let unknown = must_fail(
        run_cli(&[std::ffi::OsString::from("--approved-policy-revision")]),
        "retired option",
    )
    .to_string();
    assert!(unknown.contains("--approved-policy-revision"), "{unknown}");
    let duplicate = must_fail(
        run_cli(&[
            std::ffi::OsString::from("--candidate-manifest"),
            std::ffi::OsString::from("/first.json"),
            std::ffi::OsString::from("--candidate-manifest"),
            std::ffi::OsString::from("/second.json"),
        ]),
        "candidate manifest given twice",
    )
    .to_string();
    assert!(duplicate.contains("--candidate-manifest"), "{duplicate}");
    assert!(duplicate.contains("given twice"), "{duplicate}");
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Candidate render exception
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn fake_candidate_renderer(directory: &Path, closure: &str, revision: &str) -> PathBuf {
    let binary = directory.join("candidate");
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --revision ]; then echo {revision}; exit 0; fi\nif [ \"$1\" = --closure ]; then echo {closure}; exit 0; fi\ncp -r \"$1/.\" \"$3/\"\n"
            ),
        ),
        "write fake candidate renderer",
    );
    {
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark fake candidate renderer executable",
        );
    }
    binary
}

#[cfg(unix)]
fn fake_mutating_candidate_renderer(directory: &Path, closure: &str, revision: &str) -> PathBuf {
    let binary = directory.join("candidate-mutating");
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --revision ]; then echo {revision}; exit 0; fi\nif [ \"$1\" = --closure ]; then echo {closure}; exit 0; fi\ncp -r \"$1/.\" \"$3/\"\nprintf 'tampered\\n' > \"$1/crates/velnor-workflow/src/lib.rs\" 2>/dev/null || true\n"
            ),
        ),
        "write mutating fake candidate renderer",
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark mutating fake candidate renderer executable",
        );
    }
    binary
}

#[cfg(unix)]
fn fake_environment_observing_candidate_renderer(
    directory: &Path,
    closure: &str,
    revision: &str,
    sentinel: &Path,
) -> PathBuf {
    let binary = directory.join("candidate-env");
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nsafe_env() {{ [ \"${{HOME-}}\" = /tmp ] && [ \"${{ACTIONS_RUNTIME_TOKEN+x}}\" != x ]; }}\nif [ \"$1\" = --revision ]; then safe_env || echo invalid; safe_env && echo {revision}; exit 0; fi\nif [ \"$1\" = --closure ]; then safe_env || echo invalid; safe_env && echo {closure}; exit 0; fi\nsafe_env || {{ touch \"{}\"; exit 17; }}\ncp -R \"$1/.github\" \"$3/\"\n",
                sentinel.display(),
            ),
        ),
        "write environment-observing candidate renderer",
    );
    {
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark environment-observing candidate renderer executable",
        );
    }
    binary
}

#[cfg(unix)]
fn closure_fixture(name: &str) -> (PathBuf, String) {
    let root = temporary_directory(name);
    git_ok(&root, &["init", "-q", "-b", "main"]);
    write(
        &root.join("crates/velnor-workflow/src/lib.rs"),
        "pub fn f() {}\n",
    );
    write(
        &root.join("crates/velnor-workflow/Cargo.toml"),
        "[package]\nname = \"velnor-workflow\"\n[dependencies]\n",
    );
    write(&root.join("Cargo.toml"), "[workspace]\n");
    write(&root.join("Cargo.lock"), "# lock\n");
    write(&root.join(".github/workflows/ci-pr.yml"), "tree\n");
    let head = commit(&root, "fixture");
    (root, head)
}

#[cfg(unix)]
fn squash_fixture(name: &str, main_touches_closure: bool) -> (PathBuf, String, String) {
    let root = temporary_directory(name);
    git_ok(&root, &["init", "-q", "-b", "main"]);
    write(
        &root.join("crates/velnor-workflow/src/lib.rs"),
        "pub fn f() {}\n",
    );
    write(
        &root.join("crates/velnor-workflow/Cargo.toml"),
        "[package]\nname = \"velnor-workflow\"\n[dependencies]\n",
    );
    write(&root.join("Cargo.toml"), "[workspace]\n");
    write(&root.join("Cargo.lock"), "# base-lock\n");
    write(&root.join(".github/workflows/ci-pr.yml"), "tree\n");
    commit(&root, "base");
    git_ok(&root, &["checkout", "-q", "-b", "pr"]);
    write(
        &root.join("crates/velnor-workflow/src/lib.rs"),
        "pub fn f() { println!(\"pr\"); }\n",
    );
    let pr_head = commit(&root, "pr generator change");
    git_ok(&root, &["checkout", "-q", "main"]);
    if main_touches_closure {
        write(&root.join("Cargo.lock"), "# main-lock\n");
    } else {
        write(&root.join("main-only.txt"), "main change\n");
    }
    commit(&root, "main change");
    git_ok(&root, &["merge", "--squash", "--no-commit", "pr"]);
    let merge = commit(&root, "squash merge");
    (root, pr_head, merge)
}

// NOTE: `candidate_render_is_accepted_only_from_a_head_authentic_binary` was
// deleted here. It asserted that an env-slot binary is accepted when its
// `--closure` stdout echoes the wanted closure — but that expectation WAS
// the vulnerability itself: a `--closure` echo is a self-report by
// untrusted bytes, and treating it as authenticity let any binary that
// prints the right string execute as the candidate. Acceptance now
// requires the manifest binding (closure equality plus digest match before
// any execution); the tests below pin each clause of that rule.

/// A candidate manifest binding `binary` to `closure`: the digest is the
/// real SHA-256 of the binary bytes, so only a byte-identical binary
/// satisfies the binding.
#[cfg(unix)]
fn candidate_manifest_for(
    directory: &Path,
    name: &str,
    binary: &Path,
    closure: &str,
    revision: &str,
) -> PathBuf {
    candidate_manifest_for_with_build_revision(directory, name, binary, closure, revision, revision)
}

#[cfg(unix)]
fn candidate_manifest_for_with_build_revision(
    directory: &Path,
    name: &str,
    binary: &Path,
    closure: &str,
    revision: &str,
    build_revision: &str,
) -> PathBuf {
    use sha2::Digest as _;
    let bytes = must(fs::read(binary), "read fake binary bytes");
    let mut digest = String::with_capacity(64);
    for byte in sha2::Sha256::digest(&bytes) {
        let _ = std::fmt::Write::write_fmt(&mut digest, format_args!("{byte:02x}"));
    }
    let manifest = directory.join(name);
    must(
        fs::write(
            &manifest,
            serde_json::json!({
                "profile": "debug",
                "platform": "Linux-X64",
                "repository": crate::workflow_setup_action_repository(),
                "run_id": "123",
                "revision": revision,
                "closure": closure,
                "build_revision": build_revision,
                "binary_sha256": digest,
            })
            .to_string(),
        ),
        "write candidate manifest",
    );
    manifest
}

/// A fake candidate renderer whose matching closure and tree output would be
/// accepted if the digest or manifest gate mistakenly allowed execution.
#[cfg(unix)]
fn fake_probed_candidate_renderer(directory: &Path, closure: &str, sentinel: &Path) -> PathBuf {
    let binary = directory.join("candidate");
    let sentinel = sentinel.display();
    must(
        fs::write(
            &binary,
            format!("#!/bin/sh\ntouch '{sentinel}'\nif [ \"$1\" = --revision ]; then echo {}; exit 0; fi\nif [ \"$1\" = --closure ]; then echo {closure}; exit 0; fi\ncp -R \"$1/.\" \"$3/\"\n", crate::GENERATOR_REVISION),
        ),
        "write fake probed candidate renderer",
    );
    {
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark fake probed candidate renderer executable",
        );
    }
    binary
}

#[cfg(unix)]
fn fake_absolute_path_probe_candidate(
    directory: &Path,
    revision: &str,
    closure: &str,
    protected_path: &Path,
) -> PathBuf {
    let binary = directory.join("candidate-absolute-path-probe");
    let protected = protected_path.display();
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\n/bin/sleep 60 &\nprotected='{protected}'\npipe_probe() {{\n  /usr/bin/dd if=/dev/zero bs=1048576 count=16 of=\"/proc/$PPID/fd/1\" 2>/dev/null || true\n  /usr/bin/dd if=/dev/zero bs=1048576 count=16 of=\"/proc/$PPID/fd/2\" 2>/dev/null || true\n}}\nprobe() {{\n  if [ -r \"$protected\" ]; then cat \"$protected\"; else printf '%s\\n' \"$2\"; fi\n  mkdir -p \"$(dirname \"$protected\")\" 2>/dev/null || true\n  printf '%s\\n' 'candidate mutation' > \"$protected\" 2>/dev/null || true\n}}\nif [ \"$1\" = --revision ]; then pipe_probe; probe unused {revision}; exit 0; fi\nif [ \"$1\" = --closure ]; then pipe_probe; probe unused {closure}; exit 0; fi\nroot=\"$1\"; shift\n[ \"$1\" = --output ] || exit 3\noutput=\"$2\"\nmkdir -p \"$output\"\npipe_probe\nif [ -r \"$protected\" ]; then cat \"$protected\" > \"$output/leaked-secret.txt\"; fi\nmkdir -p \"$(dirname \"$protected\")\" 2>/dev/null || true\nprintf '%s\\n' 'candidate mutation' > \"$protected\" 2>/dev/null || true\ncp -R \"$root/.github\" \"$output/\"\n"
            ),
        ),
        "write absolute-path probing candidate",
    );
    must(
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
        "mark absolute-path candidate executable",
    );
    binary
}

#[cfg(unix)]
fn fake_candidate_with_late_mutating_descendant(
    directory: &Path,
    revision: &str,
    closure: &str,
) -> PathBuf {
    let binary = directory.join("candidate-late-descendant");
    must(
        fs::write(
            &binary,
            format!(
                r#"#!/bin/sh
if [ "$1" = --revision ]; then echo {revision}; exit 0; fi
if [ "$1" = --closure ]; then echo {closure}; exit 0; fi
root="$1"; shift
[ "$1" = --output ] || exit 3
output="$2"
mkdir -p "$output"
/usr/bin/setsid /bin/sh -c 'parent=$1; output=$2; while kill -0 "$parent" 2>/dev/null; do /bin/sleep 0.01; done; /bin/sleep 0.25; count=0; while :; do count=$((count + 1)); printf "late-%s" "$count" > "$output/late-file"; done' candidate-descendant "$$" "$output" >/dev/null 2>&1 &
cp -R "$root/.github" "$output/"
"#
            ),
        ),
        "write candidate with a late-mutating descendant",
    );
    must(
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
        "mark late-mutating candidate executable",
    );
    binary
}

#[cfg(unix)]
#[test]
fn candidate_descendant_cannot_mutate_render_after_parent_exits() {
    let (root, head) = closure_fixture("candidate-late-descendant");
    let wanted = must(
        crate::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let scratch = temporary_directory("candidate-late-descendant-scratch");
    let binary = fake_candidate_with_late_mutating_descendant(&root, &head, &wanted);
    let manifest =
        candidate_manifest_for(&root, "candidate-manifest.json", &binary, &wanted, &head);
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    let rendered = must(
        render_with_candidate(&root, &root, &scratch, "main", &lookup),
        "candidate with a late descendant renders",
    );
    let late_file = scratch.join("late-file");
    let late_file_at_return = fs::read(&late_file).ok();
    match (rendered.as_deref(), late_file_at_return.as_deref()) {
        (Some(closure), None) => assert_eq!(closure, wanted),
        (None, Some(_)) => {
            // The candidate was rejected only if the frozen archive proves
            // the writer changed output before the container pause.
            let differences = must(
                compare_rendered_tree(&scratch, &root),
                "compare the candidate output changed before pause",
            );
            assert_eq!(differences.len(), 1);
            assert_eq!(
                differences[0], "late-file: missing from the tree",
                "the archived late write must be the reason candidate output is rejected"
            );
        }
        (Some(_), Some(_)) => panic!("candidate output containing a late write was accepted"),
        (None, None) => panic!("candidate rejection had no captured late write to explain it"),
    }

    // Candidate descendants write only inside the container's output volume;
    // `scratch` is the extracted archive and cleanup removes the container
    // before render returns. Poll beyond the deliberate delay to prove the
    // copied output and checkout stay stable after return.
    let root_late_file = root.join("late-file");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
    loop {
        assert_eq!(
            fs::read(&late_file).ok(),
            late_file_at_return,
            "candidate descendant changed copied output after render returned"
        );
        assert!(
            fs::symlink_metadata(&root_late_file).is_err(),
            "candidate descendant changed the read-only checkout after render returned"
        );
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn bound_candidate_matching_head_tree_is_accepted() {
    let (root, head) = closure_fixture("candidate-bound");
    let wanted = must(
        crate::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let scratch = temporary_directory("candidate-bound-scratch");
    let binary = fake_candidate_renderer(&root, &wanted, &head);
    let manifest =
        candidate_manifest_for(&root, "candidate-manifest.json", &binary, &wanted, &head);
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    assert_eq!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &lookup),
            "bound candidate reproduces the tree",
        )
        .as_deref(),
        Some(wanted.as_str())
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn candidate_render_cannot_mutate_authoritative_checkout() {
    let (root, head) = closure_fixture("candidate-immutable-source");
    let wanted = must(
        crate::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of fixture",
    );
    let original = must(
        fs::read(root.join("crates/velnor-workflow/src/lib.rs")),
        "read authoritative source before candidate",
    );
    let artifacts = temporary_directory("candidate-immutable-source-artifacts");
    let binary = fake_mutating_candidate_renderer(&artifacts, &wanted, &head);
    let manifest = candidate_manifest_for(
        &artifacts,
        "candidate-manifest.json",
        &binary,
        &wanted,
        &head,
    );
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    let scratch = temporary_directory("candidate-immutable-source-scratch");
    assert_eq!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &lookup),
            "candidate mutation is isolated from authoritative checkout",
        )
        .as_deref(),
        Some(wanted.as_str())
    );
    assert_eq!(
        must(
            fs::read(root.join("crates/velnor-workflow/src/lib.rs")),
            "read authoritative source after candidate",
        ),
        original,
        "candidate writes stay inside the immutable source snapshot"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(artifacts);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn squash_merge_with_closure_clean_main_accepts_pr_candidate() {
    let (root, pr_head, merge) = squash_fixture("candidate-squash-clean", false);
    let head_closure = must(
        crate::closure::candidate_closure_of_tree(&root, &pr_head),
        "PR head candidate closure",
    );
    let merge_closure = must(
        crate::closure::candidate_closure_of_tree(&root, &merge),
        "squash merge candidate closure",
    );
    assert_eq!(head_closure, merge_closure);
    let scratch = temporary_directory("candidate-squash-clean-scratch");
    let binary = fake_candidate_renderer(&root, &head_closure, &merge);
    let manifest = candidate_manifest_for_with_build_revision(
        &root,
        "candidate-manifest.json",
        &binary,
        &head_closure,
        &pr_head,
        &merge,
    );
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    assert_eq!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &lookup),
            "bound squash candidate renders the merge tree",
        )
        .as_deref(),
        Some(head_closure.as_str())
    );
    assert!(scratch.join("main-only.txt").is_file());
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn shallow_squash_merge_fetches_pr_head_before_candidate_closure() {
    let (origin, pr_head, merge) = squash_fixture("candidate-squash-shallow", false);
    let shallow = temporary_directory("candidate-squash-shallow-checkout");
    let _ = fs::remove_dir_all(&shallow);
    let origin_url = format!("file://{}", origin.display());
    git_ok(
        &origin,
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            "--single-branch",
            "--branch",
            "main",
            &origin_url,
            &shallow.display().to_string(),
        ],
    );
    assert_eq!(git_ok(&shallow, &["rev-parse", "HEAD"]), merge);
    assert!(
        !commit_exists(&shallow, &pr_head),
        "a depth-1 merge checkout starts without the PR head object"
    );

    // This is the exact fetch the trusted acquire step performs before it
    // derives the candidate artifact name. The policy consumer then has the
    // PR-head object needed to recompute the manifest's bound closure.
    git_ok(&shallow, &["fetch", "--no-tags", &origin_url, &pr_head]);
    assert!(
        commit_exists(&shallow, &pr_head),
        "fetching the resolved PR head makes its commit object available"
    );

    let closure = must(
        crate::closure::candidate_closure_of_tree(&origin, &pr_head),
        "PR head candidate closure",
    );
    let artifacts = temporary_directory("candidate-squash-shallow-artifacts");
    let binary = fake_candidate_renderer(&artifacts, &closure, &merge);
    let manifest = candidate_manifest_for_with_build_revision(
        &artifacts,
        "candidate-manifest.json",
        &binary,
        &closure,
        &pr_head,
        &merge,
    );
    let lookup = lookup_with_manifest(Some(binary), None, artifacts.join("install"), manifest);
    let scratch = temporary_directory("candidate-squash-shallow-scratch");
    assert_eq!(
        must(
            render_with_candidate(&shallow, &shallow, &scratch, "main", &lookup,),
            "fetched PR-head candidate renders the shallow merge checkout",
        )
        .as_deref(),
        Some(closure.as_str())
    );
    assert!(scratch.join("main-only.txt").is_file());

    let _ = fs::remove_dir_all(origin);
    let _ = fs::remove_dir_all(shallow);
    let _ = fs::remove_dir_all(artifacts);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn squash_merge_with_closure_changed_main_fails_before_candidate_execution() {
    let (root, pr_head, merge) = squash_fixture("candidate-squash-lock", true);
    let head_closure = must(
        crate::closure::candidate_closure_of_tree(&root, &pr_head),
        "PR head candidate closure",
    );
    let merge_closure = must(
        crate::closure::candidate_closure_of_tree(&root, &merge),
        "squash merge candidate closure",
    );
    assert_ne!(head_closure, merge_closure);
    let scratch = temporary_directory("candidate-squash-lock-scratch");
    let sentinel = root.join("candidate-executed");
    let binary = fake_probed_candidate_renderer(&root, &head_closure, &sentinel);
    let manifest = candidate_manifest_for(
        &root,
        "candidate-manifest.json",
        &binary,
        &head_closure,
        &pr_head,
    );
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    let error = must_fail(
        render_with_candidate(&root, &root, &scratch, "main", &lookup),
        "a PR-head candidate cannot render a merge with changed closure inputs",
    )
    .to_string();
    assert!(
        error.contains("differs from audited render revision"),
        "{error}"
    );
    assert!(
        error.contains("update the PR branch/rebuild the candidate"),
        "{error}"
    );
    assert!(
        !sentinel.exists(),
        "closure mismatch fails before candidate execution"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn candidate_binary_revision_must_match_manifest_build_revision() {
    let (root, head) = closure_fixture("candidate-build-revision");
    let wanted = must(
        crate::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let scratch = temporary_directory("candidate-build-revision-scratch");
    let binary = fake_candidate_renderer(&root, &wanted, &head);
    let manifest = candidate_manifest_for_with_build_revision(
        &root,
        "candidate-manifest.json",
        &binary,
        &wanted,
        &head,
        PIN_A,
    );
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    assert!(must(
        render_with_candidate(&root, &root, &scratch, "main", &lookup),
        "build revision mismatch does not prove a candidate",
    )
    .is_none());
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn candidate_renderer_receives_no_ambient_environment() {
    assert!(
        env::var_os("HOME").is_some(),
        "Unix test environment must provide HOME to detect ambient inheritance"
    );
    let (root, head) = closure_fixture("candidate-hermetic-env");
    let wanted = must(
        crate::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let scratch = temporary_directory("candidate-hermetic-env-scratch");
    let sentinel = root.join("ambient-environment-observed");
    let binary = fake_environment_observing_candidate_renderer(&root, &wanted, &head, &sentinel);
    let manifest =
        candidate_manifest_for(&root, "candidate-manifest.json", &binary, &wanted, &head);
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    assert_eq!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &lookup),
            "hermetic candidate reproduces the tree",
        )
        .as_deref(),
        Some(wanted.as_str())
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn candidate_probe_and_render_cannot_read_or_mutate_absolute_checkout_paths() {
    let (root, head) = closure_fixture("candidate-absolute-path-sandbox");
    let wanted = must(
        crate::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let protected = root.join("validator-only-secret.txt");
    let secret = b"validator-only-secret\n";
    must(
        fs::write(&protected, secret),
        "write privileged checkout marker",
    );
    let scratch = temporary_directory("candidate-absolute-path-scratch");
    let binary = fake_absolute_path_probe_candidate(&root, &head, &wanted, &protected);
    assert_eq!(
        must(
            binary_revision(&binary),
            "isolated candidate revision probe"
        ),
        head
    );
    assert_eq!(
        must(binary_closure(&binary), "isolated candidate closure probe"),
        wanted
    );
    let manifest =
        candidate_manifest_for(&root, "candidate-manifest.json", &binary, &wanted, &head);
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    assert_eq!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &lookup),
            "isolated candidate reproduces the tree",
        )
        .as_deref(),
        Some(wanted.as_str()),
        "candidate metadata probes cannot read the host secret"
    );
    assert_eq!(
        must(fs::read(&protected), "read privileged checkout marker"),
        secret,
        "candidate metadata and render cannot mutate an absolute checkout path"
    );
    assert!(
        !scratch.join("leaked-secret.txt").exists(),
        "candidate render cannot read the host secret through an absolute path"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn lying_binary_whose_digest_misses_the_manifest_is_rejected() {
    let (root, head) = closure_fixture("candidate-lying");
    let wanted = must(
        crate::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let scratch = temporary_directory("candidate-lying-scratch");
    let sentinel = root.join("candidate-executed");
    let binary = fake_probed_candidate_renderer(&root, &wanted, &sentinel);
    // Bind the manifest to *other* bytes, so the binary echoes the wanted
    // closure but its digest misses.
    let other = root.join("other-bytes");
    must(fs::write(&other, "different bytes"), "write decoy bytes");
    let manifest = candidate_manifest_for(&root, "candidate-manifest.json", &other, &wanted, &head);
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    assert!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &lookup),
            "digest mismatch never errors, it simply does not prove",
        )
        .is_none(),
        "a binary whose digest misses the manifest is not the candidate"
    );
    assert!(
        !sentinel.exists(),
        "digest mismatch must precede candidate probes"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn candidate_snapshot_preflight_rejects_external_dangling_and_looping_links() {
    use std::os::unix::fs::symlink;

    let outside = temporary_directory("snapshot-link-outside");
    write(&outside.join("secret.txt"), "secret\n");
    let external_root = temporary_directory("snapshot-link-external");
    must(
        symlink(outside.join("secret.txt"), external_root.join("leak")),
        "create external symlink",
    );
    let error = must_fail(
        validate_and_seal_snapshot(&external_root),
        "external absolute symlink must fail",
    )
    .to_string();
    assert!(error.contains("escapes the immutable snapshot"), "{error}");

    let dangling_root = temporary_directory("snapshot-link-dangling");
    must(
        symlink("missing.txt", dangling_root.join("dangling")),
        "create dangling symlink",
    );
    let error = must_fail(
        validate_and_seal_snapshot(&dangling_root),
        "dangling symlink must fail",
    )
    .to_string();
    assert!(
        error.contains("resolve candidate source symlink"),
        "{error}"
    );

    let loop_root = temporary_directory("snapshot-link-loop");
    must(
        symlink("second", loop_root.join("first")),
        "create first loop symlink",
    );
    must(
        symlink("first", loop_root.join("second")),
        "create second loop symlink",
    );
    let error = must_fail(
        validate_and_seal_snapshot(&loop_root),
        "looping symlink must fail",
    )
    .to_string();
    assert!(
        error.contains("resolve candidate source symlink"),
        "{error}"
    );

    for root in [external_root, dangling_root, loop_root, outside] {
        let _ = fs::remove_dir_all(root);
    }
}

#[cfg(unix)]
#[test]
fn immutable_snapshot_rejects_a_symlink_destination() {
    use std::os::unix::fs::symlink;

    let (root, head) = closure_fixture("snapshot-symlink-destination");
    let parent = temporary_directory("snapshot-symlink-destination-parent");
    let target = temporary_directory("snapshot-symlink-destination-target");
    let destination = parent.join("source");
    must(
        symlink(&target, &destination),
        "create symlink snapshot destination",
    );
    let error = must_fail(
        immutable_git_snapshot(&root, &head, &destination),
        "snapshot extraction must reject a symlink destination",
    )
    .to_string();
    assert!(error.contains("already exists"), "{error}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(parent);
    let _ = fs::remove_dir_all(target);
}

#[cfg(unix)]
#[test]
fn immutable_snapshot_is_bound_to_the_requested_revision() {
    let (root, first) = closure_fixture("snapshot-revision-binding");
    write(
        &root.join("crates/velnor-workflow/src/lib.rs"),
        "pub fn f() { println!(\"new\"); }\n",
    );
    write(&root.join("new-at-head.txt"), "new\n");
    let second = commit(&root, "second fixture revision");
    assert_ne!(first, second);

    let snapshot_parent = temporary_directory("snapshot-revision-binding-output");
    let snapshot = snapshot_parent.join("snapshot");
    must(
        immutable_git_snapshot(&root, &first, &snapshot),
        "materialize the requested historical revision",
    );
    assert_eq!(
        must(
            fs::read_to_string(snapshot.join("crates/velnor-workflow/src/lib.rs")),
            "read historical source",
        ),
        "pub fn f() {}\n"
    );
    assert!(
        !snapshot.join("new-at-head.txt").exists(),
        "snapshot must not silently archive the mutable checkout HEAD"
    );
    restore_writable_tree(&snapshot);
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(snapshot_parent);
}

#[cfg(unix)]
#[test]
fn candidate_snapshot_preflight_allows_confined_directory_links_and_seals_tree() {
    use std::os::unix::fs::symlink;

    let root = temporary_directory("snapshot-confined-directory-link");
    write(&root.join("target/config.toml"), "key = true\n");
    write(&root.join("target/readme.txt"), "read me\n");
    must(
        symlink("target", root.join("alias")),
        "create confined directory symlink",
    );
    must(
        symlink("target/readme.txt", root.join("readme-link")),
        "create confined file symlink",
    );
    must(
        validate_and_seal_snapshot(&root),
        "confined directory symlink is safe",
    );
    assert!(must(fs::metadata(&root), "inspect sealed test root")
        .permissions()
        .readonly());
    assert!(must(
        fs::metadata(root.join("target/config.toml")),
        "inspect sealed test file",
    )
    .permissions()
    .readonly());
    cleanup_snapshot(&root);
    assert!(
        !root.exists(),
        "sealed snapshot cleanup must remove the tree"
    );
}

#[cfg(unix)]
#[test]
fn candidate_sandbox_uid_can_read_sealed_workflow_config_but_cannot_write_it() {
    use std::ffi::OsString;

    let root = temporary_directory("snapshot-sandbox-uid");
    let config = root.join(".github-gen/velnor-workflow.toml");
    write(&config, "schema = 1\n");
    must(
        validate_and_seal_snapshot(&root),
        "seal workflow config snapshot",
    );

    let artifacts = temporary_directory("snapshot-sandbox-uid-artifacts");
    let candidate = artifacts.join("candidate.sh");
    write(
        &candidate,
        "#!/bin/sh\nset -eu\ntest \"$(id -u)\" = 65534\nconfig=/workspace/.github-gen/velnor-workflow.toml\ntest \"$(cat \"$config\")\" = 'schema = 1'\nif printf 'tampered\\n' > \"$config\" 2>/dev/null; then exit 71; fi\ncat \"$config\"\n",
    );
    must(
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)),
        "make candidate executable",
    );
    let output = temporary_directory("snapshot-sandbox-uid-output");
    let result = must(
        crate::candidate_sandbox::run(
            &candidate,
            Some(&root),
            Some(&output),
            &[OsString::from("--unused")],
        ),
        "run candidate in uid-dropped sandbox",
    );
    assert_eq!(
        result.status,
        0,
        "candidate stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"schema = 1\n");
    assert_eq!(
        must(fs::read(&config), "read source config after candidate"),
        b"schema = 1\n",
        "candidate write attempt must leave sealed config unchanged"
    );

    cleanup_snapshot(&root);
    assert!(
        !root.exists(),
        "cleanup must reopen and remove sealed snapshot"
    );
    let _ = fs::remove_dir_all(artifacts);
    let _ = fs::remove_dir_all(output);
}

#[cfg(unix)]
#[test]
fn candidate_snapshot_preflight_rejects_special_files() {
    use std::os::unix::net::UnixListener;

    let root = PathBuf::from("/tmp").join(format!(
        "vw-policy-special-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    must(
        fs::create_dir_all(&root),
        "create short-path special-file root",
    );
    let listener = must(
        UnixListener::bind(root.join("socket")),
        "create socket special file",
    );
    let error = must_fail(validate_and_seal_snapshot(&root), "special files must fail").to_string();
    assert!(error.contains("non-regular entry"), "{error}");
    drop(listener);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn candidate_snapshot_preflight_rejects_symlinked_policy_roots() {
    use std::os::unix::fs::symlink;

    let root = temporary_directory("snapshot-policy-root-link");
    write(&root.join("source/ci.yml"), "name: policy\n");
    must(
        symlink("source", root.join(".github")),
        "create linked GitHub root",
    );
    let error = must_fail(
        validate_and_seal_snapshot(&root),
        "symlinked policy root must fail",
    )
    .to_string();
    assert!(error.contains("symlinked policy path: .github"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn candidate_manifest_for_another_tree_is_rejected() {
    let (root, head) = closure_fixture("candidate-other-tree");
    let wanted = must(
        crate::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let scratch = temporary_directory("candidate-other-tree-scratch");
    // Self-consistent but for another tree: the binary echoes CLOSURE_A and
    // the manifest binds CLOSURE_A with a matching digest.
    let binary = fake_candidate_renderer(&root, CLOSURE_A, &head);
    let manifest =
        candidate_manifest_for(&root, "candidate-manifest.json", &binary, CLOSURE_A, &head);
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    let error = must_fail(
        render_with_candidate(&root, &root, &scratch, "main", &lookup),
        "manifest for another tree",
    )
    .to_string();
    assert!(error.contains("names closure"), "{error}");
    assert!(error.contains(CLOSURE_A), "{error}");
    assert!(error.contains(&wanted), "{error}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn unbound_env_candidate_is_rejected_without_manifest() {
    let (root, head) = closure_fixture("candidate-unbound");
    let wanted = must(
        crate::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let scratch = temporary_directory("candidate-unbound-scratch");
    let sentinel = root.join("closure-probed");
    let binary = fake_probed_candidate_renderer(&root, &wanted, &sentinel);
    let lookup = candidate_lookup(Some(binary), None, root.join("install"));
    assert!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &lookup),
            "missing manifest never errors, it disables the env slot",
        )
        .is_none(),
        "an env-slot binary without a manifest is never the candidate"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn malformed_candidate_manifest_is_rejected() {
    let (root, head) = closure_fixture("candidate-malformed");
    let wanted = must(
        crate::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let scratch = temporary_directory("candidate-malformed-scratch");
    let binary = fake_candidate_renderer(&root, &wanted, &head);
    let digest = {
        use sha2::Digest as _;
        let bytes = must(fs::read(&binary), "read fake binary bytes");
        let mut digest = String::with_capacity(64);
        for byte in sha2::Sha256::digest(&bytes) {
            let _ = std::fmt::Write::write_fmt(&mut digest, format_args!("{byte:02x}"));
        }
        digest
    };
    let cases = [
        ("truncated".to_owned(), "{not json".to_owned()),
        (
            "short-revision".to_owned(),
            serde_json::json!({"revision": "abc", "closure": wanted, "binary_sha256": digest})
                .to_string(),
        ),
        (
            "short-closure".to_owned(),
            serde_json::json!({"revision": head, "closure": "abc", "binary_sha256": digest})
                .to_string(),
        ),
        (
            "short-digest".to_owned(),
            serde_json::json!({"revision": head, "closure": wanted, "binary_sha256": "abc"})
                .to_string(),
        ),
    ];
    for (name, body) in &cases {
        let manifest = root.join(format!("candidate-manifest-{name}.json"));
        must(fs::write(&manifest, body), "write malformed manifest");
        let lookup =
            lookup_with_manifest(Some(binary.clone()), None, root.join("install"), manifest);
        let error = must_fail(
            render_with_candidate(&root, &root, &scratch, "main", &lookup),
            &format!("malformed manifest {name}"),
        )
        .to_string();
        assert!(
            error.contains(&format!("candidate-manifest-{name}.json")),
            "{name}: the error names the manifest: {error}"
        );
    }
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[test]
fn candidate_manifest_source_prefers_flag_over_env() {
    // The flag wins over the environment, so generated CI stays auditable.
    // (In-process: setting process env needs `unsafe`, which this crate
    // forbids, so the flag directions — which hold in every environment —
    // are pinned here, and the environment-fallback direction is pinned by
    // the `policy` CLI subprocess test in `velnor_first_ci.rs`, which owns
    // the child's environment.)
    assert_eq!(
        candidate_manifest_source_with_env(Some("/flag/manifest.json"), &|name| env::var_os(name)),
        Some(PathBuf::from("/flag/manifest.json"))
    );
    // An explicit empty flag disables the binding.
    assert_eq!(
        candidate_manifest_source_with_env(Some(""), &|name| env::var_os(name)),
        None
    );
    // Without either source the env-slot candidate is disabled. Hermetic:
    // the empty lookup observes no ambient environment.
    assert_eq!(candidate_manifest_source_with_env(None, &|_| None), None);
}

#[test]
fn from_env_consent_mapping_is_fail_closed() {
    // Hermetic: fixed lookups observe no ambient environment.
    // Without `--pin-build` the pin is never built, in every environment.
    assert!(PinnedBinaryLookup::from_env_with(PIN_A, false, None, &|_| None).build_forbidden);
    assert!(
        PinnedBinaryLookup::from_env_with(
            PIN_A,
            false,
            Some(PathBuf::from("/manifest.json")),
            &|_| None
        )
        .build_forbidden
    );
    // The explicit manifest survives; without one the environment fallback
    // applies (`None` under the empty lookup).
    assert_eq!(
        PinnedBinaryLookup::from_env_with(
            PIN_A,
            false,
            Some(PathBuf::from("/manifest.json")),
            &|_| None
        )
        .candidate_manifest,
        Some(PathBuf::from("/manifest.json"))
    );
    assert_eq!(
        PinnedBinaryLookup::from_env_with(PIN_A, false, None, &|_| None).candidate_manifest,
        None
    );
    // The environment fallback direction: a manifest in the environment
    // binds the env-slot candidate.
    let manifest_env = PinnedBinaryLookup::from_env_with(PIN_A, false, None, &|name| {
        if name == VELNOR_WORKFLOW_CANDIDATE_MANIFEST_ENV {
            Some(std::ffi::OsString::from("/env/manifest.json"))
        } else {
            None
        }
    });
    assert_eq!(
        manifest_env.candidate_manifest,
        Some(PathBuf::from("/env/manifest.json"))
    );
    let env_slots = PinnedBinaryLookup::from_env_with(PIN_A, false, None, &|name| match name {
        VELNOR_WORKFLOW_PINNED_BINARY_ENV => Some(std::ffi::OsString::from("/pin/binary")),
        VELNOR_WORKFLOW_CANDIDATE_BINARY_ENV => Some(std::ffi::OsString::from("/candidate/binary")),
        _ => None,
    });
    assert_eq!(
        env_slots.pinned_binary,
        Some(PathBuf::from("/pin/binary")),
        "the trusted pin slot remains separate from the candidate slot"
    );
    assert_eq!(
        env_slots.candidate_binary,
        Some(PathBuf::from("/candidate/binary")),
        "the candidate uses its dedicated env slot"
    );
    // `CARGO_NET_OFFLINE=true` forbids the build even with `--pin-build`
    // (also pinned end to end by the `--check` CLI subprocess test in
    // `velnor_first_ci.rs`, which owns the child's environment).
    let offline = PinnedBinaryLookup::from_env_with(PIN_A, true, None, &|name| {
        if name == "CARGO_NET_OFFLINE" {
            Some(std::ffi::OsString::from("true"))
        } else {
            None
        }
    });
    assert!(offline.build_forbidden);
}

#[cfg(unix)]
#[test]
fn candidate_render_rejects_a_binary_claiming_another_closure() {
    let (root, head) = closure_fixture("candidate-foreign");
    let wanted = must(
        crate::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let scratch = temporary_directory("candidate-foreign-scratch");
    // Valid binding for the audited tree (manifest closure plus the real
    // digest of the binary bytes), but the binary echoes CLOSURE_A: the
    // `--closure` self-report stays as the final tripwire.
    let binary = fake_candidate_renderer(&root, CLOSURE_A, &head);
    let manifest =
        candidate_manifest_for(&root, "candidate-manifest.json", &binary, &wanted, &head);
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    assert!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &lookup),
            "foreign closure never errors, it simply does not prove",
        )
        .is_none(),
        "a binary reporting another closure is not the tree's candidate"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[test]
fn candidate_render_is_unavailable_without_git_history() {
    let root = temporary_directory("candidate-nogit");
    let scratch = temporary_directory("candidate-nogit-scratch");
    let binary = root.join("missing");
    let lookup = lookup(Some(binary), None, root.join("install"));
    assert!(must(
        render_with_candidate(&root, &root, &scratch, "main", &lookup),
        "no checkout, no candidate",
    )
    .is_none());
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[test]
fn generated_tree_report_distinguishes_pin_candidate_and_stale_main() {
    let pin = generated_tree_report(PIN_A, Ok(TreeComparison::Pin), false);
    assert!(pin.passed);
    assert!(pin.reason.contains(PIN_A), "{}", pin.reason);
    let flight = generated_tree_report(
        PIN_A,
        Ok(TreeComparison::Candidate(CLOSURE_A.to_owned())),
        false,
    );
    assert!(flight.passed, "{:?}", flight.details);
    assert!(flight.reason.contains("in flight"), "{}", flight.reason);
    assert!(flight.reason.contains("after merge"), "{}", flight.reason);
    let stale = generated_tree_report(
        PIN_A,
        Ok(TreeComparison::Candidate(CLOSURE_A.to_owned())),
        true,
    );
    assert!(!stale.passed);
    assert!(
        stale.reason.contains("stale on mainline"),
        "{}",
        stale.reason
    );
    let drift = generated_tree_report(
        PIN_A,
        Ok(TreeComparison::Differences(vec!["ci-pr.yml".to_owned()])),
        false,
    );
    assert!(!drift.passed);
    assert_eq!(drift.details, vec!["ci-pr.yml".to_owned()]);
}

/// A base validator from before the product re-architecture scans the
/// entrypoint for `--rev ` (space) and the revision env only. Every value it
/// can see in a current entrypoint must equal the pin, or the transition
/// itself could never pass policy.
#[test]
fn rendered_entrypoints_pass_the_legacy_space_marker_scan() {
    for repository in [
        crate::workflow_setup_action_repository(),
        "example/consumer",
    ] {
        let job = crate::policy_job(&PolicyJobSpec {
            candidate_artifact_wiring: true,
            name: "Policy",
            revision: PIN_A,
            runner: "ubuntu-24.04",
            repository,
            cache_backend: "github",
            trusted_gate: None,
            default_branch: "main",
            declared_ruleset_contexts: "ci-required,DCO,Policy",
        });
        let mut values = Vec::new();
        for line in job.lines() {
            for marker in ["--rev ", &format!("{BASE_REVISION_ENV}: ")] {
                if let Some(index) = line.find(marker) {
                    let value = line[index + marker.len()..]
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .trim_matches(|character| character == '"' || character == '\\');
                    values.push(value.to_owned());
                }
            }
        }
        assert!(
            !values.is_empty(),
            "the legacy scan must see the pin it enforces for {repository}"
        );
        assert!(
            values.iter().all(|value| value == PIN_A),
            "legacy markers must all name the pin for {repository}: {values:?}"
        );
    }
}

#[test]
fn policy_sibling_setup_action_is_a_reviewed_local_path() {
    assert!(
        is_approved_local_action(crate::VELNOR_WORKFLOW_POLICY_SETUP_ACTION),
        "the owner policy job resolves its setup composite out of the sibling checkout"
    );
    assert!(
        !is_approved_local_action("./policy-setup-action/.github/actions/anything-else"),
        "the sibling allowance is the exact setup composite, never a second local path"
    );
    assert!(
        !is_approved_local_action("./policy-setup-action/.github/workflows/ci-pr.yml"),
        "the sibling checkout carries no reusable workflows"
    );
}

// ---------------------------------------------------------------------------
// Complete-tree comparison
// ---------------------------------------------------------------------------

fn file_entry(content: &str, executable: bool) -> TreeEntry {
    TreeEntry::File {
        bytes: content.as_bytes().to_vec(),
        executable,
    }
}

fn link_entry(target: &str) -> TreeEntry {
    TreeEntry::Symlink {
        target: PathBuf::from(target),
    }
}

fn compare(expected: &TreeEntry, found: Option<&TreeEntry>) -> Vec<String> {
    let mut differences = Vec::new();
    compare_tree_entry(
        Path::new(".github/probe"),
        expected,
        found,
        &mut differences,
    );
    differences
}

#[test]
fn tree_comparison_names_every_drift_class() {
    assert!(
        compare(
            &file_entry("same\n", false),
            Some(&file_entry("same\n", false))
        )
        .is_empty(),
        "identical files compare clean"
    );
    assert!(
        compare(&file_entry("same\n", false), None)
            .join("\n")
            .contains("missing from the tree"),
        "an absent path reports missing"
    );
    assert!(
        compare(&file_entry("a\n", false), Some(&file_entry("b\n", false)))
            .join("\n")
            .contains("differs from the pinned render"),
        "edited bytes report a diff"
    );
    assert!(
        compare(
            &file_entry("same\n", false),
            Some(&file_entry("same\n", true))
        )
        .join("\n")
        .contains("is executable in the tree"),
        "a set execute bit reports mode drift"
    );
    assert!(
        compare(&link_entry("AGENTS.md"), Some(&link_entry("AGENTS.md"))).is_empty(),
        "an identical link compares clean"
    );
    assert!(
        compare(&link_entry("AGENTS.md"), Some(&link_entry("other.md")))
            .join("\n")
            .contains("points at"),
        "a retargeted link reports its target"
    );
    // The type-blindness regression: a regular file with identical bytes
    // must not satisfy a link, and a link must not satisfy a file.
    assert!(
        compare(
            &link_entry("AGENTS.md"),
            Some(&file_entry("AGENTS.md", false))
        )
        .join("\n")
        .contains("is a regular file in the tree but the pinned render has a symlink"),
        "a file squatting a link reports mistyped"
    );
    assert!(
        compare(
            &file_entry("content\n", false),
            Some(&link_entry("content"))
        )
        .join("\n")
        .contains("is a symlink in the tree but the pinned render has a regular file"),
        "a link squatting a file reports mistyped"
    );
    assert!(
        compare(&file_entry("content\n", false), Some(&TreeEntry::Directory))
            .join("\n")
            .contains("is a directory in the tree"),
        "a directory squatting a file reports mistyped"
    );
}

#[cfg(unix)]
#[test]
fn tree_collection_never_follows_symlinks() {
    let root = temporary_directory("tree-collection");
    let outside = temporary_directory("tree-collection-outside");
    write(&outside.join("kept.txt"), "external bytes\n");
    write(&root.join(".github/real.txt"), "real\n");
    must(
        std::os::unix::fs::symlink(outside.join("kept.txt"), root.join(".github/link.txt")),
        "link outside the tree",
    );
    let mut entries = BTreeMap::new();
    must(
        collect_tree_entries(&root, &root.join(".github"), &mut entries),
        "collect the tree",
    );
    assert_eq!(
        entries.get(&PathBuf::from(".github/real.txt")),
        Some(&file_entry("real\n", false)),
        "a regular file collects by bytes"
    );
    let target_path = outside.join("kept.txt");
    let target = must(
        target_path.to_str().ok_or("non-utf8 temp path"),
        "render the link target",
    );
    assert_eq!(
        entries.get(&PathBuf::from(".github/link.txt")),
        Some(&link_entry(target)),
        "a link collects by target, never by the bytes it points at"
    );
    assert!(
        !entries.values().any(|entry| matches!(entry,
            TreeEntry::File { bytes, .. } if bytes == b"external bytes\n")),
        "external bytes must not leak into the collection"
    );
    assert_eq!(
        must(
            stat_tree_entry(&root.join(".github/link.txt")),
            "stat the link"
        ),
        Some(link_entry(target)),
        "a single stat classifies the link itself"
    );
    assert_eq!(
        must(
            stat_tree_entry(&root.join(".github/absent.txt")),
            "stat an absent path"
        ),
        None,
        "an absent path stats as missing"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn stat_rejects_a_symlinked_parent_before_reading_the_child() {
    let root = temporary_directory("tree-parent-symlink");
    let outside = temporary_directory("tree-parent-symlink-outside");
    write(&outside.join("child.txt"), "external bytes\n");
    link(&outside, &root.join("link"));

    assert_eq!(
        must(
            stat_tree_entry(&root.join("link/child.txt")),
            "stat a child below a symlink"
        ),
        None,
        "a symlinked parent is treated as absent; its target bytes never enter the comparison"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
}

#[cfg(unix)]
fn link(target: &Path, link: &Path) {
    must(
        std::os::unix::fs::symlink(target, link),
        "create test symlink",
    );
}

#[cfg(unix)]
#[test]
fn confined_render_symlink_compares_clean() {
    let rendered = temporary_directory("render-symlink-confined");
    let tree = temporary_directory("render-symlink-confined-tree");
    write(&rendered.join(".github/AGENTS.md"), "agents\n");
    write(&tree.join(".github/AGENTS.md"), "agents\n");
    link(Path::new("AGENTS.md"), &rendered.join(".github/link.txt"));
    link(Path::new("AGENTS.md"), &tree.join(".github/link.txt"));
    let differences = must(
        compare_rendered_tree(&rendered, &tree),
        "a confined in-render symlink compares by target",
    );
    assert!(
        differences.is_empty(),
        "confined symlink must compare clean: {differences:?}"
    );
    let _ = fs::remove_dir_all(rendered);
    let _ = fs::remove_dir_all(tree);
}

#[cfg(unix)]
#[test]
fn render_symlink_escaping_its_root_is_an_error() {
    let rendered = temporary_directory("render-symlink-escape");
    let outside = temporary_directory("render-symlink-escape-outside");
    let secret = outside.join("secret.txt");
    write(&secret, "outside\n");
    write(&rendered.join(".github/AGENTS.md"), "agents\n");
    link(&secret, &rendered.join(".github/link.txt"));
    let tree = temporary_directory("render-symlink-escape-tree");
    let error = must_fail(
        compare_rendered_tree(&rendered, &tree),
        "a render symlink outside its root",
    )
    .to_string();
    assert!(error.contains("escapes its root"), "{error}");
    let _ = fs::remove_dir_all(rendered);
    let _ = fs::remove_dir_all(outside);
    let _ = fs::remove_dir_all(tree);
}

#[cfg(unix)]
#[test]
fn dangling_render_symlink_is_an_error() {
    let rendered = temporary_directory("render-symlink-dangling");
    write(&rendered.join(".github/AGENTS.md"), "agents\n");
    link(Path::new("MISSING.md"), &rendered.join(".github/link.txt"));
    let tree = temporary_directory("render-symlink-dangling-tree");
    let error = must_fail(
        compare_rendered_tree(&rendered, &tree),
        "a dangling render symlink",
    )
    .to_string();
    assert!(error.contains("dangles"), "{error}");
    let _ = fs::remove_dir_all(rendered);
    let _ = fs::remove_dir_all(tree);
}

#[cfg(unix)]
#[test]
fn render_symlink_to_directory_is_an_error() {
    let rendered = temporary_directory("render-symlink-directory");
    write(&rendered.join(".github/AGENTS.md"), "agents\n");
    must(
        fs::create_dir_all(rendered.join(".github/nested")),
        "create in-render directory",
    );
    link(Path::new("nested"), &rendered.join(".github/link.txt"));
    let tree = temporary_directory("render-symlink-directory-tree");
    let error = must_fail(
        compare_rendered_tree(&rendered, &tree),
        "a render symlink to a directory",
    )
    .to_string();
    assert!(error.contains("does not name a file"), "{error}");
    let _ = fs::remove_dir_all(rendered);
    let _ = fs::remove_dir_all(tree);
}

fn declared_tree(required_checks: &[&str]) -> DeclaredTree {
    DeclaredTree {
        pin: None,
        repository: None,
        default_branch: "main".to_owned(),
        velnor_policy: VelnorPolicyContract::default(),
        required_checks: required_checks
            .iter()
            .map(|context| (*context).to_owned())
            .collect(),
        external_checks: Vec::new(),
    }
}

const REQUIRED_CHECKS_PR_AGGREGATE: &str = "jobs:\n  ci-required:\n    name: ci-required\n";
const REQUIRED_CHECKS_POLICY_ENTRYPOINT: &str = "jobs:\n  policy:\n    name: Policy\n";

fn write_required_checks_fixture(root: &Path, entrypoint: bool) {
    write(
        &root.join(PULL_REQUEST_AGGREGATE),
        REQUIRED_CHECKS_PR_AGGREGATE,
    );
    if entrypoint {
        write(
            &root.join(POLICY_ENTRYPOINT),
            REQUIRED_CHECKS_POLICY_ENTRYPOINT,
        );
    }
}

#[test]
fn required_checks_accepts_policy_from_the_entrypoint() {
    let root = temporary_directory("required-checks-policy");
    write_required_checks_fixture(&root, true);
    let declared = declared_tree(&["ci-required", "Policy"]);
    let report = required_checks(&root, &declared, None);
    assert!(
        report.passed,
        "{}: {}",
        report.reason,
        report.details.join("; ")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn required_checks_rejects_contexts_absent_from_both_workflows() {
    let root = temporary_directory("required-checks-bogus");
    write_required_checks_fixture(&root, true);
    let declared = declared_tree(&["ci-required", "Policy", "bogus-context"]);
    let report = required_checks(&root, &declared, None);
    assert!(!report.passed, "a bogus context must fail the rule");
    assert!(
        report
            .details
            .iter()
            .any(|finding| finding.contains("bogus-context")),
        "the rule names the absent context: {}",
        report.details.join("; ")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn required_checks_has_no_hard_coded_policy_exemption() {
    let root = temporary_directory("required-checks-no-entrypoint");
    write_required_checks_fixture(&root, false);
    let declared = declared_tree(&["Policy"]);
    let report = required_checks(&root, &declared, None);
    assert!(
        !report.passed,
        "Policy without a rendered entrypoint must fail the rule"
    );
    assert!(
        report
            .details
            .iter()
            .any(|finding| finding.contains("Policy")),
        "the rule names the absent context: {}",
        report.details.join("; ")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn required_checks_passes_when_live_matches_declared_plus_entrypoint() {
    let root = temporary_directory("required-checks-live");
    write_required_checks_fixture(&root, true);
    let mut declared = declared_tree(&["ci-required", "Policy"]);
    declared.external_checks = vec!["DCO".to_owned()];
    let live = vec![
        "ci-required".to_owned(),
        "DCO".to_owned(),
        "Policy".to_owned(),
    ];
    let report = required_checks(&root, &declared, Some(&live));
    assert!(
        report.passed,
        "{}: {}",
        report.reason,
        report.details.join("; ")
    );
    let _ = fs::remove_dir_all(root);
}

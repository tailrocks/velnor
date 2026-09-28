#![expect(
    clippy::panic,
    reason = "tests need setup failures to name their root cause"
)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::s2::{PolicyJobSpec, ProjectConfig};

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

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        must(fs::create_dir_all(parent), "create parent directory");
    }
    must(fs::write(path, content), "write file");
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

#[cfg(unix)]
fn private_snapshot_destination(name: &str) -> (PathBuf, PathBuf) {
    let workspace = temporary_directory(name);
    must(
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)),
        "protect snapshot workspace",
    );
    let destination = workspace.join("source");
    (workspace, destination)
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
        pin_source(root, Some(crate::s2::workflow_setup_action_repository())),
        PinSource::Checkout(root.to_path_buf())
    );
    assert_eq!(
        pin_source(root, Some("example/consumer")),
        PinSource::Remote(crate::s2::VELNOR_WORKFLOW_INSTALL_GIT_URL.to_owned())
    );
    assert_eq!(
        pin_source(root, None),
        PinSource::Remote(crate::s2::VELNOR_WORKFLOW_INSTALL_GIT_URL.to_owned())
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
        "schema = 2\n\n[generator]\nrepository = \"{}\"\nrevision = \"{revision}\"\n",
        crate::s2::workflow_setup_action_repository()
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
fn removed_static_row_fixture(name: &str) -> PathBuf {
    let root = temporary_directory(name);
    git_ok(&root, &["init", "-q", "-b", "main"]);
    write(
        &root.join(GENERATION_CONFIG),
        "schema = 2\n\n[workflow]\nfiles = [\"ci-policy.yml\"]\n\n[[static_files]]\nfile = \".github/workflows/ci-static.yml\"\nsource = \".github-gen/ci-static.yml\"\n",
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
    let _ = commit(&root, "base owns static workflow");

    write(
        &root.join(GENERATION_CONFIG),
        "schema = 2\n\n[workflow]\nfiles = [\"ci-policy.yml\"]\n\n[policy]\nexclude_workflows = [\"ci-ſtatic.yml\"]\n",
    );
    write(
        &root.join(".github/ci/.github-actions-generator-state"),
        &ownership_state_with_outputs(&[]),
    );
    let _ = commit(
        &root,
        "candidate removes static row and adds retired policy field",
    );
    root
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
        crate::s2::scan::scan_shape(
            &fixture,
            &crate::s2::provider::ProviderId::ALL.into_iter().collect(),
            "main",
            &[],
            &crate::s2::scan::rust::AppleNativePolicy::default(),
        ),
        "scan entrypoint fixture",
    );
    let _ = fs::remove_dir_all(fixture);
    let mut config = ProjectConfig::from(shape);
    revision.clone_into(&mut config.workflow_revision);
    must(
        crate::s2::render_policy_entrypoint(&config),
        "render policy entrypoint",
    )
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
        entrypoint.contains("references no secrets, persists no credentials, and never compiles."),
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
    let job = crate::s2::policy_job(&PolicyJobSpec {
        candidate_artifact_wiring: true,
        name: "Policy",
        revision: PIN_A,
        runner: "ubuntu-24.04",
        repository: crate::s2::workflow_setup_action_repository(),
        cache_backend: "github",
        trusted_gate: None,
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,Policy",
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
    let job = crate::s2::policy_job(&PolicyJobSpec {
        candidate_artifact_wiring: true,
        name: "Policy",
        revision: PIN_A,
        runner: "ubuntu-24.04",
        repository: crate::s2::workflow_setup_action_repository(),
        cache_backend: "github",
        trusted_gate: None,
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,Policy",
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
    let job = crate::s2::policy_job(&PolicyJobSpec {
        candidate_artifact_wiring: true,
        name: "Policy",
        revision: PIN_A,
        runner: "ubuntu-24.04",
        repository: crate::s2::workflow_setup_action_repository(),
        cache_backend: "github",
        trusted_gate: None,
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,Policy",
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
    let job = crate::s2::policy_job(&PolicyJobSpec {
        candidate_artifact_wiring: true,
        name: "Policy",
        revision: PIN_A,
        runner: "ubuntu-24.04",
        repository: crate::s2::workflow_setup_action_repository(),
        cache_backend: "github",
        trusted_gate: None,
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,Policy",
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
    let job = crate::s2::policy_job(&PolicyJobSpec {
        candidate_artifact_wiring: true,
        name: "Policy",
        revision: PIN_A,
        runner: "[self-hosted, velnor]",
        repository: crate::s2::regen_repository_marker(),
        cache_backend: "local",
        trusted_gate: Some(&crate::s2::control_plane_trusted_gate("main")),
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,Policy",
    });
    assert!(!job.contains("--pin-build"), "{job}");
    assert!(job.contains("    if: ${{ github.event_name == 'pull_request_target' ||"));
    assert!(!job.contains("--ruleset-contexts"), "{job}");
}

// ---------------------------------------------------------------------------
// Semantic rules over a synthetic tree
// ---------------------------------------------------------------------------

/// The Velnor selector the synthetic trees declare: the labels are the
/// tree's own routing, matched by set equality against `runs-on`.
const VELNOR_SELECTOR: &str = "example-velnor";

fn velnor_tree(name: &str, pr_workflow: &str) -> PathBuf {
    let root = temporary_directory(name);
    write(
        &root.join(GENERATION_CONFIG),
        &format!(
            "schema = 2\n\n[generator]\nrepository = \"example/consumer\"\n\n[workflow]\nproviders = [\"github-hosted\", \"velnor\"]\nautomatic_providers = [\"github-hosted\", \"velnor\"]\ndefault_branch = \"main\"\n\n[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n[workflow.selectors.velnor]\nruns_on = [\"{VELNOR_SELECTOR}\"]\n"
        ),
    );
    write(
        &root.join(RUNTIME_CONFIG),
        "schema = 3\nrepository = \"example/consumer\"\nprofile = \"generic\"\nverified = true\ndefault_branch = \"main\"\nproviders = [\"github-hosted\", \"velnor\"]\nautomatic_providers = [\"github-hosted\", \"velnor\"]\ndefault_dispatch_providers = [\"github-hosted\", \"velnor\"]\n",
    );
    write(&root.join(PULL_REQUEST_AGGREGATE), pr_workflow);
    write(&root.join(POLICY_ENTRYPOINT), &hosted_entrypoint(PIN_A));
    root
}

/// A pull-request aggregate with a hosted required job and one Velnor job
/// on the declared selector, carrying the generated provider admission: a
/// provider-selecting dispatch on any ref or the automatic events, with the
/// trusted-event conjunct.
fn gated_trusted_job() -> String {
    format!(
        "name: CI / PR\non:\n  pull_request:\njobs:\n  ci-required:\n    name: ci-required\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ok\n  velnor-docker:\n    name: Docker\n    if: ${{{{ (!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot'))) }}}}\n    runs-on: [{VELNOR_SELECTOR}]\n    steps:\n      - run: echo trusted\n"
    )
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
                && detail.contains("local-provider jobs require a trusted-event gate")
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
fn every_workflow_gets_full_semantic_audit_checks() {
    let root = velnor_tree("workflow-audit-s2", &gated_trusted_job());
    let generation_config = root.join(GENERATION_CONFIG);
    let base = must(
        fs::read_to_string(&generation_config),
        "read generation config",
    );
    write(
        &generation_config,
        &format!(
            "{base}\n[[static_files]]\nfile = \".github/workflows/ci-static.yml\"\nsource = \".github-gen/ci-static.yml\"\n"
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
    for workflow in ["ci-static.yml", "ci-policy.yml", "ci-old.yml"] {
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
        "a non-entrypoint pull_request_target workflow must fail: {:?}",
        audit.pull_request_target
    );
    assert!(
        audit.structure.iter().any(|finding| {
            finding.contains("ci-invalid.yml") && finding.contains("parse workflow")
        }),
        "a malformed workflow must fail closed: {:?}",
        audit.structure
    );
    assert!(
        !audit
            .pull_request_target
            .iter()
            .any(|finding| finding.contains("ci-policy.yml")),
        "the policy entrypoint alone is admitted to pull_request_target"
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn workflow_audit_rejects_symlinked_roots_and_files() {
    use std::os::unix::fs::symlink;

    let root = velnor_tree("workflow-audit-symlink-github-s2", &gated_trusted_job());
    let target = temporary_directory("workflow-audit-symlink-github-target-s2");
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

    let root = velnor_tree("workflow-audit-symlink-workflows-s2", &gated_trusted_job());
    let target = temporary_directory("workflow-audit-symlink-workflows-target-s2");
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

    let root = velnor_tree("workflow-audit-symlink-file-s2", &gated_trusted_job());
    let target = temporary_directory("workflow-audit-symlink-file-target-s2");
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
    assert!(error.contains("escapes the repository"), "{error}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(target);
}

#[cfg(unix)]
#[test]
fn declared_pin_rejects_a_symlinked_policy_entrypoint() {
    use std::os::unix::fs::symlink;

    let root = velnor_tree("declared-pin-symlink-entrypoint-s2", &gated_trusted_job());
    let target = temporary_directory("declared-pin-symlink-entrypoint-target-s2");
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
    assert!(error.contains("escapes the repository"), "{error}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(target);
}

#[test]
fn policy_preflight_rejects_a_missing_policy_entrypoint() {
    let root = velnor_tree("policy-entrypoint-preflight-s2", &gated_trusted_job());
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
        "policy-owned-workflow-static-overrides-s2",
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
            crate::s2::workflow_setup_action_repository()
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

/// The generated provider gate is the bare trusted-event conjunct for an
/// automatic provider, conjoined with the dispatch predicate for a manual
/// one: a dispatch selects the static universe on any ref — dispatch
/// authorship is write-authorized — and no input match survives.
#[test]
fn provider_gate_admits_dispatch_on_any_ref() {
    let trusted = "(!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot')))";
    // An automatic provider renders the bare trusted-event conjunct.
    let automatic = format!("${{{{ {trusted} }}}}");
    assert!(
        is_generated_provider_gate(&automatic, "velnor"),
        "provider gate admitted: {automatic}"
    );
    // A manual provider conjoins it with the dispatch predicate: dispatches
    // select the static universe on any ref, with no input to match.
    let manual = format!("${{{{ (github.event_name == 'workflow_dispatch') && {trusted} }}}}");
    assert!(
        is_generated_provider_gate(&manual, "velnor"),
        "provider gate admitted: {manual}"
    );
    // An input-matching gate is not generated: no input selects providers.
    let input_match = format!(
        "${{{{ ((github.event_name == 'workflow_dispatch' && contains(format(',{{0}},', github.event.inputs.providers), ',velnor,')) || (github.event_name != 'workflow_dispatch')) && {trusted} }}}}",
    );
    assert!(
        !is_generated_provider_gate(&input_match, "velnor"),
        "an input-matching gate is not generated: {input_match}"
    );
}

/// A top-level conjunction carrying the exact trusted-event predicate
/// passes whatever the functional side narrows; near-misses fail: a
/// top-level `||` widens past the conjunct, and an inexact predicate is
/// not the predicate.
#[test]
fn trusted_conjunct_members_pass_and_near_misses_fail() {
    let trusted = "(!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot')))";
    for gate in [
        format!("${{{{ {trusted} }}}}"),
        format!("${{{{ (needs.verify.outputs.mode == 'publish') && {trusted} }}}}"),
        format!("${{{{ {trusted} && (needs.verify.outputs.mode == 'publish') }}}}"),
        format!(
            "${{{{ (github.ref == 'refs/heads/main' && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')) && {trusted} }}}}"
        ),
    ] {
        assert!(
            has_exact_trusted_conjunct(&gate),
            "a conjunction carrying the predicate passes: {gate}"
        );
    }
    for gate in [
        // A top-level disjunction admits whatever the other side admits.
        format!("${{{{ (needs.verify.outputs.mode == 'publish') || {trusted} }}}}"),
        format!("${{{{ {trusted} && (needs.verify.outputs.mode == 'publish') || (github.event_name == 'push') }}}}"),
        // Dropped Bot clause: not the predicate.
        "${{ (!(github.event_name == 'pull_request' && github.event.pull_request.head.repo.fork)) }}".to_owned(),
        // Double-wrapped: not the generated spelling.
        format!("${{{{ (({trusted})) && (needs.verify.outputs.mode == 'publish') }}}}"),
        // No trusted conjunct at all.
        "${{ (needs.verify.outputs.mode == 'publish') }}".to_owned(),
    ] {
        assert!(
            !has_exact_trusted_conjunct(&gate),
            "a near-miss fails closed: {gate}"
        );
    }
}

/// The maintenance prune admission is a top-level `||` over `pull_request`,
/// so the bare gate fails closed on a local lane; parenthesized and
/// conjoined with the trusted-event predicate — the shape the renderer
/// emits for local maintenance — it passes.
#[test]
fn maintenance_prune_gate_needs_the_trusted_conjunct_on_local_lanes() {
    let functional = "github.event_name == 'pull_request' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main' && inputs.pull_request_number != '')";
    let trusted = "(!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot')))";
    let bare = format!("${{{{ {functional} }}}}");
    assert!(
        !has_exact_trusted_conjunct(&bare),
        "the bare prune admission fails closed: {bare}"
    );
    let gated = format!("${{{{ ({functional}) && {trusted} }}}}");
    assert!(
        has_exact_trusted_conjunct(&gated),
        "the conjoined prune gate passes: {gated}"
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
        "name: Allowed\non: push\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    strategy:\n      matrix:\n        target: [a, b]\n    env:\n      TARGET: ${{ matrix.target }}\n      PARALLEL: ${{ strategy.job-index }}\n      VERSION: ${{ needs.identity.outputs.version }}\n      REPO: ${{ github.repository }}\n      SECRET: ${{ secrets.MY_SECRET }}\n      CONFIG: ${{ vars.MY_VAR }}\n      INPUT: ${{ inputs.my_input }}\n      SELECTOR: ${{ github.event.inputs.providers }}\n    steps:\n      - id: first\n        run: echo ok\n      - run: echo ok\n        env:\n          TMP: ${{ runner.temp }}\n          PREV: ${{ steps.first.outputs.value }}\n",
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
                "repository": crate::s2::workflow_setup_action_repository(),
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
            format!(
                "#!/bin/sh\ntouch '{sentinel}'\nif [ \"$1\" = --revision ]; then echo {}; exit 0; fi\nif [ \"$1\" = --closure ]; then echo {closure}; exit 0; fi\ncp -R \"$1/.\" \"$3/\"\n",
                crate::s2::GENERATOR_REVISION
            ),
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
                "#!/bin/sh\n/bin/sleep 60 &\nprotected='{protected}'\npipe_probe() {{\n  dd if=/dev/zero of=\"/proc/$PPID/fd/1\" bs=1M count=16 status=none 2>/dev/null || true\n  dd if=/dev/zero of=\"/proc/$PPID/fd/2\" bs=1M count=16 status=none 2>/dev/null || true\n}}\nprobe() {{\n  if [ -r \"$protected\" ]; then cat \"$protected\"; else printf '%s\\n' \"$2\"; fi\n  mkdir -p \"$(dirname \"$protected\")\" 2>/dev/null || true\n  printf '%s\\n' 'candidate mutation' > \"$protected\" 2>/dev/null || true\n}}\nif [ \"$1\" = --revision ]; then pipe_probe; probe unused {revision}; exit 0; fi\nif [ \"$1\" = --closure ]; then pipe_probe; probe unused {closure}; exit 0; fi\nroot=\"$1\"; shift\n[ \"$1\" = --output ] || exit 3\noutput=\"$2\"\npipe_probe\nmkdir -p \"$output\"\nif [ -r \"$protected\" ]; then cat \"$protected\" > \"$output/leaked-secret.txt\"; fi\nmkdir -p \"$(dirname \"$protected\")\" 2>/dev/null || true\nprintf '%s\\n' 'candidate mutation' > \"$protected\" 2>/dev/null || true\ncp -R \"$root/.github\" \"$output/\"\n"
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
    let (root, head) = closure_fixture("candidate-late-descendant-s2");
    let wanted = must(
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let scratch = temporary_directory("candidate-late-descendant-s2-scratch");
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
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
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
    let candidate_snapshot = scratch.with_file_name(format!(
        "{}-candidate-source",
        scratch
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("policy-render")
    ));
    assert!(
        !candidate_snapshot.exists(),
        "sealed candidate source must be removed after a successful render"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn candidate_render_cannot_mutate_authoritative_checkout() {
    let (root, head) = closure_fixture("candidate-immutable-source");
    let wanted = must(
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
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
        crate::s2::closure::candidate_closure_of_tree(&root, &pr_head),
        "PR head candidate closure",
    );
    let merge_closure = must(
        crate::s2::closure::candidate_closure_of_tree(&root, &merge),
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
        crate::s2::closure::candidate_closure_of_tree(&origin, &pr_head),
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
        crate::s2::closure::candidate_closure_of_tree(&root, &pr_head),
        "PR head candidate closure",
    );
    let merge_closure = must(
        crate::s2::closure::candidate_closure_of_tree(&root, &merge),
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
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
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
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
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
    assert!(
        !sentinel.exists(),
        "candidate render must not inherit ambient HOME or Actions credentials"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn candidate_probe_and_render_cannot_read_or_mutate_absolute_checkout_paths() {
    let (root, head) = closure_fixture("candidate-absolute-path-sandbox");
    let wanted = must(
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
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
fn evaluate_rejects_a_symlinked_checkout_before_policy_reads() {
    use std::os::unix::fs::symlink;

    let target = velnor_tree("evaluate-symlinked-root-target-s2", &gated_trusted_job());
    let link = temporary_directory("evaluate-symlinked-root-link-s2");
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

#[cfg(unix)]
#[test]
fn immutable_snapshot_is_bound_to_the_requested_revision() {
    let (root, first) = closure_fixture("snapshot-revision-binding-s2");
    write(
        &root.join("crates/velnor-workflow/src/lib.rs"),
        "pub fn f() { println!(\"new\"); }\n",
    );
    write(&root.join("new-at-head.txt"), "new\n");
    let second = commit(&root, "second fixture revision");
    assert_ne!(first, second);

    let (snapshot_workspace, snapshot) =
        private_snapshot_destination("snapshot-revision-binding-s2-output");
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
    let _ = fs::remove_dir_all(snapshot_workspace);
}

#[cfg(unix)]
#[test]
fn immutable_snapshot_rejects_a_symlink_destination() {
    use std::os::unix::fs::symlink;

    let (root, head) = closure_fixture("snapshot-symlink-destination-s2");
    let target = temporary_directory("snapshot-symlink-destination-target-s2");
    let (destination_workspace, destination) =
        private_snapshot_destination("snapshot-symlink-destination-link-s2");
    must(symlink(&target, &destination), "create symlink destination");
    let error = must_fail(
        immutable_git_snapshot(&root, &head, &destination),
        "snapshot must reject a symlink destination",
    )
    .to_string();
    assert!(error.contains("destination already exists"), "{error}");
    assert!(
        fs::read_dir(&target).is_ok_and(|mut entries| entries.next().is_none()),
        "a rejected symlink destination must not receive archive contents"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_file(destination);
    let _ = fs::remove_dir_all(target);
    let _ = fs::remove_dir_all(destination_workspace);
}

#[cfg(unix)]
#[test]
fn immutable_snapshot_publishes_nothing_when_preflight_fails() {
    use std::os::unix::fs::symlink;

    let (root, _) = closure_fixture("snapshot-atomic-failure-s2");
    let outside = temporary_directory("snapshot-atomic-failure-outside-s2");
    write(&outside.join("secret.txt"), "outside\n");
    must(
        symlink(outside.join("secret.txt"), root.join("external-link.txt")),
        "create external tracked symlink",
    );
    let head = commit(&root, "external symlink fixture");
    let (destination_workspace, destination) =
        private_snapshot_destination("snapshot-atomic-failure-output-s2");
    let error = must_fail(
        immutable_git_snapshot(&root, &head, &destination),
        "snapshot preflight must reject the external symlink",
    )
    .to_string();
    assert!(error.contains("escapes the immutable snapshot"), "{error}");
    assert!(
        !destination.exists(),
        "failed preflight must not publish the final snapshot"
    );
    let destination_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("snapshot");
    let staging_prefix = format!(".{destination_name}-staging-");
    assert!(
        !must(
            fs::read_dir(destination.parent().unwrap_or_else(|| Path::new("."))),
            "inspect snapshot parent after failure",
        )
        .any(|entry| {
            entry
                .ok()
                .and_then(|entry| entry.file_name().into_string().ok())
                .is_some_and(|name| name.starts_with(&staging_prefix))
        }),
        "failed preflight must clean its private staging directory"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
    let _ = fs::remove_dir_all(destination_workspace);
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
fn candidate_snapshot_preflight_allows_confined_directory_links_and_seals_tree() {
    use std::os::unix::fs::symlink;

    let root = temporary_directory("snapshot-confined-directory-link");
    write(&root.join("target/config.toml"), "key = true\n");
    must(
        symlink("target", root.join("alias")),
        "create confined directory symlink",
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
    must(
        fs::set_permissions(root.join("target"), fs::Permissions::from_mode(0o755)),
        "reopen test directory for cleanup",
    );
    must(
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)),
        "reopen test root for cleanup",
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn candidate_sandbox_uid_can_read_sealed_workflow_config_but_cannot_write_it() {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt as _;

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

    remove_snapshot_tree(&root);
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
        "vw-s2-special-{}-{}",
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
fn lying_binary_whose_digest_misses_the_manifest_is_rejected() {
    let (root, head) = closure_fixture("candidate-lying");
    let wanted = must(
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
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
fn candidate_manifest_for_another_tree_is_rejected() {
    let (root, head) = closure_fixture("candidate-other-tree");
    let wanted = must(
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
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
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
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
    assert!(
        !sentinel.exists(),
        "an unbound env-slot binary is skipped, never executed"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[cfg(unix)]
#[test]
fn malformed_candidate_manifest_is_rejected() {
    let (root, head) = closure_fixture("candidate-malformed");
    let wanted = must(
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
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
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
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
        crate::s2::workflow_setup_action_repository(),
        "example/consumer",
    ] {
        let job = crate::s2::policy_job(&PolicyJobSpec {
            candidate_artifact_wiring: true,
            name: "Policy",
            revision: PIN_A,
            runner: "ubuntu-24.04",
            repository,
            cache_backend: "github",
            trusted_gate: None,
            default_branch: "main",
            declared_ruleset_contexts: "ci-required,Policy",
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
        is_approved_local_action(crate::s2::VELNOR_WORKFLOW_POLICY_SETUP_ACTION),
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
    assert!(
        is_approved_local_action(crate::s2::VELNOR_WORKFLOW_SOURCE_SETUP_ACTION),
        "the owner package publisher resolves setup from its exact source checkout"
    );
    assert!(
        !is_approved_local_action("./source/.github/actions/anything-else"),
        "the source checkout allowance is the exact setup composite"
    );
    assert!(
        !is_approved_local_action("./other-source/.github/actions/setup-velnor-workflow"),
        "other checkout paths are not implicitly trusted"
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
fn removed_static_row_with_retired_exclusion_field_is_rejected_before_render() {
    let root = removed_static_row_fixture("removed-static-row-s2");
    let candidate_config = must(
        fs::read_to_string(root.join(GENERATION_CONFIG)),
        "read candidate config",
    );
    assert!(!candidate_config.contains("[[static_files]]"));
    assert!(candidate_config.contains("exclude_workflows = [\"ci-ſtatic.yml\"]"));
    assert!(root.join(".github/workflows/ci-static.yml").is_file());
    let error = must_fail(
        DeclaredTree::read(&root),
        "reject the retired policy exclusion before any renderer runs",
    );
    let message = error.to_string();
    assert!(message.contains("unknown field"), "{message}");
    assert!(message.contains("exclude_workflows"), "{message}");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn actual_tree_collection_never_follows_symlinks() {
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
        collect_actual_tree_entries(&root, &root.join(".github"), &mut entries),
        "collect the tree",
    );
    let target_path = outside.join("kept.txt");
    let target = must(
        target_path.to_str().ok_or("non-utf8 temp path"),
        "render the link target",
    );
    assert_eq!(
        entries.get(&PathBuf::from(".github/real.txt")),
        Some(&file_entry("real\n", false)),
        "a regular file collects by bytes"
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

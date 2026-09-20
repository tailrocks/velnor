#![expect(
    clippy::panic,
    reason = "tests need setup failures to name their root cause"
)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::s2::{PolicyJobSpec, ProjectConfig, VELNOR_WORKFLOW_PINNED_CLOSURE_ENV};

type SafeRoot = super::super::safe_fs::SafeRoot;

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

fn safe_root(path: &Path) -> SafeRoot {
    must(SafeRoot::open(path), "capture policy test root")
}

fn read_declared_tree(
    root: &SafeRoot,
    trusted_repository: Option<&str>,
    trusted_default_branch: Option<&str>,
) -> Result<DeclaredTree, GeneratorError> {
    let generation = required_generation_config_with_safe_root(root)?;
    DeclaredTree::read_with_safe_root(root, generation, trusted_repository, trusted_default_branch)
}

fn read_configured_policy(
    root: &SafeRoot,
    trusted_repository: Option<&str>,
    trusted_default_branch: Option<&str>,
) -> Result<VelnorPolicyContract, GeneratorError> {
    let generation = generation_config_with_safe_root(root)?;
    configured_velnor_policy_with_safe_root(
        root,
        generation.as_ref(),
        trusted_repository,
        trusted_default_branch,
    )
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

fn fake_velnor_workflow(directory: &Path, revision: &str, closure: &str, marker: &Path) -> PathBuf {
    must(
        fs::create_dir_all(directory),
        "create fake binary directory",
    );
    let binary = directory.join("velnor-workflow");
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\n: > '{}'\nif [ \"$1\" = --revision ]; then echo {revision}; exit 0; fi\nif [ \"$1\" = --closure ]; then echo {closure}; exit 0; fi\nexit 2\n",
                marker.display()
            ),
        ),
        "write fake velnor-workflow",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark fake velnor-workflow executable",
        );
    }
    binary
}

fn lookup(install_root: PathBuf) -> PinnedBinaryLookup {
    PinnedBinaryLookup {
        provisioned_binary: None,
        provisioned_closure: None,
        install_root,
        build_forbidden: true,
    }
}

fn provisioned_lookup(binary: PathBuf, closure: &str, install_root: PathBuf) -> PinnedBinaryLookup {
    PinnedBinaryLookup {
        provisioned_binary: Some(binary),
        provisioned_closure: Some(closure.to_owned()),
        install_root,
        build_forbidden: true,
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
fn provisioned_binary_is_selected_only_with_a_matching_source_closure() {
    const CHILD_ROOT_ENV: &str = "VELNOR_TEST_PROVISIONED_BINARY_ROOT";
    if let Some(root) = env::var_os(CHILD_ROOT_ENV).map(PathBuf::from) {
        let lookup = PinnedBinaryLookup::from_env(PIN_A, false);
        let expected = [CLOSURE_A.to_owned()];
        let resolved = must(
            resolve_pinned_binary(PIN_A, &expected, &lookup, &checkout_source(&root)),
            "trusted provisioned renderer with matching source closure",
        );
        assert_eq!(
            resolved,
            PathBuf::from(
                env::var_os(VELNOR_WORKFLOW_PINNED_BINARY_ENV).expect("provisioned binary"),
            ),
            "the explicitly provisioned path is selected"
        );
        assert!(
            root.join("provisioned-binary-ran").exists(),
            "the provisioned executable reports its closure"
        );
        return;
    }

    let root = temporary_directory("provisioned-matching-closure");
    let marker = root.join("provisioned-binary-ran");
    let binary = fake_velnor_workflow(&root.join("bin"), PIN_A, CLOSURE_A, &marker);
    let test_binary = must(env::current_exe(), "locate policy test binary");
    let child = Command::new(test_binary)
        .arg("--exact")
        .arg(
            "s2::policy::tests::provisioned_binary_is_selected_only_with_a_matching_source_closure",
        )
        .arg("--nocapture")
        .env(CHILD_ROOT_ENV, &root)
        .env(VELNOR_WORKFLOW_PINNED_BINARY_ENV, &binary)
        .env(VELNOR_WORKFLOW_PINNED_CLOSURE_ENV, CLOSURE_A)
        .env("RUNNER_TEMP", root.join("runner-temp"))
        .env("PATH", root.join("empty-path"))
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .expect("spawn isolated policy test child");
    assert!(
        child.status.success(),
        "child selected the provisioned binary after validating its closure: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(
        marker.exists(),
        "the provisioned binary's closure was checked"
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn mismatched_provisioned_closure_is_rejected_before_binary_execution() {
    let root = temporary_directory("provisioned-mismatched-closure");
    let marker = root.join("untrusted-provisioned-binary-ran");
    let binary = fake_velnor_workflow(&root.join("bin"), PIN_A, CLOSURE_A, &marker);
    let lookup = provisioned_lookup(binary, CLOSURE_B, root.join("install"));
    let expected = [CLOSURE_A.to_owned()];
    let error = must_fail(
        resolve_pinned_binary(PIN_A, &expected, &lookup, &checkout_source(&root)),
        "mismatched provisioned closure",
    )
    .to_string();
    assert!(
        error.contains("does not identify declared generator pin"),
        "{error}"
    );
    assert!(
        !marker.exists(),
        "an untrusted closure is rejected before its executable is spawned"
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn path_candidates_are_ignored_when_resolving_the_pin() {
    const CHILD_ROOT_ENV: &str = "VELNOR_TEST_PINNED_PATH_ROOT";
    if let Some(root) = env::var_os(CHILD_ROOT_ENV).map(PathBuf::from) {
        let lookup = PinnedBinaryLookup::from_env(PIN_A, false);
        let expected = [CLOSURE_A.to_owned()];
        let error = must_fail(
            resolve_pinned_binary(PIN_A, &expected, &lookup, &checkout_source(&root)),
            "PATH candidate must not replace the trusted current process",
        )
        .to_string();
        assert!(error.contains("no trusted renderer matches pin"), "{error}");
        assert!(error.contains("--pin-build"), "{error}");
        assert!(
            !root.join("path-candidate-ran").exists(),
            "a PATH candidate is never spawned"
        );
        return;
    }

    let root = temporary_directory("pinned-path-ignored");
    let candidate_dir = root.join("candidate");
    let marker = root.join("path-candidate-ran");
    let _candidate = fake_velnor_workflow(&candidate_dir, PIN_A, CLOSURE_A, &marker);
    let test_binary = must(env::current_exe(), "locate policy test binary");
    let child = Command::new(test_binary)
        .arg("--exact")
        .arg("s2::policy::tests::path_candidates_are_ignored_when_resolving_the_pin")
        .arg("--nocapture")
        .env(CHILD_ROOT_ENV, &root)
        .env_remove(VELNOR_WORKFLOW_PINNED_BINARY_ENV)
        .env_remove(VELNOR_WORKFLOW_PINNED_CLOSURE_ENV)
        .env("RUNNER_TEMP", root.join("runner-temp"))
        .env("PATH", &candidate_dir)
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .expect("spawn isolated policy test child");
    assert!(
        child.status.success(),
        "the resolver ignores PATH without a provisioned identity: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(!marker.exists(), "the PATH candidate was never spawned");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn forbidden_build_fails_closed_without_inspecting_candidates() {
    let root = temporary_directory("pinned-offline");
    let stale_dir = root.join("stale");
    must(fs::create_dir_all(&stale_dir), "stale dir");
    let stale_marker = root.join("stale-candidate-ran");
    let stale = fake_velnor_workflow(&stale_dir, PIN_B, CLOSURE_B, &stale_marker);
    let install_root = root.join("install");
    must(fs::create_dir_all(install_root.join("bin")), "install bin");
    let installed_marker = root.join("installed-candidate-ran");
    let installed = fake_velnor_workflow(
        &install_root.join("bin"),
        PIN_B,
        CLOSURE_B,
        &installed_marker,
    );
    let lookup = lookup(install_root);
    let expected = [CLOSURE_A.to_owned()];
    let error = must_fail(
        resolve_pinned_binary(PIN_A, &expected, &lookup, &checkout_source(&root)),
        "forbidden build miss",
    )
    .to_string();
    assert!(error.contains("building is forbidden here"), "{error}");
    assert!(error.contains(PIN_A), "{error}");
    assert!(
        error.contains("--pin-build"),
        "the fail-closed message names the local-development escape hatch: {error}"
    );
    assert!(!error.contains(&stale.display().to_string()), "{error}");
    assert!(!error.contains(&installed.display().to_string()), "{error}");
    assert!(!stale_marker.exists(), "PATH candidate was never spawned");
    assert!(
        !installed_marker.exists(),
        "cached candidate was never spawned"
    );
    assert!(!error.contains("cargo install"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn unverified_remote_pin_is_refused_before_running_binary() {
    let root = temporary_directory("pinned-no-closure");
    let sentinel = root.join("executed");
    let binary = root.join("velnor-workflow");
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\ntouch '{}'\necho {CLOSURE_A}\n",
                sentinel.display()
            ),
        ),
        "write closure-claiming binary",
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark closure-claiming binary executable",
        );
    }
    let lookup = provisioned_lookup(binary, CLOSURE_A, root.join("install"));
    let error = must_fail(
        resolve_pinned_binary(PIN_A, &[], &lookup, &PinSource::Remote("unused".to_owned())),
        "binary without independently derived pin closure",
    )
    .to_string();
    assert!(
        error.contains("cannot verify the source closure"),
        "{error}"
    );
    assert!(error.contains("source history"), "{error}");
    assert!(
        !sentinel.exists(),
        "self-reported closure is never executed"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_running_binary_is_the_pin_when_built_at_it() {
    let root = temporary_directory("pinned-self");
    let lookup = lookup(root.join("install"));
    let expected = [SOURCE_CLOSURE.to_owned()];
    let resolved = must(
        resolve_pinned_binary(PIN_A, &expected, &lookup, &checkout_source(&root)),
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

#[test]
fn remote_pin_closure_comes_from_the_fetched_git_tree() {
    let source = temporary_directory("remote-pin-source");
    git_ok(&source, &["init", "-q", "-b", "main"]);
    write(&source.join("Cargo.toml"), "[workspace]\n");
    write(&source.join("Cargo.lock"), "# lock\n");
    write(
        &source.join("crates/velnor-workflow/src/lib.rs"),
        "pub fn source_pin() {}\n",
    );
    let pin = commit(&source, "generator pin");
    let expected = must(
        expected_closures(&source, &pin),
        "derive closure from local source tree",
    );
    let fetched = must(
        expected_closures_from_source(
            &pin,
            &PinSource::Remote(source.to_string_lossy().into_owned()),
        ),
        "fetch remote source pin before deriving closures",
    );
    assert_eq!(fetched, expected);
    let _ = fs::remove_dir_all(source);
}

#[test]
fn missing_remote_pin_history_fails_with_fetch_remediation() {
    let source = temporary_directory("remote-pin-missing");
    git_ok(&source, &["init", "-q", "-b", "main"]);
    write(&source.join("Cargo.toml"), "[workspace]\n");
    write(&source.join("Cargo.lock"), "# lock\n");
    let _ = commit(&source, "different pin");
    let error = must_fail(
        expected_closures_from_source(
            PIN_A,
            &PinSource::Remote(source.to_string_lossy().into_owned()),
        ),
        "missing remote pin source object",
    )
    .to_string();
    assert!(error.contains(PIN_A), "{error}");
    assert!(error.contains("fetch the pin's source history"), "{error}");
    let _ = fs::remove_dir_all(source);
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
    hosted_entrypoint_for_repository(revision, "")
}

fn hosted_entrypoint_for_repository(revision: &str, repository: &str) -> String {
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
        ),
        "scan entrypoint fixture",
    );
    let _ = fs::remove_dir_all(fixture);
    let mut config = ProjectConfig::from(shape);
    revision.clone_into(&mut config.workflow_revision);
    config.repository = repository.to_owned();
    crate::s2::render_policy_entrypoint(&config)
}

fn entrypoint_tree(name: &str, entrypoint: &str) -> PathBuf {
    let root = temporary_directory(name);
    write(&root.join(POLICY_ENTRYPOINT), entrypoint);
    root
}

fn entrypoint_policy_contract() -> VelnorPolicyContract {
    VelnorPolicyContract {
        default_branch: "main".to_owned(),
        ..VelnorPolicyContract::default()
    }
}

/// The generated entrypoint holds exactly the privileges the trust argument
/// in `ci-policy.yml` states: `contents: read` at both levels, no secrets,
/// no persisted credentials, no deployment environment, one hosted job, and
/// only the reviewed triggers.
#[test]
fn generated_entrypoint_satisfies_the_privilege_and_trigger_invariants() {
    let entrypoint = hosted_entrypoint(PIN_A);
    let root = entrypoint_tree("entrypoint-clean", &entrypoint);
    let audit = must(
        audit_policy_entrypoint_with_safe_root(&safe_root(&root), &entrypoint_policy_contract()),
        "audit generated entrypoint",
    );
    assert!(audit.trigger.is_empty(), "{:?}", audit.trigger);
    assert!(audit.privileges.is_empty(), "{:?}", audit.privileges);
    assert!(
        entrypoint.contains("references no secrets, persists no"),
        "the trust invariant states the absence honestly: {entrypoint}"
    );
    assert!(!entrypoint.contains("secrets."), "{entrypoint}");
    assert_eq!(
        entrypoint.matches("permissions:\n").count(),
        2,
        "workflow and job level: {entrypoint}"
    );
    assert_eq!(entrypoint.matches("contents: read\n").count(), 2);
    assert!(
        !entrypoint.contains("workflow_dispatch"),
        "the trusted policy workflow has no branch-selectable dispatch: {entrypoint}"
    );
    assert!(entrypoint.contains("# Trust invariant:"), "{entrypoint}");
    let pin = entrypoint_pin_with_safe_root(&safe_root(&root), PIN_A);
    assert!(pin.passed, "{}", pin.reason);
    let drift = entrypoint_pin_with_safe_root(&safe_root(&root), PIN_B);
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

/// The owner policy job reads the declared pin at runtime. Variable
/// references are not pin literals, so the pin rule still names the trusted
/// base revision and detects drift.
#[test]
fn owner_entrypoint_pin_ignores_variable_references() {
    let job = crate::s2::policy_job(&PolicyJobSpec {
        name: "Policy",
        revision: PIN_A,
        repository: crate::s2::workflow_setup_action_repository(),
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,Policy",
    });
    assert!(
        job.contains(&format!("{BASE_REVISION_ENV}: {PIN_A}")),
        "the entrypoint binds the trusted base revision: {job}"
    );
    assert!(
        !job.contains("--rev "),
        "rendered templates never use the space form a pre-product base validator scans for: {job}"
    );
    assert!(
        !job.contains("candidate-manifest") && !job.contains("VELNOR_WORKFLOW_CANDIDATE_MANIFEST"),
        "the policy entrypoint has no candidate-binary or manifest path: {job}"
    );
    let root = entrypoint_tree("entrypoint-owner-pin", &job);
    let pin = entrypoint_pin_with_safe_root(&safe_root(&root), PIN_A);
    assert!(pin.passed, "{:?}", pin.details);
    let drift = entrypoint_pin_with_safe_root(&safe_root(&root), PIN_B);
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
fn policy_job_uses_only_the_base_pinned_validator() {
    for repository in [
        crate::s2::workflow_setup_action_repository(),
        "example/consumer",
    ] {
        let job = crate::s2::policy_job(&PolicyJobSpec {
            name: "Policy",
            revision: PIN_A,
            repository,
            default_branch: "main",
            declared_ruleset_contexts: "ci-required,Policy",
        });
        assert!(job.contains("Set up Velnor workflow runtime"), "{job}");
        assert!(job.contains(&format!("rev: {PIN_A}")), "{job}");
        assert!(!job.contains("Read declared generator pin"), "{job}");
        assert!(!job.contains("Set up declared generator product"), "{job}");
        assert!(!job.contains(VELNOR_WORKFLOW_PINNED_BINARY_ENV), "{job}");
        assert!(
            job.contains("never acquires or executes candidate code or artifacts"),
            "{job}"
        );
    }
}

#[test]
fn entrypoint_audit_names_each_escalation() {
    let clean = hosted_entrypoint(PIN_A);
    let cases: [(&str, &str, &str, &str); 8] = [
        (
            "write",
            "permissions:\n  contents: read\n\njobs:",
            "permissions:\n  contents: write\n\njobs:",
            "workflow permissions must be exactly `contents: read`",
        ),
        (
            "job-permissions",
            "    permissions:\n      contents: read\n",
            "    permissions:\n      contents: read\n      id-token: write\n",
            "permissions must be exactly `contents: read`",
        ),
        (
            "secret",
            "GH_TOKEN: ${{ github.token }}",
            "GH_TOKEN: ${{ secrets.ADMIN_TOKEN }}",
            "must not reference `secrets.`",
        ),
        (
            "credentials",
            "persist-credentials: false",
            "persist-credentials: true",
            "checkout must set `persist-credentials: false`",
        ),
        (
            "unreviewed-trigger",
            "  pull_request_target:\n",
            "  schedule:\n    - cron: '0 0 * * *'\n  pull_request_target:\n",
            "trigger `schedule` is not admitted",
        ),
        (
            "branch-selectable-dispatch",
            "  pull_request_target:\n",
            "  workflow_dispatch:\n  pull_request_target:\n",
            "trigger `workflow_dispatch` is not admitted",
        ),
        (
            "trigger-filter",
            "    types: [opened, synchronize, reopened]",
            "    branches: [main]\n    types: [opened, synchronize, reopened]",
            "pull_request_target `branches` filters",
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
            audit_policy_entrypoint_with_safe_root(
                &safe_root(&root),
                &entrypoint_policy_contract(),
            ),
            "audit mutated entrypoint",
        );
        let findings = [audit.trigger, audit.privileges].concat();
        assert!(
            findings.iter().any(|finding| finding.contains(expected)),
            "{name}: expected a finding containing {expected:?}, got {findings:?}"
        );
        let _ = fs::remove_dir_all(root);
    }

    for (name, from, to, expected) in [
        (
            "unadmitted-push-trigger",
            "  pull_request_target:\n",
            "  push:\n    branches: [main]\n  pull_request_target:\n",
            "trigger `push` is not admitted",
        ),
        (
            "pull-request-target-path-filter",
            "    types: [opened, synchronize, reopened]",
            "    paths: ['**']\n    types: [opened, synchronize, reopened]",
            "pull_request_target `paths` filters are not admitted",
        ),
    ] {
        assert!(clean.contains(from), "{name}: fixture lacks {from:?}");
        let mutated = clean.replacen(from, to, 1);
        let root = entrypoint_tree(&format!("entrypoint-{name}"), &mutated);
        let audit = must(
            audit_policy_entrypoint_with_safe_root(
                &safe_root(&root),
                &entrypoint_policy_contract(),
            ),
            "audit mutated entrypoint trigger filters",
        );
        assert!(
            audit
                .trigger
                .iter()
                .any(|finding| finding.contains(expected)),
            "{name}: expected `{expected}`, got {:?}",
            audit.trigger
        );
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn policy_entrypoint_cannot_skip_or_stub_the_required_audit() {
    let clean = hosted_entrypoint(PIN_A);
    let cases = [
        (
            "wrong-job-id",
            clean.replacen("  policy:\n", "  policy-check:\n", 1),
            "canonical `policy`",
        ),
        (
            "wrong-context-name",
            clean.replacen("    name: Policy\n", "    name: Policy audit\n", 1),
            "job name must be `Policy`",
        ),
        (
            "job-skipped",
            clean.replacen("    name: Policy\n", "    name: Policy\n    if: false\n", 1),
            "must not have a skip condition",
        ),
        (
            "job-continues-on-error",
            clean.replacen(
                "    name: Policy\n",
                "    name: Policy\n    continue-on-error: true\n",
                1,
            ),
            "required policy job must not continue on error",
        ),
        (
            "workflow-defaults",
            clean.replacen(
                "permissions:\n",
                "defaults:\n  run:\n    working-directory: /tmp\npermissions:\n",
                1,
            ),
            "workflow-level run defaults are not admitted",
        ),
        (
            "wrong-runner",
            clean.replacen(
                "    runs-on: ubuntu-24.04\n",
                "    runs-on: ubuntu-latest\n",
                1,
            ),
            "fixed trusted GitHub-hosted image `ubuntu-24.04`",
        ),
        (
            "step-skipped",
            clean.replacen(
                "      - name: Enforce workflow policy\n",
                "      - name: Enforce workflow policy\n        if: false\n",
                1,
            ),
            "policy enforcement step must not have a skip condition",
        ),
        (
            "step-continues-on-error",
            clean.replacen(
                "      - name: Enforce workflow policy\n",
                "      - name: Enforce workflow policy\n        continue-on-error: true\n",
                1,
            ),
            "policy enforcement step must not continue on error",
        ),
        (
            "wrong-shell",
            clean.replacen(
                "      - name: Enforce workflow policy\n        shell: bash\n",
                "      - name: Enforce workflow policy\n        shell: sh\n",
                1,
            ),
            "policy enforcement step must use `shell: bash`",
        ),
        (
            "stub-command",
            clean.replacen(
                "          velnor-workflow policy \\\n",
                "          true\n",
                1,
            ),
            "must execute `velnor-workflow policy`",
        ),
        (
            "noop-command",
            clean.replacen(
                "          velnor-workflow policy \\\n",
                "          true \\\n",
                1,
            ),
            "must execute `velnor-workflow policy`",
        ),
        (
            "unapproved-root",
            clean.replacen(
                "WORKFLOW_ROOT: ${{ github.workspace }}/policy-checkout",
                "WORKFLOW_ROOT: /tmp/candidate",
                1,
            ),
            "approved `${{ github.workspace }}/policy-checkout` path",
        ),
    ];
    for (name, mutated, expected) in cases {
        let root = entrypoint_tree(&format!("entrypoint-policy-{name}"), &mutated);
        let audit = must(
            audit_policy_entrypoint_with_safe_root(
                &safe_root(&root),
                &entrypoint_policy_contract(),
            ),
            "audit mutated policy entrypoint",
        );
        assert!(
            audit
                .privileges
                .iter()
                .any(|finding| finding.contains(expected)),
            "{name}: expected `{expected}`, got {:?}",
            audit.privileges
        );
        let _ = fs::remove_dir_all(root);
    }
}

#[cfg(unix)]
#[test]
fn policy_enforcement_script_propagates_binary_failure() {
    use std::os::unix::fs::PermissionsExt as _;

    let script = policy_enforcement_script(&hosted_entrypoint(PIN_A));
    let root = temporary_directory("policy-script-failure");
    let bin = root.join("bin");
    must(
        fs::create_dir_all(&bin),
        "create fake workflow binary directory",
    );
    let called = root.join("called");
    let binary = bin.join("velnor-workflow");
    write(
        &binary,
        "#!/bin/sh\nprintf called > \"$POLICY_CALLED\"\nexit 37\n",
    );
    must(
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
        "make fake workflow binary executable",
    );
    let old_path = env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&old_path)))
            .expect("join fake workflow path");
    let output = must(
        Command::new("bash")
            .arg("-c")
            .arg(script)
            .current_dir(&root)
            .env("PATH", path)
            .env("POLICY_CALLED", &called)
            .env("WORKFLOW_ROOT", root.join("policy-checkout"))
            .env("HEAD_SHA", PIN_A)
            .env("BASE_SHA", PIN_A)
            .env("RULESET_CONTEXTS", "Policy,ci-required")
            .output(),
        "execute policy enforcement script",
    );
    assert!(
        !output.status.success(),
        "a policy command failure must fail the required context"
    );
    assert!(called.is_file(), "the expected policy command actually ran");
    let _ = fs::remove_dir_all(root);
}

fn policy_enforcement_script(entrypoint: &str) -> String {
    let document: serde_yaml::Value = must(serde_yaml::from_str(entrypoint), "parse entrypoint");
    let job = mapping_value(document.as_mapping().expect("workflow mapping"), "jobs")
        .and_then(serde_yaml::Value::as_mapping)
        .and_then(|jobs| mapping_value(jobs, "policy"))
        .and_then(serde_yaml::Value::as_mapping)
        .expect("policy job");
    let steps = mapping_value(job, "steps")
        .and_then(serde_yaml::Value::as_sequence)
        .expect("policy steps");
    steps
        .iter()
        .filter_map(serde_yaml::Value::as_mapping)
        .find(|step| {
            mapping_value(step, "name").and_then(serde_yaml::Value::as_str)
                == Some("Enforce workflow policy")
        })
        .and_then(|step| mapping_value(step, "run"))
        .and_then(serde_yaml::Value::as_str)
        .expect("policy enforcement script")
        .to_owned()
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
/// on the declared selector, admitted only on the caller repository's
/// default-branch push.
fn gated_trusted_job() -> String {
    format!(
        "name: CI / PR\non:\n  pull_request:\npermissions:\n  actions: read\n  contents: read\njobs:\n  plan:\n    name: Plan\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo plan\n  velnor-docker:\n    name: Docker\n    if: ${{{{ github.repository == 'example/consumer' && (github.event_name == 'push' && github.ref == 'refs/heads/main') }}}}\n    runs-on: [{VELNOR_SELECTOR}]\n    steps:\n      - run: echo trusted\n  ci-required:\n    name: ci-required\n    if: ${{{{ always() }}}}\n    needs: [plan, velnor-docker]\n    runs-on: ubuntu-24.04\n    steps:\n      - name: Validate generated stack results\n        env:\n          NEEDS_JSON: ${{{{ toJSON(needs) }}}}\n          SELECTED_UNITS: ${{{{ needs.plan.outputs.units }}}}\n          PLAN_DIGEST: ${{{{ needs.plan.outputs.plan_digest }}}}\n          EXCLUDED: ${{{{ needs.plan.outputs.excluded }}}}\n          PROVIDER_ADMITTED_VELNOR_TRUSTED: true\n        shell: bash\n        run: |\n          set -euo pipefail\n          if [[ -z \"$PLAN_DIGEST\" ]]; then exit 1; fi\n          result_for_job() {{ jq -r --arg job \"$1\" '.[$job].result // empty' <<<\"$NEEDS_JSON\"; }}\n          plan_expects() {{ jq -r --arg unit \"$1\" --arg provider \"$2\" 'length' <<<\"$SELECTED_UNITS\"; }}\n          result=\"$(result_for_job plan)\"\n          if [[ \"$result\" != success ]]; then exit 1; fi\n          if plan_expects docker velnor; then\n            result=\"$(result_for_job velnor-docker)\"\n            if [[ \"$PROVIDER_ADMITTED_VELNOR_TRUSTED\" == true ]]; then\n              case \"$result\" in success) ;; *) exit 1 ;; esac\n            fi\n          fi\n  required:\n    name: Control / Required\n    if: ${{{{ always() }}}}\n    needs: [ci-required]\n    runs-on: ubuntu-24.04\n    steps:\n      - run: exit 1\n"
    )
}

/// The semantic rules pass on a tree whose trusted Velnor job carries the
/// default-branch trusted-event gate, and fail — naming the job — when the
/// gate is removed. This is the ungated synthetic tree the design demands.
#[test]
fn ungated_trusted_velnor_job_fails_the_trusted_runners_rule() {
    let gated_job = gated_trusted_job();
    let gated = velnor_tree("semantic-gated", &gated_job);
    let audit = must(
        audit_workflows_with_safe_root(&safe_root(&gated), None, None),
        "audit gated tree",
    );
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
        head_sha: None,
        base_sha: None,
        base_revision: PIN_A.to_owned(),
        ruleset_contexts: Some(vec!["ci-required".to_owned(), "DCO".to_owned()]),
        trusted_repository: Some("example/consumer".to_owned()),
        trusted_default_branch: Some("main".to_owned()),
    };
    let report = must(
        evaluate_with_safe_root(&options, safe_root(&ungated)),
        "evaluate ungated tree",
    );
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

fn local_policy_contract(automatic_providers: &[&str]) -> VelnorPolicyContract {
    VelnorPolicyContract {
        providers: vec!["github-hosted".to_owned(), "velnor".to_owned()],
        automatic_providers: automatic_providers
            .iter()
            .map(|provider| (*provider).to_owned())
            .collect(),
        selectors: BTreeMap::from([
            ("github-hosted".to_owned(), vec!["ubuntu-24.04".to_owned()]),
            ("velnor".to_owned(), vec![VELNOR_SELECTOR.to_owned()]),
        ]),
        default_branch: "main".to_owned(),
        repository: Some("example/consumer".to_owned()),
    }
}

/// Local provider gates admit only an exact default-branch push. The
/// contract's automatic set decides whether that provider can run at all.
#[test]
fn local_provider_gate_is_bound_to_automatic_set_and_default_branch() {
    let automatic = local_policy_contract(&["github-hosted", "velnor"]);
    let gate = "${{ github.repository == 'example/consumer' && (github.event_name == 'push' && github.ref == 'refs/heads/main') }}";
    assert!(
        is_generated_provider_gate(gate, "velnor", &automatic),
        "{gate}"
    );
    for denied in [
        "${{ github.repository == 'example/consumer' && github.event_name == 'pull_request_target' }}",
        "${{ github.repository == 'example/consumer' && github.event_name == 'pull_request' }}",
        "${{ github.repository == 'example/consumer' && github.event_name == 'merge_group' }}",
        "${{ github.repository == 'example/consumer' && github.event_name == 'workflow_run' }}",
        "${{ github.repository == 'example/consumer' && github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main' }}",
        "${{ github.repository == 'example/consumer' && github.event_name == 'push' && github.ref == 'refs/heads/feature' }}",
        "${{ github.repository == 'example/consumer' && github.event_name == 'push' && github.ref == 'refs/tags/v1.2.3' }}",
        "${{ github.repository == 'example/consumer' && github.event_name == 'schedule' }}",
        "${{ github.repository == 'example/consumer' && (github.event_name == 'push' && github.ref == 'refs/heads/main' || github.event_name == 'workflow_dispatch') }}",
        "${{ github.repository == 'example/consumer' && github.event_name != 'pull_request' }}",
    ] {
        assert!(
            !is_generated_provider_gate(denied, "velnor", &automatic),
            "unsafe local event/ref was accepted: {denied}"
        );
    }
    let mut hosted_job = Mapping::new();
    hosted_job.insert(
        "runs-on".to_owned(),
        serde_yaml::Value::String("ubuntu-24.04".to_owned()),
    );
    assert!(
        !has_safe_runner_gate(gate, &hosted_job, &automatic),
        "a default-branch gate does not make a hosted selector a local selector"
    );
    assert!(
        !is_generated_provider_gate(gate, "velnor", &local_policy_contract(&["github-hosted"])),
        "a non-automatic local provider cannot broaden to main push"
    );
    let disabled = "${{ github.repository == 'example/consumer' && (false) }}";
    assert!(is_generated_provider_gate(
        disabled,
        "velnor",
        &local_policy_contract(&["github-hosted"])
    ));
}

#[test]
fn release_local_runner_rejects_tag_schedule_and_dispatch_gates() {
    let cases = [
        (
            "tag-push",
            "push:\n    tags: [\"v*\"]\n",
            "github.event_name == 'push' && github.ref_type == 'tag'",
        ),
        (
            "schedule",
            "schedule:\n    - cron: '0 0 * * *'\n",
            "github.event_name == 'schedule'",
        ),
        (
            "dispatch",
            "workflow_dispatch:\n",
            "github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main'",
        ),
        (
            "stale-release-gate",
            "push:\n    tags: [\"v*\"]\n  schedule:\n    - cron: '0 0 * * *'\n  workflow_dispatch:\n",
            "(github.event_name == 'push' && (github.ref_type == 'tag' || github.ref == 'refs/heads/main')) || github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main')",
        ),
    ];

    for (name, triggers, condition) in cases {
        let workflow = format!(
            "name: Release\non:\n  {triggers}permissions:\n  contents: read\njobs:\n  local-release:\n    if: ${{{{ {condition} }}}}\n    runs-on: [{VELNOR_SELECTOR}]\n    steps:\n      - run: echo release\n"
        );
        let root = velnor_tree(
            &format!("semantic-release-local-gate-{name}"),
            &gated_trusted_job(),
        );
        write(&root.join(".github/workflows/release.yml"), &workflow);
        let audit = must(
            audit_workflows_with_safe_root(&safe_root(&root), None, None),
            "audit release local runner gate",
        );
        assert!(
            audit.runners.iter().any(|finding| {
                finding.contains("release.yml")
                    && finding.contains("local-release")
                    && finding.contains("local-provider jobs require a trusted-event gate")
            }),
            "{name}: local release job admitted a noncanonical gate: {:?}",
            audit.runners
        );
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn dynamic_runner_expressions_fail_in_ci_and_release_workflows() {
    for (name, expression) in [
        ("matrix-runner", "${{ matrix.runner }}"),
        ("dotted-matrix-runner", "${{ matrix.config.runner }}"),
        (
            "from-json-runner",
            "${{ fromJSON(needs.version.outputs.runner-configs) }}",
        ),
    ] {
        let workflow = gated_trusted_job().replace(
            &format!("runs-on: [{VELNOR_SELECTOR}]"),
            &format!("runs-on: {expression}"),
        );
        let root = velnor_tree(&format!("semantic-dynamic-runner-{name}"), &workflow);
        let audit = must(
            audit_workflows_with_safe_root(&safe_root(&root), None, None),
            "audit dynamic runner expression",
        );
        assert!(
            audit.runners.iter().any(|finding| {
                finding.contains("ci-pr.yml")
                    && finding.contains("unresolved or dynamic runner label")
            }),
            "{name}: dynamic runner expression passed policy: {:?}",
            audit.runners
        );
        let _ = fs::remove_dir_all(root);
    }

    let root = velnor_tree("semantic-dynamic-release-runner", &gated_trusted_job());
    write(
        &root.join(".github/workflows/release.yml"),
        "name: Release\non:\n  workflow_dispatch:\njobs:\n  build:\n    runs-on: ${{ matrix.config.runner }}\n    strategy:\n      matrix:\n        config: []\n    steps:\n      - run: echo release\n",
    );
    let audit = must(
        audit_workflows_with_safe_root(&safe_root(&root), None, None),
        "audit dynamic release runner",
    );
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("release.yml")
                && finding.contains("unresolved or dynamic runner label")
        }),
        "release matrix must not select a runner dynamically: {:?}",
        audit.runners
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn trusted_runner_gate_requires_the_exact_repository_prefix() {
    let contract = local_policy_contract(&["velnor"]);
    let gate = "${{ github.repository == 'example/consumer' && github.event_name == 'push' && github.ref == 'refs/heads/main' }}";
    assert!(has_trusted_runner_gate(gate, &contract));
    assert!(!has_trusted_runner_gate(
        "${{ github.repository == 'example/other' && github.event_name == 'push' && github.ref == 'refs/heads/main' }}",
        &contract
    ));
    for denied in [
        "${{ github.repository == 'example/consumer' && github.event_name == 'pull_request_target' }}",
        "${{ github.repository == 'example/consumer' && github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main' }}",
        "${{ github.repository == 'example/consumer' && github.event_name == 'push' && github.ref == 'refs/heads/feature' }}",
    ] {
        assert!(!has_trusted_runner_gate(denied, &contract), "{denied}");
    }
}

#[test]
fn local_runner_gate_requires_the_configured_caller_repository() {
    let clean = gated_trusted_job();
    for (name, mutated) in [
        (
            "missing-repository",
            clean.replace("github.repository == 'example/consumer' && ", ""),
        ),
        (
            "wrong-repository",
            clean.replace(
                "github.repository == 'example/consumer'",
                "github.repository == 'example/other'",
            ),
        ),
    ] {
        let root = velnor_tree(&format!("semantic-{name}"), &mutated);
        let audit = must(
            audit_workflows_with_safe_root(&safe_root(&root), None, None),
            "audit local caller-repository gate",
        );
        assert!(
            audit.runners.iter().any(|finding| {
                finding.contains("velnor-docker")
                    && finding.contains("local-provider jobs require a trusted-event gate")
            }),
            "{name}: expected local runner to fail identity admission, got {:?}",
            audit.runners
        );
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn repository_and_default_branch_follow_the_trusted_runner_context() {
    let root = velnor_tree("semantic-trusted-config-anchor", &gated_trusted_job());
    let policy = must(
        read_configured_policy(&safe_root(&root), Some("example/consumer"), Some("main")),
        "read trusted repository contract",
    );
    assert_eq!(policy.repository.as_deref(), Some("example/consumer"));
    assert_eq!(policy.default_branch, "main");
    let _ = fs::remove_dir_all(root);

    let rewritten = velnor_tree("semantic-rewritten-config-anchor", &gated_trusted_job());
    for path in [
        rewritten.join(GENERATION_CONFIG),
        rewritten.join(RUNTIME_CONFIG),
    ] {
        let mut content = must(fs::read_to_string(&path), "read candidate identity");
        content = content.replace("example/consumer", "example/attacker");
        write(&path, &content);
    }
    let error = must_fail(
        read_configured_policy(
            &safe_root(&rewritten),
            Some("example/consumer"),
            Some("main"),
        ),
        "candidate cannot rewrite both repository identity fields",
    );
    assert!(error
        .to_string()
        .contains("does not match trusted repository"));
    let _ = fs::remove_dir_all(rewritten);

    let changed_branch = velnor_tree("semantic-rewritten-default-branch", &gated_trusted_job());
    for path in [
        changed_branch.join(GENERATION_CONFIG),
        changed_branch.join(RUNTIME_CONFIG),
    ] {
        let mut content = must(fs::read_to_string(&path), "read candidate branch");
        content = content.replace("default_branch = \"main\"", "default_branch = \"staging\"");
        write(&path, &content);
    }
    let error = must_fail(
        read_configured_policy(
            &safe_root(&changed_branch),
            Some("example/consumer"),
            Some("main"),
        ),
        "candidate cannot rewrite the trusted default branch",
    );
    assert!(error
        .to_string()
        .contains("does not match trusted default branch"));
    let _ = fs::remove_dir_all(changed_branch);
}

#[test]
fn malformed_candidate_generator_pin_cannot_fall_back_to_entrypoint_pin() {
    let root = velnor_tree("semantic-malformed-generator-pin", &gated_trusted_job());
    let path = root.join(GENERATION_CONFIG);
    let mut generation = must(fs::read_to_string(&path), "read generation pin fixture");
    generation = generation.replace(
        "repository = \"example/consumer\"",
        "repository = \"example/consumer\"\nrevision = \"not-a-full-sha\"",
    );
    write(&path, &generation);
    let error = must_fail(
        read_declared_tree(&safe_root(&root), Some("example/consumer"), Some("main")),
        "malformed config pin must not use a workflow-marker fallback",
    )
    .to_string();
    assert!(
        error.contains("revision must be a full 40-character commit SHA"),
        "{error}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn candidate_cannot_add_an_unreviewed_provider_selector() {
    let root = velnor_tree("semantic-unknown-provider-selector", &gated_trusted_job());
    let path = root.join(GENERATION_CONFIG);
    let mut generation = must(fs::read_to_string(&path), "read provider selectors");
    generation.push_str("\n[workflow.selectors.attacker]\nruns_on = [\"unreviewed-runner\"]\n");
    write(&path, &generation);
    let error = must_fail(
        read_configured_policy(&safe_root(&root), Some("example/consumer"), Some("main")),
        "candidate-added provider selector must be rejected",
    )
    .to_string();
    assert!(error.contains("unknown provider `attacker`"), "{error}");
    let _ = fs::remove_dir_all(root);

    let mut inactive = local_policy_contract(&[]);
    inactive.providers = vec!["github-hosted".to_owned()];
    assert_eq!(
        inactive.provider_for_labels(&[VELNOR_SELECTOR]),
        None,
        "selectors outside the configured provider universe cannot classify a runner"
    );
}

#[test]
fn local_reusable_workflow_cannot_use_a_pull_request_target_gate() {
    let unsafe_gate = gated_trusted_job().replace(
        "github.event_name == 'push' && github.ref == 'refs/heads/main'",
        "github.event_name == 'pull_request_target'",
    );
    let root = velnor_tree("semantic-local-prt-caller", &unsafe_gate);
    let audit = must(
        audit_workflows_with_safe_root(&safe_root(&root), None, None),
        "audit local reusable PRT gate",
    );
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("velnor-docker")
                && finding.contains("local-provider jobs require a trusted-event gate")
        }),
        "local reusable jobs must reject caller-inherited pull_request_target: {:?}",
        audit.runners
    );
    let _ = fs::remove_dir_all(root);

    let root = velnor_tree("semantic-local-prt-callee", &gated_trusted_job());
    write(
        &root.join(".github/workflows/ci-unit-docker.yml"),
        "name: CI unit Docker\non:\n  workflow_call:\njobs:\n  docker:\n    if: ${{ github.repository == 'example/consumer' && github.event_name == 'pull_request_target' }}\n    runs-on: [example-velnor]\n    steps:\n      - run: echo trusted\n",
    );
    let audit = must(
        audit_workflows_with_safe_root(&safe_root(&root), None, None),
        "audit reusable local workflow",
    );
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("ci-unit-docker.yml")
                && finding.contains("docker")
                && finding.contains("local-provider jobs require a trusted-event gate")
        }),
        "a workflow_call callee inherits the caller event and must reject PRT admission: {:?}",
        audit.runners
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn untrusted_pr_paths_reject_write_secrets_oidc_and_environment_access() {
    let cases = [
        (
            "workflow-write",
            "name: Unsafe\non: pull_request\npermissions:\n  contents: write\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo build\n",
            "workflow permission `contents: write`",
        ),
        (
            "implicit-default-permissions",
            "name: Unsafe\non: pull_request\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo build\n",
            "must declare explicit read-only `permissions`",
        ),
        (
            "job-oidc-write",
            "name: Unsafe\non: pull_request\npermissions:\n  contents: read\njobs:\n  build:\n    permissions:\n      id-token: write\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo build\n",
            "job `id-token: write`",
        ),
        (
            "secret-reference",
            "name: Unsafe\non: pull_request\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    env:\n      TOKEN: ${{ secrets.ADMIN_TOKEN }}\n    steps:\n      - run: echo build\n",
            "references the `secrets` context",
        ),
        (
            "deployment-environment",
            "name: Unsafe\non: pull_request\njobs:\n  deploy:\n    environment: production\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo deploy\n",
            "deployment environments are forbidden",
        ),
        (
            "secrets-inherit",
            "name: Unsafe\non: pull_request\njobs:\n  call:\n    uses: ./.github/workflows/ci-unit-docker.yml\n    secrets: inherit\n",
            "`secrets: inherit` is forbidden",
        ),
    ];

    for (name, workflow, expected) in cases {
        let root = velnor_tree(
            &format!("semantic-pr-privilege-{name}"),
            &gated_trusted_job(),
        );
        write(
            &root.join(format!(".github/workflows/{name}.yml")),
            workflow,
        );
        let audit = must(
            audit_workflows_with_safe_root(&safe_root(&root), None, None),
            "audit privileged PR workflow",
        );
        assert!(
            audit
                .structure
                .iter()
                .any(|finding| finding.contains(expected)),
            "{name}: expected `{expected}`, got {:?}",
            audit.structure
        );
        let _ = fs::remove_dir_all(root);
    }

    let root = velnor_tree("semantic-pr-privilege-reusable", &gated_trusted_job());
    write(
        &root.join(".github/workflows/pr-caller.yml"),
        "name: PR caller\non: pull_request\npermissions:\n  contents: read\njobs:\n  call:\n    uses: ./.github/workflows/ci-unit-docker.yml\n",
    );
    write(
        &root.join(".github/workflows/ci-unit-docker.yml"),
        "name: Docker reusable\non:\n  workflow_call:\njobs:\n  build:\n    permissions:\n      id-token: write\n    environment: production\n    runs-on: ubuntu-24.04\n    env:\n      TOKEN: ${{ secrets.DEPLOY_TOKEN }}\n    steps:\n      - run: echo build\n",
    );
    let audit = must(
        audit_workflows_with_safe_root(&safe_root(&root), None, None),
        "audit reusable PR workflow",
    );
    for expected in [
        "ci-unit-docker.yml: job build: job `id-token: write`",
        "ci-unit-docker.yml: job build: deployment environments are forbidden",
        "ci-unit-docker.yml: job build: references the `secrets` context",
    ] {
        assert!(
            audit
                .structure
                .iter()
                .any(|finding| finding.contains(expected)),
            "reachable local reusable workflow finding `{expected}` missing: {:?}",
            audit.structure
        );
    }
    let _ = fs::remove_dir_all(root);

    let root = velnor_tree("semantic-pr-privilege-push-exempt", &gated_trusted_job());
    write(
        &root.join(".github/workflows/trusted-release.yml"),
        "name: Trusted release\non: push\npermissions:\n  contents: write\njobs:\n  release:\n    environment: production\n    permissions:\n      id-token: write\n    runs-on: ubuntu-24.04\n    env:\n      TOKEN: ${{ secrets.RELEASE_TOKEN }}\n    steps:\n      - run: echo release\n",
    );
    let audit = must(
        audit_workflows_with_safe_root(&safe_root(&root), None, None),
        "audit trusted push workflow",
    );
    assert!(
        !audit
            .structure
            .iter()
            .any(|finding| finding.contains("trusted-release.yml")),
        "push-only release privileges remain outside the untrusted PR path: {:?}",
        audit.structure
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn hosted_selectors_cannot_alias_local_or_self_hosted_labels() {
    for (name, needle, replacement, expected) in [
        (
            "local-alias",
            "runs_on = [\"example-velnor\"]",
            "runs_on = [\"ubuntu-24.04\"]",
            "shared by providers",
        ),
        (
            "case-insensitive-local-alias",
            "runs_on = [\"example-velnor\"]",
            "runs_on = [\"UBUNTU-24.04\"]",
            "shared by providers",
        ),
        (
            "self-hosted-alias",
            "runs_on = [\"ubuntu-24.04\"]",
            "runs_on = [\"self-hosted\"]",
            "one static GitHub-hosted image",
        ),
        (
            "dynamic-label-alias",
            "runs_on = [\"ubuntu-24.04\"]",
            "runs_on = [\"ubuntu-24.04\", \"${{ github.event.inputs.runner }}\"]",
            "one static GitHub-hosted image",
        ),
    ] {
        let root = velnor_tree(
            &format!("semantic-hosted-selector-{name}"),
            &gated_trusted_job(),
        );
        let path = root.join(GENERATION_CONFIG);
        let config = must(fs::read_to_string(&path), "read selector fixture");
        let mutated = config.replacen(needle, replacement, 1);
        write(&path, &mutated);
        let error = must_fail(
            audit_workflows_with_safe_root(&safe_root(&root), None, None),
            "unsafe hosted selector must fail policy discovery",
        )
        .to_string();
        assert!(error.contains(expected), "{name}: {error}");
        let _ = fs::remove_dir_all(root);
    }

    let ambiguous = VelnorPolicyContract {
        selectors: std::collections::BTreeMap::from([
            ("github-hosted".to_owned(), vec!["self-hosted".to_owned()]),
            ("velnor".to_owned(), vec!["SELF-HOSTED".to_owned()]),
        ]),
        ..VelnorPolicyContract::default()
    };
    assert_eq!(
        ambiguous.provider_for_labels(&["self-hosted"]),
        None,
        "ambiguous case-insensitive matches cannot inherit the hosted provider's trust"
    );
}

#[test]
fn candidate_static_workflow_is_still_semantically_audited() {
    let root = velnor_tree("semantic-static-workflow", &gated_trusted_job());
    let config_path = root.join(GENERATION_CONFIG);
    let mut config = must(
        fs::read_to_string(&config_path),
        "read static workflow config",
    );
    config.push_str(
        "\n[[static_files]]\nfile = \".github/workflows/rogue.yml\"\nsource = \".github-gen/rogue.yml\"\n",
    );
    write(&config_path, &config);
    let rogue = "name: Rogue\non: push\njobs:\n  run:\n    runs-on: [example-velnor]\n    steps:\n      - run: echo untrusted\n";
    write(&root.join(".github-gen/rogue.yml"), rogue);
    write(&root.join(".github/workflows/rogue.yml"), rogue);

    let audit = must(
        audit_workflows_with_safe_root(&safe_root(&root), None, None),
        "audit statically declared workflow",
    );
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("rogue.yml")
                && finding.contains("local-provider jobs require a trusted-event gate")
        }),
        "static output cannot exempt itself from semantic policy: {:?}",
        audit.runners
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn candidate_cannot_configure_policy_workflow_exclusions() {
    let root = velnor_tree("semantic-policy-exclusion", &gated_trusted_job());
    let config_path = root.join(GENERATION_CONFIG);
    let mut config = must(fs::read_to_string(&config_path), "read exclusion config");
    config.push_str("\n[policy]\nexclude_workflows = [\"ci-pr.yml\"]\n");
    write(&config_path, &config);

    let error = must_fail(
        audit_workflows_with_safe_root(&safe_root(&root), None, None),
        "candidate workflow exclusions must be rejected",
    )
    .to_string();
    assert!(error.contains("exclude_workflows"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn ci_pr_trigger_cannot_omit_prs_or_narrow_by_branch_or_path() {
    let clean = gated_trusted_job();
    let baseline = velnor_tree("semantic-ci-pr-trigger-clean", &clean);
    let baseline_audit = must(
        audit_workflows_with_safe_root(&safe_root(&baseline), None, None),
        "audit unfiltered pull_request",
    );
    assert!(
        baseline_audit.structure.is_empty(),
        "{:?}",
        baseline_audit.structure
    );
    let _ = fs::remove_dir_all(baseline);

    let cases = [
        (
            "missing-pull-request",
            clean.replace("  pull_request:\n", ""),
            "`on` must be a trigger mapping containing pull_request",
        ),
        (
            "branch-filter",
            clean.replacen(
                "  pull_request:\n",
                "  pull_request:\n    branches: [main]\n",
                1,
            ),
            "pull_request `branches` filters",
        ),
        (
            "path-filter",
            clean.replacen(
                "  pull_request:\n",
                "  pull_request:\n    paths-ignore: ['docs/**']\n",
                1,
            ),
            "pull_request `paths-ignore` filters",
        ),
        (
            "activity-filter",
            clean.replacen(
                "  pull_request:\n",
                "  pull_request:\n    types: [opened]\n",
                1,
            ),
            "must include opened, reopened, and synchronize",
        ),
    ];
    for (name, workflow, expected) in cases {
        let root = velnor_tree(&format!("semantic-ci-pr-trigger-{name}"), &workflow);
        let audit = must(
            audit_workflows_with_safe_root(&safe_root(&root), None, None),
            "audit narrowed PR trigger",
        );
        assert!(
            audit
                .structure
                .iter()
                .any(|finding| finding.contains(expected)),
            "{name}: expected `{expected}`, got {:?}",
            audit.structure
        );
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn required_ci_job_always_runs_and_needs_every_aggregate_job() {
    let providers = [
        crate::s2::provider::ProviderId::GithubHosted,
        crate::s2::provider::ProviderId::Velnor,
    ];
    let (clean, _) = generated_ci_pr_fixture(&providers);
    let root = velnor_tree("semantic-ci-required-clean", &clean);
    let declared = must(
        read_declared_tree(&safe_root(&root), None, None),
        "read aggregate contract",
    );
    let report = required_checks_with_safe_root(&safe_root(&root), &declared, None);
    assert!(report.passed, "{}: {:?}", report.reason, report.details);
    let _ = fs::remove_dir_all(root);

    let dependencies = ci_required_dependencies(&clean);
    let omitted_caller = dependencies
        .iter()
        .find(|dependency| dependency.as_str() != "plan" && dependency.as_str() != "policy")
        .expect("generated ci-required has a caller dependency");
    let without_caller = dependencies
        .iter()
        .filter(|dependency| *dependency != omitted_caller)
        .cloned()
        .collect::<Vec<_>>();
    let without_plan = dependencies
        .iter()
        .filter(|dependency| dependency.as_str() != "plan")
        .cloned()
        .collect::<Vec<_>>();
    let cases = [
        (
            "conditional",
            with_required_condition(&clean, "success()"),
            "must use `if: always()`",
        ),
        (
            "missing-unit-dependency",
            with_required_needs(&clean, &without_caller),
            "needs must include every aggregate job",
        ),
        (
            "missing-plan-dependency",
            with_required_needs(&clean, &without_plan),
            "dependency graph must include the `plan` job",
        ),
    ];
    for (name, workflow, expected) in cases {
        let root = velnor_tree(&format!("semantic-ci-required-{name}"), &workflow);
        let declared = must(
            read_declared_tree(&safe_root(&root), None, None),
            "read mutated aggregate contract",
        );
        let report = required_checks_with_safe_root(&safe_root(&root), &declared, None);
        assert!(
            !report.passed
                && report
                    .details
                    .iter()
                    .any(|finding| finding.contains(expected)),
            "{name}: expected `{expected}`, got {}: {:?}",
            report.reason,
            report.details
        );
        let _ = fs::remove_dir_all(root);
    }

    let matrix = with_required_strategy(&clean);
    let root = velnor_tree("semantic-ci-required-matrix", &matrix);
    let declared = must(
        read_declared_tree(&safe_root(&root), None, None),
        "read matrix aggregate contract",
    );
    let report = required_checks_with_safe_root(&safe_root(&root), &declared, None);
    assert!(
        !report.passed
            && report
                .details
                .iter()
                .any(|finding| { finding.contains("must not use a matrix strategy") }),
        "the required verdict must remain one job, got {}: {:?}",
        report.reason,
        report.details
    );
    let _ = fs::remove_dir_all(root);

    let noop = with_required_verdict_run(&clean, "true");
    let findings = audit_ci_required_aggregate_with_contract(&noop, None);
    assert!(
        findings.iter().any(|finding| {
            finding.contains("bind every dependency result to its caller unit/provider")
        }),
        "a no-op success body must fail even when job name, always(), and needs are intact: {findings:?}"
    );
}

fn with_required_condition(workflow: &str, condition: &str) -> String {
    let mut document: serde_yaml::Value = must(
        serde_yaml::from_str(workflow),
        "parse required workflow fixture",
    );
    let job = document
        .as_mapping_mut()
        .and_then(|root| root.get_mut("jobs"))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .and_then(|jobs| jobs.get_mut("ci-required"))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .expect("fixture has ci-required mapping");
    job.insert("if", serde_yaml::Value::String(condition.to_owned()));
    must(
        serde_yaml::to_string(&document),
        "serialize required workflow fixture",
    )
}

fn with_required_needs(workflow: &str, dependencies: &[String]) -> String {
    let mut document: serde_yaml::Value = must(
        serde_yaml::from_str(workflow),
        "parse required workflow fixture",
    );
    let job = document
        .as_mapping_mut()
        .and_then(|root| root.get_mut("jobs"))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .and_then(|jobs| jobs.get_mut("ci-required"))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .expect("fixture has ci-required mapping");
    job.insert(
        "needs",
        serde_yaml::Value::Sequence(
            dependencies
                .iter()
                .cloned()
                .map(serde_yaml::Value::String)
                .collect(),
        ),
    );
    must(
        serde_yaml::to_string(&document),
        "serialize required workflow fixture",
    )
}

fn with_required_strategy(workflow: &str) -> String {
    let mut document: serde_yaml::Value = must(
        serde_yaml::from_str(workflow),
        "parse required workflow fixture",
    );
    let job = document
        .as_mapping_mut()
        .and_then(|root| root.get_mut("jobs"))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .and_then(|jobs| jobs.get_mut("ci-required"))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .expect("fixture has ci-required mapping");
    job.insert(
        "strategy",
        must(
            serde_yaml::from_str("matrix:\n  target: []\n"),
            "parse empty matrix strategy",
        ),
    );
    must(
        serde_yaml::to_string(&document),
        "serialize required workflow fixture",
    )
}

fn with_required_verdict_run(workflow: &str, run: &str) -> String {
    let mut document: serde_yaml::Value = must(
        serde_yaml::from_str(workflow),
        "parse required workflow fixture",
    );
    let job = document
        .as_mapping_mut()
        .and_then(|root| root.get_mut("jobs"))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .and_then(|jobs| jobs.get_mut("ci-required"))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .expect("fixture has ci-required mapping");
    let steps = job
        .get_mut("steps")
        .and_then(serde_yaml::Value::as_sequence_mut)
        .expect("fixture has ci-required steps");
    let step = steps[0]
        .as_mapping_mut()
        .expect("fixture has one verdict step");
    step.insert("run", serde_yaml::Value::String(run.to_owned()));
    must(
        serde_yaml::to_string(&document),
        "serialize required workflow fixture",
    )
}

fn with_required_verdict_env(workflow: &str, name: &str, value: &str) -> String {
    let mut document: serde_yaml::Value = must(
        serde_yaml::from_str(workflow),
        "parse required workflow fixture",
    );
    let step = document
        .as_mapping_mut()
        .and_then(|root| root.get_mut("jobs"))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .and_then(|jobs| jobs.get_mut("ci-required"))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .and_then(|job| job.get_mut("steps"))
        .and_then(serde_yaml::Value::as_sequence_mut)
        .and_then(|steps| steps.first_mut())
        .and_then(serde_yaml::Value::as_mapping_mut)
        .and_then(|step| step.get_mut("env"))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .expect("fixture has ci-required verdict environment");
    step.insert(name, serde_yaml::Value::String(value.to_owned()));
    must(
        serde_yaml::to_string(&document),
        "serialize required workflow fixture",
    )
}

#[cfg(unix)]
#[test]
fn ci_required_script_propagates_a_failed_selected_dependency() {
    use crate::s2::provider::ProviderId::{GithubHosted, Velnor};

    let providers = [GithubHosted, Velnor];
    let (workflow, units) = generated_ci_pr_fixture(&providers);
    let script = required_verdict_script(&workflow);
    let dependencies = ci_required_dependencies(&workflow);
    let mut results = dependencies
        .iter()
        .map(|dependency| (dependency.clone(), "success".to_owned()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let selected_job = dependencies
        .iter()
        .find(|job| job.starts_with("github-hosted-"))
        .expect("fixture has a hosted unit caller");
    results.insert(selected_job.clone(), "failure".to_owned());
    let needs = ci_needs_json(&dependencies, &results, None);
    let selected = selected_units_json(&units, &providers);
    let admissions = ci_required_admission_names(&workflow)
        .into_iter()
        .map(|name| (name, true))
        .collect::<std::collections::BTreeMap<_, _>>();
    let output = execute_required_script(&script, &needs, &selected, &admissions);
    assert!(
        !output.status.success(),
        "a selected dependency failure must make the required context fail: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn rendered_ci_required_binds_actual_unit_provider_and_admission() {
    let providers = [
        crate::s2::provider::ProviderId::GithubHosted,
        crate::s2::provider::ProviderId::Velnor,
    ];
    let (workflow, _) = generated_ci_pr_fixture(&providers);
    let contract = local_policy_contract(&["github-hosted", "velnor"]);
    let clean = audit_ci_required_aggregate_with_contract(&workflow, Some(&contract));
    assert!(
        clean.is_empty(),
        "generated ci-required audit failed: {clean:?}"
    );

    let script = required_verdict_script(&workflow);
    let header = script
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("if plan_expects \""))
        .expect("generated verdict has a caller binding");
    let fields = header
        .strip_prefix("if plan_expects \"")
        .expect("plan_expects caller header")
        .split('"')
        .collect::<Vec<_>>();
    let unit = fields.first().expect("caller unit");
    let provider = fields
        .get(2)
        .expect("caller provider")
        .trim_end_matches("; then");
    let binding = format!("plan_expects \"{unit}\" \"{provider}\"");
    let wrong_unit = script.replacen(
        &binding,
        &format!("plan_expects \"wrong-unit\" \"{provider}\""),
        1,
    );
    let wrong_unit = with_required_verdict_run(&workflow, &wrong_unit);
    let findings = audit_ci_required_aggregate_with_contract(&wrong_unit, Some(&contract));
    assert!(
        findings.iter().any(|finding| {
            finding.contains("bind every dependency result to its caller unit/provider")
        }),
        "changing the caller unit must fail the aggregate audit: {findings:?}"
    );

    let local_admission = ci_required_admission_names(&workflow)
        .into_iter()
        .find(|name| name.contains("VELNOR"))
        .expect("generated local provider admission variable");
    let wrong_admission = with_required_verdict_env(&workflow, &local_admission, "false");
    let findings = audit_ci_required_aggregate_with_contract(&wrong_admission, Some(&contract));
    assert!(
        findings.iter().any(|finding| {
            finding.contains(&format!(
                "must bind `{local_admission}` to the configured provider admission"
            ))
        }),
        "changing local admission must fail the aggregate audit: {findings:?}"
    );
}

#[cfg(unix)]
#[test]
fn rendered_ci_required_executes_against_synthetic_needs_and_admissions() {
    use crate::s2::provider::ProviderId::{GithubHosted, Velnor};

    let providers = [GithubHosted, Velnor];
    let (workflow, unit_ids) = generated_ci_pr_fixture(&providers);
    let script = required_verdict_script(&workflow);
    let dependencies = ci_required_dependencies(&workflow);
    let results = dependencies
        .iter()
        .map(|dependency| (dependency.clone(), "success".to_owned()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let needs = ci_needs_json(&dependencies, &results, None);
    let selected = selected_units_json(&unit_ids, &providers);
    let admissions = ci_required_admission_names(&workflow)
        .into_iter()
        .map(|name| (name, true))
        .collect::<std::collections::BTreeMap<_, _>>();
    let success = execute_required_script(&script, &needs, &selected, &admissions);
    assert!(
        success.status.success(),
        "rendered ci-required rejected a successful selected matrix: stdout={} stderr={}",
        String::from_utf8_lossy(&success.stdout),
        String::from_utf8_lossy(&success.stderr)
    );

    let selected_job = dependencies
        .iter()
        .find(|job| job.starts_with("github-hosted-"))
        .expect("fixture has a hosted unit caller");
    for outcome in [Some("skipped"), Some("cancelled"), Some("failure"), None] {
        let mut changed = results.clone();
        if let Some(outcome) = outcome {
            changed.insert(selected_job.clone(), outcome.to_owned());
        }
        let omitted = outcome.is_none().then_some(selected_job.as_str());
        let needs = ci_needs_json(&dependencies, &changed, omitted);
        let output = execute_required_script(&script, &needs, &selected, &admissions);
        assert!(
            !output.status.success(),
            "selected caller result {outcome:?} unexpectedly passed"
        );
    }

    let mut local_denied = admissions.clone();
    let local_admission = local_denied
        .keys()
        .find(|name| name.contains("VELNOR"))
        .expect("generated ci-required has a Velnor admission variable")
        .clone();
    *local_denied
        .get_mut(&local_admission)
        .expect("found admission variable") = false;
    let output = execute_required_script(&script, &needs, &selected, &local_denied);
    assert!(
        !output.status.success(),
        "a selected local job that succeeded outside admission must fail"
    );

    let mut unexpected = results;
    for (job, result) in &mut unexpected {
        if job != "plan" && job != "policy" {
            *result = "skipped".to_owned();
        }
    }
    let unexpected_job = dependencies
        .iter()
        .find(|job| job.starts_with("github-hosted-"))
        .expect("fixture has a hosted unit caller");
    unexpected.insert(unexpected_job.clone(), "success".to_owned());
    let needs = ci_needs_json(&dependencies, &unexpected, None);
    let output = execute_required_script(&script, &needs, "[]", &admissions);
    assert!(
        !output.status.success(),
        "an unselected caller that succeeded must fail the required context"
    );
}

fn required_verdict_script(workflow: &str) -> String {
    let document: serde_yaml::Value =
        must(serde_yaml::from_str(workflow), "parse verdict workflow");
    let job = mapping_value(document.as_mapping().expect("workflow mapping"), "jobs")
        .and_then(serde_yaml::Value::as_mapping)
        .and_then(|jobs| mapping_value(jobs, "ci-required"))
        .and_then(serde_yaml::Value::as_mapping)
        .expect("ci-required mapping");
    let steps = mapping_value(job, "steps")
        .and_then(serde_yaml::Value::as_sequence)
        .expect("ci-required steps");
    steps[0]
        .as_mapping()
        .and_then(|step| mapping_value(step, "run"))
        .and_then(serde_yaml::Value::as_str)
        .expect("ci-required run script")
        .to_owned()
}

fn generated_ci_pr_fixture(providers: &[crate::s2::provider::ProviderId]) -> (String, Vec<String>) {
    let root = temporary_directory("generated-required-fixture");
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"example\"\nversion = \"0.1.0\"\n",
    );
    write(
        &root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.91.1\"\n",
    );
    write(&root.join("src/lib.rs"), "pub fn answer() -> u8 { 42 }\n");
    let provider_set = providers.iter().copied().collect();
    let shape = must(
        crate::s2::scan::scan_shape(&root, &provider_set, "main", &[]),
        "scan generated ci-required fixture",
    );
    let mut config = ProjectConfig::from(shape);
    config.repository = "example/consumer".to_owned();
    config.providers = provider_set.clone();
    config.automatic_providers = provider_set.clone();
    config.default_dispatch_providers = provider_set;
    config.selectors = crate::s2::scan::default_selectors();
    let unit_ids = config
        .units
        .iter()
        .map(|unit| unit.id.clone())
        .collect::<Vec<_>>();
    assert!(!unit_ids.is_empty(), "fixture scans one Rust unit");
    let workflow =
        crate::s2::generated_ci_pr(&crate::s2::primitives::WorkflowIr::from_config(&config));
    let _ = fs::remove_dir_all(root);
    (workflow, unit_ids)
}

fn ci_required_dependencies(workflow: &str) -> Vec<String> {
    let document: serde_yaml::Value = must(serde_yaml::from_str(workflow), "parse generated ci-pr");
    let jobs = document
        .as_mapping()
        .and_then(|root| mapping_value(root, "jobs"))
        .and_then(serde_yaml::Value::as_mapping)
        .expect("workflow jobs mapping");
    let job = mapping_value(jobs, "ci-required")
        .and_then(serde_yaml::Value::as_mapping)
        .expect("generated ci-required job");
    mapping_value(job, "needs")
        .and_then(serde_yaml::Value::as_sequence)
        .expect("ci-required dependency array")
        .iter()
        .map(|dependency| {
            dependency
                .as_str()
                .expect("static dependency id")
                .to_owned()
        })
        .collect()
}

fn ci_required_admission_names(workflow: &str) -> Vec<String> {
    let document: serde_yaml::Value = must(serde_yaml::from_str(workflow), "parse generated ci-pr");
    let jobs = document
        .as_mapping()
        .and_then(|root| mapping_value(root, "jobs"))
        .and_then(serde_yaml::Value::as_mapping)
        .expect("workflow jobs mapping");
    let job = mapping_value(jobs, "ci-required")
        .and_then(serde_yaml::Value::as_mapping)
        .expect("generated ci-required job");
    let step = mapping_value(job, "steps")
        .and_then(serde_yaml::Value::as_sequence)
        .and_then(|steps| steps.first())
        .and_then(serde_yaml::Value::as_mapping)
        .expect("generated ci-required verdict step");
    mapping_value(step, "env")
        .and_then(serde_yaml::Value::as_mapping)
        .expect("verdict step environment")
        .keys()
        .filter(|name| name.starts_with("PROVIDER_ADMITTED_"))
        .cloned()
        .collect()
}

fn selected_units_json(
    unit_ids: &[String],
    providers: &[crate::s2::provider::ProviderId],
) -> String {
    let providers = providers
        .iter()
        .map(|provider| format!("\"{}\"", provider.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    let units = unit_ids
        .iter()
        .map(|unit| format!("{{\"unit_id\":\"{unit}\",\"providers\":[{providers}]}}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{units}]")
}

fn ci_needs_json(
    dependencies: &[String],
    results: &std::collections::BTreeMap<String, String>,
    omitted: Option<&str>,
) -> String {
    let fields = dependencies
        .iter()
        .filter(|dependency| omitted != Some(dependency.as_str()))
        .map(|dependency| {
            let result = results
                .get(dependency)
                .expect("every synthetic dependency has a result");
            format!("\"{dependency}\":{{\"result\":\"{result}\"}}")
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{fields}}}")
}

#[cfg(unix)]
fn execute_required_script(
    script: &str,
    needs: &str,
    selected_units: &str,
    admissions: &std::collections::BTreeMap<String, bool>,
) -> std::process::Output {
    use std::os::unix::fs::PermissionsExt as _;

    let root = temporary_directory("execute-generated-required");
    let bin = root.join("bin");
    must(fs::create_dir_all(&bin), "create fake jq directory");
    let jq = bin.join("jq");
    write(
        &jq,
        r#"#!/bin/sh
job=
unit=
provider=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -r|-e) shift ;;
    --arg)
      case "$2" in
        job) job="$3" ;;
        unit) unit="$3" ;;
        provider) provider="$3" ;;
      esac
      shift 3 ;;
    *) query="$1"; shift ;;
  esac
done
input="$(cat)"
if [ -n "$job" ]; then
  printf '%s\n' "$input" | sed -n "s/.*\"$job\":{\"result\":\"\\([^\"]*\\)\"}.*/\\1/p"
  exit 0
fi
if [ -n "$unit" ]; then
  if ! printf '%s\n' "$input" | grep -Fq "\"unit_id\":\"$unit\""; then
    echo 0
  elif [ -n "$provider" ]; then
    if printf '%s\n' "$input" | grep -Fq "\"$provider\""; then echo 1; else echo 0; fi
  elif printf '%s\n' "$input" | grep -Eq '"(velnor|github-self-hosted)"'; then
    echo 1
  else
    echo 0
  fi
fi
"#,
    );
    must(
        fs::set_permissions(&jq, fs::Permissions::from_mode(0o755)),
        "make fake jq executable",
    );
    let old_path = env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&old_path)))
            .expect("join fake jq path");
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(script)
        .current_dir(&root)
        .env("PATH", path)
        .env("NEEDS_JSON", needs)
        .env("SELECTED_UNITS", selected_units)
        .env("PLAN_DIGEST", "test-plan-digest")
        .env("EXCLUDED", "[]");
    for (name, admitted) in admissions {
        command.env(name, admitted.to_string());
    }
    let output = must(command.output(), "execute generated ci-required script");
    let _ = fs::remove_dir_all(root);
    output
}

#[test]
fn live_ci_required_context_cannot_be_hidden_by_candidate_config() {
    let workflow = gated_trusted_job().replace("if: ${{ always() }}", "if: ${{ success() }}");
    let root = velnor_tree("semantic-ci-required-hidden-contract", &workflow);
    let config_path = root.join(GENERATION_CONFIG);
    let mut config = must(fs::read_to_string(&config_path), "read policy config");
    config.push_str("\n[policy]\nci_required = false\n");
    write(&config_path, &config);

    let declared = must(
        read_declared_tree(&safe_root(&root), None, None),
        "read weakened policy config",
    );
    assert!(
        !declared
            .required_checks
            .iter()
            .any(|context| context == "ci-required"),
        "fixture must remove the candidate-declared context"
    );
    let live = ["ci-required".to_owned()];
    let report = required_checks_with_safe_root(&safe_root(&root), &declared, Some(&live));
    assert!(
        !report.passed
            && report
                .details
                .iter()
                .any(|finding| finding.contains("must use `if: always()`")),
        "the live required context must keep the aggregate structure audit active: {:?}",
        report.details
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_second_pull_request_target_workflow_is_refused() {
    let root = velnor_tree("semantic-prt", &gated_trusted_job());
    write(
        &root.join(".github/workflows/rogue.yml"),
        "name: Rogue\non:\n  pull_request_target:\njobs:\n  run:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo rogue\n",
    );
    let audit = must(
        audit_workflows_with_safe_root(&safe_root(&root), None, None),
        "audit tree with a rogue entrypoint",
    );
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
    let audit = must(
        audit_workflows_with_safe_root(&safe_root(&root), None, None),
        "audit tree with allowed job env",
    );
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
    let audit = must(
        audit_workflows_with_safe_root(&safe_root(&root), None, None),
        "audit tree with runner in job env",
    );
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
    let audit = must(
        audit_workflows_with_safe_root(&safe_root(&root), None, None),
        "audit tree with steps in job env",
    );
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
    let error = must_fail(
        run_cli(&[
            std::ffi::OsString::from("--workflow-root"),
            root.as_os_str().to_owned(),
        ]),
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
    let retired = must_fail(
        run_cli(&[
            std::ffi::OsString::from("--candidate-manifest"),
            std::ffi::OsString::from("/candidate.json"),
        ]),
        "retired candidate-manifest option",
    )
    .to_string();
    assert!(retired.contains("--candidate-manifest"), "{retired}");
    assert!(retired.contains("unsupported policy option"), "{retired}");
    let pin_build = must_fail(
        run_cli(&[std::ffi::OsString::from("--pin-build")]),
        "policy must not build or execute a candidate renderer",
    )
    .to_string();
    assert!(pin_build.contains("--pin-build"), "{pin_build}");
    assert!(pin_build.contains("never executes"), "{pin_build}");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn tree_comparison_rejects_unowned_workflows_without_exemptions() {
    let root = temporary_directory("comparison-unowned-workflow");
    let scratch = temporary_directory("comparison-unowned-workflow-scratch");
    let binary = root.join("empty-renderer");
    write(
        &root.join(".github/workflows/legacy.yml"),
        "name: Legacy\non: push\njobs: {}\n",
    );
    write(&binary, "#!/bin/sh\nexit 0\n");
    {
        use std::os::unix::fs::PermissionsExt as _;
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark empty renderer executable",
        );
    }
    let differences = must(
        render_and_compare(&binary, &root, &root, &scratch, "main"),
        "compare unowned workflow",
    );
    assert!(
        differences.iter().any(|finding| {
            finding.contains(".github/workflows/legacy.yml")
                && finding.contains("not generator-owned")
        }),
        "no config field can hide an unowned workflow: {differences:?}"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(scratch);
}

#[test]
fn candidate_render_drift_can_pass_the_base_pinned_static_audit() {
    let providers = [
        crate::s2::provider::ProviderId::GithubHosted,
        crate::s2::provider::ProviderId::Velnor,
    ];
    let (pinned_render, _) = generated_ci_pr_fixture(&providers);
    let candidate_render = format!("# reviewed semantic-only candidate change\n{pinned_render}");
    assert_ne!(candidate_render, pinned_render);

    let root = velnor_tree("semantic-candidate-render-drift", &candidate_render);
    write(
        &root.join(POLICY_ENTRYPOINT),
        &hosted_entrypoint_for_repository(PIN_A, "example/consumer"),
    );
    let options = PolicyOptions {
        head_sha: Some(PIN_A.to_owned()),
        base_sha: Some(PIN_A.to_owned()),
        base_revision: PIN_A.to_owned(),
        ruleset_contexts: Some(vec!["ci-required".to_owned(), "Policy".to_owned()]),
        trusted_repository: Some("example/consumer".to_owned()),
        trusted_default_branch: Some("main".to_owned()),
    };
    let report = must(
        evaluate_with_safe_root(&options, safe_root(&root)),
        "evaluate semantically safe render drift",
    );
    assert!(report.passed(), "{}", report.render());
    let audit = must_some(
        report.rule("candidate-static-audit"),
        "candidate-static-audit rule",
    );
    assert!(audit.passed, "{}", audit.reason);
    assert!(
        audit.reason.contains("not obtained or executed"),
        "the report states that policy did not invoke the candidate renderer: {}",
        audit.reason
    );
    let _ = fs::remove_dir_all(root);
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
            name: "Policy",
            revision: PIN_A,
            repository,
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

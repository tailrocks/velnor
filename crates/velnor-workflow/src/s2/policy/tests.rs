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

#[test]
fn runner_policy_requires_exact_hosted_labels_and_declared_local_selectors() {
    let mut selectors = BTreeMap::new();
    selectors.insert(
        "github-hosted".to_owned(),
        vec!["ubuntu-private".to_owned()],
    );
    selectors.insert(
        "velnor".to_owned(),
        vec!["self-hosted".to_owned(), "velnor-target-mvp".to_owned()],
    );
    let policy = VelnorPolicyContract {
        selectors,
        ..VelnorPolicyContract::default()
    };

    for label in ["ubuntu-24.04", "ubuntu-26.04", "xcode-27"] {
        let analysis = classify_static_label(label, &policy);
        assert!(!analysis.foreign, "supported exact label {label:?}");
        assert!(!analysis.local_provider, "supported exact label {label:?}");
    }
    for label in ["ubuntu-private", "self-hosted"] {
        let analysis = classify_static_label(label, &policy);
        assert!(analysis.foreign, "unverified label {label:?}");
    }
    let local = classify_static_labels(&["self-hosted", "velnor-target-mvp"], &policy);
    assert!(local.local_provider, "exact Velnor selector stays local");
    assert!(!local.foreign, "exact Velnor selector is declared");

    let grouped: Value =
        serde_yaml::from_str("group: hosted-looking-group\nlabels: ubuntu-24.04\n")
            .expect("valid runner-group mapping");
    let mut resolving = BTreeSet::new();
    let analysis = analyze_runner(&grouped, None, &mut resolving, &policy);
    assert!(analysis.foreign, "group membership has no trusted contract");
}

#[test]
fn runner_policy_rejects_custom_label_in_declared_hosted_selector() {
    let policy = VelnorPolicyContract {
        providers: vec!["github-hosted".to_owned()],
        selectors: BTreeMap::from([(
            "github-hosted".to_owned(),
            vec!["ubuntu-private".to_owned()],
        )]),
        ..VelnorPolicyContract::default()
    };
    let error = must_fail(policy.validate(), "custom hosted label selector");
    assert!(
        error
            .to_string()
            .contains("unsupported label in the `github-hosted` selector"),
        "{error}"
    );
}

#[test]
fn generator_runner_contract_api_rejects_missing_or_empty_jobs() {
    let config = hosted_project_config(PIN_A);
    for (name, workflow) in [
        ("missing", "name: Missing jobs\non: push\n"),
        ("empty", "name: Empty jobs\non: push\njobs: {}\n"),
    ] {
        let error = must_fail(
            workflow_runner_environment_matches(workflow, &config, &BTreeMap::new()),
            name,
        )
        .to_string();
        assert!(error.contains("jobs mapping"), "{name}: {error}");
    }
}

#[test]
fn generator_runner_contract_api_rejects_uncovered_dynamic_triggers() {
    let mut config = hosted_project_config(PIN_A);
    let local_provider = crate::s2::provider::ProviderId::Velnor;
    config.providers.insert(local_provider);
    config.selectors.insert(
        local_provider,
        crate::s2::provider::ProviderSelector {
            runs_on: vec!["self-hosted".to_owned(), "velnor-target-mvp".to_owned()],
        },
    );
    let shape = crate::s2::estate::APPROVED_DYNAMIC_RUNNERS[0];
    let direct = format!(
        "name: Dynamic\non: workflow_run\njobs:\n  check:\n    runs-on: >-\n      ${{{{ {shape} }}}}\n    steps:\n      - run: echo user code\n"
    );
    let error = must_fail(
        workflow_runner_environment_matches(&direct, &config, &BTreeMap::new()),
        "dynamic unsupported trigger",
    )
    .to_string();
    assert!(error.contains("unsupported trigger"), "{error}");

    let matrix = format!(
        "name: Dynamic matrix\non: workflow_run\njobs:\n  check:\n    strategy:\n      matrix:\n        runner:\n          - >-\n            ${{{{ {shape} }}}}\n    runs-on: ${{{{ matrix.runner }}}}\n    steps:\n      - run: echo user code\n"
    );
    let error = must_fail(
        workflow_runner_environment_matches(&matrix, &config, &BTreeMap::new()),
        "matrix dynamic unsupported trigger",
    )
    .to_string();
    assert!(error.contains("unsupported trigger"), "{error}");
}

#[test]
fn runner_guard_generation_requires_the_shared_pre_action_allowlist() {
    let config = hosted_project_config(PIN_A);
    let unknown = format!(
        "name: Unknown action\non: push\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: example/action@{PIN_B}\n"
    );
    let error = must_fail(
        workflow_runner_environment_matches(&unknown, &config, &BTreeMap::new()),
        "unknown full-SHA action before runner guard",
    )
    .to_string();
    assert!(error.contains("reviewed no-pre allowlist"), "{error}");

    let approved = crate::s2::ActionPin::Checkout
        .reference()
        .split_once(" #")
        .map_or(
            crate::s2::ActionPin::Checkout.reference(),
            |(reference, _)| reference,
        );
    let known = format!(
        "name: Known action\non: push\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: {approved}\n"
    );
    must(
        workflow_runner_environment_matches(&known, &config, &BTreeMap::new()),
        "current shared pre-action allowlist reference",
    );
}

#[test]
fn runner_guard_generation_validates_local_actions_from_the_file_snapshot() {
    let config = hosted_project_config(PIN_A);
    let checkout = crate::s2::ActionPin::Checkout
        .reference()
        .split_once(" #")
        .map_or(
            crate::s2::ActionPin::Checkout.reference(),
            |(reference, _)| reference,
        );
    let local_reference = "./.github/actions/local";
    let manifest =
        format!("name: local\nruns:\n  using: composite\n  steps:\n    - uses: {checkout}\n");
    let files = BTreeMap::from([(PathBuf::from(".github/actions/local/action.yml"), manifest)]);
    let manifests = runner_guard_action_manifest_snapshot(&files)
        .expect("snapshot local action metadata from generated file map");
    assert!(manifests.contains_key(local_reference));
    let workflow = format!(
        "name: Local action\non: push\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: {local_reference}\n"
    );
    must(
        workflow_runner_environment_matches(&workflow, &config, &manifests),
        "generated local composite action with reviewed nested action",
    );

    let missing = must_fail(
        workflow_runner_environment_matches(&workflow, &config, &BTreeMap::new()),
        "generated local action without file snapshot metadata",
    )
    .to_string();
    assert!(missing.contains("manifest is missing"), "{missing}");

    let nested_unknown = format!(
        "name: local\nruns:\n  using: composite\n  steps:\n    - uses: example/action@{PIN_B}\n"
    );
    let bad_files = BTreeMap::from([(
        PathBuf::from(".github/actions/local/action.yml"),
        nested_unknown,
    )]);
    let bad_manifests = runner_guard_action_manifest_snapshot(&bad_files)
        .expect("snapshot nested unknown action fixture");
    let error = must_fail(
        workflow_runner_environment_matches(&workflow, &config, &bad_manifests),
        "nested unknown external ref in generated action",
    )
    .to_string();
    assert!(error.contains("reviewed no-pre allowlist"), "{error}");
}

#[test]
fn runner_guard_generation_requires_the_bound_release_chain_for_workflow_run() {
    let config = bound_release_project_config(PIN_A);
    let runner_gate = "(github.event_name == 'push' && (github.ref_type == 'tag' || github.ref == 'refs/heads/main')) || github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main')";
    let condition = format!(
        "(({runner_gate}) || (github.event_name == 'workflow_run' && needs.publish-gate.outputs.admitted == 'true')) && ({})",
        trusted_event_conjunct()
    );
    let uninstrumented = uninstrumented_bound_release_workflow_run_fixture(&condition);
    must(
        workflow_runner_environment_matches(&uninstrumented, &config, &BTreeMap::new()),
        "valid generated Velnor-only bound-release workflow",
    );

    let unbound = "name: Unbound\non:\n  workflow_run:\n    workflows: [CI]\n    types: [completed]\n    branches: [main]\njobs:\n  check:\n    runs-on: [self-hosted, example-runner]\n    steps:\n      - run: echo unsafe\n";
    let error = must_fail(
        workflow_runner_environment_matches(unbound, &config, &BTreeMap::new()),
        "unbound static local workflow_run trigger",
    )
    .to_string();
    assert!(error.contains("bound-release admission chain"), "{error}");

    let trusted_only = uninstrumented_bound_release_workflow_run_fixture(trusted_event_conjunct());
    let error = must_fail(
        workflow_runner_environment_matches(&trusted_only, &config, &BTreeMap::new()),
        "broad trusted-event gate without producer admission",
    )
    .to_string();
    assert!(error.contains("bound-release admission chain"), "{error}");

    let wrong_head_repository = uninstrumented.replace(
        "PRODUCER_HEAD_REPOSITORY_ID: ${{ github.event.workflow_run.head_repository.id }}",
        "PRODUCER_HEAD_REPOSITORY_ID: ${{ github.event.workflow_run.repository.id }}",
    );
    let error = must_fail(
        workflow_runner_environment_matches(&wrong_head_repository, &config, &BTreeMap::new()),
        "release gate without workflow_run head_repository identity",
    )
    .to_string();
    assert!(error.contains("bound-release admission chain"), "{error}");

    let mut mixed_provider_config = config.clone();
    mixed_provider_config
        .providers
        .insert(crate::s2::provider::ProviderId::GithubHosted);
    let error = must_fail(
        workflow_runner_environment_matches(
            &uninstrumented,
            &mixed_provider_config,
            &BTreeMap::new(),
        ),
        "workflow_run cannot route local jobs in a mixed-provider tree",
    )
    .to_string();
    assert!(error.contains("bound-release admission chain"), "{error}");
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

fn hosted_project_config(revision: &str) -> ProjectConfig {
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
    config
}

fn bound_release_project_config(revision: &str) -> ProjectConfig {
    let mut config = hosted_project_config(revision);
    let velnor = crate::s2::provider::ProviderId::Velnor;
    config.providers = [velnor].into_iter().collect();
    config.automatic_providers = config.providers.clone();
    config.selectors = BTreeMap::from([(
        velnor,
        crate::s2::provider::ProviderSelector {
            runs_on: vec!["self-hosted".to_owned(), "example-runner".to_owned()],
        },
    )]);
    config.default_branch = "main".to_owned();
    config.release = Some(crate::s2::ReleaseSpec {
        producer_workflow: "CI".to_owned(),
        producer_workflow_id: 42,
        producer_workflow_path: ".github/workflows/ci.yml".to_owned(),
        ..crate::s2::ReleaseSpec::default()
    });
    config
}

fn hosted_entrypoint(revision: &str) -> String {
    let config = hosted_project_config(revision);
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
    let hosted_policy = runner_policy_fixture();
    let entrypoint = with_runner_guards(&hosted_entrypoint(PIN_A), &hosted_policy);
    write(&root.join(POLICY_ENTRYPOINT), &entrypoint);
    root
}

/// A pull-request aggregate with a hosted required job and one Velnor job
/// on the declared selector, carrying the generated provider admission: a
/// provider-selecting dispatch on any ref or the automatic events, with the
/// trusted-event conjunct.
fn gated_trusted_job() -> String {
    format!(
        "name: CI / PR\non:\n  pull_request:\njobs:\n  ci-required:\n    name: ci-required\n    runs-on: ubuntu-24.04\n    steps:\n__HOSTED_GUARD__      - run: echo ok\n        if: ${{{{ runner.environment == 'github-hosted' }}}}\n  velnor-docker:\n    name: Docker\n    if: ${{{{ (!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot'))) }}}}\n    runs-on: [{VELNOR_SELECTOR}]\n    steps:\n__LOCAL_GUARD__      - run: echo trusted\n        if: ${{{{ runner.environment == 'self-hosted' }}}}\n"
    )
    .replace("__HOSTED_GUARD__", &hosted_guard())
    .replace("__LOCAL_GUARD__", &self_hosted_guard())
}

fn runner_policy_fixture() -> VelnorPolicyContract {
    VelnorPolicyContract {
        providers: vec!["github-hosted".to_owned(), "velnor".to_owned()],
        automatic_providers: vec!["github-hosted".to_owned(), "velnor".to_owned()],
        selectors: BTreeMap::from([
            (
                "github-hosted".to_owned(),
                vec!["ubuntu-24.04".to_owned(), "ubuntu-26.04".to_owned()],
            ),
            (
                "velnor".to_owned(),
                vec!["self-hosted".to_owned(), "velnor-target-mvp".to_owned()],
            ),
        ]),
        default_branch: "main".to_owned(),
        ..VelnorPolicyContract::default()
    }
}

fn bound_release_runner_policy_fixture() -> VelnorPolicyContract {
    VelnorPolicyContract {
        providers: vec!["velnor".to_owned()],
        automatic_providers: vec!["velnor".to_owned()],
        selectors: BTreeMap::from([(
            "velnor".to_owned(),
            vec!["self-hosted".to_owned(), "example-runner".to_owned()],
        )]),
        default_branch: "main".to_owned(),
        release_producer_workflow: Some("CI".to_owned()),
        release_producer_workflow_id: Some(42),
        release_producer_workflow_path: Some(".github/workflows/ci.yml".to_owned()),
        ..VelnorPolicyContract::default()
    }
}

fn bound_release_workflow_run_fixture(check_condition: &str) -> String {
    let condition = format!("    if: ${{{{ {check_condition} }}}}\n");
    let source_run = indent_workflow_script(BOUND_RELEASE_SOURCE_RUN);
    let admit_run = indent_workflow_script(BOUND_RELEASE_ADMIT_RUN);
    r#"name: Bound release
on:
  push:
    branches: [main]
  workflow_dispatch:
  workflow_run:
    workflows: [CI]
    types: [completed]
    branches: [main]
jobs:
  source:
    name: Resolve release source
    runs-on: [self-hosted, example-runner]
    timeout-minutes: 5
    outputs:
      sha: ${{ steps.resolve.outputs.sha }}
    steps:
__SOURCE_GUARD__      - name: Resolve source revision
        id: resolve
        if: '${{ (runner.environment==''self-hosted'') }}'
        env:
          EVENT: ${{ github.event_name }}
          SHA: ${{ github.sha }}
          RUN_SHA: ${{ github.event.workflow_run.head_sha }}
          RUN_ID: ${{ github.event.workflow_run.id }}
          SOURCE_SHA: ${{ github.event.workflow_run.head_sha }}
        run: |
__SOURCE_RUN__
  publish-gate:
    name: Admit rolling publish
    needs: source
    runs-on: [self-hosted, example-runner]
    timeout-minutes: 10
    outputs:
      admitted: ${{ steps.admit.outputs.admitted }}
      mode: ${{ steps.admit.outputs.mode }}
      sha: ${{ needs.source.outputs.sha }}
    steps:
__GATE_GUARD__      - name: Admit producer or resolve drill mode
        id: admit
        if: '${{ (runner.environment==''self-hosted'') }}'
        env:
          EVENT: ${{ github.event_name }}
          REF: ${{ github.ref }}
          PRODUCER: ${{ github.event.workflow_run.name }}
          CONCLUSION: ${{ github.event.workflow_run.conclusion }}
          STATUS: ${{ github.event.workflow_run.status }}
          PRODUCER_REPOSITORY: ${{ github.event.workflow_run.repository.full_name }}
          PRODUCER_REPOSITORY_ID: ${{ github.event.workflow_run.repository.id }}
          PRODUCER_HEAD_REPOSITORY: ${{ github.event.workflow_run.head_repository.full_name }}
          PRODUCER_HEAD_REPOSITORY_ID: ${{ github.event.workflow_run.head_repository.id }}
          EXPECTED_REPOSITORY: ${{ github.repository }}
          EXPECTED_REPOSITORY_ID: ${{ github.repository_id }}
          WORKFLOW_ID: ${{ github.event.workflow_run.workflow_id }}
          WORKFLOW_PATH: ${{ github.event.workflow_run.path }}
          EXPECTED_WORKFLOW_ID: 42
          EXPECTED_WORKFLOW_PATH: '.github/workflows/ci.yml'
          PRODUCER_EVENT: ${{ github.event.workflow_run.event }}
          PRODUCER_BRANCH: ${{ github.event.workflow_run.head_branch }}
          EXPECTED_EVENT: push
          EXPECTED_BRANCH: main
          EXPECTED_REF: refs/heads/main
          RUN_ID: ${{ github.event.workflow_run.id }}
          HEAD_SHA: ${{ github.event.workflow_run.head_sha }}
          RUN_SHA: ${{ github.event.workflow_run.head_sha }}
          SOURCE_SHA: ${{ needs.source.outputs.sha }}
          MODE_INPUT: ${{ github.event_name == 'workflow_dispatch' && inputs.mode || '' }}
          EXPECTED: CI
          BRANCH: main
        run: |
__ADMIT_RUN__
  check:
    name: Build bound source
    needs: [source, publish-gate]
__CHECK_CONDITION__    runs-on: [self-hosted, example-runner]
    steps:
__CHECK_GUARD__      - name: Build admitted source
        if: ${{ runner.environment == 'self-hosted' }}
        run: echo safe
"#
    .replace("__SOURCE_GUARD__", &self_hosted_guard())
    .replace("__GATE_GUARD__", &self_hosted_guard())
    .replace("__CHECK_GUARD__", &self_hosted_guard())
    .replace("__SOURCE_RUN__", &source_run)
    .replace("__ADMIT_RUN__", &admit_run)
    .replace("__CHECK_CONDITION__", &condition)
}

fn uninstrumented_bound_release_workflow_run_fixture(check_condition: &str) -> String {
    bound_release_workflow_run_fixture(check_condition)
        .replace(&self_hosted_guard(), "")
        .replace(
            "        if: '${{ (runner.environment==''self-hosted'') }}'\n",
            "",
        )
        .replace(
            "        if: ${{ runner.environment == 'self-hosted' }}\n",
            "",
        )
}

fn indent_workflow_script(script: &str) -> String {
    script
        .lines()
        .map(|line| format!("          {line}\n"))
        .collect()
}

fn audit_runner_fixture(yaml: &str, policy: &VelnorPolicyContract) -> WorkflowAudit {
    let workflow: Value = must(serde_yaml::from_str(yaml), "parse runner policy fixture");
    let workflow = workflow.as_mapping().expect("workflow mapping");
    let jobs = mapping_value(workflow, "jobs").expect("workflow jobs");
    let mut failures = PolicyFindings::default();
    inspect_jobs(
        jobs,
        mapping_value(workflow, "on"),
        Path::new(".github/workflows/runner-fixture.yml"),
        policy,
        &mut failures,
    );
    failures.audit
}

fn with_runner_guards(content: &str, policy: &VelnorPolicyContract) -> String {
    let mut document: Value = must(
        serde_yaml::from_str(content),
        "parse guarded workflow fixture",
    );
    let workflow = document.as_mapping_mut().expect("workflow mapping");
    let jobs = mapping_value(workflow, "jobs")
        .and_then(Value::as_mapping)
        .cloned()
        .expect("workflow jobs");
    let mut jobs = jobs;
    for (_, job_value) in jobs.iter_mut() {
        let Some(job) = job_value.as_mapping().cloned() else {
            continue;
        };
        if mapping_value(&job, "uses").is_some() {
            continue;
        }
        let Some(runs_on) = mapping_value(&job, "runs-on") else {
            continue;
        };
        let matrix = mapping_value(&job, "strategy")
            .and_then(Value::as_mapping)
            .and_then(|strategy| mapping_value(strategy, "matrix"))
            .and_then(Value::as_mapping);
        let Some(expected) = runner_environment_expectation(runs_on, matrix, policy) else {
            continue;
        };
        let predicate = expected.predicate();
        let Some(mut steps) = mapping_value(&job, "steps")
            .and_then(Value::as_sequence)
            .cloned()
        else {
            continue;
        };
        let mut guard = Mapping::new();
        guard.insert("id", Value::String("runner_provenance".to_owned()));
        guard.insert(
            "shell",
            Value::String("bash --noprofile --norc -p -e -o pipefail {0}".to_owned()),
        );
        guard.insert(
            "working-directory",
            Value::String(crate::primitives::runner_guard::GUARD_WORKING_DIRECTORY.to_owned()),
        );
        guard.insert("env", runner_guard_environment_value());
        guard.insert("if", Value::String(format!("${{{{ !({predicate}) }}}}")));
        guard.insert("run", Value::String("((0))".to_owned()));
        let mut guarded_steps = vec![Value::Mapping(guard)];
        for step in &mut steps {
            if let Some(step) = step.as_mapping_mut() {
                let body = mapping_value(step, "if")
                    .and_then(Value::as_str)
                    .map(normalize_runner_expression)
                    .unwrap_or_default();
                let condition = if body.is_empty() {
                    format!("${{{{ ({predicate}) }}}}")
                } else {
                    format!("${{{{ ({body}) && ({predicate}) }}}}")
                };
                step.insert("if", Value::String(condition));
            }
        }
        guarded_steps.extend(steps);
        let mut job = job;
        job.insert("steps", Value::Sequence(guarded_steps));
        *job_value = Value::Mapping(job);
    }
    workflow.insert("jobs", Value::Mapping(jobs));
    must(
        serde_yaml::to_string(&document),
        "serialize guarded workflow fixture",
    )
}

fn runner_fixture(runs_on: &str, steps: &str, extra_job_fields: &str) -> String {
    format!(
        "name: Runner fixture\non: push\njobs:\n  check:\n{extra_job_fields}    runs-on: {runs_on}\n    steps:\n{steps}"
    )
}

fn runner_guard_environment_value() -> Value {
    Value::Mapping(Mapping::from_iter(
        crate::primitives::runner_guard::GUARD_SANITIZED_ENVIRONMENT
            .iter()
            .map(|key| ((*key).into(), Value::String(String::new()))),
    ))
}

fn runner_guard_environment_yaml() -> String {
    let entries = crate::primitives::runner_guard::GUARD_SANITIZED_ENVIRONMENT
        .iter()
        .map(|key| format!("{key}: \"\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{entries}}}")
}

fn runner_guard(expected: &str) -> String {
    runner_guard_with_condition(&format!("runner.environment != '{expected}'"))
}

fn runner_guard_with_condition(condition: &str) -> String {
    format!(
        "      - name: Verify runner environment\n        id: runner_provenance\n        shell: bash --noprofile --norc -p -e -o pipefail {{0}}\n        working-directory: '{}'\n        env: {}\n        if: ${{{{ {condition} }}}}\n        run: ((0))\n",
        crate::primitives::runner_guard::GUARD_WORKING_DIRECTORY,
        runner_guard_environment_yaml(),
    )
}

fn hosted_guard() -> String {
    runner_guard("github-hosted")
}

fn self_hosted_guard() -> String {
    runner_guard("self-hosted")
}

fn with_hosted_guard(workflow: &str) -> String {
    if workflow.contains("id: runner_provenance") {
        return workflow.to_owned();
    }
    workflow.replacen(
        "    steps:\n",
        &format!("    steps:\n{}", hosted_guard()),
        1,
    )
}

#[test]
fn provenance_guard_is_required_for_exact_hosted_and_local_runner_classes() {
    let policy = runner_policy_fixture();
    let missing = runner_fixture("ubuntu-24.04", "      - run: echo user code\n", "");
    let audit = audit_runner_fixture(&missing, &policy);
    assert!(
        audit
            .runners
            .iter()
            .any(|finding| finding.contains("first step must")),
        "missing first guard must fail: {:?}",
        audit.runners
    );

    for hosted in ["ubuntu-24.04", "ubuntu-26.04", "xcode-27"] {
        let workflow = runner_fixture(hosted, &hosted_guard(), "");
        let audit = audit_runner_fixture(&workflow, &policy);
        assert!(audit.runners.is_empty(), "{hosted}: {:?}", audit.runners);
    }

    let local_gate = "    if: ${{ (!(github.event_name=='pull_request'&&(github.event.pull_request.head.repo.fork||github.event.pull_request.user.type=='Bot'))) }}\n";
    let local = runner_fixture(
        "[self-hosted, velnor-target-mvp]",
        &self_hosted_guard(),
        local_gate,
    );
    let audit = audit_runner_fixture(&local, &policy);
    assert!(
        audit.runners.is_empty(),
        "exact local selector: {:?}",
        audit.runners
    );

    for unknown in ["ubuntu-private", "self-hosted"] {
        let workflow = runner_fixture(unknown, &hosted_guard(), "");
        let audit = audit_runner_fixture(&workflow, &policy);
        assert!(
            audit.runners.iter().any(|finding| {
                finding.contains("one verified runner environment class")
                    || finding.contains("does not match any declared provider selector")
            }),
            "unknown runner {unknown:?} must fail: {:?}",
            audit.runners
        );
    }
}

#[test]
fn local_runner_gate_rejects_untrusted_suffix_false() {
    let policy = runner_policy_fixture();
    let condition = "    if: ${{ true || github.event_name == 'pull_request' && false }}\n";
    let workflow = runner_fixture(
        "[self-hosted, velnor-target-mvp]",
        &self_hosted_guard(),
        condition,
    );
    let audit = audit_runner_fixture(&workflow, &policy);
    assert!(
        audit
            .runners
            .iter()
            .any(|finding| finding.contains("local-provider jobs require a trusted-event gate")),
        "a trailing false cannot prove a trusted gate: {:?}",
        audit.runners
    );
}

#[test]
fn workflow_run_local_runner_requires_the_exact_bound_release_admission_chain() {
    let policy = bound_release_runner_policy_fixture();
    let default_branch = policy.default_branch.as_str();
    let runner_gate = format!(
        "(github.event_name == 'push' && (github.ref_type == 'tag' || github.ref == 'refs/heads/{default_branch}')) || github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{default_branch}')"
    );
    let condition = format!(
        "(({runner_gate}) || (github.event_name == 'workflow_run' && needs.publish-gate.outputs.admitted == 'true')) && ({})",
        trusted_event_conjunct()
    );
    let generated = bound_release_workflow_run_fixture(&condition);
    let audit = audit_runner_fixture(&generated, &policy);
    assert!(
        audit.runners.is_empty(),
        "the renderer's source, producer-admission, and downstream gate must pass: {:?}",
        audit.runners
    );

    let trusted_only = bound_release_workflow_run_fixture(trusted_event_conjunct());
    let audit = audit_runner_fixture(&trusted_only, &policy);
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("local-provider jobs require a trusted-event gate")
        }),
        "a broad trusted-event predicate cannot admit workflow_run: {:?}",
        audit.runners
    );

    let admitted_but_or_bypassed = condition.replace(
        "(github.event_name == 'workflow_run' && needs.publish-gate.outputs.admitted == 'true')",
        "(true || (github.event_name == 'workflow_run' && needs.publish-gate.outputs.admitted == 'true'))",
    );
    let bypassed = bound_release_workflow_run_fixture(&admitted_but_or_bypassed);
    assert!(
        !audit_runner_fixture(&bypassed, &policy).runners.is_empty(),
        "an OR branch must not bypass producer admission"
    );

    let missing_dependency = generated.replace("needs: [source, publish-gate]", "needs: [source]");
    assert!(
        !audit_runner_fixture(&missing_dependency, &policy)
            .runners
            .is_empty(),
        "the admitted output is unusable without a direct job dependency"
    );

    let wrong_producer = generated.replace("workflows: [CI]", "workflows: [Other]");
    assert!(
        !audit_runner_fixture(&wrong_producer, &policy)
            .runners
            .is_empty(),
        "workflow_run display-name matching must use configured producer identity"
    );

    let wrong_workflow_id =
        generated.replace("EXPECTED_WORKFLOW_ID: 42", "EXPECTED_WORKFLOW_ID: 43");
    assert!(
        !audit_runner_fixture(&wrong_workflow_id, &policy)
            .runners
            .is_empty(),
        "producer gate must bind the configured Actions workflow id"
    );

    let wrong_head_repository = generated.replace(
        "PRODUCER_HEAD_REPOSITORY_ID: ${{ github.event.workflow_run.head_repository.id }}",
        "PRODUCER_HEAD_REPOSITORY_ID: ${{ github.event.workflow_run.repository.id }}",
    );
    assert!(
        !audit_runner_fixture(&wrong_head_repository, &policy)
            .runners
            .is_empty(),
        "workflow_run admission must prove the event's head_repository identity"
    );
}

#[test]
fn provenance_guard_blocks_always_steps_without_the_class_predicate() {
    let policy = runner_policy_fixture();
    let unconditional = format!(
        "{}      - name: Unconditional command\n        run: echo command\n",
        hosted_guard()
    );
    let audit = audit_runner_fixture(&runner_fixture("ubuntu-24.04", &unconditional, ""), &policy);
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("step Unconditional command")
                && finding.contains("expected runner.environment predicate")
        }),
        "an omitted condition must fail provenance: {:?}",
        audit.runners
    );

    let bypass = format!(
        "{}      - name: Always cleanup\n        if: always()\n        run: echo cleanup\n",
        hosted_guard()
    );
    let audit = audit_runner_fixture(&runner_fixture("ubuntu-24.04", &bypass, ""), &policy);
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("step Always cleanup")
                && finding.contains("expected runner.environment predicate")
        }),
        "always() must not bypass provenance: {:?}",
        audit.runners
    );

    let gated = format!(
        "{}      - name: Always cleanup\n        if: ${{{{ always() && runner.environment == 'github-hosted' }}}}\n        run: echo cleanup\n",
        hosted_guard()
    );
    let audit = audit_runner_fixture(&runner_fixture("ubuntu-24.04", &gated, ""), &policy);
    assert!(
        audit.runners.is_empty(),
        "explicit class gate: {:?}",
        audit.runners
    );
}

#[test]
fn provenance_guard_cannot_be_disabled_with_continue_on_error() {
    let policy = runner_policy_fixture();
    let guard = "      - id: runner_provenance\n        shell: bash --noprofile --norc -p -e -o pipefail {0}\n        env: {BASH_ENV: \"\", SHELLOPTS: \"\", BASHOPTS: \"\"}\n        if: ${{ runner.environment != 'github-hosted' }}\n        continue-on-error: true\n        run: ((0))\n";
    let audit = audit_runner_fixture(
        &runner_fixture("ubuntu-24.04", guard, "    continue-on-error: true\n"),
        &policy,
    );
    assert!(
        audit
            .runners
            .iter()
            .any(|finding| finding.contains("job continue-on-error")),
        "job continue-on-error must fail: {:?}",
        audit.runners
    );
    assert!(
        audit
            .runners
            .iter()
            .any(|finding| finding.contains("guard must not use continue-on-error")),
        "guard continue-on-error must fail: {:?}",
        audit.runners
    );
}

#[test]
fn provenance_guard_requires_bash_and_a_single_nonzero_exit() {
    let policy = runner_policy_fixture();
    let no_shell = "      - id: runner_provenance\n        if: ${{ runner.environment != 'github-hosted' }}\n        run: ((0))\n";
    let audit = audit_runner_fixture(&runner_fixture("ubuntu-24.04", no_shell, ""), &policy);
    assert!(
        audit
            .runners
            .iter()
            .any(|finding| finding.contains("privileged Bash shell template")),
        "guard without explicit bash must fail: {:?}",
        audit.runners
    );

    let default_bash = "      - id: runner_provenance\n        shell: bash\n        env: {BASH_ENV: \"\", SHELLOPTS: \"\", BASHOPTS: \"\"}\n        if: ${{ runner.environment != 'github-hosted' }}\n        run: ((0))\n";
    let audit = audit_runner_fixture(&runner_fixture("ubuntu-24.04", default_bash, ""), &policy);
    assert!(
        audit
            .runners
            .iter()
            .any(|finding| finding.contains("privileged Bash shell template")),
        "default Bash template imports inherited shell functions"
    );

    for run in ["exit 1", "exit 0; exit 1", "exit 0\nexit 1", "exit 256"] {
        assert!(
            !run_guarantees_nonzero(run),
            "unsafe guard script must fail closed: {run:?}"
        );
    }
    assert!(run_guarantees_nonzero("((0))"));
}

#[test]
fn provenance_guard_clears_bash_startup_environment_exactly() {
    let policy = runner_policy_fixture();
    let clean_guard = hosted_guard();
    let clean_env = format!("        env: {}\n", runner_guard_environment_yaml());
    for env in [
        None,
        Some("{}"),
        Some("{BASH_ENV: \"\", BASHOPTS: \"\"}"),
        Some("{BASH_ENV: /tmp/hostile.sh}"),
        Some("{BASH_ENV: \"\", SHELLOPTS: noexec, BASHOPTS: \"\"}"),
        Some("{BASH_ENV: \"\", SHELLOPTS: \"\", BASHOPTS: \"\", EXTRA: value}"),
    ] {
        let guard = env.map_or_else(
            || clean_guard.replace(&clean_env, ""),
            |env| clean_guard.replace(&clean_env, &format!("        env: {env}\n")),
        );
        let audit = audit_runner_fixture(&runner_fixture("ubuntu-24.04", &guard, ""), &policy);
        assert!(
            audit
                .runners
                .iter()
                .any(|finding| finding.contains("clear every shell-startup and loader override")),
            "unsafe Bash startup environment must fail: {env:?}: {:?}",
            audit.runners
        );
    }
    assert!(
        audit_runner_fixture(
            &runner_fixture("ubuntu-24.04", &hosted_guard(), ""),
            &policy
        )
        .runners
        .is_empty(),
        "the exact empty shell-startup and loader overrides allow the generated guard"
    );

    let wrong_directory = hosted_guard().replace(
        &format!(
            "working-directory: '{}'",
            crate::primitives::runner_guard::GUARD_WORKING_DIRECTORY
        ),
        "working-directory: .",
    );
    let audit = audit_runner_fixture(
        &runner_fixture("ubuntu-24.04", &wrong_directory, ""),
        &policy,
    );
    assert!(
        audit
            .runners
            .iter()
            .any(|finding| { finding.contains("runner-owned temporary working directory") }),
        "guard must execute from the runner-owned temporary directory: {:?}",
        audit.runners
    );
}

#[cfg(unix)]
#[test]
fn runner_guard_failure_survives_inherited_noexec_and_exit_function() {
    let inherited_noexec = Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-e",
            "-o",
            "pipefail",
            "-c",
            "((0))",
        ])
        .env("SHELLOPTS", "noexec")
        .env("BASH_ENV", "")
        .output()
        .expect("run Bash with inherited noexec");
    assert_eq!(
        inherited_noexec.status.code(),
        Some(0),
        "inherited SHELLOPTS=noexec skips the guard body"
    );

    let guarded = Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-p",
            "-e",
            "-o",
            "pipefail",
            "-c",
            "((0))",
        ])
        .env("BASH_ENV", "")
        .env("SHELLOPTS", "noexec")
        .env("BASHOPTS", "extdebug")
        .env("BASH_FUNC_exit%%", "() { return 0; }")
        .env("BASH_FUNC_builtin%%", "() { return 0; }")
        .output()
        .expect("run hardened Bash guard");
    assert_eq!(
        guarded.status.code(),
        Some(1),
        "arithmetic failure cannot be bypassed by imported exit function"
    );
}

#[test]
fn container_and_service_jobs_cannot_claim_runner_provenance() {
    let policy = runner_policy_fixture();
    for field in [
        "container: ubuntu:latest",
        "services:\n      database:\n        image: postgres",
    ] {
        let audit = audit_runner_fixture(
            &runner_fixture("ubuntu-24.04", &hosted_guard(), &format!("    {field}\n")),
            &policy,
        );
        assert!(
            audit
                .runners
                .iter()
                .any(|finding| finding.contains("container or services")),
            "container/service must fail provenance audit: {field}: {:?}",
            audit.runners
        );
    }
}

#[test]
fn runner_provenance_resolves_only_homogeneous_finite_matrix_classes() {
    let policy = runner_policy_fixture();
    let hosted_matrix =
        "    strategy:\n      matrix:\n        runner: [ubuntu-24.04, ubuntu-26.04]\n";
    let audit = audit_runner_fixture(
        &runner_fixture("${{ matrix.runner }}", &hosted_guard(), hosted_matrix),
        &policy,
    );
    assert!(
        audit.runners.is_empty(),
        "same-class matrix: {:?}",
        audit.runners
    );

    let mixed_matrix = "    strategy:\n      matrix:\n        include:\n          - runner: ubuntu-24.04\n          - runner: [self-hosted, velnor-target-mvp]\n";
    let audit = audit_runner_fixture(
        &runner_fixture("${{ matrix.runner }}", &hosted_guard(), mixed_matrix),
        &policy,
    );
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("one verified runner environment class")
                || finding.contains("does not match any declared provider selector")
        }),
        "mixed hosted/local matrix must fail closed: {:?}",
        audit.runners
    );

    let unresolved = "    strategy:\n      matrix:\n        config: [{runner: ubuntu-24.04}]\n";
    let audit = audit_runner_fixture(
        &runner_fixture("${{ matrix.config.runner }}", &hosted_guard(), unresolved),
        &policy,
    );
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("unresolved or dynamic runner label")
                || finding.contains("one verified runner environment class")
        }),
        "unresolved matrix selector must fail: {:?}",
        audit.runners
    );
}

#[test]
fn approved_dynamic_provider_selector_tracks_its_runner_environment_branch() {
    let policy = runner_policy_fixture();
    let shape = crate::s2::estate::APPROVED_DYNAMIC_RUNNERS[0];
    let workflow_without_guard = runner_fixture(
        &format!(">-\n      ${{{{ {shape} }}}}"),
        "      - run: echo user code\n",
        "",
    );
    let document: Value = must(
        serde_yaml::from_str(&workflow_without_guard),
        "parse dynamic runner fixture",
    );
    let job = mapping_value(
        mapping_value(document.as_mapping().expect("workflow mapping"), "jobs")
            .and_then(Value::as_mapping)
            .expect("jobs mapping"),
        "check",
    )
    .and_then(Value::as_mapping)
    .expect("check job");
    let runs_on = mapping_value(job, "runs-on").expect("runs-on value");
    let expected = runner_environment_expectation(runs_on, None, &policy)
        .expect("approved selector has a branch-matched expectation")
        .predicate();
    assert!(expected.contains("runner.environment=='github-hosted'"));
    assert!(expected.contains("runner.environment=='self-hosted'"));
    assert!(expected.contains("inputs.providers"));

    let steps = format!(
        "{}      - name: Always cleanup\n        if: ${{{{ always() && ({expected}) }}}}\n        run: echo cleanup\n",
        runner_guard_with_condition(&format!("!({expected})"))
    );
    let guarded = runner_fixture(&format!(">-\n      ${{{{ {shape} }}}}"), &steps, "");
    let audit = audit_runner_fixture(&guarded, &policy);
    assert!(
        audit.runners.is_empty(),
        "approved dynamic shape: {:?}",
        audit.runners
    );

    let unsupported_event = guarded.replace("on: push", "on: workflow_run");
    let audit = audit_runner_fixture(&unsupported_event, &policy);
    assert!(
        audit
            .runners
            .iter()
            .any(|finding| finding.contains("local-provider jobs require a trusted-event gate")),
        "uncovered trigger falls through to local runner: {:?}",
        audit.runners
    );

    let matrix_fields = format!(
        "    if: ${{{{ {} }}}}\n    strategy:\n      matrix:\n        runner:\n          - >-\n            ${{{{ {shape} }}}}\n",
        trusted_event_conjunct()
    );
    let matrix_workflow = runner_fixture("${{ matrix.runner }}", &steps, &matrix_fields)
        .replace("on: push", "on: workflow_run");
    let audit = audit_runner_fixture(&matrix_workflow, &policy);
    assert!(
        audit
            .runners
            .iter()
            .any(|finding| finding.contains("local-provider jobs require a trusted-event gate")),
        "matrix dynamic selector on an uncovered trigger must fail: {:?}",
        audit.runners
    );

    let custom = shape.replace("velnor-target-mvp", "runner-private");
    let invalid = runner_fixture(
        &format!(">-\n      ${{{{ {custom} }}}}"),
        &hosted_guard(),
        "",
    );
    let audit = audit_runner_fixture(&invalid, &policy);
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("unresolved or dynamic runner label")
                || finding.contains("one verified runner environment class")
        }),
        "custom dynamic selector must fail: {:?}",
        audit.runners
    );

    let whitespace_in_selector = shape.replace("velnor-target-mvp", "velnor -target-mvp");
    assert!(
        approved_dynamic_runner_host_condition(&whitespace_in_selector, &policy).is_none(),
        "whitespace inside a quoted selector must remain significant"
    );
}

#[test]
fn group_runners_and_remote_reusable_calls_fail_closed() {
    let policy = runner_policy_fixture();
    let grouped = runner_fixture(
        "\n      group: hosted-looking-group\n      labels: ubuntu-24.04",
        &hosted_guard(),
        "",
    );
    let audit = audit_runner_fixture(&grouped, &policy);
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("one verified runner environment class")
                || finding.contains("does not match any declared provider selector")
        }),
        "unknown group must fail: {:?}",
        audit.runners
    );

    let remote = "name: Remote\non: push\njobs:\n  call:\n    uses: other-org/.github/workflows/ci.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
    let audit = audit_runner_fixture(remote, &policy);
    assert!(
        audit
            .actions
            .iter()
            .any(|finding| finding.contains("audited local callee")),
        "remote reusable workflow must fail: {:?}",
        audit.actions
    );
}

#[test]
fn action_pin_audit_rejects_unreviewed_full_sha_and_accepts_reviewed_pin() {
    let policy = runner_policy_fixture();
    let approved = crate::s2::ActionPin::Checkout
        .reference()
        .split_once(" #")
        .map_or(
            crate::s2::ActionPin::Checkout.reference(),
            |(reference, _)| reference,
        );
    let known_steps = format!(
        "{}      - uses: {approved}\n        if: ${{{{ runner.environment == 'github-hosted' }}}}\n",
        hosted_guard()
    );
    let known = audit_runner_fixture(&runner_fixture("ubuntu-24.04", &known_steps, ""), &policy);
    assert!(
        known.actions.is_empty(),
        "reviewed action pin: {:?}",
        known.actions
    );

    let unknown_steps = format!(
        "{}      - uses: example/action@{PIN_B}\n        if: ${{{{ runner.environment == 'github-hosted' }}}}\n",
        hosted_guard()
    );
    let unknown =
        audit_runner_fixture(&runner_fixture("ubuntu-24.04", &unknown_steps, ""), &policy);
    assert!(
        unknown
            .actions
            .iter()
            .any(|finding| finding.contains("lacks a reviewed pre-step safety contract")),
        "unreviewed full-SHA action must fail: {:?}",
        unknown.actions
    );
}

#[test]
fn local_action_audit_resolves_and_recursively_checks_composite_manifests() {
    let reference = "./.github/actions/local";
    let checkout = crate::s2::ActionPin::Checkout
        .reference()
        .split_once(" #")
        .map_or(
            crate::s2::ActionPin::Checkout.reference(),
            |(reference, _)| reference,
        );
    let valid_manifest = format!(
        "name: local\ndescription: local composite\nruns:\n  using: composite\n  steps:\n    - name: Nested reviewed action\n      uses: {checkout}\n"
    );
    let root = temporary_directory("safe-local-action");
    must(
        fs::create_dir_all(root.join(".github/workflows")),
        "create local action test workflow directory",
    );
    must(
        fs::create_dir_all(root.join(".github/actions/local")),
        "create local action test action directory",
    );
    let workflow = with_hosted_guard(&format!(
        "name: Local action\non: push\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: {reference}\n"
    ));
    write(&root.join(".github/workflows/local.yml"), &workflow);
    write(
        &root.join(".github/actions/local/action.yml"),
        &valid_manifest,
    );
    let audit = must(audit_workflows(&root), "audit safe local action closure");
    assert!(
        audit.actions.is_empty(),
        "safe composite: {:?}",
        audit.actions
    );
    let _ = fs::remove_dir_all(&root);

    for (case, manifest) in [
        (
            "missing",
            None,
        ),
        (
            "docker",
            Some("name: local\nruns:\n  using: docker\n  image: Dockerfile\n".to_owned()),
        ),
        (
            "pre-if",
            Some(
                "name: local\nruns:\n  using: composite\n  pre-if: always()\n  steps:\n    - run: echo unsafe\n      shell: bash\n"
                    .to_owned(),
            ),
        ),
        (
            "nested-unknown",
            Some(format!(
                "name: local\nruns:\n  using: composite\n  steps:\n    - uses: example/action@{PIN_B}\n"
            )),
        ),
        (
            "malformed",
            Some("runs: [not-a-mapping]\n".to_owned()),
        ),
    ] {
        let root = temporary_directory(&format!("unsafe-local-action-{case}"));
        must(
            fs::create_dir_all(root.join(".github/workflows")),
            "create unsafe local action workflow directory",
        );
        must(
            fs::create_dir_all(root.join(".github/actions/local")),
            "create unsafe local action directory",
        );
        write(
            &root.join(".github/workflows/local.yml"),
            &with_hosted_guard(&format!(
                "name: Local action\non: push\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: {reference}\n"
            )),
        );
        if let Some(manifest) = manifest {
            write(&root.join(".github/actions/local/action.yml"), &manifest);
        }
        let audit = must(audit_workflows(&root), "audit unsafe local action");
        assert!(
            audit.actions.iter().any(|finding| {
                finding.contains("local action failed the pre-step metadata contract")
            }),
            "{case} action metadata must fail: {:?}",
            audit.actions
        );
        let _ = fs::remove_dir_all(&root);
    }

    let root = temporary_directory("nested-local-action");
    must(
        fs::create_dir_all(root.join(".github/workflows")),
        "create nested local action workflow directory",
    );
    must(
        fs::create_dir_all(root.join(".github/actions/local")),
        "create parent composite directory",
    );
    must(
        fs::create_dir_all(root.join(".github/actions/child")),
        "create nested composite directory",
    );
    write(
        &root.join(".github/workflows/local.yml"),
        &with_hosted_guard(&format!(
            "name: Nested local action\non: push\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: {reference}\n"
        )),
    );
    write(
        &root.join(".github/actions/local/action.yml"),
        "name: local\nruns:\n  using: composite\n  steps:\n    - uses: ./.github/actions/child\n",
    );
    write(
        &root.join(".github/actions/child/action.yml"),
        &format!(
            "name: child\nruns:\n  using: composite\n  steps:\n    - uses: example/action@{PIN_B}\n"
        ),
    );
    let audit = must(audit_workflows(&root), "audit nested local action closure");
    assert!(
        audit
            .actions
            .iter()
            .any(|finding| { finding.contains("reviewed no-pre allowlist") }),
        "nested local composites must recursively validate external refs: {:?}",
        audit.actions
    );
    let _ = fs::remove_dir_all(&root);
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
    let unpinned = with_hosted_guard(
        "name: Workflow\non:\n  workflow_dispatch:\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v4\n",
    );
    write(&root.join(".github-gen/ci-static.yml"), &unpinned);
    write(&root.join(".github/workflows/ci-static.yml"), &unpinned);
    write(
        &root.join(".github/workflows/ci-old.yml"),
        &with_hosted_guard(
            "name: Old workflow\non:\n  pull_request_target:\n    types: [opened]\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v4\n",
        ),
    );
    write(
        &root.join(".github/workflows/ci-invalid.yml"),
        "name: Invalid workflow\non: pull_request_target\non: workflow_dispatch\n",
    );
    write(
        &root.join(POLICY_ENTRYPOINT),
        &with_hosted_guard(
            "name: Policy\non:\n  pull_request_target:\n    types: [opened, synchronize, reopened]\n  workflow_dispatch:\npermissions:\n  contents: read\njobs:\n  policy:\n    runs-on: ubuntu-24.04\n    permissions:\n      contents: read\n    steps:\n      - uses: actions/checkout@v4\n",
        ),
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
    let malformed_literal = format!("${{{{ {} }}}}", trusted.replace("'Bot'", "'B ot'"));
    assert!(
        !is_generated_provider_gate(&malformed_literal, "velnor"),
        "whitespace inside quoted literals must remain significant"
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
        // Whitespace inside literals changes the runtime event/type value.
        format!("${{{{ ({}) }}}}", trusted.replace("'Bot'", "'B ot'")),
        format!("${{{{ ({}) }}}}", trusted.replace("'pull_request'", "'pull_ request'")),
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
    assert!(is_approved_local_action(
        crate::s2::VELNOR_WORKFLOW_POLICY_SETUP_ACTION
    ));
    assert!(!is_approved_local_action(
        "./policy-setup-action/.github/actions/anything-else"
    ));
    assert!(!is_approved_local_action(
        "./policy-setup-action/.github/workflows/ci-pr.yml"
    ));
    assert!(is_approved_local_action(
        crate::s2::VELNOR_WORKFLOW_SOURCE_SETUP_ACTION
    ));
    assert!(!is_approved_local_action(
        "./source/.github/actions/anything-else"
    ));
    assert!(!is_approved_local_action(
        "./other-source/.github/actions/setup-velnor-workflow"
    ));
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

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
        use std::os::unix::fs::PermissionsExt as _;
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
        use std::os::unix::fs::PermissionsExt as _;
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
        search_path,
        install_root,
        build_forbidden: true,
        candidate_manifest: None,
    }
}

fn lookup_with_manifest(
    pinned_binary: Option<PathBuf>,
    search_path: Option<std::ffi::OsString>,
    install_root: PathBuf,
    candidate_manifest: PathBuf,
) -> PinnedBinaryLookup {
    PinnedBinaryLookup {
        pinned_binary,
        search_path,
        install_root,
        build_forbidden: true,
        candidate_manifest: Some(candidate_manifest),
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
        ),
        "scan entrypoint fixture",
    );
    let _ = fs::remove_dir_all(fixture);
    let mut config = ProjectConfig::from(shape);
    revision.clone_into(&mut config.workflow_revision);
    crate::s2::render_policy_entrypoint(&config)
}

fn entrypoint_tree(name: &str, entrypoint: &str) -> PathBuf {
    let root = temporary_directory(name);
    write(&root.join(POLICY_ENTRYPOINT), entrypoint);
    root
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

/// The owner policy job acquires products (no `--rev <sha>` install) and its
/// candidate step passes shell variables to `closure --rev=`: those variable
/// references are not pin literals, so the pin rule still passes on the
/// exported revision and still names it on drift.
#[test]
fn owner_entrypoint_pin_ignores_variable_references() {
    let job = crate::s2::policy_job(&PolicyJobSpec {
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

/// A Velnor-mode entrypoint runs on the approved self-hosted runner behind the
/// trusted-event gate and never builds pull-request code.
#[test]
fn velnor_entrypoint_is_gated_and_never_builds_the_pin() {
    let job = crate::s2::policy_job(&PolicyJobSpec {
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
        "name: CI / PR\non:\n  pull_request:\njobs:\n  ci-required:\n    name: ci-required\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ok\n  velnor-docker:\n    name: Docker\n    if: ${{{{ ((github.event_name == 'workflow_dispatch' && contains(format(',{{0}},', github.event.inputs.providers), ',velnor,')) || (github.event_name != 'workflow_dispatch')) && (!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot'))) }}}}\n    runs-on: [{VELNOR_SELECTOR}]\n    steps:\n      - run: echo trusted\n"
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

/// The generated provider gate admits a provider-selecting dispatch on any
/// ref — dispatch authorship is write-authorized — or the automatic events,
/// with the trusted-event conjunct. A dispatch selecting another provider is
/// not this provider's gate.
#[test]
fn provider_gate_admits_dispatch_on_any_ref() {
    let trusted = "(!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot')))";
    for automatic in ["(github.event_name != 'workflow_dispatch')", "(false)"] {
        let gate = format!(
            "${{{{ ((github.event_name == 'workflow_dispatch' && contains(format(',{{0}},', github.event.inputs.providers), ',velnor,')) || {automatic}) && {trusted} }}}}",
        );
        assert!(
            is_generated_provider_gate(&gate, "velnor"),
            "provider gate admitted: {gate}"
        );
        assert!(
            !is_generated_provider_gate(&gate, "github-hosted"),
            "a dispatch selecting Velnor is not the hosted gate: {gate}"
        );
    }
    let github_only = format!(
        "${{{{ ((github.event_name == 'workflow_dispatch' && contains(format(',{{0}},', github.event.inputs.providers), ',github-hosted,')) || (github.event_name != 'workflow_dispatch')) && {trusted} }}}}",
    );
    assert!(
        !is_generated_provider_gate(&github_only, "velnor"),
        "a dispatch selecting only the hosted provider is not a Velnor gate",
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
fn fake_candidate_renderer(directory: &Path, closure: &str) -> PathBuf {
    let binary = directory.join("candidate");
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --closure ]; then echo {closure}; exit 0; fi\ncp -r \"$1/.\" \"$3/\"\n"
            ),
        ),
        "write fake candidate renderer",
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark fake candidate renderer executable",
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
    write(&root.join("Cargo.toml"), "[workspace]\n");
    write(&root.join("Cargo.lock"), "# lock\n");
    write(&root.join(".github/workflows/ci-pr.yml"), "tree\n");
    let head = commit(&root, "fixture");
    (root, head)
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
                "binary_sha256": digest,
            })
            .to_string(),
        ),
        "write candidate manifest",
    );
    manifest
}

/// A fake candidate renderer that records every `--closure` probe in
/// `sentinel`, so tests can prove the binary was never executed at all.
#[cfg(unix)]
fn fake_probed_candidate_renderer(directory: &Path, closure: &str, sentinel: &Path) -> PathBuf {
    let binary = directory.join("candidate");
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --closure ]; then touch \"{}\"; echo {closure}; exit 0; fi\ncp -r \"$1/.\" \"$3/\"\n",
                sentinel.display()
            ),
        ),
        "write fake probed candidate renderer",
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        must(
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)),
            "mark fake probed candidate renderer executable",
        );
    }
    binary
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
    let binary = fake_candidate_renderer(&root, &wanted);
    let manifest =
        candidate_manifest_for(&root, "candidate-manifest.json", &binary, &wanted, &head);
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    let excludes = std::collections::BTreeSet::new();
    assert_eq!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &excludes, &lookup),
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
fn lying_binary_whose_digest_misses_the_manifest_is_rejected() {
    let (root, head) = closure_fixture("candidate-lying");
    let wanted = must(
        crate::s2::closure::candidate_closure_of_tree(&root, &head),
        "candidate closure of the fixture",
    );
    let scratch = temporary_directory("candidate-lying-scratch");
    let sentinel = root.join("closure-probed");
    let binary = fake_probed_candidate_renderer(&root, &wanted, &sentinel);
    // Bind the manifest to *other* bytes, so the binary echoes the wanted
    // closure but its digest misses.
    let other = root.join("other-bytes");
    must(fs::write(&other, "different bytes"), "write decoy bytes");
    let manifest = candidate_manifest_for(&root, "candidate-manifest.json", &other, &wanted, &head);
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    let excludes = std::collections::BTreeSet::new();
    assert!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &excludes, &lookup),
            "digest mismatch never errors, it simply does not prove",
        )
        .is_none(),
        "a binary whose digest misses the manifest is not the candidate"
    );
    assert!(
        !sentinel.exists(),
        "the digest gate fires before any execution, not even `--closure`"
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
    let binary = fake_candidate_renderer(&root, CLOSURE_A);
    let manifest =
        candidate_manifest_for(&root, "candidate-manifest.json", &binary, CLOSURE_A, &head);
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    let excludes = std::collections::BTreeSet::new();
    let error = must_fail(
        render_with_candidate(&root, &root, &scratch, "main", &excludes, &lookup),
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
    let lookup = lookup(Some(binary), None, root.join("install"));
    let excludes = std::collections::BTreeSet::new();
    assert!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &excludes, &lookup),
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
    let binary = fake_candidate_renderer(&root, &wanted);
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
        let excludes = std::collections::BTreeSet::new();
        let error = must_fail(
            render_with_candidate(&root, &root, &scratch, "main", &excludes, &lookup),
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
        candidate_manifest_source(Some("/flag/manifest.json")),
        Some(PathBuf::from("/flag/manifest.json"))
    );
    // An explicit empty flag disables the binding.
    assert_eq!(candidate_manifest_source(Some("")), None);
    // Without either source the env-slot candidate is disabled. Like
    // `cli_requires_the_base_validator_revision`, this relies on the ambient
    // test environment not exporting the variable.
    assert_eq!(candidate_manifest_source(None), None);
}

#[test]
fn from_env_consent_mapping_is_fail_closed() {
    // Without `--pin-build` the pin is never built, in every environment.
    assert!(PinnedBinaryLookup::from_env(PIN_A, false, None).build_forbidden);
    assert!(
        PinnedBinaryLookup::from_env(PIN_A, false, Some(PathBuf::from("/manifest.json")))
            .build_forbidden
    );
    // The explicit manifest survives; without one the environment fallback
    // applies (unset in the ambient test environment, so `None` here).
    assert_eq!(
        PinnedBinaryLookup::from_env(PIN_A, false, Some(PathBuf::from("/manifest.json")))
            .candidate_manifest,
        Some(PathBuf::from("/manifest.json"))
    );
    assert_eq!(
        PinnedBinaryLookup::from_env(PIN_A, false, None).candidate_manifest,
        None
    );
    // `CARGO_NET_OFFLINE=true` forbids the build even with `--pin-build`;
    // setting process env needs `unsafe`, which this crate forbids, so that
    // direction is pinned by the `--check` CLI subprocess test in
    // `velnor_first_ci.rs`, which owns the child's environment.
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
    let binary = fake_candidate_renderer(&root, CLOSURE_A);
    let manifest =
        candidate_manifest_for(&root, "candidate-manifest.json", &binary, &wanted, &head);
    let lookup = lookup_with_manifest(Some(binary), None, root.join("install"), manifest);
    let excludes = std::collections::BTreeSet::new();
    assert!(
        must(
            render_with_candidate(&root, &root, &scratch, "main", &excludes, &lookup),
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
    let excludes = std::collections::BTreeSet::new();
    assert!(must(
        render_with_candidate(&root, &root, &scratch, "main", &excludes, &lookup),
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

#[test]
fn exclude_hatch_covers_top_level_workflows_only() {
    let excludes = BTreeSet::from(["hand.yml".to_owned()]);
    assert!(
        is_excluded_workflow(Path::new(".github/workflows/hand.yml"), &excludes),
        "a named top-level workflow is excluded"
    );
    assert!(
        !is_excluded_workflow(Path::new(".github/workflows/other.yml"), &excludes),
        "an unnamed workflow is not excluded"
    );
    assert!(
        !is_excluded_workflow(Path::new(".github/workflows/nested/hand.yml"), &excludes),
        "a nested path never matches the hatch"
    );
    assert!(
        !is_excluded_workflow(Path::new(".github/notes.txt"), &excludes),
        "a non-workflow path never matches the hatch"
    );
    assert!(
        !is_excluded_workflow(Path::new(".github/workflows/hand.yml"), &BTreeSet::new()),
        "no excludes means no hatch"
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

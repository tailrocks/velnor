#![expect(
    clippy::panic,
    reason = "tests need setup failures to name their root cause"
)]

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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
        crate::s2::unique_suffix()
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

/// The owner policy entrypoint is a typed four-role transport: the producer
/// lives in `ci-pr.yml`, acquire selects its exact API identity, execute runs
/// only inside the reviewed sandbox, and policy verifies the render. No
/// candidate bytes execute in the trusted verifier and no mutable artifact
/// name is used as a download selector.
#[test]
fn owner_entrypoint_renders_the_isolated_candidate_transport() {
    let job = crate::s2::policy_job(&PolicyJobSpec {
        name: "Policy",
        revision: PIN_A,
        runner: "ubuntu-24.04",
        repository: crate::s2::workflow_setup_action_repository(),
        cache_backend: "github",
        trusted_gate: None,
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,Policy",
        acquire_pull_request_candidate: true,
    });
    assert_candidate_transport_acquisition(&job);
    assert_candidate_transport_sandbox(&job);
    let template = hosted_entrypoint(PIN_A);
    let (prefix, _) = must_some(template.split_once("jobs:\n"), "hosted workflow jobs");
    let root = entrypoint_tree("entrypoint-owner-pin", &format!("{prefix}jobs:\n{job}"));
    let audit = must(
        audit_policy_entrypoint(&root, &VelnorPolicyContract::default()),
        "audit owner candidate transport",
    );
    assert!(audit.trigger.is_empty(), "{:?}\n{job}", audit.trigger);
    assert!(audit.privileges.is_empty(), "{:?}", audit.privileges);
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
fn candidate_transport_rejects_malformed_manifest_and_open_publishers() {
    let job = crate::s2::policy_job(&PolicyJobSpec {
        name: "Policy",
        revision: PIN_A,
        runner: "ubuntu-24.04",
        repository: crate::s2::workflow_setup_action_repository(),
        cache_backend: "github",
        trusted_gate: None,
        default_branch: "main",
        declared_ruleset_contexts: "ci-required,Policy",
        acquire_pull_request_candidate: true,
    });
    for marker in [
        "step_pattern = re.compile",
        "external_workflow_pattern",
        "external reusable workflow is outside the closed artifact publisher contract",
        "upload step has no unique action or with.name",
        "shell step can POST to the Actions artifact service",
    ] {
        assert!(
            job.contains(marker),
            "closed publisher contract missing {marker}: {job}"
        );
    }

    let script = crate::s2::candidate_manifest_validation_script();
    let marker = "python3 - \"$manifest_schema\" \"$manifest\" <<'PY'\n";
    let start = must_some(script.find(marker), "manifest validator start") + marker.len();
    let end = must_some(
        script[start..].find("\n          PY\n"),
        "manifest validator end",
    ) + start;
    let body = script[start..end]
        .lines()
        .map(|line| line.strip_prefix("          ").map_or(line, |value| value))
        .collect::<Vec<_>>()
        .join("\n");
    let root = temporary_directory("malformed-manifest");
    let schema = root.join("schema.json");
    let manifest = root.join("manifest.json");
    write(&schema, crate::s2::CANDIDATE_MANIFEST_SCHEMA_JSON);
    write(
        &manifest,
        &format!(
            "{{\"schema\":\"{}\",\"profile\":\"debug\",\"features\":[],\"platform\":\"linux-amd64\",\"repository\":\"example/consumer\",\"run_id\":1,\"revision\":\"{PIN_A}\",\"closure\":\"{CLOSURE_A}\",\"binary_sha256\":\"{CLOSURE_A}\",\"extra\":true}}",
            crate::s2::CANDIDATE_MANIFEST_SCHEMA
        ),
    );
    let mut child = must(
        Command::new("python3")
            .arg("-")
            .arg(&schema)
            .arg(&manifest)
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn(),
        "spawn manifest validator",
    );
    let mut stdin = must_some(child.stdin.take(), "manifest validator stdin");
    must(stdin.write_all(body.as_bytes()), "write manifest validator");
    drop(stdin);
    let output = must(child.wait_with_output(), "wait manifest validator");
    assert!(
        !output.status.success(),
        "extra manifest field must fail closed"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("producer manifest keys do not match"),
        "malformed manifest rejection names the closed-schema failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

fn candidate_namespace_script_body() -> String {
    let script = crate::s2::candidate_namespace_scan_script();
    let marker =
        "          python3 - \"$base_workflow_archive\" \"$head_workflow_archive\" \"$BASE_SHA\" \"$HEAD_SHA\" \"$base_tree_sha\" \"$head_tree_sha\" \"$base_tree_api_digest\" \"$head_tree_api_digest\" <<'PY' > \"$RUNNER_TEMP/candidate-workflow-contract.txt\"\n";
    let start = must_some(script.find(marker), "namespace scanner start") + marker.len();
    let end = must_some(
        script[start..].find("\n          PY\n"),
        "namespace scanner end",
    ) + start;
    script[start..end]
        .lines()
        .map(|line| line.strip_prefix("          ").map_or(line, |value| value))
        .collect::<Vec<_>>()
        .join("\n")
}

fn namespace_workflow_archive_with_action(
    root: &Path,
    name: &str,
    workflow: &str,
    action_script: &str,
) -> PathBuf {
    namespace_workflow_archive_with_action_and_dependency(
        root,
        name,
        workflow,
        action_script,
        action_script,
    )
}

fn namespace_workflow_archive_with_action_and_dependency(
    root: &Path,
    name: &str,
    workflow: &str,
    action_script: &str,
    dependency_script: &str,
) -> PathBuf {
    let tree = root.join(format!("{name}-tree"));
    write(&tree.join(".github/workflows/ci-pr.yml"), workflow);
    write(
        &tree.join(".github/actions/setup/action.yml"),
        "name: setup\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: .github/scripts/publish.sh\n",
    );
    write(&tree.join(".github/actions/setup/script.sh"), action_script);
    write(&tree.join(".github/scripts/publish.sh"), dependency_script);
    write(
        &tree.join("actions/setup/action.yml"),
        "name: setup\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: .github/scripts/publish.sh\n",
    );
    write(&tree.join("actions/setup/script.sh"), action_script);
    let archive = root.join(format!("{name}.tar"));
    let output = must(
        Command::new("tar")
            .args([
                "-cf",
                must_some(archive.to_str(), "namespace archive path"),
                "-C",
            ])
            .arg(must_some(tree.to_str(), "namespace tree path"))
            .args([".github", "actions"])
            .env("COPYFILE_DISABLE", "1")
            .output(),
        "create namespace archive",
    );
    assert!(output.status.success(), "tar stderr: {:?}", output.stderr);
    archive
}

fn run_namespace_scanner(root: &Path, base_workflow: &str, head_workflow: &str) -> Output {
    run_namespace_scanner_with_action(
        root,
        base_workflow,
        head_workflow,
        "#!/bin/sh\necho setup\n",
        "#!/bin/sh\necho setup\n",
    )
}

fn run_namespace_scanner_with_action(
    root: &Path,
    base_workflow: &str,
    head_workflow: &str,
    base_action_script: &str,
    head_action_script: &str,
) -> Output {
    run_namespace_scanner_with_action_and_dependency(
        root,
        base_workflow,
        head_workflow,
        base_action_script,
        head_action_script,
        base_action_script,
        head_action_script,
    )
}

fn run_namespace_scanner_with_action_and_dependency(
    root: &Path,
    base_workflow: &str,
    head_workflow: &str,
    base_action_script: &str,
    head_action_script: &str,
    base_dependency_script: &str,
    head_dependency_script: &str,
) -> Output {
    let base = namespace_workflow_archive_with_action_and_dependency(
        root,
        "base",
        base_workflow,
        base_action_script,
        base_dependency_script,
    );
    let head = namespace_workflow_archive_with_action_and_dependency(
        root,
        "head",
        head_workflow,
        head_action_script,
        head_dependency_script,
    );
    let mut child = must(
        Command::new("python3")
            .arg("-")
            .arg(base)
            .arg(head)
            .args([PIN_A, PIN_B, PIN_A, PIN_B, CLOSURE_A, CLOSURE_B])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn(),
        "spawn namespace scanner",
    );
    let mut stdin = must_some(child.stdin.take(), "namespace scanner stdin");
    must(
        stdin.write_all(candidate_namespace_script_body().as_bytes()),
        "write namespace scanner",
    );
    drop(stdin);
    must(child.wait_with_output(), "wait namespace scanner")
}

#[test]
fn candidate_namespace_scan_rejects_unnamed_external_and_shell_publishers() {
    let root = temporary_directory("namespace-scanner");
    let fixed = r"jobs:
  candidate_producer:
    steps:
      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
        id: candidate_upload
        with:
          name: velnor-workflow-candidate-linux-x64
          path: candidate
";
    let accepted = run_namespace_scanner(&root, fixed, fixed);
    assert!(
        accepted.status.success(),
        "fixed publisher at {}: {accepted:?}",
        root.display()
    );
    assert_eq!(
        String::from_utf8_lossy(&accepted.stdout)
            .split_whitespace()
            .count(),
        2,
        "accepted scanner must emit contract and trusted base binding digests"
    );

    let local_action = fixed.replace(
        "      - uses: actions/upload-artifact@",
        "      - uses: ./.github/actions/setup\n      - uses: actions/upload-artifact@",
    );
    let accepted = run_namespace_scanner(&root, &local_action, &local_action);
    assert!(
        accepted.status.success(),
        "recursive local action contract escaped: {accepted:?}"
    );
    let generic_local_action = local_action.replace("./.github/actions/setup", "./actions/setup");
    let accepted = run_namespace_scanner(&root, &generic_local_action, &generic_local_action);
    assert!(
        accepted.status.success(),
        "repository-root local action resolution escaped: {accepted:?}"
    );
    let rejected = run_namespace_scanner_with_action(
        &root,
        &local_action,
        &local_action,
        "#!/bin/sh\necho base\n",
        "#!/bin/sh\necho head\n",
    );
    assert!(
        !rejected.status.success(),
        "local action implementation drift escaped source closure comparison: {rejected:?}"
    );
    let rejected = run_namespace_scanner_with_action_and_dependency(
        &root,
        &local_action,
        &local_action,
        "#!/bin/sh\necho stable action\n",
        "#!/bin/sh\necho stable action\n",
        "#!/bin/sh\necho base dependency\n",
        "#!/bin/sh\necho head dependency\n",
    );
    assert!(
        !rejected.status.success(),
        "composite action dependency drift escaped trusted .github source closure: {rejected:?}"
    );

    let changed_top_level = fixed.replace("jobs:\n", "env:\n  CANDIDATE_FEATURE: changed\njobs:\n");
    let rejected = run_namespace_scanner(&root, fixed, &changed_top_level);
    assert!(
        !rejected.status.success(),
        "top-level workflow controls escaped semantic contract comparison: {rejected:?}"
    );

    let unnamed = fixed.replace(
        "          path: candidate\n",
        "          path: candidate\n  other:\n    steps:\n      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a\n        with:\n          name: velnor-workflow-candidate-linux-x64\n          path: other\n",
    );
    let rejected = run_namespace_scanner(&root, fixed, &unnamed);
    assert!(
        !rejected.status.success(),
        "unnamed publisher escaped: {rejected:?}"
    );

    let external = fixed.replace(
        "jobs:\n",
        "jobs:\n  external:\n    uses: attacker/repo/.github/workflows/publish.yml@0123456789012345678901234567890123456789\n",
    );
    let rejected = run_namespace_scanner(&root, fixed, &external);
    assert!(
        !rejected.status.success(),
        "external publisher escaped: {rejected:?}"
    );

    let shell = fixed.replace(
        "jobs:\n",
        "jobs:\n  shell_publish:\n    steps:\n      - run: |\n          curl -X POST \"$ACTIONS_RUNTIME_URL\"\n",
    );
    let rejected = run_namespace_scanner(&root, fixed, &shell);
    assert!(
        !rejected.status.success(),
        "shell publisher escaped: {rejected:?}"
    );

    let python = fixed.replace(
        "jobs:\n",
        "jobs:\n  python_publish:\n    steps:\n      - run: |\n          import os\n          import urllib.request\n          urllib.request.urlopen(urllib.request.Request(os.environ[\"ACTIONS_RUNTIME_URL\"], method=\"POST\"))\n",
    );
    let rejected = run_namespace_scanner(&root, fixed, &python);
    assert!(
        !rejected.status.success(),
        "Python artifact-service publisher escaped: {rejected:?}"
    );

    let opaque = fixed.replace(
        "jobs:\n",
        "jobs:\n  opaque_publish:\n    steps:\n      - uses: attacker/publisher@0123456789012345678901234567890123456789\n",
    );
    let rejected = run_namespace_scanner(&root, fixed, &opaque);
    assert!(
        !rejected.status.success(),
        "opaque third-party publisher escaped: {rejected:?}"
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
        acquire_pull_request_candidate: false,
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
        candidate_render: None,
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
    let retired = must_fail(
        run_cli(&[
            std::ffi::OsString::from("--candidate-manifest"),
            std::ffi::OsString::from("/first.json"),
        ]),
        "retired candidate manifest option",
    )
    .to_string();
    assert!(retired.contains("--candidate-manifest"), "{retired}");
    assert!(retired.contains("unsupported policy option"), "{retired}");
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Candidate render exception
// Candidate bytes enter policy only through the independently verified hosted render artifact.

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
            acquire_pull_request_candidate: false,
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
}
fn assert_candidate_transport_acquisition(job: &str) {
    assert!(job.contains("policy_acquire:\n"), "{job}");
    assert!(job.contains("candidate_execute:\n"), "{job}");
    assert!(job.contains("  policy:\n"), "{job}");
    assert!(job.contains("needs: [candidate_execute]"), "{job}");
    assert!(
        job.contains("needs.policy_acquire.outputs.handoff_id"),
        "{job}"
    );
    assert!(job.contains("RESULT_ID:"), "{job}");
    assert!(
        job.contains("name: velnor-workflow-candidate-linux-x64"),
        "{job}"
    );
    assert!(
        job.contains("name: velnor-workflow-candidate-handoff"),
        "{job}"
    );
    assert!(
        job.contains("name: velnor-workflow-candidate-result"),
        "{job}"
    );
    assert!(
        job.contains("actions/runs/$run_id/jobs?per_page=100"),
        "{job}"
    );
    assert!(
        job.contains("actions/runs/$run_id/artifacts?per_page=100"),
        "{job}"
    );
    assert!(job.contains("actions/artifacts/$artifact_id/zip"), "{job}");
    assert!(job.contains("/commits/$HEAD_SHA"), "{job}");
    assert!(job.contains(".commit.tree.sha"), "{job}");
    assert!(
        job.contains("git -c init.templateDir=/dev/null init --bare"),
        "{job}"
    );
    assert!(
        job.contains("fetch --no-tags --depth=1 \"$GITHUB_SERVER_URL/$HEAD_REPOSITORY\""),
        "{job}"
    );
    assert!(
        job.contains("git -C \"$source_repo\" archive --format=tar \"$HEAD_SHA\""),
        "{job}"
    );
    assert!(
        job.contains("git -C \"$verifier_source_repo\" ls-tree -r \"$HEAD_SHA\""),
        "{job}"
    );
    assert!(
        job.contains("source_repo=\"$verifier_source_repo\""),
        "final verifier must point the recursive workflow scan at its fresh source checkout: {job}"
    );
    assert!(job.contains("candidate_workflow_binding_sha256"), "{job}");
    assert!(
        !job.contains("git rev-parse \"$HEAD_SHA^{{tree}}\""),
        "{job}"
    );
    assert!(job.contains("repository_api"), "{job}");
    assert!(job.contains("artifact_raw_zip_sha256"), "{job}");
    assert!(job.contains("fromdateiso8601 > now"), "{job}");
    assert!(
        job.contains("test \"$raw_zip_sha256\" = \"$service_digest\""),
        "{job}"
    );
    assert!(
        job.contains("test \"$result_raw_zip_sha256\" = \"$result_service_digest\""),
        "{job}"
    );
    assert!(
        job.contains("test \"$handoff_raw_zip_sha256\" = \"$handoff_service_digest\""),
        "{job}"
    );
    assert!(
        job.contains("test \"$(sha256sum \"$producer_archive\" | awk '{print $1}')\" = \"$producer_service_digest\""),
        "{job}"
    );
    assert!(job.contains("candidate_closure"), "{job}");
    assert!(
        job.contains("Verify candidate transport provenance"),
        "{job}"
    );
    assert!(job.contains("workflow_id"), "{job}");
    assert!(job.contains(".workflow_run.id | tonumber"), "{job}");
    assert!(job.contains("actions: read\n      contents: read"), "{job}");
    assert!(job.contains("run_attempt"), "{job}");
    assert!(job.contains("target_repository_id"), "{job}");
    assert!(job.contains("uses: actions/checkout@"), "{job}");
    assert!(job.contains(".pr_number == $expected_pr"), "{job}");
    assert!(job.contains(".pr_number == $pr"), "{job}");
    assert!(
        job.contains(".head_repository.full_name == $head_repo"),
        "{job}"
    );
    assert!(
        job.contains("grep -Ec '^[[:space:]]+uses: .*upload-artifact@'"),
        "{job}"
    );
    assert!(job.contains("PR_NUMBER:"), "{job}");
    assert!(job.contains(".pull_requests | any"), "{job}");
    assert!(
        job.contains("PR workflow changed the trusted candidate producer contract"),
        "{job}"
    );
}

fn assert_candidate_transport_sandbox(job: &str) {
    assert!(job.contains("--network=none"), "{job}");
    assert!(job.contains("--read-only"), "{job}");
    assert!(job.contains("--pid=private"), "{job}");
    assert!(job.contains("--cap-drop=ALL"), "{job}");
    assert!(job.contains("uid=65532; gid=65532"), "{job}");
    assert!(job.contains("SOURCE_CLOSURE"), "{job}");
    assert!(job.contains("docker_cmd()"), "{job}");
    assert!(job.contains("env -i PATH=\"$PATH\""), "{job}");
    assert!(!job.contains("--hostname=velnor-sandbox"), "{job}");
    assert!(!job.contains("--env HOSTNAME=velnor-sandbox"), "{job}");
    assert!(
        job.contains("--tmpfs /tmp:rw,noexec,nosuid,nodev,size=64m"),
        "{job}"
    );
    assert!(job.contains("nr_inodes=4096"), "{job}");
    assert!(job.contains("--ulimit fsize=67108864:67108864"), "{job}");
    assert!(job.contains("--log-driver=none"), "{job}");
    assert!(
        job.contains("--security-opt no-new-privileges=true"),
        "{job}"
    );
    assert!(
        job.contains("--mount \"type=bind,src=$input,dst=/input,readonly"),
        "{job}"
    );
    assert!(
        job.contains("--mount \"type=bind,src=$candidate,dst=/candidate,readonly"),
        "{job}"
    );
    assert!(
        job.contains("--candidate-render \"$RUNNER_TEMP/candidate-result-verified/render\""),
        "{job}"
    );
    assert!(job.contains("result archive has too many members"), "{job}");
    assert!(job.contains("result archive is incomplete"), "{job}");
    assert!(
        !job.contains("Download candidate verification result"),
        "{job}"
    );
    assert!(job.contains("SANDBOX_IMAGE_DIGEST"), "{job}");
    assert!(job.contains("test -n \"$SANDBOX_IMAGE_DIGEST\""), "{job}");
    assert!(job.contains("if name.endswith(\"/\")"), "{job}");
    assert!(job.contains("if not member.isdir()"), "{job}");
    assert!(job.contains("unsafe source archive member"), "{job}");
    assert!(!job.contains("gh run download"), "{job}");
    assert!(
        !job.contains("--candidate-manifest"),
        "the hosted verifier consumes result bytes, not the legacy manifest path: {job}"
    );
}

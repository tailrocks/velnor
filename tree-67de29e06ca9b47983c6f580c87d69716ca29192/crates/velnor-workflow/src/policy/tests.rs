#![expect(
    clippy::panic,
    reason = "tests need setup failures to name their root cause"
)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::{PolicyJobSpec, ProjectConfig, RunnerMode};

const PIN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PIN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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

fn fake_velnor_workflow(directory: &Path, revision: &str) -> PathBuf {
    let binary = directory.join("velnor-workflow");
    must(
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --revision ]; then echo {revision}; exit 0; fi\nexit 2\n"
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
    let pinned = fake_velnor_workflow(&root, PIN_A);
    let lookup = lookup(Some(pinned.clone()), None, root.join("install"));
    assert_eq!(
        must(
            resolve_pinned_binary(PIN_A, &lookup, &checkout_source(&root)),
            "env binary at the pin"
        ),
        pinned
    );
    let error = must_fail(
        resolve_pinned_binary(PIN_B, &lookup, &checkout_source(&root)),
        "env binary at another revision",
    )
    .to_string();
    assert!(error.contains(VELNOR_WORKFLOW_PINNED_BINARY_ENV), "{error}");
    assert!(
        error.contains(&format!("reports revision {PIN_A}")),
        "an explicit pointer at the wrong revision is refused, not skipped: {error}"
    );
    assert!(error.contains(PIN_B), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn path_binary_is_used_when_its_reported_revision_is_the_pin() {
    let root = temporary_directory("pinned-path");
    let stale = root.join("stale");
    let current = root.join("current");
    must(fs::create_dir_all(&stale), "stale dir");
    must(fs::create_dir_all(&current), "current dir");
    fake_velnor_workflow(&stale, PIN_B);
    let wanted = fake_velnor_workflow(&current, PIN_A);
    let lookup = lookup(
        None,
        env::join_paths([&stale, &current]).ok(),
        root.join("install"),
    );
    assert_eq!(
        must(
            resolve_pinned_binary(PIN_A, &lookup, &checkout_source(&root)),
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
    let stale = fake_velnor_workflow(&stale_dir, PIN_B);
    let install_root = root.join("install");
    must(fs::create_dir_all(install_root.join("bin")), "install bin");
    let installed = fake_velnor_workflow(&install_root.join("bin"), PIN_B);
    let lookup = lookup(None, env::join_paths([&stale_dir]).ok(), install_root);
    let error = must_fail(
        resolve_pinned_binary(PIN_A, &lookup, &checkout_source(&root)),
        "forbidden build miss",
    )
    .to_string();
    assert!(error.contains("building one is forbidden here"), "{error}");
    assert!(error.contains(PIN_A), "{error}");
    assert!(
        error.contains(&format!("{} reports revision {PIN_B}", stale.display())),
        "{error}"
    );
    assert!(
        error.contains(&format!("{} reports revision {PIN_B}", installed.display())),
        "a previously installed binary is proven by revision, not by its directory name: {error}"
    );
    assert!(error.contains(VELNOR_WORKFLOW_PINNED_BINARY_ENV), "{error}");
    assert!(!error.contains("cargo install"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_running_binary_is_the_pin_when_built_at_it() {
    let root = temporary_directory("pinned-self");
    let lookup = lookup(None, None, root.join("install"));
    let resolved = must(
        resolve_pinned_binary(SOURCE_REVISION, &lookup, &checkout_source(&root)),
        "the running binary at its own revision",
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
            .any(|detail| detail.contains("--rev") && detail.contains(PIN_A)),
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
    let job = crate::policy_job(&PolicyJobSpec {
        name: "Policy",
        revision: PIN_A,
        runner: "[self-hosted, velnor]",
        cache_backend: "local",
        trusted_gate: Some(&crate::control_plane_trusted_gate("main")),
        default_branch: "main",
    });
    assert!(job.contains("--no-pin-build"), "{job}");
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
    let _ = fs::remove_dir_all(root);
}

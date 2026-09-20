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
        _pinned_binary_guard: None,
        pinned_binary_sha256: None,
        pinned_binary_revision: None,
        pinned_binary_closure: None,
        search_path,
        install_root,
        build_forbidden: true,
    }
}

fn lookup_product(
    binary: PathBuf,
    search_path: Option<std::ffi::OsString>,
    install_root: PathBuf,
    revision: &str,
    closure: &str,
) -> PinnedBinaryLookup {
    PinnedBinaryLookup {
        pinned_binary: Some(binary.clone()),
        _pinned_binary_guard: None,
        pinned_binary_sha256: Some(must(sha256_file(&binary), "hash product binary")),
        pinned_binary_revision: Some(revision.to_owned()),
        pinned_binary_closure: Some(closure.to_owned()),
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
    let lookup = lookup_product(pinned.clone(), None, root.join("install"), PIN_A, CLOSURE_A);
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
fn explicit_product_pointer_requires_verified_manifest_identity() {
    let root = temporary_directory("pinned-unbound");
    let binary = fake_velnor_workflow(&root, PIN_A, CLOSURE_A);
    let lookup = lookup(Some(binary), None, root.join("install"));
    let expected = [CLOSURE_A.to_owned()];
    let error = must_fail(
        resolve_pinned_binary(PIN_A, Some(&expected), &lookup, &checkout_source(&root)),
        "product pointer without manifest identity",
    )
    .to_string();
    assert!(error.contains("verified product manifest"), "{error}");
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
fn shallow_owner_checkout_fetches_d19_history_before_ancestry_checks() {
    let source = temporary_directory("shallow-history-source");
    git_ok(&source, &["init", "-q", "-b", "main"]);
    write(&source.join("README.md"), "pin\n");
    let pin = commit(&source, "pin");
    write(&source.join("README.md"), "base\n");
    let base = commit(&source, "base");
    write(&source.join("README.md"), "head\n");
    let head = commit(&source, "head");
    write(&source.join("README.md"), "tip\n");
    let _tip = commit(&source, "tip");

    let shallow = temporary_directory("shallow-history-clone");
    let origin = format!("file://{}", source.display());
    let output = must(
        Command::new("git")
            .args([
                "clone", "--quiet", "--depth", "1", "--branch", "main", &origin,
            ])
            .arg(&shallow)
            .output(),
        "clone shallow owner checkout",
    );
    assert!(
        output.status.success(),
        "git clone: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(is_shallow_checkout(&shallow));
    assert!(!commit_exists(&shallow, &pin));
    assert!(!commit_exists(&shallow, &base));
    assert!(!commit_exists(&shallow, &head));

    must(
        ensure_pin_history(&shallow, &[&pin, &base, &head]),
        "fetch D19 pin, base, and head history from origin",
    );
    assert!(!is_shallow_checkout(&shallow));
    assert!(commit_exists(&shallow, &pin));
    assert!(commit_exists(&shallow, &base));
    assert!(commit_exists(&shallow, &head));
    assert_eq!(is_ancestor(&shallow, &pin, &base), Some(true));
    assert_eq!(is_ancestor(&shallow, &pin, &head), Some(true));
    assert_eq!(is_ancestor(&shallow, &base, &head), Some(true));

    let _ = fs::remove_dir_all(source);
    let _ = fs::remove_dir_all(shallow);
}

#[cfg(unix)]
#[test]
fn failed_shallow_pin_fetch_reports_full_history_remedy() {
    let source = temporary_directory("shallow-fetch-failure-source");
    git_ok(&source, &["init", "-q", "-b", "main"]);
    write(&source.join("README.md"), "pin\n");
    let pin = commit(&source, "pin");
    write(&source.join("README.md"), "base\n");
    let base = commit(&source, "base");
    write(&source.join("README.md"), "head\n");
    let head = commit(&source, "head");
    write(&source.join("README.md"), "tip\n");
    let _tip = commit(&source, "tip");

    let shallow = temporary_directory("shallow-fetch-failure-clone");
    let origin = format!("file://{}", source.display());
    let clone = must(
        Command::new("git")
            .args([
                "clone", "--quiet", "--depth", "1", "--branch", "main", &origin,
            ])
            .arg(&shallow)
            .output(),
        "clone shallow owner checkout",
    );
    assert!(
        clone.status.success(),
        "git clone: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    assert!(is_shallow_checkout(&shallow));
    let inaccessible = format!("file://{}/missing-origin", source.display());
    git_ok(&shallow, &["remote", "set-url", "origin", &inaccessible]);

    let error = must_fail(
        ensure_pin_history(&shallow, &[&pin, &base, &head]),
        "missing D19 history with inaccessible origin",
    )
    .to_string();
    assert!(is_shallow_checkout(&shallow));
    for revision in [&pin, &base, &head] {
        assert!(
            error.contains(revision),
            "the diagnostic names missing revision {revision}: {error}"
        );
    }
    assert!(
        error.contains("history remains shallow after fetching from origin"),
        "the diagnostic distinguishes shallow history: {error}"
    );
    assert!(
        error.contains("actions/checkout `fetch-depth: 0`"),
        "the diagnostic tells the workflow owner how to fetch complete history: {error}"
    );
    assert!(
        error.contains("verify `origin` can fetch the D19 pin and its ancestry"),
        "the diagnostic identifies the remote access requirement: {error}"
    );

    let _ = fs::remove_dir_all(source);
    let _ = fs::remove_dir_all(shallow);
}

#[cfg(target_os = "linux")]
#[test]
fn pinned_product_exec_uses_sealed_bytes_after_shared_slot_replacement() {
    let (root, head) = closure_fixture("pinned-product-toctou");
    let expected = must(expected_closures(&root, &head), "compute pin closures");
    let product_closure = &expected[0];
    let product_revision = PIN_A;
    let shared_slot = root.join("shared-policy-slot");
    let replacement = root.join("replacement-policy");
    let rendered = root.join("original-product-rendered");
    write(
        &replacement,
        "#!/bin/sh\nif [ \"$1\" = --revision ]; then echo ".to_owned()
            + PIN_B
            + "; exit 0; fi\nif [ \"$1\" = --closure ]; then echo "
            + CLOSURE_B
            + "; exit 0; fi\nexit 91\n",
    );
    write(
        &shared_slot,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --revision ]; then echo {product_revision}; exit 0; fi\nif [ \"$1\" = --closure ]; then cp '{}' '{}'; echo {product_closure}; exit 0; fi\ntouch '{}'; cp -R \"$1/.\" \"$3/\"\n",
            replacement.display(),
            shared_slot.display(),
            rendered.display()
        ),
    );
    for binary in [&shared_slot, &replacement] {
        use std::os::unix::fs::PermissionsExt as _;
        must(
            fs::set_permissions(binary, fs::Permissions::from_mode(0o755)),
            "mark fixture runtime executable",
        );
    }
    let lookup = lookup_product(
        shared_slot.clone(),
        None,
        root.join("install"),
        product_revision,
        product_closure,
    );
    let excludes = std::collections::BTreeSet::new();
    let comparison = must(
        regenerate_and_compare(
            &root,
            &root,
            &head,
            "main",
            &excludes,
            &lookup,
            &checkout_source(&root),
        ),
        "render through the verified product snapshot",
    );
    assert!(matches!(comparison, TreeComparison::Pin));
    assert!(
        rendered.is_file(),
        "the verified product completed rendering"
    );
    assert_eq!(
        must(fs::read(&shared_slot), "read replaced shared product slot"),
        must(fs::read(&replacement), "read replacement product"),
        "the probe replaced the mutable shared slot before the render exec"
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
fn manifest_bound_product_revision_may_differ_from_pin_for_the_same_closure() {
    let (root, pin) = closure_fixture("pinned-manifest-same-closure");
    let closure = must(
        crate::s2::closure::candidate_closure_of_tree(&root, &pin),
        "closure at declared pin",
    );
    let product_revision = commit(&root, "equivalent product commit");
    assert_ne!(pin, product_revision, "the commit identities differ");
    assert_eq!(
        closure,
        must(
            crate::s2::closure::candidate_closure_of_tree(&root, &product_revision),
            "closure at product commit",
        ),
        "both commits name the same source closure",
    );
    let product_dir = root.join("product");
    must(fs::create_dir_all(&product_dir), "product directory");
    let product = fake_velnor_workflow(&product_dir, &product_revision, &closure);
    let lookup = lookup_product(
        product.clone(),
        None,
        root.join("install"),
        &product_revision,
        &closure,
    );
    assert_eq!(
        must(
            resolve_pinned_binary(&pin, None, &lookup, &checkout_source(&root)),
            "manifest-bound product with equivalent source closure",
        ),
        product,
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn manifest_bound_product_binary_revision_must_match_manifest() {
    let root = temporary_directory("pinned-manifest-revision-mismatch");
    let binary = fake_velnor_workflow(&root, PIN_A, CLOSURE_A);
    let lookup = lookup_product(
        binary,
        None,
        root.join("install"),
        PIN_B,
        CLOSURE_A,
    );
    let error = must_fail(
        resolve_pinned_binary(PIN_A, None, &lookup, &checkout_source(&root)),
        "manifest whose revision differs from the binary",
    )
    .to_string();
    assert!(error.contains("reports revision"), "{error}");
    assert!(error.contains(PIN_A), "{error}");
    assert!(error.contains(PIN_B), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn manifest_bound_product_still_requires_a_closure_report() {
    let root = temporary_directory("pinned-manifest-legacy");
    let legacy_dir = root.join("legacy");
    must(fs::create_dir_all(&legacy_dir), "legacy directory");
    let legacy = fake_legacy_velnor_workflow(&legacy_dir, PIN_A);
    let lookup = lookup_product(legacy, None, root.join("install"), PIN_A, CLOSURE_A);
    let error = must_fail(
        resolve_pinned_binary(PIN_A, None, &lookup, &checkout_source(&root)),
        "binary without a closure report",
    )
    .to_string();
    assert!(error.contains("manifest names"), "{error}");
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

fn hosted_policy_contract() -> VelnorPolicyContract {
    VelnorPolicyContract {
        repository: None,
        providers: vec!["github-hosted".to_owned()],
        automatic_providers: vec!["github-hosted".to_owned()],
        selectors: BTreeMap::from([("github-hosted".to_owned(), vec!["ubuntu-24.04".to_owned()])]),
        selector_groups: BTreeMap::new(),
        require_local_runner_group: false,
        default_branch: "main".to_owned(),
    }
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
        audit_policy_entrypoint(&root, &hosted_policy_contract()),
        "audit generated entrypoint",
    );
    assert!(audit.trigger.is_empty(), "{:?}", audit.trigger);
    assert!(audit.privileges.is_empty(), "{:?}", audit.privileges);
    assert!(
        entrypoint.contains("with no secret references"),
        "the trust invariant states the absence honestly: {entrypoint}"
    );
    assert!(!entrypoint.contains("secrets."), "{entrypoint}");
    assert!(entrypoint.contains("permissions: {}\n"), "{entrypoint}");
    assert_eq!(entrypoint.matches("permissions:\n").count(), 2, "{entrypoint}");
    assert_eq!(entrypoint.matches("contents: read\n").count(), 2);
    assert!(entrypoint.contains("branches: [main]"), "{entrypoint}");
    assert!(!entrypoint.contains("workflow_dispatch:"), "{entrypoint}");
    assert!(entrypoint.contains("acquire-source:"), "{entrypoint}");
    assert!(entrypoint.contains("render-candidate:"), "{entrypoint}");
    assert!(entrypoint.contains("if: always()"), "{entrypoint}");
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
        !job.contains("candidate-manifest") && !job.contains("VELNOR_WORKFLOW_CANDIDATE"),
        "generic mainline policy does not consume PR-produced candidate binaries: {job}"
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
            "workflow permissions must be `{}`",
        ),
        (
            "job-permissions",
            "    permissions:\n      contents: read\n",
            "    permissions:\n      contents: read\n      id-token: write\n",
            "permissions do not match its least-privilege contract",
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
            "  pull_request_target:\n",
            "  push:\n  pull_request_target:\n",
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
            audit_policy_entrypoint(&root, &hosted_policy_contract()),
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

/// The local selector and runner group declared by synthetic trees.
const VELNOR_SELECTOR: &str = "example-velnor";
const VELNOR_GROUP: &str = "example-trusted";

fn velnor_tree(name: &str, pr_workflow: &str) -> PathBuf {
    let root = temporary_directory(name);
    write(
        &root.join(GENERATION_CONFIG),
        &format!(
            "schema = 2\n\n[generator]\nrepository = \"example/consumer\"\n\n[workflow]\nproviders = [\"github-hosted\", \"velnor\"]\nautomatic_providers = [\"github-hosted\", \"velnor\"]\ndefault_branch = \"main\"\nrequire_local_runner_group = true\n\n[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n[workflow.selectors.velnor]\ngroup = \"{VELNOR_GROUP}\"\nruns_on = [\"{VELNOR_SELECTOR}\"]\n"
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

#[test]
fn policy_resolves_only_explicit_complete_provider_selectors() {
    let root = temporary_directory("explicit-provider-selectors");
    write(
        &root.join(GENERATION_CONFIG),
        "schema = 2\n\n[generator]\nrepository = \"example/consumer\"\n\n[workflow]\nproviders = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]\n\n[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n[workflow.selectors.github-self-hosted]\nruns_on = [\"bastion-scale-set\"]\n\n[workflow.selectors.velnor]\nruns_on = [\"self-hosted\", \"example-velnor\"]\n",
    );
    let policy = must(
        configured_velnor_policy(&root),
        "load explicit provider policy",
    );
    assert_eq!(
        policy.provider_for_labels(&["BASTION-SCALE-SET"]),
        Some("github-self-hosted")
    );
    assert_eq!(
        policy.provider_for_labels(&["SELF-HOSTED", "EXAMPLE-VELNOR"]),
        Some("velnor")
    );
    let _ = fs::remove_dir_all(&root);

    let missing = temporary_directory("missing-provider-selector");
    write(
        &missing.join(GENERATION_CONFIG),
        "schema = 2\n\n[generator]\nrepository = \"example/consumer\"\n\n[workflow]\nproviders = [\"github-hosted\"]\n",
    );
    let error = must_fail(
        configured_velnor_policy(&missing),
        "missing declared selector must fail policy resolution",
    );
    assert!(
        error
            .to_string()
            .contains("requires [workflow.selectors.github-hosted]"),
        "{error}"
    );
    let _ = fs::remove_dir_all(&missing);

    let extra = temporary_directory("extra-provider-selector");
    write(
        &extra.join(GENERATION_CONFIG),
        "schema = 2\n\n[generator]\nrepository = \"example/consumer\"\n\n[workflow]\nproviders = [\"github-hosted\"]\n\n[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n[workflow.selectors.velnor]\nruns_on = [\"example-velnor\"]\n",
    );
    let error = must_fail(
        configured_velnor_policy(&extra),
        "selector outside the provider universe must fail policy resolution",
    );
    assert!(
        error
            .to_string()
            .contains("selector for `velnor` outside the configured provider universe"),
        "{error}"
    );
    let _ = fs::remove_dir_all(&extra);
}

#[test]
fn policy_rejects_case_insensitive_selector_collisions() {
    let cases = [
        (
            "hosted-local-label-collision",
            "[workflow]\nproviders = [\"github-hosted\", \"github-self-hosted\"]\n\n[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n[workflow.selectors.github-self-hosted]\nruns_on = [\"UBUNTU-24.04\"]\n",
            "case-insensitive",
        ),
        (
            "local-label-collision",
            "[workflow]\nproviders = [\"github-self-hosted\", \"velnor\"]\n\n[workflow.selectors.github-self-hosted]\nruns_on = [\"Bastion-Scale-Set\"]\n\n[workflow.selectors.velnor]\nruns_on = [\"bASTION-sCALE-sET\"]\n",
            "ignoring case",
        ),
        (
            "duplicate-label",
            "[workflow]\nproviders = [\"github-hosted\"]\n\n[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\", \"UBUNTU-24.04\"]\n",
            "GitHub runner labels are case-insensitive",
        ),
        (
            "hosted-self-hosted-label",
            "[workflow]\nproviders = [\"github-hosted\"]\n\n[workflow.selectors.github-hosted]\nruns_on = [\"SELF-HOSTED\"]\n",
            "hosted selector cannot name self-hosted runners",
        ),
    ];

    for (name, workflow, expected) in cases {
        let root = temporary_directory(name);
        write(
            &root.join(GENERATION_CONFIG),
            &format!(
                "schema = 2\n\n[generator]\nrepository = \"example/consumer\"\n\n{workflow}"
            ),
        );
        let error = must_fail(
            configured_velnor_policy(&root),
            "case-insensitive selector collision must fail policy validation",
        );
        assert!(
            error.to_string().contains(expected),
            "{name}: expected {expected:?}, got {error}"
        );
        let _ = fs::remove_dir_all(root);
    }
}

/// PR-controlled workflows contain hosted work only. The required aggregate
/// itself must always evaluate so a skipped unit cannot produce success.
fn hosted_pull_request_workflow() -> String {
    "name: CI / PR\non:\n  pull_request:\njobs:\n  ci-required:\n    name: ci-required\n    if: ${{ always() }}\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ok\n"
        .to_owned()
}

fn local_reusable_workflow(if_expression: &str) -> String {
    format!(
        "name: Local unit\non:\n  workflow_call:\njobs:\n  run:\n    if: {if_expression}\n    runs-on:\n      group: {VELNOR_GROUP}\n      labels: [{VELNOR_SELECTOR}]\n    steps:\n      - run: echo trusted\n"
    )
}

fn trusted_local_gate() -> String {
    "${{ ((false) || (github.repository == 'example/consumer' && github.event_name == 'push' && github.ref == 'refs/heads/main')) && (!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot'))) }}"
        .to_owned()
}

fn local_main_caller() -> String {
    "name: CI / Main\non:\n  push:\n    branches: [main]\njobs:\n  rust:\n    uses: ./.github/workflows/ci-unit-rust-velnor.yml\n"
        .to_owned()
}

/// Local runners require a base-configured label/group, exact repository
/// identity, and a default-branch push caller. PR workflows cannot call the
/// local reusable workflow even if its job carries an `if:` gate.
#[test]
fn local_runner_reusable_graph_is_repo_and_default_push_bound() {
    let root = velnor_tree("semantic-local-runner", &hosted_pull_request_workflow());
    let local_file = root.join(".github/workflows/ci-unit-rust-velnor.yml");
    write(&local_file, &local_reusable_workflow(&trusted_local_gate()));
    write(&root.join(".github/workflows/ci-main.yml"), &local_main_caller());

    let audit = must(audit_workflows(&root), "audit trusted local caller");
    assert!(audit.runners.is_empty(), "{:?}", audit.runners);
    assert!(audit.structure.is_empty(), "{:?}", audit.structure);

    let wrong_repository = local_reusable_workflow(
        &trusted_local_gate().replace(
            "github.repository == 'example/consumer' && ",
            "",
        ),
    );
    write(&local_file, &wrong_repository);
    let audit = must(audit_workflows(&root), "audit local caller without identity");
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("ci-unit-rust-velnor.yml")
                && finding.contains("local-provider jobs require a trusted-event gate")
        }),
        "repository identity omission must fail: {:?}",
        audit.runners
    );

    write(&local_file, &local_reusable_workflow(&trusted_local_gate()));
    write(
        &root.join(".github/workflows/ci-main.yml"),
        &local_main_caller().replace("push:\n    branches: [main]", "pull_request:"),
    );
    let audit = must(audit_workflows(&root), "audit PR-rooted local caller");
    assert!(
        audit.runners.iter().any(|finding| {
            finding.contains("ci-main.yml")
                && finding.contains("push to the trusted default branch")
        }),
        "PR-rooted caller must fail even with a job gate: {:?}",
        audit.runners
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn provider_gate_binds_repository_and_denies_unbrokered_dispatch() {
    let tree = velnor_tree("provider-gate-identity", &hosted_pull_request_workflow());
    let policy = must(configured_velnor_policy(&tree), "load provider policy");
    let trusted = "(!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot')))";
    let gate = format!(
        "${{{{ ((false) || (github.repository == 'example/consumer' && github.event_name == 'push' && github.ref == 'refs/heads/main')) && {trusted} }}}}"
    );
    assert!(is_generated_provider_gate(&gate, "velnor", &policy), "{gate}");
    assert!(
        !has_trusted_runner_gate(
            "github.event_name == 'push' && github.ref == 'refs/heads/main'",
            "main",
            Some("example/consumer"),
        ),
        "a branch/ref check without repository identity is insufficient"
    );
    assert!(
        !is_generated_provider_gate(
            &gate.replace("github.repository == 'example/consumer' && ", ""),
            "velnor",
            &policy,
        ),
        "a local gate without repository identity must fail"
    );
    let _ = fs::remove_dir_all(tree);
}

#[test]
fn a_second_pull_request_target_workflow_is_refused() {
    let root = velnor_tree("semantic-prt", &hosted_pull_request_workflow());
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
    let root = velnor_tree("semantic-job-env-allowed", &hosted_pull_request_workflow());
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
    let root = velnor_tree("semantic-job-env-runner", &hosted_pull_request_workflow());
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
    let root = velnor_tree("semantic-job-env-steps", &hosted_pull_request_workflow());
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
    let legacy = must_fail(
        run_cli(&[
            std::ffi::OsString::from("--candidate-manifest"),
            std::ffi::OsString::from("/first.json"),
        ]),
        "retired candidate-binary manifest option",
    )
    .to_string();
    assert!(legacy.contains("--candidate-manifest"), "{legacy}");
    assert!(legacy.contains("unsupported policy option"), "{legacy}");
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Candidate render artifacts

#[cfg(unix)]
fn closure_fixture(name: &str) -> (PathBuf, String) {
    let root = temporary_directory(name);
    git_ok(&root, &["init", "-q", "-b", "main"]);
    write(
        &root.join("crates/velnor-workflow/src/lib.rs"),
        "pub fn f() {}\n",
    );
    write(&root.join("Cargo.toml"), "[workspace]\n");
    write(&root.join("Cargo.lock"), "version = 4\n");
    write(&root.join(".github/workflows/ci-pr.yml"), "tree\n");
    let head = commit(&root, "fixture");
    (root, head)
}

#[test]
fn cargo_vendor_admission_allows_only_external_packages_present_in_the_trusted_lock() {
    const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";
    const TRUSTED_GIT: &str = "git+https://github.com/tailrocks/termrock.git?rev=5283c2acf9154d0cfcd37b1ffe821c00faf90ea2#5283c2acf9154d0cfcd37b1ffe821c00faf90ea2";
    let root = temporary_directory("cargo-source-admission");
    let trusted = root.join("trusted");
    let candidate = root.join("candidate");
    must(fs::create_dir_all(&trusted), "create trusted source root");
    must(fs::create_dir_all(&candidate), "create candidate source root");
    write(&trusted.join("Cargo.toml"), "[workspace]\nmembers = []\n");
    write(&candidate.join("Cargo.toml"), "[workspace]\nmembers = []\n");
    write(
        &trusted.join("Cargo.lock"),
        &format!(
            "version = 4\n\n[[package]]\nname = \"trusted-registry\"\nversion = \"1.0.0\"\nsource = \"{REGISTRY}\"\n\n[[package]]\nname = \"termrock\"\nversion = \"0.1.0\"\nsource = \"{TRUSTED_GIT}\"\n"
        ),
    );
    write(
        &candidate.join("Cargo.lock"),
        &format!(
            "version = 4\n\n[[package]]\nname = \"trusted-registry\"\nversion = \"1.0.0\"\nsource = \"{REGISTRY}\"\n\n[[package]]\nname = \"termrock\"\nversion = \"0.1.0\"\nsource = \"{TRUSTED_GIT}\"\n"
        ),
    );
    // Cargo configuration inside the candidate is not read by the base
    // acquisition job. Exact external package identities in the trusted lock
    // authorize what may be fetched.
    write(
        &candidate.join(".cargo/config.toml"),
        "[registries.untrusted]\nindex = \"sparse+http://127.0.0.1:9/sentinel/\"\n",
    );
    must(
        validate_candidate_cargo_sources(&candidate, &trusted),
        "allow exact crates.io and Git package identities already in the trusted lock",
    );

    write(
        &candidate.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"sentinel\"\nversion = \"1.0.0\"\nsource = \"registry+http://127.0.0.1:9/sentinel\"\n",
    );
    let error = must_fail(
        validate_candidate_cargo_sources(&candidate, &trusted),
        "reject a candidate-selected registry before Cargo network access",
    )
    .to_string();
    assert!(error.contains("adds or changes external package"), "{error}");
    assert!(error.contains("127.0.0.1:9"), "{error}");

    write(
        &candidate.join("Cargo.lock"),
        &format!(
            "version = 4\n\n[[package]]\nname = \"trusted-registry\"\nversion = \"9.9.9\"\nsource = \"{REGISTRY}\"\n"
        ),
    );
    let error = must_fail(
        validate_candidate_cargo_sources(&candidate, &trusted),
        "reject a candidate-selected crates.io version absent from the trusted lock",
    )
    .to_string();
    assert!(error.contains("trusted-registry 9.9.9"), "{error}");
    must(fs::remove_dir_all(root), "remove cargo source fixture");
}

#[test]
fn cargo_vendor_admission_rejects_local_paths_outside_the_snapshot() {
    let root = temporary_directory("cargo-path-admission");
    let trusted = root.join("trusted");
    let candidate = root.join("candidate");
    must(fs::create_dir_all(&trusted), "create trusted path root");
    must(fs::create_dir_all(candidate.join("crates/member")), "create candidate member");
    must(fs::create_dir_all(root.join("outside")), "create outside dependency");
    write(&trusted.join("Cargo.toml"), "[workspace]\nmembers = []\n");
    write(&trusted.join("Cargo.lock"), "version = 4\n");
    write(
        &candidate.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&candidate.join("Cargo.lock"), "version = 4\n");
    write(
        &candidate.join("crates/member/Cargo.toml"),
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\n\n[dependencies]\noutside = { path = \"../../../outside\" }\n",
    );
    write(&root.join("outside/Cargo.toml"), "[package]\nname = \"outside\"\nversion = \"0.1.0\"\n");
    let error = must_fail(
        validate_candidate_cargo_sources(&candidate, &trusted),
        "reject a path dependency outside the materialized snapshot",
    )
    .to_string();
    assert!(error.contains("resolves outside the source tree"), "{error}");
    must(fs::remove_dir_all(root), "remove cargo path fixture");
}

#[test]
fn sealed_vendor_config_is_rebased_and_routes_every_source_offline() {
    let root = temporary_directory("vendor-config-rebase");
    let registry = root.join("vendor/registry");
    must(fs::create_dir_all(&registry), "create vendor registry");
    let raw = format!(
        "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.\"git+https://example.invalid/repo?rev=abc#abc\"]\ngit = \"https://example.invalid/repo\"\nrev = \"abc\"\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = {:?}\n",
        registry.to_string_lossy()
    );
    let rewritten = must(rewrite_vendor_config(&raw, &registry), "rebase vendor config");
    assert!(rewritten.contains("directory = \"/vendor/registry\""), "{rewritten}");
    assert!(!rewritten.contains(&root.to_string_lossy().to_string()), "{rewritten}");
    must(
        validate_sealed_vendor_config(&rewritten),
        "validate offline vendor config",
    );
    let bypass = "[source.vendored-sources]\ndirectory = \"/vendor/registry\"\n\n[source.untrusted]\nregistry = \"https://example.invalid\"\n";
    let error = must_fail(
        validate_sealed_vendor_config(bypass),
        "reject unvendored Cargo source",
    )
    .to_string();
    assert!(error.contains("bypasses vendored-sources"), "{error}");
    must(fs::remove_dir_all(root), "remove vendor config fixture");
}

#[cfg(unix)]
fn remove_snapshot_for_test(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            must(
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)),
                "make snapshot directory removable",
            );
            for entry in must(fs::read_dir(path), "read snapshot for cleanup") {
                let entry = must(entry, "read snapshot cleanup entry");
                let child = entry.path();
                if must(fs::symlink_metadata(&child), "inspect snapshot cleanup entry")
                    .is_dir()
                {
                    remove_snapshot_for_test(&child);
                } else if !must(
                    fs::symlink_metadata(&child),
                    "inspect snapshot cleanup file",
                )
                .file_type()
                .is_symlink()
                {
                    must(
                        fs::set_permissions(&child, fs::Permissions::from_mode(0o600)),
                        "make snapshot file removable",
                    );
                }
            }
        }
    }
    must(fs::remove_dir_all(path), "remove candidate snapshot");
}

#[cfg(unix)]
fn candidate_source_artifact_fixture(root: &Path, head: &str) -> PathBuf {
    let closure = must(
        closure_identity::candidate_closure_of_tree(root, head),
        "compute candidate source closure",
    );
    let artifact = root.with_extension("candidate-source-artifact");
    must(fs::create_dir(&artifact), "create candidate source artifact");
    let pack = artifact.join("candidate-source.pack");
    must(
        create_candidate_source_pack(root, head, &pack),
        "pack candidate source objects",
    );
    let policy_tool = artifact.join("policy-tool");
    write(&policy_tool, "trusted base policy tool fixture\n");
    let vendor_root = artifact.join("vendor");
    must(
        fs::create_dir_all(vendor_root.join("registry")),
        "create empty vendored registry",
    );
    write(
        &vendor_root.join("config.toml"),
        "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"/vendor/registry\"\n",
    );
    let repository = crate::s2::workflow_setup_action_repository();
    let tree = git_ok(root, &["rev-parse", &format!("{head}^{{tree}}")]);
    let policy_tool_closure = must(
        closure_identity::closure_of_tree(
            root,
            head,
            closure_identity::CI_FEATURES,
            closure_identity::PROFILE_RELEASE,
        ),
        "compute trusted policy tool closure",
    );
    let manifest = CandidateSourceManifest {
        schema: "velnor.candidate-source/v2".to_owned(),
        repository: repository.to_owned(),
        source_repository: repository.to_owned(),
        run_id: "123".to_owned(),
        revision: head.to_owned(),
        base_revision: head.to_owned(),
        tree,
        closure,
        pack_sha256: must(sha256_file(&pack), "hash candidate source pack"),
        vendor_sha256: Some(must(render_tree_digest(&vendor_root), "hash empty fixture vendor")),
        policy_tool_revision: head.to_owned(),
        policy_tool_closure,
        policy_tool_sha256: must(sha256_file(&policy_tool), "hash trusted policy tool"),
    };
    must(
        write_json_create_new(&artifact.join("candidate-source-manifest.json"), &manifest),
        "write candidate source manifest",
    );
    artifact
}

#[cfg(unix)]
fn candidate_artifact_fixture(root: &Path, head: &str) -> (PathBuf, PathBuf, String) {
    let source_artifact = candidate_source_artifact_fixture(root, head);
    let source_manifest_path = source_artifact.join("candidate-source-manifest.json");
    let source_manifest_bytes = must(
        fs::read(&source_manifest_path),
        "read candidate source manifest",
    );
    let source_manifest: CandidateSourceManifest = must(
        serde_json::from_slice(&source_manifest_bytes),
        "parse candidate source manifest",
    );
    let artifact = root.with_extension("candidate-render-artifact");
    must(fs::create_dir(&artifact), "create candidate render artifact");
    let rendered = artifact.join("rendered");
    write(
        &rendered.join(".github/workflows/ci-pr.yml"),
        "tree\n",
    );
    let render_manifest = CandidateRenderManifest {
        schema: "velnor.candidate-render/v2".to_owned(),
        repository: source_manifest.repository,
        source_repository: source_manifest.source_repository,
        run_id: source_manifest.run_id,
        platform: "Linux-X64".to_owned(),
        revision: source_manifest.revision,
        base_revision: source_manifest.base_revision,
        tree: source_manifest.tree,
        closure: source_manifest.closure.clone(),
        source_pack_sha256: source_manifest.pack_sha256,
        vendor_sha256: must(
            source_manifest
                .vendor_sha256
                .clone()
                .ok_or("fixture has no vendor digest"),
            "read fixture vendor digest",
        ),
        builder_image: CANDIDATE_BUILDER_IMAGE.to_owned(),
        render_sha256: must(render_tree_digest(&rendered), "digest candidate render"),
    };
    must(
        write_json_create_new(&artifact.join("candidate-render-manifest.json"), &render_manifest),
        "seal candidate render artifact",
    );
    (source_artifact, artifact, source_manifest.closure)
}

#[cfg(unix)]
#[test]
fn raw_snapshot_preserves_export_attributes_without_archive_transforms() {
    let parent = temporary_directory("raw-snapshot-parent");
    let checkout = parent.join("checkout");
    must(fs::create_dir(&checkout), "create checkout");
    git_ok(&checkout, &["init", "-q", "-b", "main"]);
    write(
        &checkout.join(".gitattributes"),
        "ignored.txt export-ignore\nexpanded.txt export-subst\n",
    );
    write(&checkout.join("ignored.txt"), "must remain in raw tree\n");
    write(&checkout.join("expanded.txt"), "$Format:%H$\n");
    let head = commit(&checkout, "raw snapshot source");
    let snapshot = parent.join("snapshot");

    must(
        materialize_git_snapshot(&checkout, &head, &snapshot),
        "materialize raw tree",
    );

    assert_eq!(
        must(fs::read(snapshot.join("ignored.txt")), "read export-ignore file"),
        b"must remain in raw tree\n",
        "raw ls-tree/cat-file materialization must ignore archive export rules",
    );
    assert_eq!(
        must(fs::read(snapshot.join("expanded.txt")), "read export-subst file"),
        b"$Format:%H$\n",
        "raw blob bytes must not receive export-subst rewriting",
    );
    remove_snapshot_for_test(&snapshot);
    must(fs::remove_dir_all(parent), "remove raw snapshot fixture");
}

#[test]
fn source_pack_contains_unchanged_objects_and_empty_tree_delta_commits() {
    let parent = temporary_directory("source-pack-closure");
    let checkout = parent.join("checkout");
    must(fs::create_dir(&checkout), "create source checkout");
    git_ok(&checkout, &["init", "-q", "-b", "main"]);
    write(&checkout.join("unchanged.txt"), "present in every tree\n");
    write(&checkout.join("changed.txt"), "before\n");
    let base = commit(&checkout, "base tree");
    let base_tree = git_ok(&checkout, &["rev-parse", &format!("{base}^{{tree}}")]);

    write(&checkout.join("changed.txt"), "after\n");
    let changed_head = commit(&checkout, "change one file");
    let changed_tree = git_ok(&checkout, &["rev-parse", &format!("{changed_head}^{{tree}}")]);
    assert_ne!(changed_tree, base_tree, "one changed blob changes the tree");
    let changed_pack = parent.join("changed.pack");
    must(
        create_candidate_source_pack(&checkout, &changed_head, &changed_pack),
        "pack changed candidate source",
    );
    let changed_output = parent.join("changed-source");
    must(
        create_source_checkout(&changed_pack, &changed_head, &changed_tree, &changed_output),
        "materialize changed candidate source into an empty repository",
    );
    assert_eq!(
        must(fs::read(changed_output.join("unchanged.txt")), "read unchanged source"),
        b"present in every tree\n",
    );
    assert_eq!(
        must(fs::read(changed_output.join("changed.txt")), "read changed source"),
        b"after\n",
    );

    let empty_delta_head = commit(&checkout, "same tree, new commit");
    let empty_delta_tree = git_ok(
        &checkout,
        &["rev-parse", &format!("{empty_delta_head}^{{tree}}")],
    );
    assert_eq!(empty_delta_tree, git_ok(&checkout, &["rev-parse", &format!("{changed_head}^{{tree}}")]),
        "the second commit has the same tree as its parent");
    let empty_delta_pack = parent.join("empty-delta.pack");
    must(
        create_candidate_source_pack(&checkout, &empty_delta_head, &empty_delta_pack),
        "pack candidate commit with an unchanged tree",
    );
    let empty_delta_output = parent.join("empty-delta-source");
    must(
        create_source_checkout(
            &empty_delta_pack,
            &empty_delta_head,
            &empty_delta_tree,
            &empty_delta_output,
        ),
        "materialize commit with unchanged tree into an empty repository",
    );
    assert_eq!(
        must(fs::read(empty_delta_output.join("unchanged.txt")), "read empty-delta source"),
        b"present in every tree\n",
    );
    must(fs::remove_dir_all(parent), "remove source pack fixture");
}

#[cfg(unix)]
#[test]
fn raw_snapshot_rejects_dirty_checkout_and_mismatched_head_before_creating_output() {
    let (checkout, head) = closure_fixture("raw-snapshot-head");
    let snapshot = checkout.with_extension("snapshot-output");
    write(&checkout.join("untracked.txt"), "dirty\n");
    let dirty = must_fail(
        materialize_git_snapshot(&checkout, &head, &snapshot),
        "dirty checkout must be refused",
    )
    .to_string();
    assert!(dirty.contains("dirty"), "{dirty}");
    assert!(!snapshot.exists(), "refusal happens before output creation");
    must(fs::remove_file(checkout.join("untracked.txt")), "clean fixture");

    let other_head = commit(&checkout, "different explicit HEAD");
    assert_ne!(head, other_head);
    let mismatch = must_fail(
        materialize_git_snapshot(&checkout, &head, &snapshot),
        "a caller-supplied non-HEAD revision must be refused",
    )
    .to_string();
    assert!(mismatch.contains("does not match the explicit audited head"), "{mismatch}");
    assert!(!snapshot.exists(), "mismatch happens before output creation");
    must(fs::remove_dir_all(checkout), "remove raw snapshot checkout");
}

#[test]
fn raw_snapshot_rejects_case_and_file_directory_collisions() {
    let entry = |path: &str, mode: &str| RawTreeEntry {
        mode: mode.to_owned(),
        kind: if mode == "160000" { "commit" } else { "blob" }.to_owned(),
        object: "a".repeat(40),
        path: path.to_owned(),
    };
    for entries in [
        vec![entry("README.md", "100644"), entry("readme.md", "100644")],
        vec![entry("path", "100644"), entry("path/child", "100644")],
        vec![entry("Folder/child", "100644"), entry("folder", "100644")],
    ] {
        let error = must_fail(
            validate_snapshot_path_collisions(&entries),
            "ambiguous snapshot path set",
        )
        .to_string();
        assert!(
            error.contains("case-colliding") || error.contains("both a file and a directory"),
            "{error}"
        );
    }
}

#[cfg(unix)]
#[test]
fn raw_snapshot_rejects_escaping_symlinks() {
    assert!(validate_symlink_target("nested/link", b"../target").is_ok());
    let error = must_fail(
        validate_symlink_target("nested/link", b"../../../outside"),
        "escaping Git symlink",
    )
    .to_string();
    assert!(error.contains("escapes the candidate source snapshot"), "{error}");
}

#[test]
fn sanitized_git_command_removes_repository_and_object_store_redirects() {
    let env: BTreeMap<_, _> = closure_identity::sanitized_git_command().get_envs().collect();
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_REPLACE_REF_BASE",
        "GIT_NAMESPACE",
    ] {
        assert_eq!(env.get(std::ffi::OsStr::new(key)), Some(&None), "{key}");
    }
    assert_eq!(
        env.get(std::ffi::OsStr::new("GIT_CONFIG_NOSYSTEM")),
        Some(&Some(std::ffi::OsStr::new("1"))),
    );
}

#[cfg(unix)]
#[test]
fn candidate_render_artifact_binds_identity_and_never_executes_candidate_bytes() {
    let (root, head) = closure_fixture("candidate-artifact");
    let sentinel = root.join("candidate-executed");
    let (source_artifact, artifact, closure) = candidate_artifact_fixture(&root, &head);
    let expected_repository = crate::s2::workflow_setup_action_repository();

    assert_eq!(
        must(
            verify_candidate_render_artifact(
                &artifact,
                &source_artifact,
                &root,
                &root,
                &head,
                &head,
                &BTreeSet::new(),
                Some(expected_repository),
                Some(expected_repository),
                Some("123"),
                Some("Linux-X64"),
                Some(CANDIDATE_BUILDER_IMAGE),
            ),
            "verify candidate artifact as data",
        ),
        Some(closure.clone()),
    );
    let materialized_parent = temporary_directory("candidate-source-materialized");
    let materialized = materialized_parent.join("source");
    let pack = source_artifact.join("candidate-source.pack");
    let tree = git_ok(&root, &["rev-parse", &format!("{head}^{{tree}}")]);
    must(
        create_source_checkout(&pack, &head, &tree, &materialized),
        "materialize candidate source artifact",
    );
    assert_eq!(
        must(
            verify_candidate_source_artifact(
                &source_artifact,
                &root,
                &materialized,
                &head,
                &head,
                expected_repository,
                expected_repository,
                "123",
            ),
            "verify candidate source artifact",
        ),
        closure,
    );
    assert!(!sentinel.exists(), "policy verification must never execute candidate bytes");

    let wrong_repo = must_fail(
        verify_candidate_render_artifact(
            &artifact,
            &source_artifact,
            &root,
            &root,
            &head,
            &head,
            &BTreeSet::new(),
            Some("another/repository"),
            Some(expected_repository),
            Some("123"),
            Some("Linux-X64"),
            Some(CANDIDATE_BUILDER_IMAGE),
        ),
        "artifact from the wrong repository",
    )
    .to_string();
    assert!(wrong_repo.contains("identity does not match"), "{wrong_repo}");
    let wrong_run = must_fail(
        verify_candidate_render_artifact(
            &artifact,
            &source_artifact,
            &root,
            &root,
            &head,
            &head,
            &BTreeSet::new(),
            Some(expected_repository),
            Some(expected_repository),
            Some("456"),
            Some("Linux-X64"),
            Some(CANDIDATE_BUILDER_IMAGE),
        ),
        "artifact from the wrong producer run",
    )
    .to_string();
    assert!(wrong_run.contains("identity does not match"), "{wrong_run}");
    assert!(!sentinel.exists(), "invalid artifacts are data-only too");

    must(
        fs::remove_dir_all(&artifact),
        "remove candidate artifact fixture",
    );
    must(
        fs::remove_dir_all(&source_artifact),
        "remove candidate source artifact fixture",
    );
    remove_snapshot_for_test(&materialized_parent);
    must(fs::remove_dir_all(root), "remove candidate source fixture");
}

#[cfg(unix)]
#[test]
fn candidate_source_manifest_binds_hidden_vendored_files() {
    let (root, head) = closure_fixture("vendor-digest");
    let artifact = candidate_source_artifact_fixture(&root, &head);
    let manifest_bytes = must(
        fs::read(artifact.join("candidate-source-manifest.json")),
        "read candidate source manifest",
    );
    let manifest: CandidateSourceManifest =
        must(serde_json::from_slice(&manifest_bytes), "parse candidate source manifest");
    must(
        verify_source_vendor(&artifact, &manifest),
        "verify original vendor tree",
    );
    write(
        &artifact.join("vendor/registry/.cargo-checksum.json"),
        "{}\n",
    );
    let error = must_fail(
        verify_source_vendor(&artifact, &manifest),
        "detect hidden vendor tampering",
    )
    .to_string();
    assert!(error.contains("candidate vendor digest mismatch"), "{error}");
    must(fs::remove_dir_all(&artifact), "remove source vendor fixture");
    must(fs::remove_dir_all(root), "remove source vendor git fixture");
}

#[cfg(unix)]
#[test]
fn candidate_render_digest_is_path_independent_and_rejects_symlinks() {
    let first = temporary_directory("render-digest-a");
    let second = temporary_directory("render-digest-b");
    write(&first.join("nested/output.yml"), "bytes\n");
    write(&second.join("nested/output.yml"), "bytes\n");
    assert_eq!(
        must(render_tree_digest(&first), "digest first render tree"),
        must(render_tree_digest(&second), "digest second render tree"),
        "the digest uses relative names and file bytes only",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        must(
            symlink("/etc/passwd", first.join("nested/outside")),
            "create hostile candidate symlink",
        );
        let error = must_fail(
            render_tree_digest(&first),
            "candidate render symlink",
        )
        .to_string();
        assert!(error.contains("contains a symlink"), "{error}");
    }
    must(fs::remove_dir_all(first), "remove first digest fixture");
    must(fs::remove_dir_all(second), "remove second digest fixture");
}

#[cfg(unix)]
#[test]
fn candidate_render_artifact_rejects_tampered_output_digest() {
    let (root, head) = closure_fixture("candidate-artifact-tamper");
    let (source_artifact, artifact, _) = candidate_artifact_fixture(&root, &head);
    write(
        &artifact.join("rendered/.github/workflows/ci-pr.yml"),
        "tampered\n",
    );
    let error = must_fail(
        verify_candidate_render_artifact(
            &artifact,
            &source_artifact,
            &root,
            &root,
            &head,
            &head,
            &BTreeSet::new(),
            Some(crate::s2::workflow_setup_action_repository()),
            Some(crate::s2::workflow_setup_action_repository()),
            Some("123"),
            Some("Linux-X64"),
            Some(CANDIDATE_BUILDER_IMAGE),
        ),
        "tampered candidate output",
    )
    .to_string();
    assert!(error.contains("candidate render digest mismatch"), "{error}");
    must(fs::remove_dir_all(artifact), "remove candidate artifact fixture");
    must(fs::remove_dir_all(source_artifact), "remove candidate source fixture");
    must(fs::remove_dir_all(root), "remove candidate source fixture");
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

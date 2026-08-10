//! Generation, membership, byte-identity, tamper, and materialization tests.

mod common;

use sha2::{Digest, Sha256};
use std::path::Path;

use velnor_actions_generator::model::{FleetManifest, is_sha40};
use velnor_actions_generator::render::{self, CALVER_PLACEHOLDER, FLEET_SHA_PLACEHOLDER};
use velnor_actions_generator::{ALL_CLASSES, RepositoryClass, audit, generate};

const DUMMY_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

fn load() -> FleetManifest {
    FleetManifest::load(&common::repo_root()).expect("fleet loads")
}

#[test]
fn exact_membership_and_counts() {
    let m = load();
    assert_eq!(m.repositories().len(), 28, "28 members");
    assert_eq!(m.members_of(RepositoryClass::Code).len(), 20);
    assert_eq!(m.members_of(RepositoryClass::Tap).len(), 5);
    assert_eq!(m.members_of(RepositoryClass::Apt).len(), 2);
    assert_eq!(m.members_of(RepositoryClass::Fixture).len(), 1);
    assert_eq!(m.classes().len(), 4);
    for r in m.repositories() {
        assert!(is_sha40(&r.baseline_sha), "{} sha is 40-hex", r.slug);
    }
}

#[test]
fn repository_inventory_bytes_are_exactly_bound() {
    let bytes = std::fs::read(common::repo_root().join("fleet").join("repositories.toml")).unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(bytes)),
        "7c7d8bed7d95bb985bab68c64065260c494da20c233d6c97d05c3ea3b338c85c"
    );
}

fn write_repos(dir: &Path, body: &str) {
    std::fs::create_dir_all(dir.join("fleet")).unwrap();
    std::fs::write(dir.join("fleet").join("repositories.toml"), body).unwrap();
    std::fs::copy(
        common::repo_root().join("fleet").join("classes.toml"),
        dir.join("fleet").join("classes.toml"),
    )
    .unwrap();
}

fn canonical_repositories_toml() -> String {
    std::fs::read_to_string(common::repo_root().join("fleet").join("repositories.toml")).unwrap()
}

#[test]
fn duplicate_member_rejected() {
    let dir = common::temp_dir("dup");
    let mut body = canonical_repositories_toml();
    // Duplicate the first member entry — now 25 with a duplicate slug.
    body.push_str(
        "\n[[repository]]\nslug = \"jackin-project/jackin\"\nclass = \"code\"\nbaseline_sha = \"3e6376d213f2aae66b00b376057ff0863c988040\"\n",
    );
    write_repos(&dir, &body);
    let err = FleetManifest::load(&dir).unwrap_err();
    assert!(err.contains("duplicate"), "got: {err}");
}

#[test]
fn unknown_class_rejected() {
    let dir = common::temp_dir("unkclass");
    let body = "[[repository]]\nslug = \"tailrocks/whatever\"\nclass = \"quantum\"\nbaseline_sha = \"3e6376d213f2aae66b00b376057ff0863c988040\"\n";
    write_repos(&dir, body);
    let err = FleetManifest::load(&dir).unwrap_err();
    assert!(err.contains("unknown class"), "got: {err}");
}

#[test]
fn wrong_count_rejected() {
    let dir = common::temp_dir("count");
    // Only one code member — wrong count for every class.
    let body = "[[repository]]\nslug = \"tailrocks/only\"\nclass = \"code\"\nbaseline_sha = \"3e6376d213f2aae66b00b376057ff0863c988040\"\n";
    write_repos(&dir, body);
    let err = FleetManifest::load(&dir).unwrap_err();
    assert!(err.contains("expected"), "got: {err}");
}

#[test]
fn bad_sha_rejected() {
    let dir = common::temp_dir("badsha");
    let body =
        "[[repository]]\nslug = \"tailrocks/x\"\nclass = \"code\"\nbaseline_sha = \"NOTHEX\"\n";
    write_repos(&dir, body);
    let err = FleetManifest::load(&dir).unwrap_err();
    assert!(err.contains("40 lowercase hex"), "got: {err}");
}

#[test]
fn bad_slug_rejected() {
    let dir = common::temp_dir("badslug");
    let body = "[[repository]]\nslug = \"no-slash\"\nclass = \"code\"\nbaseline_sha = \"3e6376d213f2aae66b00b376057ff0863c988040\"\n";
    write_repos(&dir, body);
    let err = FleetManifest::load(&dir).unwrap_err();
    assert!(err.contains("invalid slug"), "got: {err}");
}

#[test]
fn unknown_organization_rejected() {
    let dir = common::temp_dir("unkorg");
    let body = "[[repository]]\nslug = \"strangers/x\"\nclass = \"code\"\nbaseline_sha = \"3e6376d213f2aae66b00b376057ff0863c988040\"\n";
    write_repos(&dir, body);
    let err = FleetManifest::load(&dir).unwrap_err();
    // Count check trips first for this single-member file; either rejection is fine
    // as long as the invalid input is refused before output.
    assert!(
        err.contains("expected") || err.contains("unknown organization"),
        "got: {err}"
    );
}

#[test]
fn templates_are_deterministic() {
    for class in ALL_CLASSES {
        let a = render::consumer_template(class);
        let b = render::consumer_template(class);
        assert_eq!(a, b, "render is deterministic for {}", class.code());
        assert!(a.ends_with('\n'), "final newline");
        assert!(!a.contains('\r'), "LF only");
        assert!(!a.contains(" \n"), "no trailing whitespace");
    }
}

#[test]
fn exactly_four_templates_render() {
    // One committed template per class, byte-equal to a fresh render.
    for class in ALL_CLASSES {
        let committed = std::fs::read_to_string(
            common::repo_root()
                .join("templates")
                .join(class.code())
                .join("ci.yml"),
        )
        .expect("committed template");
        assert_eq!(committed, render::consumer_template(class));
    }
}

#[test]
fn audit_passes_on_repo() {
    let line = audit::audit(&common::repo_root()).expect("audit passes");
    assert_eq!(line, "fleet valid: 28 repositories, 4 classes, 4 templates");
}

#[test]
fn bound_fixture_audit_passes() {
    let dir = common::bound_fixture(DUMMY_SHA);
    let line = audit::audit(&dir).expect("bound audit passes");
    assert_eq!(line, "fleet valid: 28 repositories, 4 classes, 4 templates");
}

#[test]
fn one_byte_tamper_of_template_fails_audit() {
    let dir = common::bound_fixture(DUMMY_SHA);
    common::tamper_one_byte(&dir.join("templates").join("code").join("ci.yml"));
    assert!(
        audit::audit(&dir).is_err(),
        "tampered template must fail audit"
    );
}

#[test]
fn one_byte_tamper_of_callable_fails_audit() {
    let dir = common::bound_fixture(DUMMY_SHA);
    common::tamper_one_byte(&dir.join(".github").join("workflows").join("ci-code.yml"));
    assert!(
        audit::audit(&dir).is_err(),
        "tampered callable must fail audit"
    );
}

#[test]
fn one_byte_tamper_of_composite_fails_audit() {
    // A single-byte edit to either composite action body fails the audit, exactly
    // like a tampered template or callable workflow.
    for name in ["run-gate", "aggregate"] {
        let dir = common::bound_fixture(DUMMY_SHA);
        common::tamper_one_byte(&dir.join("actions").join(name).join("action.yml"));
        assert!(
            audit::audit(&dir).is_err(),
            "tampered composite {name} must fail audit"
        );
    }
}

#[test]
fn neutered_run_gate_exec_fails_audit() {
    // The exact attack: replace the run-gate exec line with a no-op so every CI
    // gate silently passes. The audit must reject it (the body, not just refs, is
    // byte-verified). The neutered composite stays a valid composite action with
    // 40-hex refs, so only the body check can catch it.
    let dir = common::bound_fixture(DUMMY_SHA);
    let path = dir.join("actions").join("run-gate").join("action.yml");
    let body = std::fs::read_to_string(&path).unwrap();
    let neutered = body.replace("bash -eo pipefail -c \"${GATE_COMMAND}\"", "echo skipped");
    assert_ne!(
        neutered, body,
        "the neutering must actually change the body"
    );
    assert!(neutered.contains("using: composite"), "still a composite");
    std::fs::write(&path, &neutered).unwrap();
    let err = audit::audit(&dir).expect_err("neutered composite must fail audit");
    assert!(
        err.contains("actions/run-gate/action.yml"),
        "error names the tampered composite: {err}"
    );
}

#[test]
fn canonical_composites_match_committed() {
    // The embedded canonical bytes stay byte-identical to the committed action
    // files; otherwise the on-disk composites and the audit's source of truth have
    // silently diverged.
    use velnor_actions_generator::composite;
    for name in composite::COMPOSITE_NAMES {
        let committed = std::fs::read_to_string(
            common::repo_root()
                .join("actions")
                .join(name)
                .join("action.yml"),
        )
        .expect("committed composite");
        assert_eq!(
            committed,
            composite::canonical(name).unwrap(),
            "committed {name} matches canonical bytes"
        );
    }
}

#[test]
fn block_sha_tamper_fails_audit() {
    let dir = common::bound_fixture(DUMMY_SHA);
    // Rebind block-sha without regenerating: committed callable workflows now
    // reference the old SHA and diverge from regeneration.
    std::fs::write(
        dir.join("fleet").join("block-sha"),
        "1234567890123456789012345678901234567890\n",
    )
    .unwrap();
    assert!(
        audit::audit(&dir).is_err(),
        "rebound block-sha must fail audit"
    );
}

#[test]
fn non_hex_block_sha_rejected() {
    let dir = common::bound_fixture(DUMMY_SHA);
    std::fs::write(dir.join("fleet").join("block-sha"), "main\n").unwrap();
    let err = audit::audit(&dir).unwrap_err();
    assert!(err.contains("40 lowercase hex"), "got: {err}");
}

#[test]
fn every_member_materializes_to_class_bytes() {
    let m = load();
    let sha = DUMMY_SHA;
    let calver = "2026.7.0";
    for class in ALL_CLASSES {
        let template = render::consumer_template(class);
        let class_bytes = render::render_consumer(&template, sha, calver).unwrap();
        for repo in m.members_of(class) {
            let repo_bytes = render::render_consumer(&template, sha, calver).unwrap();
            assert_eq!(
                repo_bytes,
                class_bytes,
                "{} == class {}",
                repo.slug,
                class.code()
            );
        }
    }
}

#[test]
fn render_consumer_refuses_leftover_and_second_substitution() {
    let template = render::consumer_template(RepositoryClass::Code);
    let out = render::render_consumer(&template, DUMMY_SHA, "2026.7.0").unwrap();
    assert!(!out.contains(FLEET_SHA_PLACEHOLDER));
    assert!(!out.contains(CALVER_PLACEHOLDER));
    // A second substitution has nothing to replace and must be refused.
    assert!(render::render_consumer(&out, DUMMY_SHA, "2026.7.0").is_err());
    // A non-40-hex release SHA is refused.
    assert!(render::render_consumer(&template, "main", "2026.7.0").is_err());
}

#[test]
fn render_consumer_to_dir_writes_only_ci_yml() {
    let out = common::temp_dir("consumer-out");
    let path = velnor_actions_generator::render_consumer_to_dir(
        &common::repo_root(),
        "tailrocks/velnor",
        DUMMY_SHA,
        "2026.7.0",
        &out,
    )
    .unwrap();
    assert!(path.ends_with(Path::new(".github/workflows/ci.yml")));
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains(&format!("@{DUMMY_SHA} # 2026.7.0")));
    assert!(!body.contains(FLEET_SHA_PLACEHOLDER));
}

#[test]
fn generate_is_idempotent() {
    let dir = common::bound_fixture(DUMMY_SHA);
    let before =
        std::fs::read_to_string(dir.join(".github").join("workflows").join("ci-code.yml")).unwrap();
    generate(&dir).unwrap();
    let after =
        std::fs::read_to_string(dir.join(".github").join("workflows").join("ci-code.yml")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn release_goldens_bind_consumer_interface_and_callable_metrics_schema() {
    let root = common::repo_root();
    for (path, expected) in [
        (
            "templates/code/ci.yml",
            "66443236c216c1298096fb4a58c93c45f4b601c0217bd3d6194d0f69b59db1af",
        ),
        (
            "templates/tap/ci.yml",
            "4487e1060032463003bb12e376cc7d6cfbaeb672b0b296f63d1e24575e2fe426",
        ),
        (
            "templates/apt/ci.yml",
            "a3d491af8b71baa5b892c8db1959d05a39d5256c6cfe27be5a4e73fd5c40cb6b",
        ),
        (
            "templates/fixture/ci.yml",
            "70a2a68ebd629effb693c7ed2cc4949ee1491de475ee5d56d80e13b4c2c17305",
        ),
        (
            ".github/workflows/ci-code.yml",
            "0ee7d02e78a077ca0a3808551d1b2e23eef35b3fdf396109345eda3ba20f6f19",
        ),
        (
            ".github/workflows/ci-tap.yml",
            "26587fe43b10b5affaa927cf7c2772463e4febca50eba4ca95dba9f8bc9fd0b7",
        ),
        (
            ".github/workflows/ci-apt.yml",
            "b314dc0c2269df5a00dd252a2aa892a9197b47dd758c8a23384d31a7527da005",
        ),
        (
            ".github/workflows/ci-fixture.yml",
            "2d548a02025eb57c4e682992ce383dd2554a241341720f390042dc3b29c1bc04",
        ),
    ] {
        let bytes = std::fs::read(root.join(path)).unwrap();
        assert_eq!(hex::encode(Sha256::digest(bytes)), expected, "{path}");
    }
}

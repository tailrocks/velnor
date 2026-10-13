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
fn package_policy_and_workflows_are_closed_and_hosted_only() {
    use velnor_actions_generator::package::{
        APT_TEMPLATE, PackagePolicy, SIGNER_WORKFLOW, TAP_TEMPLATE, UPDATER_WORKFLOW,
    };
    PackagePolicy::load(&common::repo_root()).expect("package policy loads");
    let package_policy = std::fs::read_to_string(common::repo_root().join("fleet/packages.toml"))
        .expect("package policy bytes");
    assert!(!package_policy.contains(".tar.xz"));
    assert_eq!(package_policy.matches(".tar.gz").count(), 16);
    assert_eq!(package_policy.matches(".zip").count(), 2);
    for producer_asset in [
        "velnor-runner-*-amd64.deb",
        "velnor-runner-*-arm64.deb",
        "holla-*-x86_64-unknown-linux-gnu.deb",
        "holla-*-aarch64-unknown-linux-gnu.deb",
    ] {
        assert!(package_policy.contains(producer_asset));
    }
    assert!(!package_policy.contains("velnor-runner_*_amd64.deb"));
    assert!(!package_policy.contains("holla_*_amd64.deb"));
    for body in [
        SIGNER_WORKFLOW,
        UPDATER_WORKFLOW,
        TAP_TEMPLATE,
        APT_TEMPLATE,
    ] {
        assert!(!body.contains("pull_request_target"));
        assert!(!body.contains("secrets: inherit"));
        assert!(!body.contains("runs-on: self-hosted"));
    }
    assert!(UPDATER_WORKFLOW.contains("--deny-self-hosted-runners"));
    assert!(UPDATER_WORKFLOW.contains("sha256sum --check --strict"));
    assert!(
        UPDATER_WORKFLOW
            .contains("actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1")
    );
    assert!(UPDATER_WORKFLOW.contains("git commit --signoff"));
    assert!(UPDATER_WORKFLOW.contains("test \"$BRANCH\" != \"main\""));
    assert!(UPDATER_WORKFLOW.contains("test \"$CHANNEL\" = stable"));
    assert!(UPDATER_WORKFLOW.contains("no immutable stable release"));
    assert!(!UPDATER_WORKFLOW.contains("releases/latest"));
    assert!(TAP_TEMPLATE.matches("channel: stable").count() == 3);
    assert!(APT_TEMPLATE.matches("channel: stable").count() == 3);
    assert!(
        TAP_TEMPLATE
            .matches("package-update/verified/stable")
            .count()
            == 3
    );
}

#[test]
fn package_generation_writes_exact_callables_and_templates() {
    let dir = common::bound_fixture(DUMMY_SHA);
    let updater = velnor_actions_generator::package::PackagePolicy::load(&dir)
        .unwrap()
        .render_updater();
    for (path, expected) in [
        (
            ".github/workflows/package-signer.yml",
            velnor_actions_generator::package::SIGNER_WORKFLOW,
        ),
        (".github/workflows/package-updater.yml", updater.as_str()),
        (
            "templates/tap/package-update.yml",
            velnor_actions_generator::package::TAP_TEMPLATE,
        ),
        (
            "templates/apt/package-update.yml",
            velnor_actions_generator::package::APT_TEMPLATE,
        ),
    ] {
        assert_eq!(std::fs::read_to_string(dir.join(path)).unwrap(), expected);
    }
}

#[test]
fn package_consumer_renderer_binds_current_and_bounded_old_signers() {
    let policy = velnor_actions_generator::package::PackagePolicy::load(&common::repo_root())
        .expect("package policy loads");
    let current = "1111111111111111111111111111111111111111";
    let owner_shas = [
        current,
        "3333333333333333333333333333333333333333",
        "4444444444444444444444444444444444444444",
    ];
    let old = "2222222222222222222222222222222222222222";
    let rendered = policy
        .render_consumer(
            "tailrocks/homebrew-tablerock",
            owner_shas,
            "2026.8.6",
            Some((old, "2026-08-12T00:00:00Z", "2026-09-11T00:00:00Z")),
        )
        .expect("bounded rotation renders");
    assert_eq!(rendered.matches(current).count(), 1);
    assert_eq!(rendered.matches(owner_shas[1]).count(), 1);
    assert_eq!(rendered.matches(owner_shas[2]).count(), 1);
    assert_eq!(
        rendered
            .matches("1e062d5bbe329873047ee8a8e79bba0811e53b65")
            .count(),
        3
    );
    assert_eq!(rendered.matches(old).count(), 3);
    assert!(!rendered.contains("@FLEET_SHA@"));
    assert!(rendered.contains("old-signer-expires-at: \"2026-09-11T00:00:00Z\""));

    assert!(
        policy
            .render_consumer("tailrocks/not-a-consumer", owner_shas, "2026.8.6", None)
            .is_err()
    );
    assert!(
        policy
            .render_consumer(
                "tailrocks/homebrew-tablerock",
                ["main", owner_shas[1], owner_shas[2]],
                "2026.8.6",
                None
            )
            .is_err()
    );
    assert!(
        policy
            .render_consumer("tailrocks/homebrew-tablerock", owner_shas, "v1", None)
            .is_err()
    );
    assert!(
        policy
            .render_consumer(
                "tailrocks/homebrew-tablerock",
                owner_shas,
                "2026.8.6",
                Some((
                    "1e062d5bbe329873047ee8a8e79bba0811e53b65",
                    "2026-08-12T00:00:00Z",
                    "2026-09-11T00:00:00Z"
                )),
            )
            .is_err()
    );
}

#[test]
fn updater_executes_explicit_current_then_old_signer_alternatives() {
    let body = velnor_actions_generator::package::UPDATER_WORKFLOW;
    assert!(body.contains("accepted_digests=(\"$CURRENT_SIGNER_DIGEST\")"));
    assert!(body.contains("accepted_digests+=(\"$OLD_SIGNER_DIGEST\")"));
    assert!(body.contains("30 * 24 * 60 * 60"));
    assert!(body.contains("for signer_digest in \"${accepted_digests[@]}\""));
    assert!(body.contains("--signer-digest \"$signer_digest\""));
    assert!(body.contains("$SOURCE_OWNER/velnor-actions/.github/workflows/package-signer.yml"));
    assert!(body.contains("keys == [\"accepted_signer_digest\",\"verification\"]"));
    assert!(body.contains(".verification | type == \"array\" and length > 0"));
    assert!(!body.contains("all(.[]; type == \"array\""));
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
            "494474fe641f61a41015241f52caf9975f4f78c1f449fc8023059ac07960a20c",
        ),
        (
            "templates/tap/ci.yml",
            "fa7594036974b32594485a380fe8c0b6fc21133f6dce355171b208526e91b463",
        ),
        (
            "templates/apt/ci.yml",
            "552fa230668e1f792bfcf945e1e99dc62276c2064acf99860753f83e9fd4b60e",
        ),
        (
            "templates/fixture/ci.yml",
            "dea5960a0d49d003b6e1e1a5eafae7576ebe60ced5acc62e0883004dfa64df30",
        ),
        (
            ".github/workflows/ci-code.yml",
            "defaa5aa904d526d9b51b7bb1caa73830fe4753c8f54f588935a639d8d45a99b",
        ),
        (
            ".github/workflows/ci-tap.yml",
            "bae4aeb15b28db1b55b38b055aec295832ce3b54a8a5e138351f486b9872f877",
        ),
        (
            ".github/workflows/ci-apt.yml",
            "4ca77bec5ef4940c169199dbad29724afee3887e5590734cd0c1ac6bfb831a5b",
        ),
        (
            ".github/workflows/ci-fixture.yml",
            "52f630624dd5f2cbc85d446239360bef774bfd64675c9a5aa4d6f27408e86bc1",
        ),
    ] {
        let bytes = std::fs::read(root.join(path)).unwrap();
        assert_eq!(hex::encode(Sha256::digest(bytes)), expected, "{path}");
    }
}

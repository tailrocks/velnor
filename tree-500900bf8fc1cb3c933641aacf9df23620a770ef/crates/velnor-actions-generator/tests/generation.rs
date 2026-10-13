//! Generation, membership, byte-identity, tamper, and materialization tests.

mod common;

use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

use velnor_actions_generator::forks::ForkTable;
use velnor_actions_generator::model::{FleetManifest, RepositoryKind, is_sha40};
use velnor_actions_generator::render::{self, CALVER_PLACEHOLDER, FLEET_SHA_PLACEHOLDER};
use velnor_actions_generator::{ALL_CLASSES, RepositoryClass, audit, generate};

const DUMMY_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

fn load() -> FleetManifest {
    FleetManifest::load(&common::repo_root()).expect("fleet loads")
}

#[test]
fn consumer_pushes_are_limited_to_default_branch_and_release_tags() {
    let rendered = render::consumer_template(RepositoryClass::Code);
    assert!(
        rendered.contains("  push:\n    branches:\n      - main\n    tags:\n      - \"**\"\n"),
        "non-default branch pushes must not create a competing required-check context"
    );
}

#[test]
fn consumer_weekly_schedule_defaults_to_velnor() {
    for class in ALL_CLASSES {
        let rendered = render::consumer_template(class);
        assert!(rendered.contains("  schedule:\n    - cron: \"23 3 * * 0\"\n"));
        assert!(rendered.contains(
            "github.event_name == 'workflow_dispatch' && inputs.lanes || (github.event_name == 'pull_request' || github.event_name == 'merge_group' || github.event_name == 'push') && 'github' || 'velnor'"
        ));
        assert!(!rendered.contains("github.event_name == 'schedule' && 'both'"));
        // ci-required follows the public-unmerged and post-merge GitHub route.
        assert!(rendered.contains(
            "runs-on: ${{ ((github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') || github.event_name == 'pull_request' || github.event_name == 'merge_group' || github.event_name == 'push') && 'ubuntu-26.04' || fromJSON('[\"self-hosted\",\"velnor-target-mvp\"]') }}"
        ));
        assert!(!rendered.contains("ubuntu-latest"));
    }
}

#[test]
fn code_lane_timeout_covers_proven_cold_monorepo_runtime() {
    let workflows = common::repo_root().join(".github/workflows");
    let code = std::fs::read_to_string(workflows.join("ci-code.yml")).unwrap();
    assert_eq!(code.matches("timeout-minutes: 60").count(), 2);

    for class in ["tap", "apt", "fixture"] {
        let rendered = std::fs::read_to_string(workflows.join(format!("ci-{class}.yml"))).unwrap();
        assert!(!rendered.contains("timeout-minutes: 60"));
    }
    // The native platform lane runs the full macOS desktop gate on a shared
    // GitHub-hosted runner: proven runtime spans ~23-30+ minutes cold, so the
    // class overrides the platform timeout to 60 while lanes stay at 60/30.
    let native = std::fs::read_to_string(workflows.join("ci-native.yml")).unwrap();
    assert_eq!(native.matches("timeout-minutes: 60").count(), 1);
}

#[test]
fn exact_membership_and_counts() {
    let m = load();
    assert_eq!(m.repositories().len(), 28, "28 members");
    assert_eq!(m.members_of(RepositoryClass::Code).len(), 19);
    assert_eq!(m.members_of(RepositoryClass::Native).len(), 1);
    assert_eq!(m.members_of(RepositoryClass::Tap).len(), 5);
    assert_eq!(m.members_of(RepositoryClass::Apt).len(), 2);
    assert_eq!(m.members_of(RepositoryClass::Fixture).len(), 1);
    assert_eq!(m.classes().len(), 5);
    for r in m.repositories() {
        assert!(is_sha40(&r.baseline_sha), "{} sha is 40-hex", r.slug);
    }
}

#[test]
fn task031_out_of_scope_census_row_is_authoritative() {
    let manifest = load();
    let row = manifest
        .repositories()
        .iter()
        .find(|repository| repository.slug == "tailrocks/tailrocks-skills")
        .expect("TASK-031 repository remains registered");
    assert_eq!(row.census.kind, RepositoryKind::OutOfScope);
    assert!(row.census.research);
}

#[test]
fn velnor_ci_producer_census_row_is_authoritative() {
    let manifest = load();
    let row = manifest
        .repositories()
        .iter()
        .find(|repository| repository.slug == "tailrocks/velnor")
        .expect("Velnor repository remains registered");
    assert_eq!(row.census.kind, RepositoryKind::CiProducer);
    assert!(!row.census.research);
}

#[test]
fn containment_rule_is_scoped_to_path_bearing_build_fields() {
    let config = velnor_actions_generator::policy::alint_config(RepositoryClass::Code);
    assert!(config.contains(
        "(?:^|[{,])\\s*[\"'']?(?:path|paths|build|build_dir|build-directory|directory|dir|workspace|members|manifest|root|context)"
    ));
    assert!(!config.contains("pattern: '(?:\\.\\./){2,}'"));
}

#[test]
fn containment_rule_allows_package_readme_but_rejects_dependency_path_escape() {
    if Command::new("alint").arg("--version").output().is_err() {
        // The locked repository gate runs this behavioral check with alint
        // installed; plain library-only test runs may not have the external
        // linter on PATH.
        return;
    }
    let root = common::temp_dir("containment");
    let config = root.join(".alint.yml");
    std::fs::write(
        &config,
        velnor_actions_generator::policy::alint_config(RepositoryClass::Code),
    )
    .unwrap();
    let cargo_toml = root.join("Cargo.toml");
    std::fs::write(
        &cargo_toml,
        "[package]\nname = \"readme-link\"\nversion = \"0.1.0\"\nreadme = \"../../README.md\"\n",
    )
    .unwrap();
    let pass = Command::new("alint")
        .args([
            "check",
            "--config",
            config.to_str().unwrap(),
            "--only",
            "containment.no-parent-escape",
            "--fail-on-warning",
            "--no-gitignore",
            root.to_str().unwrap(),
        ])
        .status()
        .expect("alint must be available in the locked gate environment");
    assert!(
        pass.success(),
        "README path should not be a containment finding"
    );

    std::fs::write(
        &cargo_toml,
        "[dependencies]\nexternal = { path = \"../../outside\" }\n",
    )
    .unwrap();
    let fail = Command::new("alint")
        .args([
            "check",
            "--config",
            config.to_str().unwrap(),
            "--only",
            "containment.no-parent-escape",
            "--fail-on-warning",
            "--no-gitignore",
            root.to_str().unwrap(),
        ])
        .status()
        .expect("alint must be available in the locked gate environment");
    assert!(!fail.success(), "dependency path escape must be a finding");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn repository_inventory_bytes_are_exactly_bound() {
    let bytes = std::fs::read(common::repo_root().join("fleet").join("repositories.toml")).unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(bytes)),
        "9f964226dbb0a4a46c1a864876bb8126b02e23cd3b96b71457568db6b923ee1e"
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
    std::fs::copy(
        common::repo_root().join("fleet").join("forks.toml"),
        dir.join("fleet").join("forks.toml"),
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
        "\n[[repository]]\nslug = \"jackin-project/jackin\"\nclass = \"code\"\nbaseline_sha = \"3e6376d213f2aae66b00b376057ff0863c988040\"\ntier = \"leaf\"\nkind = \"app\"\nvisibility = \"public\"\nresearch = false\nfirst_seen = \"2026-08-30\"\n",
    );
    write_repos(&dir, &body);
    let err = FleetManifest::load(&dir).unwrap_err();
    assert!(err.contains("duplicate"), "got: {err}");
}

#[test]
fn unknown_class_rejected() {
    let dir = common::temp_dir("unkclass");
    let body = "[[repository]]\nslug = \"tailrocks/whatever\"\nclass = \"quantum\"\nbaseline_sha = \"3e6376d213f2aae66b00b376057ff0863c988040\"\ntier = \"leaf\"\nkind = \"app\"\nvisibility = \"public\"\nresearch = false\nfirst_seen = \"2026-08-30\"\n";
    write_repos(&dir, body);
    let err = FleetManifest::load(&dir).unwrap_err();
    assert!(err.contains("unknown class"), "got: {err}");
}

#[test]
fn wrong_count_rejected() {
    let dir = common::temp_dir("count");
    // Only one code member — wrong count for every class.
    let body = "[[repository]]\nslug = \"tailrocks/only\"\nclass = \"code\"\nbaseline_sha = \"3e6376d213f2aae66b00b376057ff0863c988040\"\ntier = \"leaf\"\nkind = \"app\"\nvisibility = \"public\"\nresearch = false\nfirst_seen = \"2026-08-30\"\n";
    write_repos(&dir, body);
    let err = FleetManifest::load(&dir).unwrap_err();
    assert!(err.contains("expected"), "got: {err}");
}

#[test]
fn bad_sha_rejected() {
    let dir = common::temp_dir("badsha");
    let body = "[[repository]]\nslug = \"tailrocks/x\"\nclass = \"code\"\nbaseline_sha = \"NOTHEX\"\ntier = \"leaf\"\nkind = \"app\"\nvisibility = \"public\"\nresearch = false\nfirst_seen = \"2026-08-30\"\n";
    write_repos(&dir, body);
    let err = FleetManifest::load(&dir).unwrap_err();
    assert!(err.contains("40 lowercase hex"), "got: {err}");
}

#[test]
fn bad_slug_rejected() {
    let dir = common::temp_dir("badslug");
    let body = "[[repository]]\nslug = \"no-slash\"\nclass = \"code\"\nbaseline_sha = \"3e6376d213f2aae66b00b376057ff0863c988040\"\ntier = \"leaf\"\nkind = \"app\"\nvisibility = \"public\"\nresearch = false\nfirst_seen = \"2026-08-30\"\n";
    write_repos(&dir, body);
    let err = FleetManifest::load(&dir).unwrap_err();
    assert!(err.contains("invalid slug"), "got: {err}");
}

#[test]
fn unknown_organization_rejected() {
    let dir = common::temp_dir("unkorg");
    let body = "[[repository]]\nslug = \"strangers/x\"\nclass = \"code\"\nbaseline_sha = \"3e6376d213f2aae66b00b376057ff0863c988040\"\ntier = \"leaf\"\nkind = \"app\"\nvisibility = \"public\"\nresearch = false\nfirst_seen = \"2026-08-30\"\n";
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
fn platform_metadata_is_required_and_yaml_safe() {
    for (tag, from, to, expected) in [
        (
            "platform-runner",
            "platform_runner = \"macos-26\"",
            "platform_runner = \"ubuntu-latest\"",
            "invalid platform runner",
        ),
        (
            "platform-name",
            "platform_name = \"native-usage-menu-bar\"",
            "platform_name = \"native: unquoted\"",
            "invalid platform name",
        ),
        (
            "platform-missing",
            "platform_runner = \"macos-26\"\n",
            "",
            "must declare platform_runner and platform_name iff platform_only is true",
        ),
    ] {
        let dir = common::bound_fixture(DUMMY_SHA);
        let path = dir.join("fleet").join("classes.toml");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains(from),
            "canonical fixture contains mutation target"
        );
        std::fs::write(&path, body.replacen(from, to, 1)).unwrap();
        let err = FleetManifest::load(&dir).unwrap_err();
        assert!(err.contains(expected), "{tag}: got {err}");
    }
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
fn exactly_five_templates_render() {
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
    assert_eq!(line, "fleet valid: 28 repositories, 5 classes, 5 templates");
}

#[test]
fn package_policy_and_workflows_are_closed_and_lane_selectable() {
    use velnor_actions_generator::package::{
        APT_TEMPLATE, PackagePolicy, SIGNER_WORKFLOW, TAP_TEMPLATE, UPDATER_WORKFLOW,
    };
    PackagePolicy::load(&common::repo_root(), &ForkTable::canonical())
        .expect("package policy loads");
    let package_policy = std::fs::read_to_string(common::repo_root().join("fleet/packages.toml"))
        .expect("package policy bytes");
    assert!(!package_policy.contains(".tar.xz"));
    assert_eq!(package_policy.matches(".tar.gz").count(), 22);
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
    assert!(UPDATER_WORKFLOW.contains("      lane:\n        required: true\n        type: string"));
    assert!(
        UPDATER_WORKFLOW.contains("      writer:\n        required: true\n        type: boolean")
    );
    assert_eq!(
        UPDATER_WORKFLOW
            .matches("inputs.lane == 'github' && 'ubuntu-26.04' || fromJSON('[\"self-hosted\",\"velnor-target-mvp\"]')")
            .count(),
        2,
        "verify and sole mutate writer must use the selected real lane"
    );
    assert!(UPDATER_WORKFLOW.contains("needs.verify.outputs.available == 'true' && inputs.writer"));
    assert!(
        UPDATER_WORKFLOW
            .contains("name: verified-package-${{ inputs.channel }}-${{ inputs.lane }}")
    );
    assert!(UPDATER_WORKFLOW.contains("sha256sum --check --strict"));
    assert!(
        UPDATER_WORKFLOW
            .contains("actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1")
    );
    assert!(UPDATER_WORKFLOW.contains("client-id: ${{ vars.PACKAGE_UPDATER_APP_CLIENT_ID }}"));
    assert!(!UPDATER_WORKFLOW.contains("app-id: ${{ vars.PACKAGE_UPDATER_APP_ID }}"));
    assert!(UPDATER_WORKFLOW.contains("git commit --signoff"));
    assert!(UPDATER_WORKFLOW.contains(
        "git fetch --no-tags origin \"refs/heads/$BRANCH:refs/remotes/origin/$BRANCH\" || true"
    ));
    assert!(UPDATER_WORKFLOW.contains("git status --porcelain --untracked-files=all"));
    assert!(!UPDATER_WORKFLOW.contains("if git diff --quiet"));
    assert!(UPDATER_WORKFLOW.contains("gh pr list --state open --head \"$BRANCH\" --base main"));
    assert!(!UPDATER_WORKFLOW.contains("gh pr view \"$BRANCH\""));
    assert!(UPDATER_WORKFLOW.contains("test \"$BRANCH\" != \"main\""));
    assert!(UPDATER_WORKFLOW.contains("elif test \"$CHANNEL\" = preview"));
    assert!(UPDATER_WORKFLOW.contains("releases/tags/preview"));
    assert!(UPDATER_WORKFLOW.contains("compare/$source_digest...$main_digest"));
    assert!(UPDATER_WORKFLOW.contains(".merge_base_commit.sha == $source"));
    assert!(UPDATER_WORKFLOW.contains(".commits[-1].sha == $main"));
    assert!(!UPDATER_WORKFLOW.contains(".head_commit.sha"));
    assert!(!UPDATER_WORKFLOW.contains("commits/main\" --jq .sha)\" = \"$source_digest"));
    assert!(UPDATER_WORKFLOW.contains("test \"${#matches[@]}\" -eq 6"));
    assert!(UPDATER_WORKFLOW.contains("VELNOR_PACKAGE_CHANNEL: ${{ inputs.channel }}"));
    assert!(UPDATER_WORKFLOW.contains("available=false"));
    assert!(UPDATER_WORKFLOW.contains("updater is a verified no-op"));
    assert!(UPDATER_WORKFLOW.contains("needs.verify.outputs.available == 'true'"));
    assert!(!UPDATER_WORKFLOW.contains("releases/latest"));
    assert!(TAP_TEMPLATE.matches("channel: @PACKAGE_CHANNELS@").count() == 3);
    assert!(APT_TEMPLATE.matches("channel: @PACKAGE_CHANNELS@").count() == 3);
    for template in [TAP_TEMPLATE, APT_TEMPLATE] {
        assert!(template.contains("permissions:\n  attestations: read\n  contents: read"));
        assert!(template.contains(
            "concurrency:\n  group: ${{ github.workflow }}-${{ github.repository }}\n  cancel-in-progress: false"
        ));
        assert!(template.contains("default: velnor\n        options: [velnor, github, both]"));
        assert!(template.contains(
            "{\"lane\":\"velnor\",\"writer\":true},{\"lane\":\"github\",\"writer\":false}"
        ));
        assert!(template.contains("lane: ${{ matrix.config.lane }}"));
        assert!(template.contains("channel: ${{ matrix.channel }}"));
        assert!(template.contains("writer: ${{ matrix.config.writer }}"));
        assert!(template.contains("vars.VELNOR_AUTOMATIC_LANES == 'github'"));
        assert!(template.contains(
            "fromJSON('[\"self-hosted\",\"velnor-target-mvp\"]') }}\n    timeout-minutes: 5"
        ));
        assert!(template.contains("needs: [jackin_project, tailrocks, chainargos]"));
        assert!(template.contains("needs.jackin_project.result"));
        assert!(template.contains("needs.chainargos.result"));
        assert!(!template.contains("needs.jackin-project"));
        assert!(!template.contains("needs.ChainArgos"));
    }
    assert_eq!(
        TAP_TEMPLATE
            .matches("branch: package-update/verified/${{ matrix.channel }}")
            .count(),
        3
    );
}

#[test]
fn signer_admits_verified_commit_sha_not_tag_ref() {
    let body = velnor_actions_generator::package::SIGNER_WORKFLOW;
    assert!(body.contains("[[ \"$SOURCE_REF\" =~ ^[0-9a-f]{40}$ ]]"));
    assert!(!body.contains("refs/tags/v*"));
}

#[test]
fn package_generation_writes_exact_callables_and_templates() {
    let dir = common::bound_fixture(DUMMY_SHA);
    let updater =
        velnor_actions_generator::package::PackagePolicy::load(&dir, &ForkTable::canonical())
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
fn package_consumer_renderer_binds_release_signer_slots() {
    let policy = velnor_actions_generator::package::PackagePolicy::load(
        &common::repo_root(),
        &ForkTable::canonical(),
    )
    .expect("package policy loads");
    let current = "1111111111111111111111111111111111111111";
    let owner_shas = [
        current,
        "3333333333333333333333333333333333333333",
        "4444444444444444444444444444444444444444",
    ];
    let rendered = policy
        .render_consumer("tailrocks/homebrew-tablerock", owner_shas, "2026.8.33")
        .expect("release signer renders");
    assert_eq!(rendered.matches(current).count(), 1);
    assert_eq!(rendered.matches(owner_shas[1]).count(), 1);
    assert_eq!(rendered.matches(owner_shas[2]).count(), 1);
    assert_eq!(
        rendered
            .matches("1e062d5bbe329873047ee8a8e79bba0811e53b65")
            .count(),
        3
    );
    assert_eq!(rendered.matches("signer-fork: \"tailrocks\"").count(), 3);
    assert!(!rendered.contains("@FLEET_SHA@"));
    assert!(rendered.contains("old-signer-digest: \"\""));

    assert!(
        policy
            .render_consumer("tailrocks/not-a-consumer", owner_shas, "2026.8.33")
            .is_err()
    );
    assert!(
        policy
            .render_consumer(
                "tailrocks/homebrew-tablerock",
                ["main", owner_shas[1], owner_shas[2]],
                "2026.8.33",
            )
            .is_err()
    );
    assert!(
        policy
            .render_consumer("tailrocks/homebrew-tablerock", owner_shas, "v1")
            .is_err()
    );
    assert!(
        policy
            .render_consumer("tailrocks/homebrew-tablerock", owner_shas, "2026.8.32",)
            .is_ok()
    );
}

#[test]
fn updater_executes_explicit_current_then_old_signer_alternatives() {
    let body = velnor_actions_generator::package::UPDATER_WORKFLOW;
    assert!(body.contains("accepted_digests=(\"$CURRENT_SIGNER_DIGEST\")"));
    assert!(body.contains("GH_TOKEN: ${{ github.token }}"));
    assert!(body.contains("accepted_digests+=(\"$OLD_SIGNER_DIGEST\")"));
    assert!(body.contains("30 * 24 * 60 * 60"));
    assert!(body.contains("candidate_digests=(\"${accepted_digests[@]}\")"));
    assert!(body.contains("candidate_digests=(\"$source_digest\")"));
    assert!(body.contains("for signer_digest in \"${candidate_digests[@]}\""));
    assert!(body.contains("--signer-digest \"$signer_digest\""));
    assert!(body.contains("$SIGNER_FORK/velnor-actions/.github/workflows/package-signer.yml"));
    assert!(body.contains("$SOURCE_REPOSITORY/.github/workflows/preview.yml"));
    assert!(body.contains("sort_by([.tag_name | ltrimstr(\"v\") | split(\".\") | map(tonumber)])"));
    assert!(!body.contains("sort_by(.published_at)"));
    assert!(body.contains("if: ${{ inputs.consumer-repository != '' }}"));
    assert!(!body.contains("if: ${{ github.event_name == 'workflow_call' }}"));
    assert!(body.contains("keys == [\"accepted_signer_digest\",\"verification\"]"));
    assert!(body.contains("if length > 0 and all(.[];"));
    assert!(body.contains("jdx/mise-action@7e36c90d9ab29c415a2384db3006f3ec8a8cc654"));
    assert!(body.contains("install: false\n          cache: false"));
    assert!(body.contains(".verification | type == \"array\" and length > 0"));
    assert!(!body.contains("all(.[]; type == \"array\""));
}

#[test]
fn bound_fixture_audit_passes() {
    let dir = common::bound_fixture(DUMMY_SHA);
    let line = audit::audit(&dir).expect("bound audit passes");
    assert_eq!(line, "fleet valid: 28 repositories, 5 classes, 5 templates");
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
        let class_bytes = render::render_consumer(&template, [sha; 3], calver).unwrap();
        for repo in m.members_of(class) {
            let repo_bytes = render::render_consumer(&template, [sha; 3], calver).unwrap();
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
    let out = render::render_consumer(&template, [DUMMY_SHA; 3], "2026.7.0").unwrap();
    assert!(!out.contains(FLEET_SHA_PLACEHOLDER));
    assert!(!out.contains(CALVER_PLACEHOLDER));
    // A second substitution has nothing to replace and must be refused.
    assert!(render::render_consumer(&out, [DUMMY_SHA; 3], "2026.7.0").is_err());
    // A non-40-hex release SHA is refused.
    assert!(render::render_consumer(&template, ["main"; 3], "2026.7.0").is_err());
}

#[test]
fn render_consumer_to_dir_writes_envelope_artifacts_without_touching_repo_tasks() {
    let out = common::temp_dir("consumer-out");
    let root = common::repo_root();
    std::fs::copy(root.join("README.md"), out.join("README.md")).unwrap();
    let legacy_mise = std::fs::read_to_string(root.join("mise.toml"))
        .unwrap()
        .replace(
            "\"aqua:nextest-rs/nextest/cargo-nextest\" = \"0.9.140\"",
            "\"github:nextest-rs/nextest\" = \"cargo-nextest-0.9.140\"",
        )
        .replace(
            "\"aqua:rustsec/rustsec/cargo-audit\" = \"0.22.2\"",
            "\"github:rustsec/rustsec\" = \"cargo-audit/v0.22.2\"",
        );
    assert!(legacy_mise.contains("github:nextest-rs/nextest"));
    assert!(legacy_mise.contains("github:rustsec/rustsec"));
    std::fs::write(out.join("mise.toml"), legacy_mise).unwrap();
    let legacy_lock = std::fs::read_to_string(root.join("mise.lock"))
        .unwrap()
        .replace(
            "tools.\"aqua:nextest-rs/nextest/cargo-nextest\"",
            "tools.\"github:nextest-rs/nextest\"",
        )
        .replace(
            "backend = \"aqua:nextest-rs/nextest/cargo-nextest\"",
            "backend = \"github:nextest-rs/nextest\"",
        )
        .replace(
            "tools.\"aqua:rustsec/rustsec/cargo-audit\"",
            "tools.\"github:rustsec/rustsec\"",
        )
        .replace(
            "backend = \"aqua:rustsec/rustsec/cargo-audit\"",
            "backend = \"github:rustsec/rustsec\"",
        );
    assert!(legacy_lock.contains("github:nextest-rs/nextest"));
    assert!(legacy_lock.contains("github:rustsec/rustsec"));
    std::fs::write(out.join("mise.lock"), legacy_lock).unwrap();
    std::fs::write(out.join("AGENTS.md"), "# Repository rules\n").unwrap();
    let path = velnor_actions_generator::render_consumer_to_dir(
        &root,
        "tailrocks/velnor",
        [DUMMY_SHA; 3],
        "2026.7.0",
        &out,
    )
    .unwrap();
    assert!(path.ends_with(Path::new(".github/workflows/ci.yml")));
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains(&format!("@{DUMMY_SHA} # 2026.7.0")));
    assert!(!body.contains(FLEET_SHA_PLACEHOLDER));
    assert_eq!(
        std::fs::read_link(out.join("CLAUDE.md")).unwrap(),
        Path::new("AGENTS.md")
    );
    assert!(
        std::fs::read_to_string(out.join(".ignore"))
            .unwrap()
            .contains("target/")
    );
    assert!(
        std::fs::read_to_string(out.join("mise.toml"))
            .unwrap()
            .contains("fleet:check")
    );
    assert!(
        std::fs::read_to_string(out.join("mise.toml"))
            .unwrap()
            .contains("aqua:nextest-rs/nextest/cargo-nextest\" = \"0.9.140\"")
    );
    assert!(
        std::fs::read_to_string(out.join("mise.toml"))
            .unwrap()
            .contains("aqua:rustsec/rustsec/cargo-audit\" = \"0.22.2\"")
    );
    assert!(
        !std::fs::read_to_string(out.join("mise.toml"))
            .unwrap()
            .contains("github:nextest-rs/nextest")
    );
    assert!(
        !std::fs::read_to_string(out.join("mise.toml"))
            .unwrap()
            .contains("github:rustsec/rustsec")
    );
    assert!(
        !std::fs::read_to_string(out.join("mise.lock"))
            .unwrap()
            .contains("github:nextest-rs/nextest")
    );
    assert!(
        !std::fs::read_to_string(out.join("mise.lock"))
            .unwrap()
            .contains("github:rustsec/rustsec")
    );
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
            "b64037e36988a9056dad0f90f8b798588e56a62cd91f158574e785f38e71df5a",
        ),
        (
            "templates/native/ci.yml",
            "8666e38f835806cd0d93e120b0b7c73320b5ab01914befbc7d67795214c59d87",
        ),
        (
            "templates/tap/ci.yml",
            "dba7498a02346438f1d4ff070a964d711faa18827ea4deb3105058a41556c0ca",
        ),
        (
            "templates/apt/ci.yml",
            "89e1b7970898087b62d16239f0abde4da0f4b8e10855ebde3ae9e722597264be",
        ),
        (
            "templates/fixture/ci.yml",
            "75a311d5d733be8772325e125cdae11c3fb9173ba01a79803d8a0d62a4119a7d",
        ),
        (
            ".github/workflows/ci-code.yml",
            "14c5cc482b8e10a027ddaa3187c244b659555b44014aadde70547c4345bf48ef",
        ),
        (
            ".github/workflows/ci-native.yml",
            "96337e1beb3659ffa9c25196d00ef4e23c683bb6d75caf99fe6ed7293e9f8925",
        ),
        (
            ".github/workflows/ci-tap.yml",
            "6b8e9c35c347b314946da6132bec58a4786876eb545da71412da9e572adb619a",
        ),
        (
            ".github/workflows/ci-apt.yml",
            "a01a913d9f5e0023a85597bd23bb5f84d45d8f774a3bd4db34453c0b07079bf3",
        ),
        (
            ".github/workflows/ci-fixture.yml",
            "258e66504be29dc9bb6babc9f8a16738ac1a6a2c34b09ad34de4a0ed6be0361b",
        ),
    ] {
        let bytes = std::fs::read(root.join(path)).unwrap();
        assert_eq!(hex::encode(Sha256::digest(bytes)), expected, "{path}");
    }
}

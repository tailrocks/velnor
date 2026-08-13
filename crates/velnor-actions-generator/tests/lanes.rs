//! Lane-mode tests: selector, independence, platform-only applicability, and
//! public-PR routing.

mod common;

use sha2::Digest;
use velnor_actions_generator::RepositoryClass;
use velnor_actions_generator::cache::CacheContract;
use velnor_actions_generator::model::{Applicability, FleetManifest, Lane, resolve_lanes};
use velnor_actions_generator::render;

const DUMMY_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

fn callable(class: RepositoryClass) -> String {
    let root = common::repo_root();
    let manifest = FleetManifest::load(&root).unwrap();
    let caches = CacheContract::load(&root.join("fleet").join("caches.toml")).unwrap();
    render::callable_workflow(manifest.class(class), &caches, DUMMY_SHA)
}

#[test]
fn lane_selector_accepts_only_three_values() {
    assert_eq!(Lane::from_token("velnor"), Some(Lane::Velnor));
    assert_eq!(Lane::from_token("github"), Some(Lane::GitHub));
    assert_eq!(Lane::from_token("both"), Some(Lane::Both));
    assert_eq!(Lane::from_token("VELNOR"), None);
    assert_eq!(Lane::from_token("nightly"), None);
    assert_eq!(Lane::from_token(""), None);
}

#[test]
fn resolve_lanes_expands_both_independently() {
    assert_eq!(resolve_lanes(Lane::Velnor), vec![Lane::Velnor]);
    assert_eq!(resolve_lanes(Lane::GitHub), vec![Lane::GitHub]);
    assert_eq!(resolve_lanes(Lane::Both), vec![Lane::Velnor, Lane::GitHub]);
}

#[test]
fn omitted_lane_uses_exact_organization_default() {
    let t = render::consumer_template(RepositoryClass::Code);
    assert!(t.contains("default: \"\""));
    assert!(t.contains("github.repository_owner == 'jackin-project' && 'github'"));
    assert!(t.contains("github.repository_owner == 'tailrocks'"));
    assert!(t.contains("github.repository_owner == 'ChainArgos'"));
    assert!(t.contains("&& 'velnor'"));
    assert!(t.contains("|| 'invalid'"), "unknown owner fails closed");
}

#[test]
fn tap_has_explicit_platform_only_gate() {
    let m = FleetManifest::load(&common::repo_root()).unwrap();
    let tap = m.class(RepositoryClass::Tap);
    assert!(tap.platform_only, "tap declares a platform-only gate");
    let test_gate = tap.gates.iter().find(|g| g.name == "test").unwrap();
    assert_eq!(test_gate.applicability, Applicability::Github);
    // Velnor lane excludes the platform-only gate; GitHub lane includes it.
    let velnor: Vec<_> = tap
        .applicable_gates(Lane::Velnor)
        .iter()
        .map(|g| g.name.clone())
        .collect();
    let github: Vec<_> = tap
        .applicable_gates(Lane::GitHub)
        .iter()
        .map(|g| g.name.clone())
        .collect();
    assert!(
        !velnor.contains(&"test".to_string()),
        "velnor lane omits platform-only test"
    );
    assert!(
        github.contains(&"test".to_string()),
        "github lane runs platform-only test"
    );
}

#[test]
fn code_standard_project_command_is_mise_run_ci() {
    assert_eq!(
        velnor_actions_generator::model::CODE_STANDARD_COMMAND,
        "mise run ci"
    );
}

#[test]
fn code_class_is_lane_portable() {
    let m = FleetManifest::load(&common::repo_root()).unwrap();
    let code = m.class(RepositoryClass::Code);
    assert!(!code.platform_only);
    assert_eq!(code.applicable_gates(Lane::Velnor).len(), 5);
    assert_eq!(code.applicable_gates(Lane::GitHub).len(), 5);
    for gate in &code.gates {
        assert_eq!(gate.applicability, Applicability::Both);
        assert!(!gate.command.trim().is_empty(), "gate command non-empty");
    }
}

#[test]
fn public_unmerged_routes_velnor_lane_to_github_hosted() {
    let wf = callable(RepositoryClass::Code);
    assert!(
        wf.contains(
            "runs-on: ${{ (github.event_name == 'pull_request' || github.event_name == 'merge_group') && 'ubuntu-latest' || 'velnor-trusted' }}"
        ),
        "velnor lane routes public unmerged code to GitHub-hosted"
    );
}

#[test]
fn both_lanes_are_independent_and_neither_substitutes() {
    let wf = callable(RepositoryClass::Code);
    // Two separately named lane jobs; both required in `both`.
    assert!(wf.contains("velnor-lane:"));
    assert!(wf.contains("github-lane:"));
    // The `both` branch requires success on BOTH lanes; no lane substitution.
    assert!(wf.contains("one lane never substitutes for the other"));
    // Skipped or failed selected lanes are never credited as success.
    assert!(wf.contains("is never"));
}

#[test]
fn same_gate_semantics_on_both_lanes_for_portable_gates() {
    let wf = callable(RepositoryClass::Code);
    // The install/build/test/lint/format gate commands appear on both lanes.
    for cmd in [
        "mise install --locked",
        "mise run ci",
        "mise run test",
        "mise run lint",
        "mise run fmt",
    ] {
        assert_eq!(
            wf.matches(&format!("command: {cmd}")).count(),
            4,
            "{cmd} on both ordinary and proof lanes"
        );
    }
}

#[test]
fn optional_operation_interface_is_complete_and_non_selector() {
    let template = render::consumer_template(RepositoryClass::Code);
    for input in [
        "recovery_proof_id",
        "benchmark_campaign",
        "benchmark_generation",
        "benchmark_cache_id",
        "benchmark_cache_mode",
        "benchmark_fanout",
        "benchmark_wave",
        "benchmark_reservation",
        "cache_proof_id",
        "cache_generation",
        "cache_temperature",
    ] {
        assert!(template.contains(&format!("{input}:")), "missing {input}");
    }
    assert_eq!(
        template.matches("\n      lane:").count(),
        4,
        "one root input plus three forwarded lane values"
    );
    assert!(template.contains("cancel-in-progress: ${{ inputs.benchmark_campaign == '' }}"));
}

#[test]
fn benchmark_waves_on_same_ref_do_not_cancel_each_other() {
    let template = render::consumer_template(RepositoryClass::Code);
    let documents = yaml_rust2::YamlLoader::load_from_str(&template).unwrap();
    let concurrency = &documents[0]["concurrency"];
    let group = concurrency["group"].as_str().unwrap();
    let cancel = concurrency["cancel-in-progress"].as_str().unwrap();
    assert!(group.contains("inputs.benchmark_campaign"));
    assert!(group.contains("inputs.benchmark_wave"));
    assert!(group.contains("github.workflow"));
    assert_eq!(cancel, "${{ inputs.benchmark_campaign == '' }}");
}

#[test]
fn tap_platform_gate_is_required_on_macos_independent_of_lane() {
    let workflow = callable(RepositoryClass::Tap);
    assert!(workflow.contains("platform-lane:"));
    assert!(workflow.contains("runs-on: macos-latest"));
    assert!(workflow.contains("PLATFORM_REQUIRED: true"));
    assert!(workflow.contains("- platform-lane"));
    assert_eq!(workflow.matches("command: mise run test").count(), 1);
}

fn run_request_validator(overrides: &[(&str, &str)]) -> bool {
    request_validator_result(overrides).0
}

fn request_validator_result(overrides: &[(&str, &str)]) -> (bool, String) {
    use sha2::{Digest, Sha256};
    let output = common::temp_dir("request-validator").join("github-output");
    let mut command = std::process::Command::new("bash");
    command.arg("-c").arg(render::VALIDATE_REQUEST_SCRIPT);
    for key in [
        "RECOVERY_PROOF_ID",
        "BENCHMARK_CAMPAIGN",
        "BENCHMARK_GENERATION",
        "BENCHMARK_CACHE_ID",
        "BENCHMARK_CACHE_MODE",
        "BENCHMARK_FANOUT",
        "BENCHMARK_WAVE",
        "BENCHMARK_RESERVATION",
        "CACHE_PROOF_ID",
        "CACHE_GENERATION",
        "CACHE_TEMPERATURE",
    ] {
        command.env(key, "");
    }
    for (key, value) in [
        ("EVENT_NAME", "push"),
        ("REF_TYPE", "branch"),
        ("REF_PROTECTED", "false"),
        ("REF_NAME", ""),
        ("HEAD_SHA", DUMMY_SHA),
        ("WORKFLOW_SHA", DUMMY_SHA),
        ("DECLARED_CACHE_IDS", "tools,dependencies,build-output"),
        ("RUN_ID", "123"),
        ("REPOSITORY", "tailrocks/example"),
        ("GH_TOKEN", "test-token"),
    ] {
        command.env(key, value);
    }
    command.env(
        "EXPECTED_CALLER_TEMPLATE_SHA256",
        hex::encode(Sha256::digest(
            render::consumer_template(RepositoryClass::Code).as_bytes(),
        )),
    );
    for (key, value) in overrides {
        command.env(key, value);
    }
    command.env("GITHUB_OUTPUT", &output);
    let success = command.status().unwrap().success();
    let emitted = std::fs::read_to_string(output).unwrap_or_default();
    (success, emitted)
}

#[test]
fn optional_operations_fail_closed_before_project_commands() {
    assert!(run_request_validator(&[]));
    assert!(!run_request_validator(&[("BENCHMARK_FANOUT", "8")]));
    assert!(!run_request_validator(&[
        ("BENCHMARK_CAMPAIGN", "campaign-0001"),
        ("BENCHMARK_GENERATION", "campaign/code/github/core/01"),
        ("BENCHMARK_CACHE_ID", "unknown"),
        ("BENCHMARK_CACHE_MODE", "enabled"),
        ("BENCHMARK_FANOUT", "8"),
        ("BENCHMARK_WAVE", "wave-0001"),
        ("BENCHMARK_RESERVATION", "reservation-0001"),
    ]));
    let bin = fake_recovery_gh();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    assert!(run_request_validator(&[
        ("PATH", path.as_str()),
        ("EVENT_NAME", "workflow_dispatch"),
        ("REF_TYPE", "tag"),
        ("REF_PROTECTED", "true"),
        ("REF_NAME", "2026.7.0"),
        ("BENCHMARK_CAMPAIGN", "campaign-0001"),
        ("BENCHMARK_GENERATION", "campaign/code/github/core/01"),
        ("BENCHMARK_CACHE_ID", "tools"),
        ("BENCHMARK_CACHE_MODE", "enabled"),
        ("BENCHMARK_FANOUT", "8"),
        ("BENCHMARK_WAVE", "wave-0001"),
        ("BENCHMARK_RESERVATION", "reservation-0001"),
    ]));
    assert!(!run_request_validator(&[
        ("EVENT_NAME", "workflow_dispatch"),
        ("RECOVERY_PROOF_ID", "recovery-deadbeef-operation"),
    ]));
}

fn fake_recovery_gh() -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let root = common::temp_dir("fake-recovery-gh");
    let executable = root.join("gh");
    let workflow = render::render_consumer(
        &render::consumer_template(RepositoryClass::Code),
        [DUMMY_SHA; 3],
        "2026.7.0",
    )
    .unwrap();
    std::fs::write(root.join("workflow.yml"), workflow).unwrap();
    std::fs::write(
        &executable,
        r#"#!/usr/bin/env bash
set -euo pipefail
script_dir="$(cd "$(dirname "$0")" && pwd)"
url=""
raw=false
for argument in "$@"; do
  [[ "${argument}" == repos/* ]] && url="${argument}"
  [[ "${argument}" == 'Accept: application/vnd.github.raw+json' ]] && raw=true
done
if [[ "${url}" == *'/pulls?'* ]]; then
  if [[ "${FAKE_PR_COUNT-1}" == 1 ]]; then printf '[[{"head":{"sha":"__HEAD__"}}],[]]\n'; else printf '[[],[]]\n'; fi
elif [[ "${url}" == *'/actions/runs/123' ]]; then printf '%s\n' "${FAKE_RUN_MATCH-true}"
elif [[ "${url}" == *'/actions/runs?'* ]]; then
  if [[ "${FAKE_DUPLICATE_COUNT-1}" == 1 ]]; then printf '[{"workflow_runs":[{"display_title":"CI recovery recovery-operation-0001","head_sha":"__HEAD__"}]},{"workflow_runs":[]}]\n'; else printf '[{"workflow_runs":[{"display_title":"CI recovery recovery-operation-0001","head_sha":"__HEAD__"},{"display_title":"CI recovery recovery-operation-0001","head_sha":"__HEAD__"}]}]\n'; fi
elif [[ "${url}" == *'/git/ref/tags/2026.7.0' ]]; then printf '%s\t%s\n' "${FAKE_TAG_TYPE-commit}" "${FAKE_TAG_SHA-__HEAD__}"
elif [[ "${url}" == *'/contents/.github/workflows/ci.yml?'* ]]; then
  if ${raw}; then
    cat "${script_dir}/workflow.yml"
    [[ -z "${FAKE_WORKFLOW_SUFFIX-}" ]] || printf '%s' "${FAKE_WORKFLOW_SUFFIX}"
  elif [[ -n "${FAKE_BLOB-}" ]]; then
    printf '%s\n' "${FAKE_BLOB}"
  else
    suffix="${FAKE_WORKFLOW_SUFFIX-}"; size="$(wc -c < "${script_dir}/workflow.yml" | tr -d ' ')"; size=$((size + ${#suffix}))
    { printf 'blob %s\0' "${size}"; cat "${script_dir}/workflow.yml"; printf '%s' "${suffix}"; } | sha1sum | cut -d' ' -f1
  fi
else exit 64
fi
"#.replace("__HEAD__", DUMMY_SHA),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&executable, permissions).unwrap();
    root
}

#[test]
fn recovery_marker_binds_allocated_run_unique_pr_head_and_duplicate_history() {
    let bin = fake_recovery_gh();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let valid = [
        ("PATH", path.as_str()),
        ("EVENT_NAME", "workflow_dispatch"),
        ("REF_TYPE", "branch"),
        ("RECOVERY_PROOF_ID", "recovery-operation-0001"),
    ];
    assert!(run_request_validator(&valid));

    let duplicate = [
        ("PATH", path.as_str()),
        ("EVENT_NAME", "workflow_dispatch"),
        ("REF_TYPE", "branch"),
        ("RECOVERY_PROOF_ID", "recovery-operation-0001"),
        ("FAKE_DUPLICATE_COUNT", "2"),
    ];
    assert!(!run_request_validator(&duplicate));

    let wrong_run = [
        ("PATH", path.as_str()),
        ("EVENT_NAME", "workflow_dispatch"),
        ("REF_TYPE", "branch"),
        ("RECOVERY_PROOF_ID", "recovery-operation-0001"),
        ("FAKE_RUN_MATCH", "false"),
    ];
    assert!(!run_request_validator(&wrong_run));
}

#[test]
fn benchmark_dispatch_rejects_tag_target_or_blob_mismatch() {
    let bin = fake_recovery_gh();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let common = [
        ("PATH", path.as_str()),
        ("EVENT_NAME", "workflow_dispatch"),
        ("REF_TYPE", "tag"),
        ("REF_PROTECTED", "true"),
        ("REF_NAME", "2026.7.0"),
        ("BENCHMARK_CAMPAIGN", "campaign-0001"),
        ("BENCHMARK_GENERATION", "campaign/code/github/core/01"),
        ("BENCHMARK_CACHE_ID", "tools"),
        ("BENCHMARK_CACHE_MODE", "enabled"),
        ("BENCHMARK_FANOUT", "1"),
        ("BENCHMARK_WAVE", "wave-0001"),
        ("BENCHMARK_RESERVATION", "reservation-0001"),
    ];
    assert!(run_request_validator(&common));
    let (success, emitted) = request_validator_result(&common);
    assert!(success);
    let expected_generation = hex::encode(sha2::Sha256::digest(b"campaign/code/github/core/01"));
    assert!(emitted.contains(&format!(
        "cache_namespace=benchmark-v1/{expected_generation}\n"
    )));
    assert_eq!(request_validator_result(&common), (true, emitted.clone()));
    let mut next_generation = common.to_vec();
    next_generation.push(("BENCHMARK_GENERATION", "campaign/code/github/core/02"));
    let (success, next_emitted) = request_validator_result(&next_generation);
    assert!(success);
    assert_ne!(emitted, next_emitted);
    let normal_core = [
        ("PATH", path.as_str()),
        ("EVENT_NAME", "workflow_dispatch"),
        ("REF_TYPE", "tag"),
        ("REF_PROTECTED", "true"),
        ("REF_NAME", "2026.7.0"),
        ("BENCHMARK_CAMPAIGN", "campaign-0001"),
        ("BENCHMARK_GENERATION", "campaign/code/github/core/01"),
        ("BENCHMARK_CACHE_MODE", "normal"),
        ("BENCHMARK_FANOUT", "1"),
        ("BENCHMARK_WAVE", "wave-0001"),
        ("BENCHMARK_RESERVATION", "reservation-0001"),
    ];
    assert!(run_request_validator(&normal_core));
    let (ordinary_success, ordinary) = request_validator_result(&[]);
    assert!(ordinary_success);
    assert!(ordinary.contains("cache_namespace=ci-v1\n"));
    let cache_generation_two = [
        ("PATH", path.as_str()),
        ("EVENT_NAME", "workflow_dispatch"),
        ("REF_TYPE", "tag"),
        ("REF_PROTECTED", "true"),
        ("REF_NAME", "2026.7.0"),
        ("CACHE_PROOF_ID", "cache-proof-0001"),
        ("CACHE_GENERATION", "2"),
        ("CACHE_TEMPERATURE", "cold"),
    ];
    let (cache_success, cache_output) = request_validator_result(&cache_generation_two);
    assert!(cache_success);
    assert!(cache_output.contains("cache_namespace=ci-v2\n"));
    let mut wrong = common.to_vec();
    wrong.push(("FAKE_TAG_SHA", "cccccccccccccccccccccccccccccccccccccccc"));
    assert!(!run_request_validator(&wrong));
    let mut mutable_name = common.to_vec();
    mutable_name.push(("REF_NAME", "latest"));
    assert!(!run_request_validator(&mutable_name));
    let mut bad_blob = common.to_vec();
    bad_blob.push(("FAKE_BLOB", "not-a-git-blob"));
    assert!(!run_request_validator(&bad_blob));
    let mut spoofed_generated_blob = common.to_vec();
    spoofed_generated_blob.push(("FAKE_WORKFLOW_SUFFIX", "# attacker-controlled valid blob\n"));
    assert!(!run_request_validator(&spoofed_generated_blob));
    let mut benchmark_generation_overflow = common.to_vec();
    benchmark_generation_overflow.push(("BENCHMARK_GENERATION", "2"));
    assert!(!run_request_validator(&benchmark_generation_overflow));
    let mut benchmark_with_cache_proof_field = common.to_vec();
    benchmark_with_cache_proof_field.push(("CACHE_TEMPERATURE", "cold"));
    assert!(!run_request_validator(&benchmark_with_cache_proof_field));
    assert!(!run_request_validator(&[
        ("PATH", path.as_str()),
        ("EVENT_NAME", "workflow_dispatch"),
        ("REF_TYPE", "tag"),
        ("REF_PROTECTED", "true"),
        ("REF_NAME", "2026.7.0"),
        ("CACHE_PROOF_ID", "cache-proof-0001"),
        ("CACHE_GENERATION", "3"),
        ("CACHE_TEMPERATURE", "cold"),
    ]));
}

#[test]
fn operation_input_domains_reject_every_foreign_field() {
    let bin = fake_recovery_gh();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let protected = [
        ("PATH", path.as_str()),
        ("EVENT_NAME", "workflow_dispatch"),
        ("REF_TYPE", "tag"),
        ("REF_PROTECTED", "true"),
        ("REF_NAME", "2026.7.0"),
    ];

    let mut cache_proof = protected.to_vec();
    cache_proof.extend([
        ("CACHE_PROOF_ID", "cache-proof-0001"),
        ("CACHE_GENERATION", "1"),
        ("CACHE_TEMPERATURE", "cold"),
    ]);
    assert!(run_request_validator(&cache_proof));
    for foreign in [
        ("BENCHMARK_CAMPAIGN", "campaign-0001"),
        ("BENCHMARK_GENERATION", "campaign/code/github/core/01"),
        ("BENCHMARK_CACHE_ID", "tools"),
        ("BENCHMARK_CACHE_MODE", "enabled"),
        ("BENCHMARK_FANOUT", "1"),
        ("BENCHMARK_WAVE", "wave-0001"),
        ("BENCHMARK_RESERVATION", "reservation-0001"),
    ] {
        let mut mixed = cache_proof.clone();
        mixed.push(foreign);
        assert!(!run_request_validator(&mixed), "accepted {foreign:?}");
    }

    let recovery = [
        ("PATH", path.as_str()),
        ("EVENT_NAME", "workflow_dispatch"),
        ("REF_TYPE", "branch"),
        ("RECOVERY_PROOF_ID", "recovery-operation-0001"),
    ];
    assert!(run_request_validator(&recovery));
    for foreign in [
        ("BENCHMARK_GENERATION", "campaign/code/github/core/01"),
        ("BENCHMARK_CACHE_ID", "tools"),
        ("BENCHMARK_CACHE_MODE", "enabled"),
        ("BENCHMARK_FANOUT", "1"),
        ("BENCHMARK_WAVE", "wave-0001"),
        ("BENCHMARK_RESERVATION", "reservation-0001"),
        ("CACHE_GENERATION", "1"),
        ("CACHE_TEMPERATURE", "cold"),
    ] {
        let mut mixed = recovery.to_vec();
        mixed.push(foreign);
        assert!(!run_request_validator(&mixed), "accepted {foreign:?}");
    }
}

#[test]
fn parsed_workflows_bind_recovery_permissions_and_unmerged_cache_isolation() {
    let contract = velnor_actions_generator::cache::CacheContract::load(
        &common::repo_root().join("fleet").join("caches.toml"),
    )
    .expect("cache contract");
    let manifest =
        velnor_actions_generator::model::FleetManifest::load(&common::repo_root()).unwrap();
    for class in [
        RepositoryClass::Code,
        RepositoryClass::Tap,
        RepositoryClass::Apt,
        RepositoryClass::Fixture,
    ] {
        let workflow = velnor_actions_generator::render::callable_workflow(
            manifest.class(class),
            &contract,
            DUMMY_SHA,
        );
        let documents = yaml_rust2::YamlLoader::load_from_str(&workflow).expect("valid YAML");
        assert_eq!(documents.len(), 1);
        let root = &documents[0];
        let permissions = &root["jobs"]["validate-request"]["permissions"];
        assert_eq!(permissions["actions"].as_str(), Some("read"));
        assert_eq!(permissions["contents"].as_str(), Some("read"));
        assert_eq!(permissions["pull-requests"].as_str(), Some("read"));

        let rendered = workflow.as_str();
        assert!(rendered.contains("Check out protected base for unmerged cache identity"));
        assert!(rendered.contains("Publish isolated unmerged"));
        assert!(rendered.contains("actions/cache/save@"));
        assert!(!rendered.contains("github.event.pull_request.head.sha"));
        assert!(!rendered.contains("github.event.merge_group.head_sha"));
        assert!(!rendered.contains(
            "if: ${{ github.event_name == 'pull_request' || github.event_name == 'merge_group' }}\n        uses: actions/cache@"
        ));
    }
}

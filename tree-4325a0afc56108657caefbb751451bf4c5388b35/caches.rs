//! Canonical cache declaration, key, prefix, and fail-closed interface tests.

mod common;

use velnor_actions_generator::RepositoryClass;
use velnor_actions_generator::cache::{
    ArtifactClass, CACHE_SCHEMA_VERSION, CacheContract, CacheKeyInputs, build_key_plan,
};

const DIGEST_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const DIGEST_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn canonical_path() -> std::path::PathBuf {
    common::repo_root().join("fleet").join("caches.toml")
}

fn load() -> CacheContract {
    CacheContract::load(&canonical_path()).expect("cache contract loads")
}

fn mutated_file(tag: &str, mutate: impl FnOnce(String) -> String) -> std::path::PathBuf {
    let dir = common::temp_dir(tag);
    let path = dir.join("caches.toml");
    let source = std::fs::read_to_string(canonical_path()).unwrap();
    std::fs::write(&path, mutate(source)).unwrap();
    path
}

#[test]
fn canonical_contract_covers_every_class_and_artifact_kind() {
    let contract = load();
    assert_eq!(CACHE_SCHEMA_VERSION, 1);
    assert_eq!(contract.declaration_sha256().len(), 64);
    for class in velnor_actions_generator::ALL_CLASSES {
        assert!(
            contract
                .declarations()
                .iter()
                .any(|declaration| declaration.class == class),
            "{} has a cache declaration",
            class.code()
        );
    }
    for artifact_class in [
        ArtifactClass::ToolDownload,
        ArtifactClass::Dependency,
        ArtifactClass::BuildOutput,
    ] {
        assert!(
            contract
                .declarations()
                .iter()
                .any(|declaration| declaration.artifact_class == artifact_class)
        );
    }
}

#[test]
fn key_and_restore_prefix_order_are_exact_and_bounded() {
    let contract = load();
    let plan = build_key_plan(
        &contract,
        &CacheKeyInputs {
            class: RepositoryClass::Code,
            cache_id: "dependencies",
            os: "linux",
            arch: "arm64",
            toolchain_digest: DIGEST_A,
            lock_digest: DIGEST_B,
            phase_digest: DIGEST_C,
        },
    )
    .unwrap();
    let root = format!("ci-v1/code/dependencies/linux-arm64/{DIGEST_A}/");
    let lock_root = format!("{root}{DIGEST_B}/");
    assert_eq!(plan.exact, format!("{lock_root}{DIGEST_C}"));
    assert_eq!(plan.restore_prefixes, vec![lock_root]);
    for forbidden in ["commit", "branch", "pull", "run", "attempt", "date", "wave"] {
        assert!(!plan.exact.contains(forbidden));
    }
}

#[test]
fn build_output_never_restores_across_lock_digest() {
    let contract = load();
    let plan = build_key_plan(
        &contract,
        &CacheKeyInputs {
            class: RepositoryClass::Code,
            cache_id: "build-output",
            os: "linux",
            arch: "x64",
            toolchain_digest: DIGEST_A,
            lock_digest: DIGEST_B,
            phase_digest: DIGEST_C,
        },
    )
    .unwrap();
    assert_eq!(plan.restore_prefixes.len(), 1);
    assert!(plan.restore_prefixes[0].contains(DIGEST_B));
}

#[test]
fn undeclared_cache_and_invalid_digest_fail_closed() {
    let contract = load();
    let base = CacheKeyInputs {
        class: RepositoryClass::Code,
        cache_id: "missing",
        os: "linux",
        arch: "x64",
        toolchain_digest: DIGEST_A,
        lock_digest: DIGEST_B,
        phase_digest: DIGEST_C,
    };
    assert!(
        build_key_plan(&contract, &base)
            .unwrap_err()
            .contains("undeclared")
    );
    let invalid = CacheKeyInputs {
        cache_id: "tools",
        lock_digest: "not-a-digest",
        ..base
    };
    assert!(
        build_key_plan(&contract, &invalid)
            .unwrap_err()
            .contains("64 lowercase hex")
    );
}

#[test]
fn contributor_selected_or_escaping_paths_are_rejected() {
    for (tag, replacement) in [
        ("expression", "${{ github.event.inputs.cache_path }}"),
        ("parent", "../host-cache"),
        ("absolute", "/var/cache"),
        ("workspace", "."),
        ("root-star", "*"),
        ("root-globstar", "**"),
        ("dot-star", "./*"),
        ("dot-globstar", "./**"),
        ("recursive-all", "**/*"),
    ] {
        let path = mutated_file(tag, |source| source.replacen(".cache/mise", replacement, 1));
        let error = CacheContract::load(&path).unwrap_err();
        assert!(
            error.contains("unsafe paths") || error.contains("undeclared paths"),
            "{tag}: {error}"
        );
    }
}

#[test]
fn dependency_install_state_never_restores_across_lock_digest() {
    let contract = load();
    let plan = build_key_plan(
        &contract,
        &CacheKeyInputs {
            class: RepositoryClass::Code,
            cache_id: "dependencies",
            os: "linux",
            arch: "x64",
            toolchain_digest: DIGEST_A,
            lock_digest: DIGEST_B,
            phase_digest: DIGEST_C,
        },
    )
    .unwrap();
    assert_eq!(plan.restore_prefixes.len(), 1);
    assert!(plan.restore_prefixes[0].contains(DIGEST_B));
}

#[test]
fn high_cardinality_identity_and_missing_class_are_rejected() {
    let high = mutated_file("high-cardinality", |source| {
        source.replacen("id = \"tools\"", "id = \"run-tools\"", 1)
    });
    assert!(
        CacheContract::load(&high)
            .unwrap_err()
            .contains("high-cardinality")
    );

    let missing = mutated_file("missing-class", |source| {
        let marker = "\n[[cache]]\nclass = \"fixture\"";
        let start = source.find(marker).unwrap();
        source[..start].to_owned()
    });
    assert!(
        CacheContract::load(&missing)
            .unwrap_err()
            .contains("fixture has no cache declaration")
    );
}

#[test]
fn declaration_bytes_are_cryptographically_bound() {
    let original = load();
    let changed = mutated_file("bound", |source| {
        source.replacen("phase = \"install\"", "phase = \"setup\"", 1)
    });
    let changed = CacheContract::load(&changed).unwrap();
    assert_ne!(original.declaration_sha256(), changed.declaration_sha256());
}

#[test]
fn omitted_correctness_input_fails_closed() {
    for (tag, input) in [
        ("mise", "mise.toml"),
        ("npm", "**/package-lock.json"),
        ("npm-config", "**/.npmrc"),
        ("package-manifest", "**/package.json"),
        ("pnpm", "**/pnpm-lock.yaml"),
        ("pnpm-workspace", "**/pnpm-workspace.yaml"),
        ("yarn", "**/yarn.lock"),
        ("yarn-config", "**/.yarnrc.yml"),
        ("gradle", "**/settings.gradle.kts"),
        ("gradle-properties", "**/gradle.properties"),
        ("cargo", "**/Cargo.toml"),
    ] {
        let path = mutated_file(tag, |source| {
            source.replacen(&format!("\"{input}\", "), "", 1)
        });
        let error = CacheContract::load(&path).unwrap_err();
        assert!(error.contains("omits correctness input"), "{tag}: {error}");
    }
}

#[test]
fn cache_composite_enforces_runtime_authority_fields() {
    let body = std::fs::read_to_string(
        common::repo_root()
            .join("actions")
            .join("cache-contract")
            .join("action.yml"),
    )
    .unwrap();
    for required in [
        "CACHE_DECLARATION_SHA256",
        "EXPECTED_CACHE_DECLARATION_SHA256",
        "EXPECTED_CACHE_ID",
        "EXPECTED_CACHE_OWNER",
        "EXPECTED_CACHE_RESERVATION_ID",
        "CACHE_REQUIRED_PEAK_BYTES",
        "CACHE_QUOTA_RESERVED_BYTES",
        "CACHE_ATTRIBUTED_BYTES",
        "CACHE_CLEANUP_STATE",
        "CACHE_MATERIALIZATION_ID",
        "CACHE_SCOPE",
    ] {
        assert!(body.contains(required), "missing {required}");
    }
    assert!(body.contains("CACHE_ATTRIBUTED_BYTES <= CACHE_QUOTA_RESERVED_BYTES"));
    assert!(body.contains("CACHE_CLEANUP_STATE}\" == \"clean"));
}

fn validator_script() -> String {
    let source = velnor_actions_generator::composite::CACHE_CONTRACT_ACTION;
    let body = source.split_once("      run: |\n").unwrap().1;
    body.lines()
        .map(|line| line.strip_prefix("        ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn run_validator(overrides: &[(&str, &str)]) -> bool {
    let output = common::temp_dir("cache-validator").join("github-output");
    let mut command = std::process::Command::new("bash");
    command.arg("-c").arg(validator_script());
    for (key, value) in [
        ("CACHE_SCHEMA_VERSION", "1"),
        ("CACHE_DECLARATION_SHA256", DIGEST_A),
        ("EXPECTED_CACHE_DECLARATION_SHA256", DIGEST_A),
        ("CACHE_ID", "dependencies"),
        ("EXPECTED_CACHE_ID", "dependencies"),
        ("CACHE_SCOPE", "trusted"),
        ("EXPECTED_CACHE_SCOPE", "trusted"),
        ("CACHE_OWNER", "tailrocks/velnor"),
        ("EXPECTED_CACHE_OWNER", "tailrocks/velnor"),
        ("CACHE_RESERVATION_ID", "reservation-1"),
        ("EXPECTED_CACHE_RESERVATION_ID", "reservation-1"),
        ("CACHE_REQUIRED_PEAK_BYTES", "1000"),
        ("CACHE_QUOTA_RESERVED_BYTES", "1000"),
        ("CACHE_ATTRIBUTED_BYTES", "900"),
        ("CACHE_CLEANUP_STATE", "clean"),
        ("CACHE_MATERIALIZATION_ID", "run-1.job-1.cache-1"),
        ("EXPECTED_CACHE_MATERIALIZATION_ID", "run-1.job-1.cache-1"),
    ] {
        command.env(key, value);
    }
    for (key, value) in overrides {
        command.env(key, value);
    }
    command.env("GITHUB_OUTPUT", output);
    command.status().unwrap().success()
}

#[test]
fn cache_validator_accepts_only_bound_authority_record() {
    assert!(run_validator(&[]));
    for mismatch in [
        ("CACHE_DECLARATION_SHA256", DIGEST_B),
        ("CACHE_ID", "tools"),
        ("CACHE_SCOPE", "unmerged"),
        ("CACHE_OWNER", "other/repository"),
        ("CACHE_RESERVATION_ID", "reservation-2"),
        ("CACHE_MATERIALIZATION_ID", "run-2.job-1.cache-1"),
        ("CACHE_CLEANUP_STATE", "dirty"),
    ] {
        assert!(!run_validator(&[mismatch]), "accepted {mismatch:?}");
    }
}

#[test]
fn cache_validator_rejects_quota_overflow_and_under_reservation() {
    assert!(!run_validator(&[("CACHE_QUOTA_RESERVED_BYTES", "999")]));
    assert!(!run_validator(&[("CACHE_ATTRIBUTED_BYTES", "1001")]));
    assert!(!run_validator(&[(
        "CACHE_QUOTA_RESERVED_BYTES",
        "18446744073709551616",
    )]));
    assert!(!run_validator(&[("CACHE_ATTRIBUTED_BYTES", "0001")]));
}

#[test]
fn quota_pressure_exhaustion_aliasing_and_missing_attribution_fail_closed() {
    assert!(run_validator(&[("CACHE_QUOTA_RESERVED_BYTES", "1000")]));
    assert!(!run_validator(&[("CACHE_QUOTA_RESERVED_BYTES", "999")]));
    assert!(!run_validator(&[("CACHE_ATTRIBUTED_BYTES", "1001")]));
    assert!(!run_validator(&[("CACHE_ATTRIBUTED_BYTES", "")]));
    assert!(!run_validator(&[(
        "CACHE_MATERIALIZATION_ID",
        "other-job.materialization-1",
    )]));
}

fn run_temperature_contract(temperature: &str, github_hit: &str, velnor_hit: &str) -> bool {
    let script = format!(
        "set -euo pipefail\n{}\nverify_cache_temperature \"${{TEMPERATURE}}\" \"${{GITHUB_HIT}}\" \"${{VELNOR_HIT}}\"",
        velnor_actions_generator::render::CACHE_TEMPERATURE_FUNCTION
    );
    std::process::Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("TEMPERATURE", temperature)
        .env("GITHUB_HIT", github_hit)
        .env("VELNOR_HIT", velnor_hit)
        .status()
        .unwrap()
        .success()
}

#[test]
fn cold_hit_and_warm_miss_are_rejected_on_either_lane() {
    assert!(run_temperature_contract("cold", "false", "false"));
    assert!(!run_temperature_contract("cold", "true", "false"));
    assert!(!run_temperature_contract("cold", "false", "true"));
    assert!(run_temperature_contract("warm", "true", "true"));
    assert!(!run_temperature_contract("warm", "false", "true"));
    assert!(!run_temperature_contract("warm", "true", "false"));
    assert!(!run_temperature_contract("unknown", "true", "true"));
}

fn benchmark_cache_state(
    mode: &str,
    role: &str,
    github_hit: &str,
    velnor_hit: &str,
) -> Option<String> {
    let script = format!(
        "set -euo pipefail\n{}\nbenchmark_cache_state \"${{MODE}}\" \"${{ROLE}}\" \"${{GITHUB_HIT}}\" \"${{VELNOR_HIT}}\"",
        velnor_actions_generator::render::CACHE_TEMPERATURE_FUNCTION
    );
    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("MODE", mode)
        .env("ROLE", role)
        .env("GITHUB_HIT", github_hit)
        .env("VELNOR_HIT", velnor_hit)
        .output()
        .unwrap();
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).unwrap())
}

#[test]
fn benchmark_fresh_seed_publishes_and_identical_warm_reuse_does_not() {
    for mask in 0_u8..8 {
        let mut misses = 0;
        for cache in 0..3 {
            let hit = if mask & (1 << cache) == 0 {
                "false"
            } else {
                "true"
            };
            let state = benchmark_cache_state("normal", "target", hit, hit).unwrap();
            misses += usize::from(state == "miss\n");
        }
        assert_eq!(misses > 0, mask != 7, "mask {mask:03b}");
    }
    assert_eq!(
        benchmark_cache_state("enabled", "target", "false", "false").as_deref(),
        Some("miss\n")
    );
    assert_eq!(
        benchmark_cache_state("enabled", "target", "true", "true").as_deref(),
        Some("hit\n")
    );
    assert_eq!(
        benchmark_cache_state("disabled", "target", "false", "false").as_deref(),
        Some("ignored\n")
    );
    assert!(benchmark_cache_state("enabled", "non-target", "false", "false").is_none());
    assert!(benchmark_cache_state("enabled", "non-target", "true", "false").is_none());
    assert_eq!(
        benchmark_cache_state("enabled", "non-target", "true", "true").as_deref(),
        Some("ignored\n")
    );

    let contract = load();
    let manifest =
        velnor_actions_generator::model::FleetManifest::load(&common::repo_root()).unwrap();
    let workflow = velnor_actions_generator::render::callable_workflow(
        manifest.class(RepositoryClass::Code),
        &contract,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    for cache in contract
        .declarations()
        .iter()
        .filter(|cache| cache.class == RepositoryClass::Code)
    {
        let selector = format!(
            "inputs.benchmark_cache_id != '{}' && 'ci-v1' || needs.validate-request.outputs.cache_namespace",
            cache.id
        );
        assert!(
            workflow.contains(&selector),
            "missing A/B namespace split for {}",
            cache.id
        );
    }
}

#[test]
fn generated_lanes_embed_identical_bounded_cache_contract() {
    let contract = load();
    let manifest =
        velnor_actions_generator::model::FleetManifest::load(&common::repo_root()).unwrap();
    for class in velnor_actions_generator::ALL_CLASSES {
        let workflow = velnor_actions_generator::render::callable_workflow(
            manifest.class(class),
            &contract,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        assert!(workflow.contains(&format!(
            "# Cache declaration SHA-256: {}",
            contract.declaration_sha256()
        )));
        let declarations = contract
            .declarations()
            .iter()
            .filter(|cache| cache.class == class)
            .count();
        assert_eq!(
            workflow
                .matches("uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0")
                .count(),
            declarations * 2,
            "each cache appears once per lane"
        );
        let cache_keys = workflow
            .lines()
            .filter(|line| {
                line.trim_start().starts_with("key: ci-v1/")
                    || line.trim_start().starts_with("ci-v1/")
            })
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "github.sha",
            "github.run_id",
            "github.run_attempt",
            "github.ref_name",
        ] {
            assert!(
                !cache_keys.contains(forbidden),
                "cache key uses {forbidden}"
            );
        }
    }
}

#[test]
fn both_mode_has_one_cache_publisher_and_validated_nonempty_digests() {
    let contract = load();
    let manifest =
        velnor_actions_generator::model::FleetManifest::load(&common::repo_root()).unwrap();
    let workflow = velnor_actions_generator::render::callable_workflow(
        manifest.class(RepositoryClass::Code),
        &contract,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    assert!(workflow.contains(
        "if: ${{ inputs.lane == 'both' || github.event_name == 'pull_request' || github.event_name == 'merge_group' }}\n        uses: actions/cache/restore@"
    ));
    assert!(workflow.contains(
        "if: ${{ inputs.lane != 'both' && github.event_name != 'pull_request' && github.event_name != 'merge_group' }}\n        uses: actions/cache@"
    ));
    assert!(workflow.contains("actions/cache/save@"));
    assert!(!workflow.contains("github.event.pull_request.head.sha"));
    assert!(!workflow.contains("github.event.merge_group.head_sha"));
    assert!(workflow.contains(".velnor-protected-base"));
    assert!(workflow.contains("cache key digest missing or invalid"));
    assert!(workflow.contains("^[0-9a-f]{64}$"));
    assert_eq!(
        workflow
            .matches("inputs.lane == 'both' || github.event_name")
            .count(),
        3,
        "each Velnor cache becomes restore-only in both mode"
    );
}

fn initialize_digest_repo(path: &std::path::Path, marker: &str) {
    std::fs::create_dir_all(path).unwrap();
    for (name, body) in [
        ("mise.lock", "mise-lock\n"),
        ("mise.toml", "[tools]\n"),
        ("rust-toolchain.toml", "[toolchain]\nchannel='stable'\n"),
        ("Cargo.toml", "[workspace]\nmembers=[]\n"),
        ("Cargo.lock", marker),
    ] {
        std::fs::write(path.join(name), body).unwrap();
    }
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "add",
                "mise.lock",
                "mise.toml",
                "rust-toolchain.toml",
                "Cargo.toml",
                "Cargo.lock"
            ])
            .current_dir(path)
            .status()
            .unwrap()
            .success()
    );
}

fn run_git_digest(root: &std::path::Path, tree: &str) -> String {
    let contract = load();
    let declarations = contract
        .declarations()
        .iter()
        .filter(|cache| cache.class == RepositoryClass::Code)
        .collect::<Vec<_>>();
    let output = root.join(format!("digest-{}.out", tree.replace('/', "-")));
    if output.exists() {
        std::fs::remove_file(&output).unwrap();
    }
    let status = std::process::Command::new("bash")
        .arg("-c")
        .arg(velnor_actions_generator::render::git_cache_digest_script(
            &declarations,
            tree,
        ))
        .env("GITHUB_OUTPUT", &output)
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read_to_string(output).unwrap()
}

#[test]
fn protected_base_and_current_digests_are_cryptographically_independent() {
    let root = common::temp_dir("independent-cache-digests");
    initialize_digest_repo(&root, "head-one\n");
    initialize_digest_repo(&root.join("protected-base"), "base-one\n");

    let current_one = run_git_digest(&root, ".");
    let base_one = run_git_digest(&root, "protected-base");
    std::fs::write(root.join("protected-base/Cargo.lock"), "base-two\n").unwrap();
    let current_two = run_git_digest(&root, ".");
    let base_two = run_git_digest(&root, "protected-base");
    assert_eq!(current_one, current_two);
    assert_ne!(base_one, base_two);

    std::fs::write(root.join("Cargo.lock"), "head-two\n").unwrap();
    let current_three = run_git_digest(&root, ".");
    let base_three = run_git_digest(&root, "protected-base");
    assert_ne!(current_two, current_three);
    assert_eq!(base_two, base_three);
}

#[test]
fn generated_cache_proof_dag_is_evidence_bearing_and_single_writer() {
    let contract = load();
    let manifest =
        velnor_actions_generator::model::FleetManifest::load(&common::repo_root()).unwrap();
    let workflow = velnor_actions_generator::render::callable_workflow(
        manifest.class(RepositoryClass::Code),
        &contract,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    for job in [
        "github_restore:",
        "velnor_restore:",
        "restore_barrier:",
        "github_execute:",
        "velnor_execute:",
        "cache_publish:",
        "cache-proof-contract:",
    ] {
        assert!(workflow.contains(job), "missing {job}");
    }
    assert!(workflow.contains("actions/cache/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9"));
    assert!(workflow.contains("actions/cache/save@55cc8345863c7cc4c66a329aec7e433d2d1c52a9"));
    assert!(workflow.contains("--sort=name --mtime='@0' --owner=0 --group=0"));
    assert!(workflow.contains("sha256sum"));
    assert!(workflow.contains("slot: ${{ fromJSON(inputs.benchmark_fanout == '8'"));
    assert!(workflow.contains("cache_restore_ms"));
    assert!(workflow.contains("cache_lock_wait_ms"));
    assert!(workflow.contains("if ${publish_expected}; then"));
}

#[test]
fn velnor_cache_access_requires_bound_runtime_authority() {
    let contract = load();
    let manifest =
        velnor_actions_generator::model::FleetManifest::load(&common::repo_root()).unwrap();
    let workflow = velnor_actions_generator::render::callable_workflow(
        manifest.class(RepositoryClass::Code),
        &contract,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    assert!(workflow.contains("Capture tools Velnor cache authority"));
    assert!(workflow.contains("actions/cache-contract@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    assert!(workflow.contains("required-peak-bytes: 2147483648"));
    assert!(workflow.contains("VELNOR_CACHE_${CACHE_ENV_PREFIX}_"));
}

fn run_proof_contract(overrides: &[(&str, &str)]) -> bool {
    let output = common::temp_dir("proof-contract").join("github-output");
    let mut command = std::process::Command::new("bash");
    command
        .arg("-c")
        .arg(velnor_actions_generator::render::PROOF_CONTRACT_SCRIPT);
    for key in [
        "GITHUB_RESTORE",
        "VELNOR_RESTORE",
        "RESTORE_BARRIER",
        "GITHUB_EXECUTE",
        "VELNOR_EXECUTE",
        "PROOF_RECONCILE",
    ] {
        command.env(key, "success");
    }
    command.env("CACHE_PUBLISH", "skipped");
    command.env("TEMPERATURE", "warm");
    command.env("BENCHMARK_MODE", "");
    command.env("CACHE_PROOF_ID", "proof");
    command.env("BENCHMARK_CAMPAIGN", "");
    command.env("PUBLISH_NEEDED", "false");
    for (key, value) in overrides {
        command.env(key, value);
    }
    command.env("GITHUB_OUTPUT", output);
    command.status().unwrap().success()
}

#[test]
fn proof_contract_rejects_early_execute_and_wrong_publisher_truth() {
    assert!(run_proof_contract(&[]));
    assert!(!run_proof_contract(&[("RESTORE_BARRIER", "failure")]));
    assert!(!run_proof_contract(&[("GITHUB_EXECUTE", "skipped")]));
    assert!(!run_proof_contract(&[("CACHE_PUBLISH", "success")]));
    assert!(run_proof_contract(&[
        ("TEMPERATURE", "cold"),
        ("PUBLISH_NEEDED", "true"),
        ("CACHE_PUBLISH", "success"),
    ]));
    assert!(!run_proof_contract(&[("TEMPERATURE", "cold")]));
    assert!(run_proof_contract(&[("BENCHMARK_MODE", "normal")]));
    assert!(run_proof_contract(&[("BENCHMARK_MODE", "disabled")]));
    assert!(!run_proof_contract(&[
        ("BENCHMARK_MODE", "disabled"),
        ("CACHE_PUBLISH", "success"),
    ]));
    assert!(run_proof_contract(&[
        ("CACHE_PROOF_ID", ""),
        ("BENCHMARK_CAMPAIGN", "campaign"),
        ("BENCHMARK_MODE", "enabled"),
    ]));
    assert!(run_proof_contract(&[
        ("CACHE_PROOF_ID", ""),
        ("BENCHMARK_CAMPAIGN", "campaign"),
        ("BENCHMARK_MODE", "enabled"),
        ("PUBLISH_NEEDED", "true"),
        ("CACHE_PUBLISH", "success"),
    ]));
    assert!(!run_proof_contract(&[("GITHUB_EXECUTE", "skipped")]));
    assert!(!run_proof_contract(&[("VELNOR_EXECUTE", "skipped")]));
}

#[test]
fn skipped_project_or_reconciliation_job_cannot_satisfy_proof_contract() {
    for job in [
        "GITHUB_RESTORE",
        "VELNOR_RESTORE",
        "RESTORE_BARRIER",
        "GITHUB_EXECUTE",
        "VELNOR_EXECUTE",
        "PROOF_RECONCILE",
    ] {
        assert!(!run_proof_contract(&[(job, "skipped")]), "accepted {job}");
    }
}

#[test]
fn parsed_proof_yaml_prevents_lane_warm_and_duplicate_publisher_saves() {
    let contract = load();
    let manifest =
        velnor_actions_generator::model::FleetManifest::load(&common::repo_root()).unwrap();
    let workflow = velnor_actions_generator::render::callable_workflow(
        manifest.class(RepositoryClass::Code),
        &contract,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    let documents = yaml_rust2::YamlLoader::load_from_str(&workflow).unwrap();
    let jobs = &documents[0]["jobs"];
    let publisher = &jobs["cache_publish"];
    let condition = publisher["if"].as_str().unwrap();
    assert!(condition.contains("needs.restore_barrier.outputs.publish_needed == 'true'"));
    assert_eq!(
        publisher["concurrency"]["group"].as_str(),
        Some(
            "velnor-cache-writer-github-${{ github.repository }}-${{ needs.validate-request.outputs.cache_namespace }}"
        )
    );
    assert_eq!(
        publisher["concurrency"]["cancel-in-progress"].as_bool(),
        Some(false)
    );
    // Ordinary CI on unrelated refs must never share GitHub's lossy concurrency
    // queue. Only authenticated proof publication is serialized.
    assert!(jobs["github-lane"]["concurrency"].is_badvalue());
    assert!(jobs["velnor-lane"]["concurrency"].is_badvalue());
    for lane in ["github_restore", "velnor_restore"] {
        let restore = jobs[lane]["steps"]
            .as_vec()
            .unwrap()
            .iter()
            .find(|step| step["name"].as_str() == Some("Restore dependencies without saving"))
            .unwrap();
        assert!(
            !restore["if"]
                .as_str()
                .unwrap()
                .contains("cache_temperature")
        );
    }
    assert!(
        publisher["steps"].as_vec().unwrap().iter().any(|step| {
            step["name"].as_str() == Some("Restore published dependencies identity")
        })
    );
    let publisher_steps = publisher["steps"].as_vec().unwrap();
    let preflight = publisher_steps
        .iter()
        .position(|step| {
            step["name"].as_str() == Some("Probe dependencies immediately before publication")
        })
        .unwrap();
    let save = publisher_steps
        .iter()
        .position(|step| step["name"].as_str() == Some("Publish dependencies exactly once"))
        .unwrap();
    assert!(preflight < save);
    assert!(publisher_steps.iter().any(|step| {
        step["name"].as_str() == Some("Reject existing dependencies publication identity")
            && step["run"].as_str().unwrap().contains("CACHE_HIT")
            && step["run"].as_str().unwrap().contains("!= true")
    }));
    assert!(!condition.contains("inputs.cache_temperature"));
    let save_steps = publisher["steps"]
        .as_vec()
        .unwrap()
        .iter()
        .filter(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/cache/save@"))
        })
        .count();
    assert_eq!(save_steps, 3, "one publisher save per code cache ID");
    for execute in ["github_execute", "velnor_execute"] {
        assert!(jobs[execute]["steps"].as_vec().unwrap().iter().all(|step| {
            !step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/cache/save@"))
        }));
    }
}

#[test]
fn single_cache_disable_is_isolated_and_enabled_disabled_outputs_reconcile() {
    let contract = load();
    let manifest =
        velnor_actions_generator::model::FleetManifest::load(&common::repo_root()).unwrap();
    let workflow = velnor_actions_generator::render::callable_workflow(
        manifest.class(RepositoryClass::Code),
        &contract,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    let documents = yaml_rust2::YamlLoader::load_from_str(&workflow).unwrap();
    let jobs = &documents[0]["jobs"];
    for lane in ["github_restore", "velnor_restore"] {
        let steps = jobs[lane]["steps"].as_vec().unwrap();
        for id in ["tools", "dependencies", "build-output"] {
            let name = format!("Restore {id} without saving");
            let step = steps
                .iter()
                .find(|step| step["name"].as_str() == Some(name.as_str()))
                .unwrap();
            let condition = step["if"].as_str().unwrap();
            assert!(condition.contains(&format!("inputs.benchmark_cache_id != '{id}'")));
            assert!(condition.contains("inputs.benchmark_cache_mode != 'disabled'"));
            for other in ["tools", "dependencies", "build-output"] {
                if other != id {
                    assert!(
                        !condition.contains(&format!("inputs.benchmark_cache_id != '{other}'"))
                    );
                }
            }
        }
    }
    let reconcile_if = jobs["proof_reconcile"]["if"].as_str().unwrap();
    assert!(!reconcile_if.contains("benchmark_cache_mode"));
    let verifier = workflow
        .lines()
        .filter(|line| line.contains(".output_digest"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(verifier.contains("github_metrics"));
    assert!(verifier.contains("velnor_metrics"));
}

#[test]
fn zero_applicable_cache_proof_work_is_rejected() {
    let contract = load();
    let declarations = contract
        .declarations()
        .iter()
        .filter(|cache| cache.class == RepositoryClass::Code)
        .collect::<Vec<_>>();
    let status = std::process::Command::new("bash")
        .arg("-c")
        .arg(velnor_actions_generator::render::barrier_script(
            &declarations,
        ))
        .env("CACHE_PROOF_ID", "")
        .env("BENCHMARK_CACHE_ID", "")
        .env("CACHE_TEMPERATURE", "cold")
        .status()
        .unwrap();
    assert!(!status.success());
}

fn run_post_artifact_identity(artifacts: &str, fanout: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let root = common::temp_dir("post-artifact-identities");
    let gh = root.join("gh");
    std::fs::write(
        &gh,
        "#!/usr/bin/env bash\nprintf '%s\\n' \"${FAKE_ARTIFACTS}\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&gh).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&gh, permissions).unwrap();
    let output = root.join("output");
    let path = format!("{}:{}", root.display(), std::env::var("PATH").unwrap());
    std::process::Command::new("bash")
        .arg("-c")
        .arg(velnor_actions_generator::render::POST_ARTIFACT_IDENTITY_SCRIPT)
        .env("PATH", path)
        .env("FAKE_ARTIFACTS", artifacts)
        .env("REPOSITORY", "tailrocks/example")
        .env("RUN_ID", "123")
        .env("FANOUT", fanout)
        .env("GITHUB_OUTPUT", output)
        .status()
        .unwrap()
        .success()
}

#[test]
fn post_artifact_identity_rejects_missing_duplicate_and_unbound_digest() {
    let github =
        format!(r#"{{"id":1,"name":"proof-post-github-123-0","digest":"sha256:{DIGEST_A}"}}"#);
    let velnor =
        format!(r#"{{"id":2,"name":"proof-post-velnor-123-0","digest":"sha256:{DIGEST_B}"}}"#);
    let valid = format!(r#"[{{"artifacts":[{github},{velnor}]}}]"#);
    assert!(run_post_artifact_identity(&valid, "1"));
    assert!(!run_post_artifact_identity(
        &format!(r#"[{{"artifacts":[{github}]}}]"#),
        "1"
    ));
    assert!(!run_post_artifact_identity(
        &format!(r#"[{{"artifacts":[{github},{github}]}}]"#),
        "1"
    ));
    assert!(!run_post_artifact_identity(
        &valid.replace(&format!("sha256:{DIGEST_B}"), "sha256:invalid"),
        "1"
    ));
}

fn run_evidence_verifier(tamper: &str) -> bool {
    let root = common::temp_dir("evidence-verifier");
    let script = format!(
        r#"set -euo pipefail
mkdir -p source/cache evidence
printf 'bound payload\n' > source/cache/payload
chmod 640 source/cache/payload
digest="$(sha256sum source/cache/payload | cut -d' ' -f1)"
encoded="$(printf '%s' cache/payload | openssl base64 -A)"
printf 'f 640 %s %s -\n' "${{digest}}" "${{encoded}}" > evidence/manifest.txt
(cd source && tar -cf ../evidence/cache.tar cache/payload)
sha256sum evidence/manifest.txt | cut -d' ' -f1 > evidence/manifest.sha256
sha256sum evidence/cache.tar | cut -d' ' -f1 > evidence/archive.sha256
printf 'false\n' > evidence/hit
printf 'tools\n' > evidence/cache-id
printf 'github\n' > evidence/lane
printf '12\n' > evidence/restore-ms
printf '0\n' > evidence/lock-wait-ms
size="$(stat -c '%s' evidence/cache.tar 2>/dev/null || stat -f '%z' evidence/cache.tar)"; printf '%s\n' "${{size}}" > evidence/cache-bytes
printf '1\n' > evidence/cache-files
printf 'miss\n' > evidence/hit-source
printf '%s\n' '-1' > evidence/age-seconds
printf 'unknown\n' > evidence/eviction-risk
{tamper}
{verifier}
verify_evidence evidence github tools
"#,
        verifier = velnor_actions_generator::render::EVIDENCE_VERIFIER,
    );
    std::process::Command::new("bash")
        .arg("-c")
        .arg(script)
        .current_dir(root)
        .status()
        .unwrap()
        .success()
}

#[test]
fn immutable_evidence_verifier_rejects_content_identity_and_set_tampering() {
    assert!(run_evidence_verifier(":"));
    assert!(!run_evidence_verifier(
        "printf 'other payload\\n' > source/cache/payload; (cd source && tar -cf ../evidence/cache.tar cache/payload); sha256sum evidence/cache.tar | cut -d' ' -f1 > evidence/archive.sha256"
    ));
    assert!(!run_evidence_verifier(
        "sed -i.bak 's/^f 640 /f 600 /' evidence/manifest.txt; sha256sum evidence/manifest.txt | cut -d' ' -f1 > evidence/manifest.sha256"
    ));
    assert!(!run_evidence_verifier(
        "cp evidence/manifest.txt duplicate-line; cat duplicate-line >> evidence/manifest.txt; sha256sum evidence/manifest.txt | cut -d' ' -f1 > evidence/manifest.sha256"
    ));
    assert!(!run_evidence_verifier(
        "printf extra > source/extra; (cd source && tar -rf ../evidence/cache.tar extra); sha256sum evidence/cache.tar | cut -d' ' -f1 > evidence/archive.sha256"
    ));
    assert!(!run_evidence_verifier("printf 'velnor\\n' > evidence/lane"));
}

fn run_typed_evidence_verifier(link_target: &str) -> bool {
    let root = common::temp_dir("typed-evidence-verifier");
    let script = format!(
        r#"set -euo pipefail
mkdir -p source/cache/empty evidence
printf payload > source/cache/payload
ln -s -- "${{LINK_TARGET}}" source/cache/link
cd source
: > ../evidence/manifest.txt
for entry in cache/payload cache/link cache/empty; do
  mode="$(stat -c '%a' -- "${{entry}}" 2>/dev/null || stat -f '%Lp' -- "${{entry}}")"
  path="$(printf '%s' "${{entry}}" | openssl base64 -A)"
  if [[ -L "${{entry}}" ]]; then
    target="$(readlink -- "${{entry}}")"; digest="$(printf '%s' "${{target}}" | sha256sum | cut -d' ' -f1)"; target="$(printf '%s' "${{target}}" | openssl base64 -A)"; kind=l
  elif [[ -d "${{entry}}" ]]; then
    digest="$(printf '' | sha256sum | cut -d' ' -f1)"; target=-; kind=d
  else
    digest="$(sha256sum -- "${{entry}}" | cut -d' ' -f1)"; target=-; kind=f
  fi
  printf '%s %s %s %s %s\n' "${{kind}}" "${{mode}}" "${{digest}}" "${{path}}" "${{target}}" >> ../evidence/manifest.txt
done
LC_ALL=C sort -o ../evidence/manifest.txt ../evidence/manifest.txt
printf 'cache/payload\0cache/link\0cache/empty\0' | tar --null --no-recursion --files-from=- -cf ../evidence/cache.tar
cd ..
sha256sum evidence/manifest.txt | cut -d' ' -f1 > evidence/manifest.sha256
sha256sum evidence/cache.tar | cut -d' ' -f1 > evidence/archive.sha256
printf 'false\ntools\ngithub\n12\n' > /dev/null
printf 'false\n' > evidence/hit; printf 'tools\n' > evidence/cache-id; printf 'github\n' > evidence/lane; printf '12\n' > evidence/restore-ms; printf '0\n' > evidence/lock-wait-ms
size="$(stat -c '%s' evidence/cache.tar 2>/dev/null || stat -f '%z' evidence/cache.tar)"; printf '%s\n' "${{size}}" > evidence/cache-bytes; printf '3\n' > evidence/cache-files; printf 'miss\n' > evidence/hit-source; printf '%s\n' -1 > evidence/age-seconds; printf 'unknown\n' > evidence/eviction-risk
{verifier}
verify_evidence evidence github tools
"#,
        verifier = velnor_actions_generator::render::EVIDENCE_VERIFIER,
    );
    std::process::Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("LINK_TARGET", link_target)
        .current_dir(root)
        .status()
        .unwrap()
        .success()
}

#[test]
fn immutable_evidence_preserves_symlinks_and_empty_directories_without_escape() {
    assert!(run_typed_evidence_verifier("payload"));
    assert!(!run_typed_evidence_verifier("../../etc/passwd"));
}

fn reservation_record(participants: usize) -> String {
    let slots = (0..participants)
        .map(|slot| {
            format!(
                r#"{{"materialization_id":"materialization-{slot:02}","reserved_bytes":9000,"slot":{slot}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    format!(
        r#"{{"campaign":"campaign-0001","expires_at_epoch":{expires},"owner":"tailrocks/example","participant_count":{participants},"ready_count":{participants},"reservation_id":"reservation-0001","schema":1,"slots":[{slots}],"state":"released","wave":"wave-0001"}}"#
    )
}

fn run_reservation_barrier(record: &str, participants: usize) -> bool {
    let output = common::temp_dir("reservation-barrier").join("output");
    std::process::Command::new("bash")
        .arg("-c")
        .arg(velnor_actions_generator::render::RESERVATION_BARRIER_SCRIPT)
        .env("VELNOR_BENCHMARK_COORDINATOR_V1", record)
        .env("EXPECTED_OWNER", "tailrocks/example")
        .env("EXPECTED_CAMPAIGN", "campaign-0001")
        .env("EXPECTED_WAVE", "wave-0001")
        .env("EXPECTED_RESERVATION", "reservation-0001")
        .env("EXPECTED_PARTICIPANTS", participants.to_string())
        .env("REQUIRED_PEAK_BYTES", "8192")
        .env("GITHUB_OUTPUT", output)
        .status()
        .unwrap()
        .success()
}

#[test]
fn reservation_barrier_enforces_exact_one_or_eight_slot_release() {
    assert!(run_reservation_barrier(&reservation_record(1), 1));
    assert!(run_reservation_barrier(&reservation_record(8), 8));
    assert!(!run_reservation_barrier(&reservation_record(1), 8));
    assert!(!run_reservation_barrier(&reservation_record(2), 2));
    assert!(!run_reservation_barrier(
        &reservation_record(8).replace("materialization-07", "materialization-06"),
        8
    ));
    assert!(!run_reservation_barrier(
        &reservation_record(8).replace("\"slot\":7", "\"slot\":6"),
        8
    ));
    assert!(!run_reservation_barrier(
        &reservation_record(8).replace("\"reserved_bytes\":9000", "\"reserved_bytes\":1"),
        8
    ));
    assert!(!run_reservation_barrier(
        &reservation_record(8).replace("\"reserved_bytes\":9000", "\"reserved_bytes\":9000.5"),
        8
    ));
    assert!(!run_reservation_barrier(
        &reservation_record(8).replace(
            "\"reserved_bytes\":9000",
            "\"reserved_bytes\":9007199254740992"
        ),
        8
    ));
    assert!(!run_reservation_barrier(
        &reservation_record(1).replace("campaign-0001", "campaign-wrong"),
        1
    ));
    let mut expired = reservation_record(1);
    let start = expired.find("\"expires_at_epoch\":").unwrap();
    let value_start = start + "\"expires_at_epoch\":".len();
    let value_end = value_start + expired[value_start..].find(',').unwrap();
    expired.replace_range(value_start..value_end, "0");
    assert!(!run_reservation_barrier(&expired, 1));
}

fn run_reservation_grant(grant: &str, slot: usize) -> bool {
    let canonical = reservation_record(8);
    let encoded = std::process::Command::new("openssl")
        .args(["base64", "-A"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(canonical.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    let mut digest_child = std::process::Command::new("openssl")
        .args(["dgst", "-sha256", "-r"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    digest_child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(canonical.as_bytes())
        .unwrap();
    let digest_output = digest_child.wait_with_output().unwrap();
    let digest = String::from_utf8(digest_output.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let grant = grant.replace("__DIGEST__", &digest);
    std::process::Command::new("bash")
        .arg("-c")
        .arg(velnor_actions_generator::render::RESERVATION_GRANT_SCRIPT)
        .env("VELNOR_BENCHMARK_GRANT_V1", grant)
        .env("EXPECTED_RECORD_DIGEST", digest)
        .env(
            "EXPECTED_COORDINATOR_RECORD_B64",
            String::from_utf8(encoded.stdout).unwrap(),
        )
        .env("EXPECTED_CAMPAIGN", "campaign-0001")
        .env("EXPECTED_WAVE", "wave-0001")
        .env("EXPECTED_RESERVATION", "reservation-0001")
        .env("EXPECTED_SLOT", slot.to_string())
        .env("REQUIRED_PEAK_BYTES", "8192")
        .status()
        .unwrap()
        .success()
}

#[test]
fn reservation_grant_is_bound_to_record_slot_and_peak() {
    let grant = r#"{"campaign":"campaign-0001","coordinator_digest":"__DIGEST__","materialization_id":"materialization-03","reservation_id":"reservation-0001","reserved_bytes":9000,"schema":1,"slot":3,"state":"released","wave":"wave-0001"}"#.to_owned();
    assert!(run_reservation_grant(&grant, 3));
    assert!(!run_reservation_grant(&grant, 2));
    assert!(!run_reservation_grant(
        &grant.replace("__DIGEST__", DIGEST_B),
        3
    ));
    assert!(!run_reservation_grant(
        &grant.replace("materialization-03", "materialization-04"),
        3
    ));
    assert!(!run_reservation_grant(
        &grant.replace("\"reserved_bytes\":9000", "\"reserved_bytes\":1"),
        3
    ));
    assert!(!run_reservation_grant(
        &grant.replace("\"reserved_bytes\":9000", "\"reserved_bytes\":9000.5"),
        3
    ));
}

fn run_metrics_validator(json: &str, correlation: &str) -> bool {
    let root = common::temp_dir("metrics-validator");
    let metrics = root.join("metrics.json");
    std::fs::write(&metrics, json).unwrap();
    std::process::Command::new("bash")
        .arg("-c")
        .arg(velnor_actions_generator::render::METRICS_VALIDATE_SCRIPT)
        .env("METRICS_FILE", metrics)
        .env("EXPECTED_METRICS_LANE", "github")
        .env("EXPECTED_METRICS_SLOT", "3")
        .env("EXPECTED_METRICS_CORRELATION", correlation)
        .status()
        .unwrap()
        .success()
}

#[test]
fn metrics_schema_enforces_units_boundaries_redaction_and_correlation() {
    let valid = format!(
        r#"{{"schema":1,"lane":"github","slot":3,"required_step_start_ms":1000,"required_step_end_ms":1250,"required_steps_ms":250,"cpu_ms":120,"peak_memory_bytes":4096,"disk_bytes":8192,"cache_restore_ms":20,"cache_save_ms":0,"cache_copy_ms":4,"cache_lock_wait_ms":0,"cache_lock_wait_source":"not-applicable-github","disk_latency_ms":2,"io_pressure_stall_ms":1,"psi":{{"cpu":"some avg10=0.00 total=1","io":"some avg10=0.00 total=2","memory":"some avg10=0.00 total=3"}},"cache_result":"exact","cache_bytes":1024,"cache_files":4,"output_digest":"{DIGEST_A}","correlation":"campaign-0001:wave-0001"}}"#
    );
    assert!(run_metrics_validator(&valid, "campaign-0001:wave-0001"));
    assert!(!run_metrics_validator(&valid, "other-correlation"));
    assert!(!run_metrics_validator(
        &valid.replace("\"required_steps_ms\":250", "\"required_steps_ms\":249"),
        "campaign-0001:wave-0001"
    ));
    assert!(!run_metrics_validator(
        &valid.replace("\"cpu_ms\":120", "\"cpu_ms\":1.5"),
        "campaign-0001:wave-0001"
    ));
    assert!(!run_metrics_validator(
        &valid.replace("some avg10=0.00 total=1", "secret-token-value"),
        "campaign-0001:wave-0001"
    ));
    assert!(!run_metrics_validator(
        &valid.replace(&format!(",\"output_digest\":\"{DIGEST_A}\""), ""),
        "campaign-0001:wave-0001"
    ));
}

#[test]
fn publisher_metrics_measure_the_only_save_boundary() {
    let root = common::temp_dir("publisher-metrics");
    let status = std::process::Command::new("bash")
        .arg("-c")
        .arg(velnor_actions_generator::render::PUBLISHER_METRICS_SCRIPT)
        .env("PUBLISH_STARTED_MS", "1000")
        .env("PUBLISH_FINISHED_MS", "1123")
        .env("PUBLISH_CACHE_ID", "tools")
        .env("PUBLISH_CORRELATION", "campaign-0001:wave-0001")
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success());
    let metrics =
        std::fs::read_to_string(root.join(".velnor-proof/publisher-metrics/tools.json")).unwrap();
    assert!(metrics.contains(r#""cache_save_ms": 123"#));
    assert!(metrics.contains(r#""cache_lock_wait_ms": 0"#));
    assert!(metrics.contains(r#""cache_lock_wait_source": "not-applicable-github""#));

    let invalid = std::process::Command::new("bash")
        .arg("-c")
        .arg(velnor_actions_generator::render::PUBLISHER_METRICS_SCRIPT)
        .env("PUBLISH_STARTED_MS", "1123")
        .env("PUBLISH_FINISHED_MS", "1000")
        .env("PUBLISH_CACHE_ID", "tools")
        .env("PUBLISH_CORRELATION", "campaign-0001:wave-0001")
        .current_dir(root)
        .status()
        .unwrap();
    assert!(!invalid.success());
}

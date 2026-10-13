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

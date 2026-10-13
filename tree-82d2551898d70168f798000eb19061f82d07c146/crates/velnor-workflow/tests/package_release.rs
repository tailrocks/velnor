//! Schema-2 package-release verification-hook contract.

#![expect(
    clippy::unwrap_used,
    reason = "fixture setup failures should identify their operation"
)]
#![expect(
    clippy::expect_used,
    reason = "fixture setup failures should identify their operation"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn temporary_root(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "velnor-package-release-{label}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn fixture_root(destination: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic-release");
    copy_tree(&source, destination);
    destination.to_path_buf()
}

fn package_config(verify_task: &str) -> String {
    package_config_for_repository("example/synthetic-release", verify_task)
}

fn package_config_for_repository(repository: &str, verify_task: &str) -> String {
    format!(
        r#"schema = 2

[generator]
repository = "{repository}"

[workflow]
providers = ["github-hosted"]
automatic_providers = ["github-hosted"]
default_dispatch_providers = ["github-hosted"]
default_branch = "main"

[workflow.selectors.github-hosted]
runs_on = ["ubuntu-24.04"]

[[declare]]
primitive = "package-release"
file = "preview.yml"

[declare.args]
build_tasks = ["build-release"]
verify_tasks = ["{verify_task}"]
package_dir = "dist"
manifest_schema = "example.consumer-manifest-v1"
source_repository = "{repository}"
source_ref = "refs/heads/main"
payloads = ["a.tar.gz"]
supporting_assets = ["SHA256SUMS"]
channel = "preview"
release_tag = "preview"
publication_lock_branch = "package-release-lock"
github_release_type = "prerelease"
publish_environment = "github-preview"
consumer_repository = "example/tap"
consumer_branch = "main"
updater = "./scripts/package-update.sh"
updater_token_secret = "TAP_TOKEN"
"#
    )
}

fn write_inputs(root: &Path, config: &str, include_verify_task: bool) {
    fs::create_dir_all(root.join(".github-gen")).unwrap();
    fs::write(root.join(".github-gen/velnor-workflow.toml"), config).unwrap();
    let verify = if include_verify_task {
        "\n[tasks.verify-release]\nrun = \"echo verify\"\n"
    } else {
        ""
    };
    let pre_publish = if config.contains("pre_publish_tasks") {
        "\n[tasks.migrate-preview-legacy]\nrun = \"echo migrate\"\n"
    } else {
        ""
    };
    fs::write(
        root.join("mise.toml"),
        format!("[tasks.build-release]\nrun = \"echo build\"\n{verify}{pre_publish}"),
    )
    .unwrap();
}

fn generate_in_place(root: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args(["--plain", "--force", "--default-branch", "main"])
        .arg(root)
        .output()
        .expect("run generator")
}

#[test]
fn package_release_hook_renders_and_passes_policy() {
    let workspace = temporary_root("policy");
    let root = fixture_root(&workspace.join("repo"));
    write_inputs(&root, &package_config("verify-release"), true);
    let generated = generate_in_place(&root);
    assert!(
        generated.status.success(),
        "generation failed:\n{}",
        String::from_utf8_lossy(&generated.stderr)
    );

    let workflow = fs::read_to_string(root.join(".github/workflows/preview.yml")).unwrap();
    assert_eq!(workflow.matches("mise run 'verify-release'").count(), 3);
    assert!(workflow.contains("Run repository package verification tasks"));
    assert!(workflow.contains("Run handoff package verification tasks"));
    assert!(workflow.contains("Run published package verification tasks"));
    assert!(
        workflow.contains("VELNOR_VERIFIED_PACKAGE_DIR: ${{ github.workspace }}/published-package")
    );
    assert!(workflow.contains(
        "existing public rolling release failed immutable validation; refusing mutation"
    ));
    assert!(!workflow.contains("discard_current_typed_rolling_draft"));
    assert!(workflow.contains("GitHub cannot undelete a release or tag"));
    assert!(workflow.contains("rolling preview ownership changed; refusing rollback mutation"));
    assert!(!workflow.contains("discard_stale_rolling_draft"));

    let policy_workflow = fs::read_to_string(root.join(".github/workflows/ci-policy.yml")).unwrap();
    let revision = policy_workflow
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("VELNOR_WORKFLOW_POLICY_REVISION: ")
                .map(str::to_owned)
        })
        .expect("generated policy revision");
    let policy = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "policy",
            "--workflow-root",
            root.to_str().unwrap(),
            "--base-revision",
            &revision,
        ])
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .expect("run generated policy");
    assert!(
        policy.status.success(),
        "policy failed:\n{}{}",
        String::from_utf8_lossy(&policy.stdout),
        String::from_utf8_lossy(&policy.stderr)
    );
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn package_release_owner_publish_verifies_runtime_against_source_checkout() {
    let workspace = temporary_root("owner-runtime");
    let root = fixture_root(&workspace.join("repo"));
    let owner_repository = ["tailrocks", "velnor"].join("/");
    write_inputs(
        &root,
        &package_config_for_repository(&owner_repository, "verify-release"),
        true,
    );
    let generated = generate_in_place(&root);
    assert!(
        generated.status.success(),
        "generation failed:\n{}",
        String::from_utf8_lossy(&generated.stderr)
    );

    let workflow = fs::read_to_string(root.join(".github/workflows/preview.yml")).unwrap();
    let publish = workflow
        .split_once("  publish:\n")
        .map(|(_, job)| job.split("\n  runtime:").next().expect("publish body"))
        .expect("publish job");
    assert!(
        publish.contains("name: Verify Velnor workflow runtime\n        shell: bash\n        working-directory: source"),
        "owner publish must bind artifact closure to its source checkout: {publish}"
    );
    assert!(!publish.contains("setup-velnor-workflow"));
    assert!(!publish.contains("cargo build"));
    assert!(
        publish.contains("EXPECTED_REVISION: ") && !publish.contains("EXPECTED_REVISION: ${{"),
        "owner publish must retain a literal runtime pin: {publish}"
    );
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn package_release_hook_rejects_undeclared_mise_task() {
    let workspace = temporary_root("missing-task");
    let root = fixture_root(&workspace.join("repo"));
    write_inputs(&root, &package_config("verify-release"), false);
    let generated = generate_in_place(&root);
    assert!(!generated.status.success(), "missing task must fail closed");
    let error = String::from_utf8_lossy(&generated.stderr);
    assert!(
        error.contains(
            "package-release verify_tasks entry verify-release is not declared by mise.toml"
        ),
        "error must identify the undeclared task: {error}"
    );
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn package_release_hook_renders_locked_pre_publish_migration_with_narrow_tokens() {
    let workspace = temporary_root("pre-publish");
    let root = fixture_root(&workspace.join("repo"));
    let config = format!(
        "{}\npre_publish_tasks = [\"migrate-preview-legacy\"]\n",
        package_config("verify-release")
    );
    write_inputs(&root, &config, true);
    let generated = generate_in_place(&root);
    assert!(
        generated.status.success(),
        "generation failed:\n{}",
        String::from_utf8_lossy(&generated.stderr)
    );

    let workflow = fs::read_to_string(root.join(".github/workflows/preview.yml")).unwrap();
    assert_eq!(
        workflow
            .matches("mise run 'migrate-preview-legacy'")
            .count(),
        1
    );
    let handoff = workflow
        .find("- name: Re-verify downloaded handoff")
        .expect("handoff verification");
    let attest = workflow
        .find("- name: Verify build attestations")
        .expect("build attestation verification");
    let lock = workflow
        .find("- name: Acquire package publication lock")
        .expect("publication lock");
    let migration = workflow
        .find("- name: Run pre-publish migration tasks")
        .expect("pre-publish migration");
    let immutable = workflow
        .find("- name: Publish immutable source-bound release")
        .expect("immutable publication");
    let published = workflow
        .find("- name: Download and re-verify published release")
        .expect("published verification");
    let finalizer = workflow
        .find("- name: Finalize package publication lock")
        .expect("publication lock finalizer");
    assert!(handoff < attest && attest < lock && lock < migration && migration < immutable);
    assert!(immutable < published && published < finalizer);
    assert_eq!(
        workflow
            .matches("- name: Finalize package publication lock")
            .count(),
        1
    );
    assert!(workflow.contains("if: ${{ always() }}"));
    assert!(workflow.contains("GH_TOKEN: ${{ github.token }}\n          VELNOR_SOURCE_CHECKOUT_DIR: ${{ github.workspace }}/source"));
    assert!(workflow.contains("        working-directory: source\n        run: |\n          set -euo pipefail\n          mise run 'migrate-preview-legacy'"));
    let publish_job = workflow
        .split_once("  publish:\n")
        .map(|(_, job)| job)
        .expect("publish job");
    let publish_env = publish_job
        .split_once("    concurrency:\n")
        .map(|(env, _)| env)
        .expect("publish job environment");
    assert!(!publish_env.contains("UPDATER_TOKEN"));
    assert!(workflow.contains("token: ${{ secrets.TAP_TOKEN }}"));
    assert_eq!(workflow.matches("${{ secrets.TAP_TOKEN }}").count(), 3);
    assert!(
        workflow.contains("contents: write\n      pull-requests: write\n      attestations: read")
    );
    let _ = fs::remove_dir_all(workspace);
}

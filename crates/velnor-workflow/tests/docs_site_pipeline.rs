//! Documentation-site pipeline contract: one `docs.yml` composes the build,
//! local checks, artifact reuse, Pages deployment, and post-deployment
//! verification from a consumer-owned `[docs]` contract. Scheduled-external
//! live checks never run beside pull-request-local checks, and the Pages
//! handoff retries while validation never does.

#![expect(
    clippy::unwrap_used,
    reason = "a test whose setup fails should panic loudly"
)]
#![expect(
    clippy::expect_used,
    reason = "a test whose setup fails should panic loudly"
)]
#![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn unique_dir(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("docs-site-{name}-{}-{id}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

/// A neutral fixture: one Rust crate, one Markdown guide under lint contract,
/// and a consumer-owned docs contract with every stage declared.
fn write_docs_fixture(root: &Path, schedule: bool) {
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.91.1\"\n",
    )
    .unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/alpha\"]\n",
    )
    .unwrap();
    fs::write(root.join("Cargo.lock"), "version = 3\n").unwrap();
    let src = root.join("crates/alpha/src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        root.join("crates/alpha/Cargo.toml"),
        "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(src.join("lib.rs"), "pub fn n() -> u8 { 1 }\n").unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("docs/guide.md"), "# Guide\n").unwrap();
    fs::write(root.join("README.md"), "# Fixture\n").unwrap();
    fs::write(root.join(".markdownlint-cli2.yaml"), "{}\n").unwrap();
    let external = if schedule {
        "schedule = \"17 4 * * *\"\n\nexternal_link_commands = [\"mise run docs:check-live\"]\n"
    } else {
        ""
    };
    fs::create_dir_all(root.join(".github-gen")).unwrap();
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        format!(
            "schema = 1\n\n[generator]\nrepository = \"example/docs-fixture\"\n\n\
             [workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n\n\
             [docs]\nenabled = true\nreason = \"Example site for pipeline tests\"\n\
             site_url = \"https://docs.example.com\"\nsite_dir = \"site\"\n{external}\
             build_commands = [\"mise run docs:build\"]\n\
             source_link_commands = [\"mise run docs:check-source-links\"]\n\
             site_link_commands = [\"mise run docs:check-site-links\"]\n\
             spell_commands = [\"mise run docs:spell\"]\n\
             verify_commands = [\"mise run docs:verify-deployed\"]\n\n\
             [[declare]]\nprimitive = \"docs-site\"\nfile = \"docs.yml\"\n",
        ),
    )
    .unwrap();
}

fn generate(root: &Path) -> PathBuf {
    let output = root.parent().unwrap().join(format!(
        "{}-out",
        root.file_name().unwrap().to_string_lossy()
    ));
    let _ = fs::remove_dir_all(&output);
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--runners",
            "github",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        outcome.status.success(),
        "generation failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    output
}

fn docs_workflow(output: &Path) -> String {
    let path = output.join(".github/workflows/docs.yml");
    assert!(path.is_file(), "docs.yml renders beside the CI surface");
    fs::read_to_string(path).unwrap()
}

fn job_gate(workflow: &str, job: &str) -> String {
    let block = workflow
        .split(job)
        .nth(1)
        .unwrap_or_else(|| panic!("job {job} renders"));
    block
        .lines()
        .find(|line| line.contains("if:"))
        .unwrap_or_else(|| panic!("job {job} gates on the event"))
        .to_owned()
}

#[test]
fn docs_pipeline_splits_local_and_scheduled_checks() {
    let root = unique_dir("event-split");
    write_docs_fixture(&root, true);
    let output = generate(&root);
    let workflow = docs_workflow(&output);
    for job in ["gate:", "source-links:", "  site:", "  spell:"] {
        let gate = job_gate(&workflow, job);
        assert!(
            gate.contains("github.event_name != 'schedule'"),
            "local job {job} skips the schedule: {gate}"
        );
    }
    let live = job_gate(&workflow, "check-live:");
    assert!(
        live.contains("github.event_name == 'schedule'"),
        "live job runs on the schedule only: {live}"
    );
    assert!(workflow.contains("- cron: \"17 4 * * *\""), "{workflow}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(output);
}

#[test]
fn docs_pipeline_retries_only_the_pages_handoff() {
    let root = unique_dir("deploy-retry");
    write_docs_fixture(&root, true);
    let output = generate(&root);
    let workflow = docs_workflow(&output);
    for attempt in ["deploy-1", "deploy-2", "deploy-3"] {
        assert!(workflow.contains(attempt), "{workflow}");
    }
    assert!(workflow.contains("sleep 30"), "{workflow}");
    assert!(workflow.contains("sleep 60"), "{workflow}");
    assert!(
        workflow.contains("Require a successful Pages deployment"),
        "{workflow}"
    );
    assert!(
        workflow.contains("refused the artifact on all 3 attempts"),
        "{workflow}"
    );
    assert!(workflow.contains("mise run docs:build"), "{workflow}");
    assert!(
        !workflow.contains("scripts/generate-docs.ts"),
        "consumer owns the build command: {workflow}"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(output);
}

#[test]
fn docs_pipeline_verifies_the_deployed_site_and_reuses_artifacts() {
    let root = unique_dir("verify-reuse");
    write_docs_fixture(&root, true);
    let output = generate(&root);
    let workflow = docs_workflow(&output);
    let verify = job_gate(&workflow, "verify-deployed:");
    assert!(
        verify.contains("needs.deploy.result == 'success'"),
        "{verify}"
    );
    assert!(workflow.contains("curl --fail"), "{workflow}");
    assert!(
        workflow.contains("Deployed site never answered"),
        "{workflow}"
    );
    assert!(
        workflow.contains("mise run docs:verify-deployed"),
        "{workflow}"
    );
    assert!(workflow.contains("docs-result-v2-"), "{workflow}");
    assert!(workflow.contains("docs-site-v2-"), "{workflow}");
    assert!(
        !workflow.contains("head.repo.full_name"),
        "no pull-request branch publishes or satisfies reuse: {workflow}"
    );
    let parsed: serde_yaml::Value =
        serde_yaml::from_str(&workflow).expect("docs.yml parses as YAML");
    assert!(parsed.get("jobs").is_some(), "{workflow}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(output);
}

#[test]
fn docs_pipeline_without_schedule_omits_the_live_check() {
    let root = unique_dir("local-only");
    write_docs_fixture(&root, false);
    let output = generate(&root);
    let workflow = docs_workflow(&output);
    assert!(!workflow.contains("schedule:"), "{workflow}");
    assert!(!workflow.contains("check-live"), "{workflow}");
    assert!(workflow.contains("verify-deployed:"), "{workflow}");
    assert!(workflow.contains("  deploy:"), "{workflow}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(output);
}

/// The recipe fingerprint the rendered artifact names key on.
fn result_recipe(workflow: &str) -> String {
    let marker = "docs-result-v2-";
    let start = workflow
        .find(marker)
        .unwrap_or_else(|| panic!("result name keys on the recipe: {workflow}"))
        + marker.len();
    let recipe = workflow[start..]
        .chars()
        .take_while(|character| *character != '-')
        .collect::<String>();
    assert!(
        recipe.len() == 16
            && recipe
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "the recipe is a 16-character lowercase hex fingerprint: {workflow}"
    );
    recipe
}

#[test]
fn docs_reuse_misses_on_a_command_only_change() {
    let root = unique_dir("recipe-miss");
    write_docs_fixture(&root, true);
    let baseline = docs_workflow(&generate(&root));
    let baseline_recipe = result_recipe(&baseline);
    assert!(
        baseline.contains(&format!("docs-site-v2-{baseline_recipe}-")),
        "both artifacts key on one recipe: {baseline}"
    );
    // Same docs inputs, one stricter build command: the recipe — and therefore
    // the lookup names — must move, so the old artifacts can never satisfy
    // the new run.
    let path = root.join(".github-gen/velnor-workflow.toml");
    let config = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        config.replace("mise run docs:build", "mise run docs:build --strict"),
    )
    .unwrap();
    let changed = docs_workflow(&generate(&root));
    let changed_recipe = result_recipe(&changed);
    assert_ne!(
        baseline_recipe, changed_recipe,
        "a command-only change misses reuse"
    );
    // Determinism: the same contract renders the same recipe.
    let replayed = docs_workflow(&generate(&root));
    assert_eq!(
        changed_recipe,
        result_recipe(&replayed),
        "the recipe is stable for one contract"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn docs_reuse_hit_verifies_the_downloaded_bytes() {
    let root = unique_dir("byte-verify");
    write_docs_fixture(&root, true);
    let output = generate(&root);
    let workflow = docs_workflow(&output);
    let gate = workflow
        .split("Look up a prior successful docs result")
        .nth(1)
        .unwrap_or_else(|| panic!("the gate lookup renders: {workflow}"));
    let gate = gate
        .split("source-links:")
        .next()
        .unwrap_or_else(|| panic!("the gate lookup ends: {workflow}"));
    for marker in [
        ".head_branch // empty",
        "/artifacts/${artifact_id}/zip",
        "docs-result.txt",
        "holds unexpected files",
        "cmp -s",
        "hit=true",
    ] {
        assert!(gate.contains(marker), "the gate verifies {marker}: {gate}");
    }
    let position = |marker: &str| {
        gate.find(marker)
            .unwrap_or_else(|| panic!("the gate verifies before it hits: {gate}"))
    };
    assert!(
        position("hit=true") > position("cmp -s"),
        "bytes verify before the hit: {gate}"
    );
    assert!(
        position("cmp -s") > position("/artifacts/${artifact_id}/zip"),
        "bytes download before they verify: {gate}"
    );
    assert!(
        workflow.contains("Write the reuse receipt for this recipe and inputs"),
        "{workflow}"
    );
    assert!(
        workflow.contains("Remove the reuse receipt before staging"),
        "{workflow}"
    );
    assert!(
        workflow
            .find("Remove the reuse receipt before staging")
            .unwrap()
            < workflow.find("Stage the Pages artifact").unwrap(),
        "the receipt never reaches the Pages artifact: {workflow}"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(output);
}

#[test]
fn docs_trusted_reuse_rejects_pull_request_artifacts() {
    let root = unique_dir("trusted-producer");
    write_docs_fixture(&root, true);
    let output = generate(&root);
    let workflow = docs_workflow(&output);
    let mut publish_conditions = 0;
    for line in workflow.lines().filter(|line| {
        line.contains("needs.gate.outputs.result-reuse != 'true' && (")
            || line.contains("steps.built-site.outputs.hit != 'true' && (")
    }) {
        publish_conditions += 1;
        assert!(
            line.contains("github.ref == 'refs/heads/main'"),
            "reuse publishes only on the default branch: {line}"
        );
        assert!(
            !line.contains("pull_request"),
            "pull requests never publish reuse: {line}"
        );
    }
    assert!(
        publish_conditions >= 5,
        "every publish step carries the trusted-producer guard: {workflow}"
    );
    let trusted_checks = workflow
        .lines()
        .filter(|line| line.contains(r#"[ "$branch" != "main" ]"#))
        .count();
    assert!(
        trusted_checks >= 2,
        "both lookups refuse artifacts from other branches: {workflow}"
    );
    let parsed: serde_yaml::Value =
        serde_yaml::from_str(&workflow).expect("docs.yml parses as YAML");
    assert!(parsed.get("jobs").is_some(), "{workflow}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(output);
}

#[test]
fn docs_schedule_without_external_check_fails_generation() {
    let root = unique_dir("invalid-schedule");
    write_docs_fixture(&root, false);
    let path = root.join(".github-gen/velnor-workflow.toml");
    let mut config = fs::read_to_string(&path).unwrap();
    config = config.replace(
        "site_dir = \"site\"\n",
        "site_dir = \"site\"\nschedule = \"17 4 * * *\"\n",
    );
    fs::write(&path, config).unwrap();
    let output = root.parent().unwrap().join(format!(
        "{}-out",
        root.file_name().unwrap().to_string_lossy()
    ));
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--runners",
            "github",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        !outcome.status.success(),
        "a schedule without external links must fail"
    );
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        stderr.contains("without external_link_commands"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(output);
}

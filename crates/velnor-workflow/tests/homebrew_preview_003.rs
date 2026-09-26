//! Git-backed tests for source-head Homebrew preview admission.
//!
//! These tests use temporary repositories and invoke the public CLI. Every
//! decision names the explicit before/head pair passed by the caller.

#![expect(
    clippy::unwrap_used,
    reason = "fixture setup failures should identify the operation"
)]
#![expect(
    clippy::expect_used,
    reason = "fixture setup failures should identify the operation"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
const REPOSITORY: &str = "example/preview-source";
const SOURCE_REF: &str = "refs/heads/main";
const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

struct GitFixture {
    root: PathBuf,
}

impl GitFixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "velnor-homebrew-admission-{label}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fixture repository");
        let fixture = Self { root };
        fixture.git(&["init", "--quiet", "--initial-branch=main"]);
        fixture.git(&["config", "user.name", "Preview test"]);
        fixture.git(&["config", "user.email", "preview-test@example.invalid"]);
        fixture.write(".github-gen/velnor-workflow.toml", &config());
        fixture.write(
            ".github-gen/visibility.toml",
            &format!("repository = \"{REPOSITORY}\"\nvisibility = \"public\"\n"),
        );
        fixture.write(".github/ci/project.toml", CI_CONFIG);
        fixture.write(
            "mise.toml",
            "[tasks.build-release]\nrun = \"true\"\n\n[tasks.verify-release]\nrun = \"true\"\n",
        );
        fixture.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/velnorctl\"]\nresolver = \"3\"\n\n[workspace.package]\nedition = \"2024\"\nlicense = \"MIT\"\n",
        );
        fixture.write(
            "crates/velnorctl/Cargo.toml",
            "[package]\nname = \"velnorctl\"\nversion = \"0.1.0\"\nedition.workspace = true\n",
        );
        fixture.write("rust-toolchain.toml", "[toolchain]\nchannel = \"1.91.1\"\n");
        fixture.write("crates/velnorctl/src/main.rs", "fn main() {}\n");
        fixture.commit_all("fixture base");
        fixture
    }

    fn write(&self, path: &str, contents: &str) {
        let destination = self.root.join(path);
        fs::create_dir_all(destination.parent().expect("fixture file has parent"))
            .expect("create fixture parent");
        fs::write(destination, contents).expect("write fixture file");
    }

    fn remove(&self, path: &str) {
        fs::remove_file(self.root.join(path)).expect("remove fixture file");
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {args:?} failed:\n{}",
            output_text(&output)
        );
        String::from_utf8(output.stdout)
            .expect("git fixture output is UTF-8")
            .trim()
            .to_owned()
    }

    fn commit_all(&self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--message", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }

    fn tree_at(&self, commit: &str) -> String {
        self.git(&["rev-parse", &format!("{commit}^{{tree}}")])
    }

    fn admit(&self, before: &str, head: &str) -> Output {
        self.admit_with_env(before, head, &[])
    }

    fn admit_as(
        &self,
        repository: &str,
        source_ref: &str,
        event: &str,
        before: &str,
        head: &str,
    ) -> Output {
        Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
            .current_dir(&self.root)
            .args([
                "release-admission",
                "--repository",
                repository,
                "--ref",
                source_ref,
                "--event",
                event,
                "--before",
                before,
                "--head",
                head,
            ])
            .output()
            .expect("run release admission for explicit event")
    }

    fn admit_with_env(&self, before: &str, head: &str, env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"));
        command.current_dir(&self.root).args([
            "release-admission",
            "--repository",
            REPOSITORY,
            "--ref",
            SOURCE_REF,
            "--event",
            "push",
            "--before",
            before,
            "--head",
            head,
        ]);
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().expect("run release admission CLI")
    }

    fn plan_ci(&self, before: &str, head: &str) -> String {
        let output_path = self.root.join("ci-plan-output");
        let expected_work_path = self.expected_work_path();
        fs::write(&output_path, "").expect("create CI plan output file");
        let output = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
            .current_dir(&self.root)
            .env("EVENT_NAME", "pull_request")
            .env("BASE_SHA", before)
            .env("HEAD_SHA", head)
            .env("VELNOR_EXPECTED_WORK_FILE", &expected_work_path)
            .env("GITHUB_OUTPUT", &output_path)
            .args(["plan", "--config", ".github/ci/project.toml"])
            .output()
            .expect("run ordinary CI affected planner");
        assert!(
            output.status.success(),
            "ordinary CI planning failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read_to_string(output_path).expect("read ordinary CI plan outputs")
    }

    fn expected_work_path(&self) -> PathBuf {
        self.root
            .join(".velnor-ci-expected-work/expected-work.json")
    }

    fn aggregate_ci_results(&self, before: &str, head: &str, results: &str) -> Output {
        let expected_work_path = self.expected_work_path();
        let results_path = self.root.join("ci-no-work-results.json");
        fs::write(&results_path, results).expect("write aggregate results fixture");
        Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
            .current_dir(&self.root)
            .env("BASE_SHA", before)
            .env("HEAD_SHA", head)
            .args(["aggregate", "--expected"])
            .arg(expected_work_path)
            .arg("--results")
            .arg(results_path)
            .output()
            .expect("run required CI aggregate for a no-work plan")
    }

    fn aggregate_ci_no_work(&self, before: &str, head: &str) -> Output {
        let expected_work = fs::read_to_string(self.expected_work_path())
            .expect("ordinary CI planner writes its expected-work handoff");
        let expected_work: Value =
            serde_json::from_str(&expected_work).expect("expected-work handoff is JSON");
        assert_eq!(expected_work["planned_no_work"], true);
        assert_eq!(expected_work["units"].as_array().map(Vec::len), Some(0));
        self.aggregate_ci_results(before, head, "{\"results\":[]}\n")
    }

    fn render_preview_workflow(&self) -> serde_yaml::Value {
        let _ = fs::remove_file(self.root.join(".github/ci/project.toml"));
        let output = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
            .args([
                "--plain",
                "--force",
                "--default-branch",
                "main",
                self.root.to_str().expect("fixture path is UTF-8"),
            ])
            .output()
            .expect("render package-release workflow fixture");
        assert!(
            output.status.success(),
            "workflow generation failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let workflow = fs::read_to_string(self.root.join(".github/workflows/preview.yml"))
            .expect("read rendered source preview workflow");
        serde_yaml::from_str(&workflow).expect("rendered source workflow is YAML")
    }

    fn run_ci_test(&self, before: &str, head: &str, outputs: &str) -> Output {
        let output_value = |key: &str| {
            outputs
                .lines()
                .find_map(|line| {
                    line.split_once('=')
                        .filter(|(name, _)| *name == key)
                        .map(|(_, value)| value)
                })
                .expect("ordinary CI plan omitted a required output")
        };
        let selection = self.root.join("ci-selection");
        fs::write(
            &selection,
            format!(
                "version=2\nbase_sha={before}\nhead_sha={head}\nscope={}\nunits={}\nfull_units={}\nplan_digest={}\n",
                output_value("scope"),
                output_value("unit_ids"),
                output_value("full_units"),
                output_value("plan_digest"),
            ),
        )
        .expect("write CI selection handoff");
        Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
            .current_dir(&self.root)
            .env("EVENT_NAME", "pull_request")
            .env("BASE_SHA", before)
            .env("HEAD_SHA", head)
            .env("VELNOR_SELECTION_FILE", selection)
            .args([
                "run",
                "--config",
                ".github/ci/project.toml",
                "--scope",
                "affected",
                "--unit",
                "test-contract",
            ])
            .output()
            .expect("run ordinary CI verification unit")
    }
}

impl Drop for GitFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn config() -> String {
    format!(
        r#"schema = 2

[generator]
repository = "{REPOSITORY}"

[workflow]
providers = ["github-hosted"]
automatic_providers = ["github-hosted"]
default_branch = "main"
files = ["ci-pr.yml", "ci-main.yml", "preview.yml"]

[workflow.selectors.github-hosted]
runs_on = ["ubuntu-24.04"]

[[declare]]
primitive = "package-release"
file = "preview.yml"

[declare.args]
build_tasks = ["build-release"]
verify_tasks = ["verify-release"]
publication_lock_branch = "package-release-lock"
package_dir = "dist"
manifest_schema = "velnor.homebrew-preview-v1"
source_repository = "{REPOSITORY}"
source_ref = "{SOURCE_REF}"
payloads = ["velnor.tar.gz"]
supporting_assets = ["SHA256SUMS"]
channel = "preview"
release_tag = "preview"
github_release_type = "prerelease"
publish_environment = "github-preview"
consumer_repository = "example/preview-tap"
consumer_branch = "main"
updater = "./scripts/package-update.sh"
updater_token_secret = "TAP_TOKEN"

[declare.args.production_inputs]
application = ["crates/velnorctl/**"]
embedded-resources = ["share/**"]
test-assets = ["tests/fixtures/embedded/**"]

[declare.args.production_dependencies]
runtime = ["vendor/runtime/**"]

[declare.args.non_production_inputs]
documentation = ["README.md", "docs/**"]
tests = ["tests/**"]
"#
    )
}

const CI_CONFIG: &str = r#"schema = 3
repository = "example/preview-source"
profile = "generic"
verified = true
default_branch = "main"
providers = ["github-hosted"]
automatic_providers = ["github-hosted"]
default_dispatch_providers = ["github-hosted"]

[[unit]]
id = "test-contract"
platform = "linux-x64"
trust = "untrusted-ok"
kind = "rust"
root = "."
watch = ["tests/**"]
pr_commands = ["echo ci-test-contract-ran > .ci-test-contract-ran"]
full_commands = ["echo ci-test-contract-ran > .ci-test-contract-ran"]
"#;

fn parse_success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "release-admission failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("release-admission emits JSON evidence")
}

fn assert_tuple(value: &Value, before: &str, head: &str) {
    assert_eq!(value["schema"], "homebrew-source-release-admission/v1");
    assert_eq!(value["repository"], REPOSITORY);
    assert_eq!(value["source_ref"], SOURCE_REF);
    assert_eq!(value["configured_source_ref"], SOURCE_REF);
    assert_eq!(value["before_sha"], before);
    assert_eq!(value["head_sha"], head);
    assert!(
        value["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "admission result must explain its disposition: {value}"
    );
    assert!(
        value["rules_digest"].as_str().is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        }),
        "admission result must identify its parsed rules: {value}"
    );
}

fn has_match(value: &Value, field: &str, id: &str, path: &str) -> bool {
    value[field]
        .as_array()
        .expect("match field is a JSON array")
        .iter()
        .any(|row| row["id"] == id && row["path"] == path)
}

fn changed(value: &Value, path: &str, previous: Option<&str>, status: &str) -> bool {
    value["changed_paths"]
        .as_array()
        .expect("changed_paths is a JSON array")
        .iter()
        .any(|row| {
            row["path"] == path
                && row["status"] == status
                && match previous {
                    Some(previous) => row["previous"] == previous,
                    None => row["previous"].is_null(),
                }
        })
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn job_step<'a>(job: &'a serde_yaml::Value, name: &str) -> &'a serde_yaml::Value {
    job["steps"]
        .as_sequence()
        .expect("generated job has a steps sequence")
        .iter()
        .find(|step| step["name"].as_str() == Some(name))
        .expect("generated job contains the requested named step")
}

fn assert_required_ci_no_work_surface(fixture: &GitFixture, file: &str, guard: &str) {
    let content = fs::read_to_string(fixture.root.join(".github/workflows").join(file))
        .expect("generation preserves the ordinary CI workflow");
    let workflow: serde_yaml::Value =
        serde_yaml::from_str(&content).expect("generated CI workflow is YAML");
    let required = &workflow["jobs"]["ci-required"];
    assert!(
        required.is_mapping(),
        "{file} has the required aggregate job"
    );
    assert_eq!(required["if"].as_str(), Some(guard));
    assert!(
        required["needs"]
            .as_sequence()
            .expect("required aggregate declares job dependencies")
            .iter()
            .any(|need| need.as_str() == Some("plan")),
        "{file} required aggregate depends on the plan even when its plan job skips"
    );

    let expected = job_step(required, "Download expected work");
    assert_eq!(
        expected["with"]["path"].as_str(),
        Some(".velnor-ci-expected-work")
    );
    let reported = job_step(required, "Download reported unit results");
    assert_eq!(reported["continue-on-error"].as_bool(), Some(true));
    assert_eq!(
        reported["with"]["path"].as_str(),
        Some(".velnor-ci-results")
    );
    let collect = job_step(required, "Collect reported unit results");
    let collect_script = collect["run"]
        .as_str()
        .expect("result collection runs a shell script");
    assert!(collect_script.contains("if (( ${#files[@]} == 0 )); then"));
    assert!(collect_script.contains("printf '{\"results\":[]}\\n'"));
    let score = job_step(required, "Score expected work against reported results");
    assert_eq!(
        score["env"]["BASE_SHA"].as_str(),
        Some("${{ needs.plan.outputs.base_sha }}")
    );
    assert_eq!(
        score["env"]["HEAD_SHA"].as_str(),
        Some("${{ needs.plan.outputs.head_sha }}")
    );
    assert!(score["run"]
        .as_str()
        .expect("aggregate verdict invokes its runtime")
        .contains("velnor-workflow aggregate --expected .velnor-ci-expected-work/expected-work.json --results .velnor-ci-results.json"));
}

fn assert_preview_trigger(workflow: &serde_yaml::Value) {
    let trigger = &workflow["on"];
    assert!(trigger["push"]["branches"]
        .as_sequence()
        .expect("preview workflow push trigger has branch list")
        .iter()
        .any(|branch| branch.as_str() == Some("main")));
    assert!(
        trigger.get("workflow_dispatch").is_some(),
        "preview workflow supports explicit workflow_dispatch"
    );
}

fn assert_admission_job_contract(workflow: &serde_yaml::Value) {
    let admission = &workflow["jobs"]["admission"];
    assert_eq!(
        admission["if"].as_str(),
        Some("github.event_name == 'push' || github.event_name == 'workflow_dispatch'")
    );
    assert_eq!(admission["permissions"]["contents"].as_str(), Some("read"));
    for (output, expected) in [
        ("disposition", "${{ steps.admit.outputs.disposition }}"),
        ("head_sha", "${{ steps.admit.outputs.head_sha }}"),
        ("head_tree", "${{ steps.admit.outputs.head_tree }}"),
    ] {
        assert_eq!(
            admission["outputs"][output].as_str(),
            Some(expected),
            "admission job must export classifier {output}"
        );
    }
    let checkout = job_step(admission, "Checkout exact event head and full history");
    assert_eq!(checkout["with"]["ref"].as_str(), Some("${{ github.sha }}"));
    assert_eq!(checkout["with"]["fetch-depth"].as_i64(), Some(0));
    assert_eq!(
        checkout["with"]["persist-credentials"].as_bool(),
        Some(false)
    );
}

fn assert_admission_classifier_contract(workflow: &serde_yaml::Value) {
    let admission = &workflow["jobs"]["admission"];
    let classify = job_step(admission, "Classify complete source push diff");
    for (key, expected) in [
        ("EVENT_REPOSITORY", "${{ github.repository }}"),
        ("EVENT_REF", "${{ github.ref }}"),
        ("EVENT_NAME", "${{ github.event_name }}"),
        ("BEFORE_SHA", "${{ github.event.before || github.sha }}"),
        ("HEAD_SHA", "${{ github.sha }}"),
        ("EXPECTED_SOURCE_REPOSITORY", REPOSITORY),
        ("EXPECTED_SOURCE_REF", SOURCE_REF),
    ] {
        assert_eq!(
            classify["env"][key].as_str(),
            Some(expected),
            "classifier input {key} must retain its event/config binding"
        );
    }
    let script = classify["run"]
        .as_str()
        .expect("admission classifier runs a shell script");
    assert!(script.contains("disposition=\"$(jq -er '.disposition' \"$result_file\")\""));
    assert!(script.contains("printf 'disposition=%s\\n' \"$disposition\" >> \"$GITHUB_OUTPUT\""));
    assert!(script.contains(
        "printf 'head_sha=%s\\n' \"$(jq -er '.head_sha' \"$result_file\")\" >> \"$GITHUB_OUTPUT\""
    ));
    assert!(script.contains(
        "printf 'head_tree=%s\\n' \"$(jq -er '.head_tree' \"$result_file\")\" >> \"$GITHUB_OUTPUT\""
    ));
    assert!(script.contains("result_file=\"$RUNNER_TEMP/package-release-admission.json\""));
}

fn assert_admission_artifact_contract(workflow: &serde_yaml::Value) {
    let admission = &workflow["jobs"]["admission"];
    let step = job_step(admission, "Retain source admission evidence");
    let path = step["with"]["path"]
        .as_str()
        .expect("admission evidence upload has a path");
    assert_eq!(
        path, "${{ runner.temp }}/package-release-admission.json",
        "artifact upload paths use GitHub expressions, not shell-only variable expansion"
    );
    assert!(!path.contains("$RUNNER_TEMP"));
    assert!(step["uses"]
        .as_str()
        .is_some_and(|action| action.starts_with("actions/upload-artifact@")));
    assert_eq!(step["with"]["if-no-files-found"].as_str(), Some("error"));
    assert_eq!(
        step["with"]["name"].as_str(),
        Some("source-release-admission-${{ github.sha }}")
    );
}

fn assert_build_job_contract(workflow: &serde_yaml::Value) {
    let build = &workflow["jobs"]["build"];
    assert_eq!(build["needs"].as_str(), Some("admission"));
    let gate = build["if"].as_str().expect("build has an admission gate");
    assert!(gate.contains("needs.admission.outputs.disposition == 'admit'"));
    assert!(gate.contains("github.ref == 'refs/heads/main'"));
    assert_eq!(build["permissions"]["contents"].as_str(), Some("read"));
    let checkout = job_step(build, "Checkout source");
    assert_eq!(
        checkout["with"]["ref"].as_str(),
        Some("${{ needs.admission.outputs.head_sha }}")
    );
    assert_eq!(checkout["with"]["fetch-depth"].as_i64(), Some(0));
    assert_eq!(
        checkout["with"]["persist-credentials"].as_bool(),
        Some(false)
    );
    assert_eq!(
        build["env"]["EXPECTED_SOURCE_TREE"].as_str(),
        Some("${{ needs.admission.outputs.head_tree }}")
    );
    let tree_check = job_step(build, "Verify admitted source tree")["run"]
        .as_str()
        .expect("source tree check runs a shell script");
    assert!(tree_check.contains("git rev-parse HEAD^{tree}"));
    assert!(tree_check.contains("[[ \"$actual_tree\" != \"$EXPECTED_SOURCE_TREE\" ]]"));
    assert!(tree_check.contains("exit 1"));
    assert!(!serde_yaml::to_string(build)
        .expect("serialize build job")
        .contains("TAP_TOKEN"));
}

fn assert_publisher_token_scope(workflow: &serde_yaml::Value) {
    let admission_text =
        serde_yaml::to_string(&workflow["jobs"]["admission"]).expect("serialize admission job");
    assert!(!admission_text.contains("TAP_TOKEN"));
    let publish = &workflow["jobs"]["publish"];
    assert_eq!(publish["needs"].as_str(), Some("build"));
    assert!(!serde_yaml::to_string(&publish["env"])
        .expect("serialize publish job environment")
        .contains("TAP_TOKEN"));
    let steps = publish["steps"]
        .as_sequence()
        .expect("publish job has steps");
    let checkout = job_step(publish, "Checkout consumer repository");
    assert_eq!(
        checkout["with"]["token"].as_str(),
        Some("${{ secrets.TAP_TOKEN }}")
    );
    assert_eq!(
        checkout["with"]["persist-credentials"].as_bool(),
        Some(false),
        "the tap token must not persist in checkout Git configuration"
    );
    let updater = job_step(publish, "Run updater and create or update consumer PR");
    for key in ["GH_TOKEN", "UPDATER_TOKEN"] {
        assert_eq!(
            updater["env"][key].as_str(),
            Some("${{ secrets.TAP_TOKEN }}"),
            "tap credential must be limited to consumer updater input {key}"
        );
    }
    for step in steps {
        if step["name"].as_str() != Some("Checkout consumer repository")
            && step["name"].as_str() != Some("Run updater and create or update consumer PR")
        {
            assert!(!serde_yaml::to_string(step)
                .expect("serialize publish step")
                .contains("TAP_TOKEN"));
        }
    }
    assert_eq!(
        serde_yaml::to_string(publish)
            .expect("serialize publish job")
            .matches("secrets.TAP_TOKEN")
            .count(),
        3
    );
    assert_eq!(
        serde_yaml::to_string(workflow)
            .expect("serialize complete workflow")
            .matches("secrets.TAP_TOKEN")
            .count(),
        3,
        "only consumer checkout and updater GH_TOKEN/UPDATER_TOKEN expose TAP_TOKEN"
    );
}

fn assert_generated_workflow_contract(fixture: &GitFixture) {
    let workflow = fixture.render_preview_workflow();
    assert_preview_trigger(&workflow);
    assert_admission_job_contract(&workflow);
    assert_admission_classifier_contract(&workflow);
    assert_admission_artifact_contract(&workflow);
    assert_build_job_contract(&workflow);
    assert_publisher_token_scope(&workflow);
    assert_required_ci_no_work_surface(fixture, "ci-pr.yml", "${{ !cancelled() }}");
    assert_required_ci_no_work_surface(fixture, "ci-main.yml", "${{ always() }}");
}

fn assert_nonqualifying_events(fixture: &GitFixture, before: &str, head: &str) {
    let wrong_repository = fixture.admit_as("example/other", SOURCE_REF, "push", before, head);
    assert!(!wrong_repository.status.success());
    let other_ref =
        parse_success(&fixture.admit_as(REPOSITORY, "refs/heads/feature", "push", before, head));
    assert_eq!(other_ref["disposition"], "skip");
    assert_eq!(other_ref["source_ref"], "refs/heads/feature");
    assert_eq!(other_ref["configured_source_ref"], SOURCE_REF);
    assert!(other_ref["changed_paths"]
        .as_array()
        .is_some_and(Vec::is_empty));
    let manual =
        parse_success(&fixture.admit_as(REPOSITORY, SOURCE_REF, "workflow_dispatch", before, head));
    assert_eq!(manual["disposition"], "skip");
    assert_eq!(manual["source_ref"], SOURCE_REF);
    assert_eq!(manual["configured_source_ref"], SOURCE_REF);
    assert!(manual["changed_paths"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert!(manual["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("not a qualifying push")));
}

fn assert_initial_push_is_admitted() {
    let initial = GitFixture::new("initial-push");
    initial.write(
        "crates/velnorctl/src/main.rs",
        "fn main() { println!(\"initial push\"); }\n",
    );
    let head = initial.commit_all("first source head");
    let value = parse_success(&initial.admit(ZERO_SHA, &head));
    assert_tuple(&value, ZERO_SHA, &head);
    assert_eq!(value["head_tree"], initial.tree_at(&head));
    assert_eq!(value["disposition"], "admit");
    assert!(changed(
        &value,
        "crates/velnorctl/src/main.rs",
        None,
        "added"
    ));
    assert!(has_match(
        &value,
        "matched_rules",
        "application",
        "crates/velnorctl/src/main.rs"
    ));
}

#[test]
fn production_edit_is_admitted() {
    let fixture = GitFixture::new("production-edit");
    let before = fixture.head();
    fixture.write(
        "crates/velnorctl/src/main.rs",
        "fn main() { println!(\"preview\"); }\n",
    );
    // A commit-message keyword cannot turn a production edit into a skip.
    let head = fixture.commit_all("docs: skip preview even though source changed");
    let command_output = fixture.admit_with_env(
        &before,
        &head,
        &[("TAP_TOKEN", "fixture-secret-must-not-leak")],
    );
    assert!(!output_text(&command_output).contains("fixture-secret-must-not-leak"));
    let value = parse_success(&command_output);
    assert_tuple(&value, &before, &head);
    assert_eq!(value["head_tree"], fixture.tree_at(&head));
    assert_eq!(value["disposition"], "admit");
    assert!(changed(
        &value,
        "crates/velnorctl/src/main.rs",
        None,
        "modified"
    ));
    assert!(has_match(
        &value,
        "matched_rules",
        "application",
        "crates/velnorctl/src/main.rs"
    ));
    assert_generated_workflow_contract(&fixture);
    assert_nonqualifying_events(&fixture, &before, &head);
    assert_initial_push_is_admitted();
}

fn assert_docs_only_skip() {
    let docs = GitFixture::new("docs-only");
    let docs_before = docs.head();
    docs.write("README.md", "Updated setup guide.\n");
    let docs_head = docs.commit_all("update documentation only");
    let docs_admission = parse_success(&docs.admit(&docs_before, &docs_head));
    assert_tuple(&docs_admission, &docs_before, &docs_head);
    assert_eq!(docs_admission["disposition"], "skip");
    assert!(changed(&docs_admission, "README.md", None, "added"));
    assert!(has_match(
        &docs_admission,
        "matched_non_production",
        "documentation",
        "README.md"
    ));
    assert!(docs_admission["matched_rules"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(docs_admission["matched_dependencies"]
        .as_array()
        .unwrap()
        .is_empty());

    // Docs-only pushes have explicit successful no-work planning, so the
    // independent CI aggregate can report success while publication skips.
    let docs_ci = docs.plan_ci(&docs_before, &docs_head);
    let docs_unit_ids = docs_ci
        .lines()
        .find_map(|line| line.strip_prefix("unit_ids="))
        .expect("docs CI plan reports selected unit ids");
    assert!(
        docs_unit_ids.is_empty(),
        "docs-only changes should produce explicit no-work CI planning: {docs_ci}"
    );
    assert!(
        docs_ci.lines().any(|line| line.starts_with("plan_digest=")),
        "successful docs-only planning must report its plan identity: {docs_ci}"
    );
    let docs_required = docs.aggregate_ci_no_work(&docs_before, &docs_head);
    assert!(
        docs_required.status.success(),
        "docs-only required aggregate failed:\n{}{}",
        String::from_utf8_lossy(&docs_required.stdout),
        String::from_utf8_lossy(&docs_required.stderr)
    );
    assert!(
        String::from_utf8_lossy(&docs_required.stdout).contains(
            "no_work_reason=planner selected zero workload units and no workload results were reported"
        ),
        "docs-only aggregate must report its successful no-work verdict"
    );
}

fn assert_tests_only_skip_but_run_ci() {
    let tests = GitFixture::new("tests-only");
    let tests_before = tests.head();
    tests.write("tests/admission.rs", "#[test] fn changed() {}\n");
    let tests_head = tests.commit_all("update test code only");
    let tests_admission = parse_success(&tests.admit(&tests_before, &tests_head));
    assert_tuple(&tests_admission, &tests_before, &tests_head);
    assert_eq!(tests_admission["disposition"], "skip");
    assert!(changed(
        &tests_admission,
        "tests/admission.rs",
        None,
        "added"
    ));
    assert!(has_match(
        &tests_admission,
        "matched_non_production",
        "tests",
        "tests/admission.rs"
    ));

    // The same test-only change still selects and runs ordinary affected CI.
    let ci_outputs = tests.plan_ci(&tests_before, &tests_head);
    let unit_ids = ci_outputs
        .lines()
        .find_map(|line| line.strip_prefix("unit_ids="))
        .expect("ordinary CI plan reports selected unit ids");
    assert!(
        unit_ids.split(',').any(|unit| unit == "test-contract"),
        "test-only changes must keep the CI verification unit selected: {ci_outputs}"
    );
    let ci_run = tests.run_ci_test(&tests_before, &tests_head, &ci_outputs);
    assert!(
        ci_run.status.success(),
        "ordinary CI test unit did not run successfully:\n{}{}",
        String::from_utf8_lossy(&ci_run.stdout),
        String::from_utf8_lossy(&ci_run.stderr)
    );
    assert_eq!(
        fs::read_to_string(tests.root.join(".ci-test-contract-ran"))
            .expect("test-only CI command leaves its execution marker"),
        "ci-test-contract-ran\n",
        "selected test-only CI command must execute"
    );
    let expected_work: Value = serde_json::from_slice(
        &fs::read(tests.expected_work_path()).expect("read tests-only expected-work handoff"),
    )
    .expect("tests-only expected-work handoff is JSON");
    assert_eq!(expected_work["planned_no_work"], false);
    assert!(expected_work["units"]
        .as_array()
        .is_some_and(|units| { units.iter().any(|unit| unit["id"] == "test-contract") }));
    let tests_required = tests.aggregate_ci_results(
        &tests_before,
        &tests_head,
        "{\"results\":[{\"unit\":\"test-contract\",\"lane\":\"github-hosted\",\"outcome\":\"success\"}]}\n",
    );
    assert!(
        tests_required.status.success(),
        "tests-only required aggregate failed:\n{}{}",
        String::from_utf8_lossy(&tests_required.stdout),
        String::from_utf8_lossy(&tests_required.stderr)
    );
}

#[test]
fn docs_and_tests_only_are_skipped() {
    assert_docs_only_skip();
    assert_tests_only_skip_but_run_ci();
}

#[test]
fn complete_diff_handles_renames_and_large_pushes() {
    let fixture = GitFixture::new("large-diff");
    fixture.write(
        "crates/velnorctl/src/rename-source.rs",
        "pub fn stable() {}\n",
    );
    fixture.write("crates/velnorctl/src/delete-me.rs", "pub fn removed() {}\n");
    let before = fixture.commit_all("seed rename and delete paths");

    fixture.git(&[
        "mv",
        "crates/velnorctl/src/rename-source.rs",
        "crates/velnorctl/src/renamed-target.rs",
    ]);
    fixture.remove("crates/velnorctl/src/delete-me.rs");
    let path_with_control_bytes = "crates/velnorctl/src/tab\tname\nwith-newline.rs";
    fixture.write(path_with_control_bytes, "pub fn unusual_name() {}\n");
    // More than GitHub's 300-path filter limit.
    for index in 0..305 {
        fixture.write(
            &format!("crates/velnorctl/src/bulk-{index:03}.rs"),
            &format!("pub const ITEM_{index}: usize = {index};\n"),
        );
    }
    fixture.commit_all("large source change one");
    fixture.write("vendor/runtime/abi.txt", "runtime ABI v2\n");
    let head = fixture.commit_all("large source change two");

    let value = parse_success(&fixture.admit(&before, &head));
    assert_tuple(&value, &before, &head);
    assert_eq!(value["disposition"], "admit");
    let records = value["changed_paths"]
        .as_array()
        .expect("changed_paths is a JSON array");
    assert!(
        records.len() > 300,
        "expected full diff, got {}",
        records.len()
    );
    assert!(changed(
        &value,
        "crates/velnorctl/src/renamed-target.rs",
        Some("crates/velnorctl/src/rename-source.rs"),
        "renamed"
    ));
    assert!(changed(
        &value,
        "crates/velnorctl/src/delete-me.rs",
        None,
        "deleted"
    ));
    for index in [0, 150, 304] {
        let path = format!("crates/velnorctl/src/bulk-{index:03}.rs");
        assert!(changed(&value, &path, None, "added"), "missing {path}");
    }
    assert!(changed(&value, path_with_control_bytes, None, "added"));
    assert!(has_match(
        &value,
        "matched_rules",
        "application",
        path_with_control_bytes
    ));
    assert!(changed(&value, "vendor/runtime/abi.txt", None, "added"));
}

#[test]
fn embedded_resource_and_dependency_are_admitted() {
    let fixture = GitFixture::new("resources-and-dependencies");
    let before = fixture.head();
    fixture.write("share/templates/help.md", "Embedded runtime help.\n");
    fixture.write(
        "tests/fixtures/embedded/runtime.md",
        "A Markdown resource copied into the package.\n",
    );
    fixture.write("vendor/runtime/libvelnor.so", "fixture runtime bytes\n");
    let head = fixture.commit_all("change embedded runtime inputs");

    let value = parse_success(&fixture.admit(&before, &head));
    assert_tuple(&value, &before, &head);
    assert_eq!(value["disposition"], "admit");
    assert!(has_match(
        &value,
        "matched_rules",
        "embedded-resources",
        "share/templates/help.md"
    ));
    assert!(has_match(
        &value,
        "matched_rules",
        "test-assets",
        "tests/fixtures/embedded/runtime.md"
    ));
    assert!(has_match(
        &value,
        "matched_dependencies",
        "runtime",
        "vendor/runtime/libvelnor.so"
    ));
    assert!(changed(&value, "share/templates/help.md", None, "added"));
    assert!(changed(
        &value,
        "vendor/runtime/libvelnor.so",
        None,
        "added"
    ));

    // Production classification takes precedence over the test-only rule for
    // the same path. This rejects blanket tests-directory suppression.
    let test_asset = GitFixture::new("production-test-resource");
    let test_asset_before = test_asset.head();
    test_asset.write(
        "tests/fixtures/embedded/runtime.md",
        "This file is copied into the shipped runtime.\n",
    );
    let test_asset_head = test_asset.commit_all("change production resource under tests");
    let test_asset_result = parse_success(&test_asset.admit(&test_asset_before, &test_asset_head));
    assert_eq!(test_asset_result["disposition"], "admit");
    assert!(has_match(
        &test_asset_result,
        "matched_rules",
        "test-assets",
        "tests/fixtures/embedded/runtime.md"
    ));
    assert!(has_match(
        &test_asset_result,
        "matched_non_production",
        "tests",
        "tests/fixtures/embedded/runtime.md"
    ));

    // No other production path changes here, so this independently proves
    // that a production dependency match admits the candidate.
    let dependency = GitFixture::new("dependency-only");
    let dependency_before = dependency.head();
    dependency.write("vendor/runtime/libvelnor.so", "changed dependency bytes\n");
    let dependency_head = dependency.commit_all("change runtime dependency only");
    let dependency_result = parse_success(&dependency.admit(&dependency_before, &dependency_head));
    assert_eq!(dependency_result["disposition"], "admit");
    assert!(has_match(
        &dependency_result,
        "matched_dependencies",
        "runtime",
        "vendor/runtime/libvelnor.so"
    ));
    assert!(dependency_result["matched_rules"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn old_admitted_sha_remains_valid() {
    let fixture = GitFixture::new("replay-stable");
    let before = fixture.head();
    fixture.write(
        "crates/velnorctl/src/main.rs",
        "fn main() { println!(\"A\"); }\n",
    );
    let admitted_head = fixture.commit_all("candidate A");
    let admitted = parse_success(&fixture.admit(&before, &admitted_head));
    assert_eq!(admitted["disposition"], "admit");

    // Advance main, then replay A from its immutable checkout and receipt.
    fixture.write("README.md", "main advances to B\n");
    let main_head = fixture.commit_all("advance main to B");
    assert_ne!(main_head, admitted_head);
    fixture.git(&["checkout", "--quiet", "--detach", &admitted_head]);
    assert_eq!(fixture.head(), admitted_head);

    let receipt = fixture.root.join("admitted-A.json");
    fs::write(
        &receipt,
        serde_json::to_vec(&admitted).expect("serialize admitted result"),
    )
    .expect("write replay receipt");
    let run_replay = |receipt_path: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
            .current_dir(&fixture.root)
            .args([
                "release-admission",
                "--repository",
                REPOSITORY,
                "--ref",
                SOURCE_REF,
                "--event",
                "push",
                "--before",
                &before,
                "--head",
                &admitted_head,
                "--replay-from",
            ])
            .arg(receipt_path)
            .output()
            .expect("run release-admission replay")
    };
    let replay = run_replay(&receipt);
    let replayed = parse_success(&replay);
    assert_eq!(replayed, admitted, "replay must preserve admitted identity");
    assert_tuple(&replayed, &before, &admitted_head);
    assert_eq!(replayed["head_tree"], fixture.tree_at(&admitted_head));
    assert_eq!(replayed["head_sha"], admitted_head);
    assert_ne!(replayed["head_sha"], main_head);

    // A changed recorded disposition cannot be replayed under the old event.
    let mut tampered = admitted.clone();
    tampered["disposition"] = Value::String("skip".to_owned());
    fs::write(
        &receipt,
        serde_json::to_vec(&tampered).expect("serialize tampered admission"),
    )
    .expect("write tampered replay receipt");
    let tampered_replay = run_replay(&receipt);
    assert!(
        !tampered_replay.status.success(),
        "a modified replay receipt must be rejected"
    );

    for (field, replacement) in [
        ("head_tree", Value::String(fixture.tree_at(&main_head))),
        ("rules_digest", Value::String("0".repeat(64))),
    ] {
        let mut tampered = admitted.clone();
        tampered[field] = replacement;
        fs::write(
            &receipt,
            serde_json::to_vec(&tampered).expect("serialize modified replay identity"),
        )
        .expect("write modified replay identity");
        let rejected = run_replay(&receipt);
        assert!(
            !rejected.status.success(),
            "tampered replay field {field} must be rejected"
        );
    }

    let mut extra_top_level = admitted.clone();
    extra_top_level["unexpected_field"] = Value::String("untrusted extension".to_owned());
    fs::write(
        &receipt,
        serde_json::to_vec(&extra_top_level).expect("serialize replay with extra top-level field"),
    )
    .expect("write replay with extra top-level field");
    assert!(
        !run_replay(&receipt).status.success(),
        "unknown top-level replay fields must be rejected"
    );

    let mut extra_nested = admitted.clone();
    extra_nested["changed_paths"][0]["unexpected_field"] =
        Value::String("untrusted extension".to_owned());
    fs::write(
        &receipt,
        serde_json::to_vec(&extra_nested).expect("serialize replay with extra path field"),
    )
    .expect("write replay with extra path field");
    assert!(
        !run_replay(&receipt).status.success(),
        "unknown nested path fields must be rejected"
    );
}

#[test]
fn invalid_rules_fail_closed() {
    let fixture = GitFixture::new("invalid-rule");
    let invalid = config().replace(
        "application = [\"crates/velnorctl/**\"]",
        "application = [\"../outside/**\"]",
    );
    fixture.write(".github-gen/velnor-workflow.toml", &invalid);
    let head = fixture.commit_all("install unsafe release glob");
    let bad_rules = fixture.admit(ZERO_SHA, &head);
    assert!(
        !bad_rules.status.success(),
        "unsafe release rules must fail closed"
    );
    let bad_rules_text = output_text(&bad_rules);
    assert!(
        bad_rules_text.contains("unsafe") || bad_rules_text.contains("relative"),
        "invalid rule rejection should explain the error: {bad_rules_text}"
    );

    let conflicting = config().replace(
        "[declare.args.non_production_inputs]",
        "[declare.args.non_production_inputs]\napplication-shadow = [\"crates/velnorctl/**\"]",
    );
    fixture.write(".github-gen/velnor-workflow.toml", &conflicting);
    let conflict_head = fixture.commit_all("install contradictory release rules");
    let conflicting_result = fixture.admit(&head, &conflict_head);
    assert!(
        !conflicting_result.status.success(),
        "production and non-production rule overlap must fail closed"
    );

    let valid_rules = GitFixture::new("malformed-sha");
    let valid_rules_head = valid_rules.head();
    let malformed_sha = valid_rules.admit("not-a-full-sha", &valid_rules_head);
    assert!(
        !malformed_sha.status.success(),
        "malformed before SHA must not become an admission result"
    );
    valid_rules.write("README.md", "Advance the checked-out candidate.\n");
    let checked_out_head = valid_rules.commit_all("advance checkout for wrong-head test");
    assert_ne!(checked_out_head, valid_rules_head);
    assert_eq!(valid_rules_head.len(), 40);
    let wrong_head = valid_rules.admit_as(
        REPOSITORY,
        SOURCE_REF,
        "push",
        &valid_rules_head,
        &valid_rules_head,
    );
    assert!(
        !wrong_head.status.success(),
        "valid-format event head that differs from checked-out HEAD must fail closed"
    );
    assert!(output_text(&wrong_head).contains("does not match event head"));

    // A shallow checkout missing the exact before object must fail; it cannot
    // silently substitute the current branch tip.
    let full = GitFixture::new("shallow-history-source");
    full.write("crates/velnorctl/src/lib.rs", "pub fn api() {}\n");
    let base = full.commit_all("source base");
    full.write("crates/velnorctl/src/lib.rs", "pub fn api() { }\n");
    let shallow_head = full.commit_all("source head");
    let shallow_success = full.root.with_extension("shallow-two");
    let positive_clone = Command::new("git")
        .args([
            "clone",
            "--quiet",
            "--depth=2",
            "--no-local",
            full.root.to_str().expect("fixture path is UTF-8"),
            shallow_success.to_str().expect("clone path is UTF-8"),
        ])
        .output()
        .expect("create two-commit shallow source fixture");
    assert!(
        positive_clone.status.success(),
        "positive shallow clone failed: {}",
        output_text(&positive_clone)
    );
    let positive_shallow_marker = Command::new("git")
        .current_dir(&shallow_success)
        .args(["rev-parse", "--is-shallow-repository"])
        .output()
        .expect("inspect two-commit shallow repository");
    assert!(positive_shallow_marker.status.success());
    assert_eq!(
        String::from_utf8_lossy(&positive_shallow_marker.stdout).trim(),
        "true"
    );
    let available_history = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .current_dir(&shallow_success)
        .args([
            "release-admission",
            "--repository",
            REPOSITORY,
            "--ref",
            SOURCE_REF,
            "--event",
            "push",
            "--before",
            &base,
            "--head",
            &shallow_head,
        ])
        .output()
        .expect("admit push with exact history available in shallow clone");
    let available_result = parse_success(&available_history);
    assert_eq!(available_result["disposition"], "admit");
    assert!(changed(
        &available_result,
        "crates/velnorctl/src/lib.rs",
        None,
        "modified"
    ));
    let _ = fs::remove_dir_all(shallow_success);

    let shallow = full.root.with_extension("shallow-clone");
    let clone = Command::new("git")
        .args([
            "clone",
            "--quiet",
            "--depth=1",
            "--no-local",
            full.root.to_str().expect("fixture path is UTF-8"),
            shallow.to_str().expect("clone path is UTF-8"),
        ])
        .output()
        .expect("create shallow Git fixture");
    assert!(
        clone.status.success(),
        "shallow clone failed: {}",
        output_text(&clone)
    );
    let shallow_marker = Command::new("git")
        .current_dir(&shallow)
        .args(["rev-parse", "--is-shallow-repository"])
        .output()
        .expect("inspect shallow repository");
    assert!(shallow_marker.status.success());
    assert_eq!(
        String::from_utf8_lossy(&shallow_marker.stdout).trim(),
        "true"
    );
    let unavailable_before = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .current_dir(&shallow)
        .args([
            "release-admission",
            "--repository",
            REPOSITORY,
            "--ref",
            SOURCE_REF,
            "--event",
            "push",
            "--before",
            &base,
            "--head",
            &shallow_head,
        ])
        .output()
        .expect("run admission with unavailable shallow before SHA");
    assert!(
        !unavailable_before.status.success(),
        "unavailable before SHA must fail, not fall back to main"
    );
    let unavailable_text = output_text(&unavailable_before);
    assert!(
        unavailable_text.contains("before") || unavailable_text.contains("history"),
        "missing history error should identify the before SHA: {unavailable_text}"
    );
    let _ = fs::remove_dir_all(shallow);
}

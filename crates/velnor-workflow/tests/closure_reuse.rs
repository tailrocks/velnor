//! Slice C through the binary: `velnor-workflow aggregate` scores reported
//! results against the planner's expected work, accepts explicit planned
//! no-work, and rejects unexpected skips and every other uncovered item.

use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

/// The plan identity every fixture binds: the harness stamps it into the
/// expected-work file and the aggregate child verifies it against the same
/// checkout SHAs, independent of the outer environment.
const FIXTURE_BASE: &str = "closure-reuse-base";
const FIXTURE_HEAD: &str = "closure-reuse-head";

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(expected: &str, results: &str) -> Result<Self, Box<dyn Error>> {
        let root = env::temp_dir().join(format!(
            "velnor-closure-reuse-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root)?;
        let mut document: serde_json::Value = serde_json::from_str(expected)?;
        document["base_sha"] = serde_json::Value::String(FIXTURE_BASE.to_owned());
        document["head_sha"] = serde_json::Value::String(FIXTURE_HEAD.to_owned());
        fs::write(
            root.join("expected.json"),
            serde_json::to_string(&document)?,
        )?;
        fs::write(root.join("results.json"), results)?;
        Ok(Self { root })
    }

    fn aggregate(&self) -> Result<Output, Box<dyn Error>> {
        Ok(Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
            .current_dir(&self.root)
            .env("BASE_SHA", FIXTURE_BASE)
            .env("HEAD_SHA", FIXTURE_HEAD)
            .args([
                "aggregate",
                "--expected",
                "expected.json",
                "--results",
                "results.json",
            ])
            .output()?)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn planned_no_work_is_accepted() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(
        r#"{"planned_no_work": true, "units": []}"#,
        r#"{"results": []}"#,
    )?;
    let output = fixture.aggregate()?;
    assert!(output.status.success(), "{}", output_text(&output));
    assert!(output_text(&output).starts_with("aggregate: PASS\n"));
    Ok(())
}

#[test]
fn unmarked_empty_expected_work_is_rejected() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(r#"{"units": []}"#, r#"{"results": []}"#)?;
    let output = fixture.aggregate()?;
    assert!(!output.status.success());
    assert!(output_text(&output).contains("no-work marker"));
    Ok(())
}

#[test]
fn unexpected_skip_is_rejected() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(
        r#"{"units": [{"id": "rust-alpha", "lanes": ["github"]}]}"#,
        r#"{"results": [
            {"unit": "rust-alpha", "lane": "github", "outcome": "skipped", "reason": "lane gate closed"}
        ]}"#,
    )?;
    let output = fixture.aggregate()?;
    assert!(!output.status.success());
    let text = output_text(&output);
    assert!(text.contains("unexpected skip of rust-alpha github"));
    assert!(text.contains("aggregate: FAIL"));
    Ok(())
}

#[test]
fn planned_skip_is_accepted() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(
        r#"{"units": [
            {"id": "rust-alpha", "lanes": ["github"], "planned_skip": "lane cannot run kind"}
        ]}"#,
        r#"{"results": [
            {"unit": "rust-alpha", "lane": "github", "outcome": "skipped", "reason": "lane gate closed"}
        ]}"#,
    )?;
    let output = fixture.aggregate()?;
    assert!(output.status.success(), "{}", output_text(&output));
    assert!(output_text(&output).contains("skipped (planned) rust-alpha github"));
    Ok(())
}

#[test]
fn executed_and_reused_successes_pass_with_explanations() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(
        r#"{"units": [
            {"id": "rust-alpha", "lanes": ["github"]},
            {"id": "node-beta", "lanes": ["github"]}
        ]}"#,
        r#"{"results": [
            {"unit": "rust-alpha", "lane": "github", "outcome": "success"},
            {"unit": "node-beta", "lane": "github", "outcome": "success", "reused_from": "run-7"}
        ]}"#,
    )?;
    let output = fixture.aggregate()?;
    assert!(output.status.success(), "{}", output_text(&output));
    let text = output_text(&output);
    assert!(text.contains("executed rust-alpha github"));
    assert!(text.contains("reused node-beta github"));
    Ok(())
}

#[test]
fn missing_result_and_incomplete_matrix_are_rejected() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(
        r#"{"units": [
            {"id": "node-beta", "lanes": ["github"], "matrix": ["cpu", "gpu"]}
        ]}"#,
        r#"{"results": [
            {"unit": "node-beta", "lane": "github", "matrix": "cpu", "outcome": "success"}
        ]}"#,
    )?;
    let output = fixture.aggregate()?;
    assert!(!output.status.success());
    let text = output_text(&output);
    assert!(text.contains("missing result for node-beta github[gpu]"));
    assert!(text.contains("incomplete matrix"));
    Ok(())
}

#[test]
fn cancelled_required_work_is_rejected() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(
        r#"{"units": [{"id": "rust-alpha", "lanes": ["github"], "required": true}]}"#,
        r#"{"results": [
            {"unit": "rust-alpha", "lane": "github", "outcome": "cancelled"}
        ]}"#,
    )?;
    let output = fixture.aggregate()?;
    assert!(!output.status.success());
    assert!(output_text(&output).contains("cancelled required work rust-alpha github"));
    Ok(())
}

#[test]
fn failed_prerequisite_blocks_a_successful_dependent() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(
        r#"{"units": [
            {"id": "rust-base", "lanes": ["github"]},
            {"id": "node-app", "lanes": ["github"]}
        ], "prerequisites": {"node-app": ["rust-base"]}}"#,
        r#"{"results": [
            {"unit": "rust-base", "lane": "github", "outcome": "failure"},
            {"unit": "node-app", "lane": "github", "outcome": "success"}
        ]}"#,
    )?;
    let output = fixture.aggregate()?;
    assert!(!output.status.success());
    assert!(output_text(&output).contains("prerequisite `rust-base` of `node-app` did not pass"));
    Ok(())
}

struct GitFixture {
    root: PathBuf,
}

impl GitFixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = env::temp_dir().join(format!(
            "velnor-closure-reuse-git-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".github/ci"))?;
        fs::create_dir_all(root.join("crates/alpha/src"))?;
        fs::create_dir_all(root.join("crates/beta/src"))?;
        fs::write(root.join(".github/ci/project.toml"), CONFIG)?;
        fs::write(root.join("crates/alpha/src/lib.rs"), "initial\n")?;
        fs::write(root.join("crates/beta/src/lib.rs"), "initial\n")?;
        let fixture = Self { root };
        fixture.git(&["init", "-q"])?;
        fixture.git(&["config", "user.email", "test@example.invalid"])?;
        fixture.git(&["config", "user.name", "Velnor test"])?;
        fixture.git(&["add", "."])?;
        fixture.git(&["commit", "-qm", "base"])?;
        Ok(fixture)
    }

    fn git(&self, args: &[&str]) -> Result<String, Box<dyn Error>> {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(format!("git {args:?} failed: {}", output_text(&output)).into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn command(&self, subcommand: &str, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"));
        command.current_dir(&self.root).arg(subcommand);
        for arg in args {
            command.arg(arg);
        }
        command
    }
}

impl Drop for GitFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn is_full_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[test]
fn select_reports_the_affected_units_as_json() -> Result<(), Box<dyn Error>> {
    let fixture = GitFixture::new()?;
    fs::write(fixture.root.join("crates/alpha/src/lib.rs"), "changed\n")?;
    fixture.git(&["commit", "-qam", "change alpha"])?;
    let base = fixture.git(&["rev-parse", "HEAD~1"])?;
    let output = fixture
        .command(
            "select",
            &[
                "--config",
                ".github/ci/project.toml",
                "--base",
                &base,
                "--head",
                "HEAD",
            ],
        )
        .output()?;
    assert!(output.status.success(), "{}", output_text(&output));
    let selection: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout))?;
    assert_eq!(selection["fallback_full"], false);
    assert_eq!(selection["required"], serde_json::json!(["rust-alpha"]));
    assert_eq!(selection["full_units"], serde_json::json!(["rust-alpha"]));
    // A rename invalidates both owners: the file left alpha, it arrived in beta.
    fixture.git(&["mv", "crates/alpha/src/lib.rs", "crates/beta/src/moved.rs"])?;
    fixture.git(&["commit", "-qm", "move across units"])?;
    let base = fixture.git(&["rev-parse", "HEAD~1"])?;
    let output = fixture
        .command(
            "select",
            &[
                "--config",
                ".github/ci/project.toml",
                "--base",
                &base,
                "--head",
                "HEAD",
            ],
        )
        .output()?;
    assert!(output.status.success(), "{}", output_text(&output));
    let selection: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout))?;
    assert_eq!(
        selection["required"],
        serde_json::json!(["rust-alpha", "rust-beta"])
    );
    Ok(())
}

#[test]
fn fingerprint_reports_full_digests_and_live_state() -> Result<(), Box<dyn Error>> {
    let fixture = GitFixture::new()?;
    let output = fixture
        .command(
            "fingerprint",
            &["--config", ".github/ci/project.toml", "--rev", "HEAD"],
        )
        .output()?;
    assert!(output.status.success(), "{}", output_text(&output));
    let report: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))?;
    assert_eq!(report["rev"], "HEAD");
    let units = report["units"].as_array().ok_or("units must be an array")?;
    assert_eq!(units.len(), 2);
    for unit in units {
        let name = unit["unit"].as_str().ok_or("unit id must print")?;
        let fingerprint = unit["fingerprint"]
            .as_str()
            .ok_or("fingerprint must print")?;
        assert!(is_full_hex(fingerprint), "not a full digest: {fingerprint}");
        let locator = unit["locator"].as_str().ok_or("locator must print")?;
        assert!(
            locator.starts_with(&format!("{name}-")) && locator.len() == name.len() + 17,
            "unexpected locator: {locator}"
        );
        assert_eq!(unit["live"], false);
    }
    let output = fixture
        .command(
            "fingerprint",
            &[
                "--config",
                ".github/ci/project.toml",
                "--unit",
                "rust-beta",
                "--live",
                "rust-beta",
            ],
        )
        .output()?;
    assert!(output.status.success(), "{}", output_text(&output));
    let report: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))?;
    assert_eq!(report["units"].as_array().ok_or("array")?.len(), 1);
    assert_eq!(report["units"][0]["unit"], "rust-beta");
    assert_eq!(report["units"][0]["live"], true);
    Ok(())
}

#[test]
fn reuse_decision_scores_evidence_against_a_request() -> Result<(), Box<dyn Error>> {
    let root = env::temp_dir().join(format!(
        "velnor-closure-reuse-decision-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root)?;
    let fingerprint = "f".repeat(64);
    let recipe = "9".repeat(64);
    fs::write(
        root.join("evidence.json"),
        format!(
            r#"{{"run_id": "run-7", "fingerprint": "{fingerprint}", "recipe": "{recipe}", "checks": ["ci/rust-alpha/github"], "aggregate_passed": true, "trust": "trusted"}}"#
        ),
    )?;
    fs::write(
        root.join("request.json"),
        format!(
            r#"{{"fingerprint": "{fingerprint}", "recipe": "{recipe}", "expected_checks": ["ci/rust-alpha/github"], "required_trust": "trusted"}}"#
        ),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .current_dir(&root)
        .args([
            "reuse-decision",
            "--evidence",
            "evidence.json",
            "--request",
            "request.json",
            "--now",
            "1700000000",
        ])
        .output()?;
    assert!(output.status.success(), "{}", output_text(&output));
    assert!(output_text(&output).starts_with("reuse run-7\n"));
    fs::write(
        root.join("request.json"),
        format!(
            r#"{{"fingerprint": "{fingerprint}", "recipe": "{recipe}", "expected_checks": ["ci/rust-alpha/github", "ci/rust-beta/github"], "required_trust": "trusted"}}"#
        ),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .current_dir(&root)
        .args([
            "reuse-decision",
            "--evidence",
            "evidence.json",
            "--request",
            "request.json",
            "--now",
            "1700000000",
        ])
        .output()?;
    assert!(output.status.success(), "{}", output_text(&output));
    assert!(output_text(&output).starts_with("execute\n"));
    assert!(output_text(&output).contains("missed expected checks"));
    let _ = fs::remove_dir_all(&root);
    Ok(())
}

const CONFIG: &str = r#"
schema = 2

[[unit]]
id = "rust-alpha"
label = "alpha"
kind = "rust"
root = "crates/alpha"
watch = ["crates/alpha/**"]
github_pr_commands = ["cargo test --locked"]
github_full_commands = ["cargo test --locked"]
velnor_pr_commands = ["cargo test --locked"]
velnor_full_commands = ["cargo test --locked"]

[[unit]]
id = "rust-beta"
label = "beta"
kind = "rust"
root = "crates/beta"
watch = ["crates/beta/**"]
github_pr_commands = ["cargo test --locked"]
github_full_commands = ["cargo test --locked"]
velnor_pr_commands = ["cargo test --locked"]
velnor_full_commands = ["cargo test --locked"]
"#;

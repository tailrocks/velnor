//! D3: the verify set schedules work; prerequisites are build inputs.
//!
//! The planner selects the changed units plus their dependents for
//! verification and records their transitive prerequisites as build inputs
//! instead of scheduling them. An opaque unit without a closed-world
//! contract is still selected — but its unchanged prerequisites earn no
//! jobs: the verify jobs compile, restore, and consume them in-job. A
//! closed-world contract whose bound excludes the changed path selects
//! nothing at all.
//!
//! Each test runs the real `plan`/`select`/`run`/`aggregate` binaries in a
//! git fixture, once per schema path (s1: no generation marker, s2: the
//! `.github-gen` marker), and asserts on plan outputs, the expected-work
//! file, and the aggregate verdict — never on labels alone.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

const CONFIG_S1: &str = r#"
schema = 2
repository = "example/verify-set"
profile = "generic"
verified = true
default_branch = "main"
runners = "github"

[[unit]]
id = "lib"
kind = "rust"
root = "."
watch = ["crates/lib/**"]
github_pr_commands = ["printf lib > lib.marker"]
github_full_commands = ["printf lib > lib.marker"]
velnor_pr_commands = ["printf lib > lib.marker"]
velnor_full_commands = ["printf lib > lib.marker"]

[[unit]]
id = "app"
kind = "rust"
root = "."
watch = ["crates/app/**"]
github_pr_commands = ["printf app > app.marker"]
github_full_commands = ["printf app > app.marker"]
velnor_pr_commands = ["printf app > app.marker"]
velnor_full_commands = ["printf app > app.marker"]
depends_on = ["lib"]

[[unit]]
id = "site"
kind = "bun"
root = "."
watch = ["site/**"]
github_pr_commands = ["sh -c 'printf site > site.marker'"]
github_full_commands = ["sh -c 'printf site > site.marker'"]
velnor_pr_commands = ["sh -c 'printf site > site.marker'"]
velnor_full_commands = ["sh -c 'printf site > site.marker'"]
depends_on = ["app"]
"#;

const CONFIG_S2: &str = r#"
schema = 3
repository = "example/verify-set"
profile = "generic"
verified = true
default_branch = "main"
providers = ["github-hosted"]
automatic_providers = ["github-hosted"]

[[unit]]
id = "lib"
platform = "linux-x64"
trust = "untrusted-ok"
kind = "rust"
root = "."
watch = ["crates/lib/**"]
pr_commands = ["printf lib > lib.marker"]
full_commands = ["printf lib > lib.marker"]

[[unit]]
id = "app"
platform = "linux-x64"
trust = "untrusted-ok"
kind = "rust"
root = "."
watch = ["crates/app/**"]
pr_commands = ["printf app > app.marker"]
full_commands = ["printf app > app.marker"]
depends_on = ["lib"]

[[unit]]
id = "site"
platform = "linux-x64"
trust = "untrusted-ok"
kind = "bun"
root = "."
watch = ["site/**"]
pr_commands = ["sh -c 'printf site > site.marker'"]
full_commands = ["sh -c 'printf site > site.marker'"]
depends_on = ["app"]
"#;

struct Fixture {
    root: PathBuf,
    base: String,
    head: String,
}

impl Fixture {
    fn new(schema_two: bool, changed: &str, changed_body: &str) -> Result<Self, Box<dyn Error>> {
        Self::with_config(
            schema_two,
            if schema_two { CONFIG_S2 } else { CONFIG_S1 },
            changed,
            changed_body,
        )
    }

    fn with_config(
        schema_two: bool,
        config: &str,
        changed: &str,
        changed_body: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let root = env::temp_dir().join(format!(
            "velnor-verify-set-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".github/ci"))?;
        if schema_two {
            fs::create_dir_all(root.join(".github-gen"))?;
            fs::write(
                root.join(".github-gen/velnor-workflow.toml"),
                "schema = 2\n",
            )?;
        }
        fs::write(root.join(".github/ci/project.toml"), config)?;
        fs::create_dir_all(root.join("crates/lib/src"))?;
        fs::create_dir_all(root.join("crates/app/src"))?;
        fs::create_dir_all(root.join("site"))?;
        fs::write(root.join("crates/lib/src/lib.rs"), "pub fn lib() {}\n")?;
        fs::write(root.join("crates/app/src/lib.rs"), "pub fn app() {}\n")?;
        fs::write(root.join("site/index.html"), "<p>site</p>\n")?;
        fs::write(root.join("AGENTS.md"), "docs\n")?;
        Self::git(&root, &["init", "-q"])?;
        Self::git(&root, &["config", "user.email", "test@example.invalid"])?;
        Self::git(&root, &["config", "user.name", "Velnor test"])?;
        Self::git(&root, &["add", "."])?;
        Self::git(&root, &["commit", "-qm", "base"])?;
        let base = Self::git(&root, &["rev-parse", "HEAD"])?;
        fs::write(root.join(changed), changed_body)?;
        Self::git(&root, &["commit", "-qam", "change"])?;
        let head = Self::git(&root, &["rev-parse", "HEAD"])?;
        Ok(Self { root, base, head })
    }

    fn git(root: &std::path::Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
        let output = Command::new("git").current_dir(root).args(args).output()?;
        if !output.status.success() {
            return Err(format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn binary() -> Command {
        Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
    }

    fn plan(&self) -> Result<PlanOutputs, Box<dyn Error>> {
        let github_output = self.root.join("github-output");
        let selection_file = self.root.join("velnor-ci-selection");
        let expected_file = self.root.join("expected-work.json");
        let output = Self::binary()
            .current_dir(&self.root)
            .env("EVENT_NAME", "pull_request")
            .env("BASE_SHA", &self.base)
            .env("SOURCE_SHA", &self.head)
            .env("HEAD_SHA", &self.head)
            .env("VELNOR_EVENT_TRUSTED", "false")
            .env("GITHUB_OUTPUT", &github_output)
            .env("VELNOR_SELECTION_FILE", &selection_file)
            .env("VELNOR_EXPECTED_WORK_FILE", &expected_file)
            .args(["plan", "--config", ".github/ci/project.toml"])
            .output()?;
        if !output.status.success() {
            return Err(format!("plan failed: {}", String::from_utf8_lossy(&output.stderr)).into());
        }
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut fields = BTreeMap::new();
        if github_output.exists() {
            for line in fs::read_to_string(&github_output)?.lines() {
                if let Some((key, value)) = line.split_once('=') {
                    fields.insert(key.to_owned(), value.to_owned());
                }
            }
        }
        Ok(PlanOutputs {
            stdout,
            fields,
            expected_file,
        })
    }

    fn select_json(&self) -> Result<serde_json::Value, Box<dyn Error>> {
        let output = Self::binary()
            .current_dir(&self.root)
            .args([
                "select",
                "--config",
                ".github/ci/project.toml",
                "--base",
                &self.base,
                "--head",
                &self.head,
                "--scope",
                "affected",
            ])
            .output()?;
        if !output.status.success() {
            return Err(
                format!("select failed: {}", String::from_utf8_lossy(&output.stderr)).into(),
            );
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    fn run_unit(&self, selection: &PlanOutputs, unit: &str) -> Result<(), Box<dyn Error>> {
        let schema_two = selection.fields.contains_key("unit_ids");
        let units = selection
            .fields
            .get("unit_ids")
            .or_else(|| selection.fields.get("units"));
        let Some(units) = units else {
            return Err("plan published no units channel".into());
        };
        let full_units = selection
            .fields
            .get("full_units")
            .cloned()
            .unwrap_or_default();
        let selection_path = self.root.join(".velnor-ci-selection-file");
        let contents = if schema_two {
            let digest = selection
                .fields
                .get("plan_digest")
                .cloned()
                .unwrap_or_default();
            format!(
                "version=2\nbase_sha={}\nhead_sha={}\nscope=affected\nunits={units}\nfull_units={full_units}\nplan_digest={digest}\n",
                self.base, self.head,
            )
        } else {
            format!(
                "version=1\nbase_sha={}\nhead_sha={}\nscope=affected\nunits={units}\nfull_units={full_units}\n",
                self.base, self.head,
            )
        };
        fs::write(&selection_path, contents)?;
        let output = Self::binary()
            .current_dir(&self.root)
            .env("EVENT_NAME", "pull_request")
            .env("BASE_SHA", &self.base)
            .env("SOURCE_SHA", &self.head)
            .env("HEAD_SHA", &self.head)
            .env("VELNOR_EVENT_TRUSTED", "false")
            .env("VELNOR_SELECTION_FILE", &selection_path)
            .args([
                "run",
                "--config",
                ".github/ci/project.toml",
                "--scope",
                "affected",
                "--unit",
                unit,
            ])
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "run {unit} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(())
    }

    fn aggregate(&self, expected: &str, results: &str) -> Result<(bool, String), Box<dyn Error>> {
        let expected_path = self.root.join("agg-expected.json");
        let results_path = self.root.join("agg-results.json");
        fs::write(&expected_path, expected)?;
        fs::write(&results_path, results)?;
        let output = Self::binary()
            .current_dir(&self.root)
            .env("BASE_SHA", &self.base)
            .env("HEAD_SHA", &self.head)
            .args([
                "aggregate",
                "--expected",
                expected_path.to_str().ok_or("expected path")?,
                "--results",
                results_path.to_str().ok_or("results path")?,
            ])
            .output()?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct PlanOutputs {
    stdout: String,
    fields: BTreeMap<String, String>,
    expected_file: PathBuf,
}

fn csv_set(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    value.split(',').map(str::to_owned).collect()
}

/// All-success results covering every (unit, lane) the expected-work file
/// names: the verdict the verify jobs would report.
fn success_results_for(expected: &serde_json::Value) -> Result<String, Box<dyn Error>> {
    let mut results = Vec::new();
    let units = expected
        .get("units")
        .and_then(serde_json::Value::as_array)
        .ok_or("expected units array")?;
    for unit in units {
        let id = unit
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or("expected unit id")?;
        let lanes = unit
            .get("lanes")
            .and_then(serde_json::Value::as_array)
            .ok_or("expected lanes")?;
        for lane in lanes {
            let lane = lane.as_str().ok_or("lane string")?;
            results.push(serde_json::json!({
                "unit": id,
                "lane": lane,
                "outcome": "success",
            }));
        }
    }
    Ok(serde_json::to_string(
        &serde_json::json!({ "results": results }),
    )?)
}

fn scheduled_ids(plan: &PlanOutputs) -> Vec<String> {
    if let Some(ids) = plan.fields.get("unit_ids") {
        return csv_set(ids);
    }
    csv_set(
        plan.fields
            .get("units")
            .cloned()
            .unwrap_or_default()
            .as_str(),
    )
}

#[test]
fn s1_plan_schedules_verify_set_and_records_prereq_inputs() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(false, "AGENTS.md", "docs v2\n")?;
    let plan = fixture.plan()?;
    assert_eq!(
        scheduled_ids(&plan),
        vec!["site".to_owned()],
        "only the opaque consumer schedules: {:?}",
        plan.fields
    );
    assert_eq!(
        plan.fields.get("full_units").cloned().unwrap_or_default(),
        "site"
    );
    assert!(
        plan.stdout.contains("prereq_inputs=app,lib"),
        "the log records the transitive inputs: {}",
        plan.stdout
    );
    assert_eq!(
        plan.fields.get("rust_matrix").cloned().unwrap_or_default(),
        "[]",
        "unchanged prerequisites earn no matrix entries: {:?}",
        plan.fields
    );
    let bun_matrix: serde_json::Value = serde_json::from_str(
        plan.fields
            .get("bun_matrix")
            .cloned()
            .unwrap_or_default()
            .as_str(),
    )?;
    assert_eq!(
        bun_matrix.as_array().map(Vec::len).unwrap_or_default(),
        1,
        "the verify unit alone: {bun_matrix}"
    );

    let expected: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&plan.expected_file)?)?;
    let ids: Vec<&str> = expected
        .get("units")
        .and_then(serde_json::Value::as_array)
        .map(|units| {
            units
                .iter()
                .filter_map(|unit| unit.get("id").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        ids,
        vec!["site"],
        "expected work names the verify unit only"
    );
    assert_eq!(
        expected.get("prerequisites"),
        Some(&serde_json::json!({ "site": ["app"] })),
        "the edges still declare the build inputs"
    );

    let selection = fixture.select_json()?;
    assert_eq!(
        selection.get("required"),
        Some(&serde_json::json!(["site"])),
        "the oracle verifies the consumer: {selection}"
    );
    assert_eq!(
        selection.get("prereq_inputs"),
        Some(&serde_json::json!(["app", "lib"])),
        "the oracle records the transitive inputs: {selection}"
    );

    let expected_text = fs::read_to_string(&plan.expected_file)?;
    let expected_json: serde_json::Value = serde_json::from_str(&expected_text)?;
    let (passed, report) =
        fixture.aggregate(&expected_text, &success_results_for(&expected_json)?)?;
    assert!(passed, "verify-only verdicts pass: {report}");
    assert!(
        report.contains("build input") && report.contains("app"),
        "the aggregate notes the out-of-plan input: {report}"
    );
    Ok(())
}

#[test]
fn s1_closed_contract_selects_no_work() -> Result<(), Box<dyn Error>> {
    let closed = CONFIG_S1.replacen(
        "watch = [\"site/**\"]",
        "watch = [\"site/**\"]\nreads_closed = true",
        1,
    );
    let fixture = Fixture::with_config(false, &closed, "AGENTS.md", "docs v2\n")?;
    let plan = fixture.plan()?;
    assert_eq!(
        plan.fields
            .get("planned_no_work")
            .cloned()
            .unwrap_or_default(),
        "true",
        "a closed contract excluding the path is zero work: {:?}",
        plan.fields
    );
    assert!(
        scheduled_ids(&plan).is_empty(),
        "nothing schedules: {:?}",
        plan.fields
    );
    let expected: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&plan.expected_file)?)?;
    assert_eq!(
        expected.get("planned_no_work"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        expected
            .get("units")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    Ok(())
}

#[test]
fn s1_changed_prereq_verifies_its_dependents() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(false, "crates/lib/src/lib.rs", "pub fn lib() {}\n// v2\n")?;
    let plan = fixture.plan()?;
    let mut scheduled = scheduled_ids(&plan);
    scheduled.sort();
    assert_eq!(
        scheduled,
        vec!["app".to_owned(), "lib".to_owned(), "site".to_owned()],
        "dependents of a changed crate still verify: {:?}",
        plan.fields
    );
    assert!(
        plan.stdout.contains("prereq_inputs=\n") || plan.stdout.ends_with("prereq_inputs="),
        "nothing is a mere input when the chain verifies: {}",
        plan.stdout
    );
    Ok(())
}

#[test]
fn s1_run_executes_only_the_verify_unit() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(false, "AGENTS.md", "docs v2\n")?;
    let plan = fixture.plan()?;
    fixture.run_unit(&plan, "site")?;
    assert!(
        fixture.root.join("site.marker").exists(),
        "the verify unit ran its commands"
    );
    assert!(
        !fixture.root.join("lib.marker").exists() && !fixture.root.join("app.marker").exists(),
        "prerequisites ran nothing: no independent jobs, no commands"
    );
    Ok(())
}

#[test]
fn s2_plan_schedules_verify_set_and_records_prereq_inputs() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(true, "AGENTS.md", "docs v2\n")?;
    let plan = fixture.plan()?;
    assert_eq!(
        scheduled_ids(&plan),
        vec!["site".to_owned()],
        "only the opaque consumer schedules: {:?}",
        plan.fields
    );
    let units_json: serde_json::Value = serde_json::from_str(
        plan.fields
            .get("units")
            .cloned()
            .unwrap_or_default()
            .as_str(),
    )?;
    let gated: Vec<&str> = units_json
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("unit_id").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        gated,
        vec!["site"],
        "the gating JSON names the verify unit only: {units_json}"
    );
    assert!(
        plan.stdout.contains("prereq_inputs=app,lib"),
        "the log records the transitive inputs: {}",
        plan.stdout
    );
    assert_eq!(
        plan.fields.get("rust_matrix").cloned().unwrap_or_default(),
        "[]",
        "unchanged prerequisites earn no matrix entries: {:?}",
        plan.fields
    );

    let expected: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&plan.expected_file)?)?;
    let ids: Vec<&str> = expected
        .get("units")
        .and_then(serde_json::Value::as_array)
        .map(|units| {
            units
                .iter()
                .filter_map(|unit| unit.get("id").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        ids,
        vec!["site"],
        "expected work names the verify unit only"
    );
    assert_eq!(
        expected.get("prerequisites"),
        Some(&serde_json::json!({ "site": ["app"] })),
        "the edges still declare the build inputs"
    );

    let selection = fixture.select_json()?;
    assert_eq!(
        selection.get("required"),
        Some(&serde_json::json!(["site"])),
        "the oracle verifies the consumer: {selection}"
    );
    assert_eq!(
        selection.get("prereq_inputs"),
        Some(&serde_json::json!(["app", "lib"])),
        "the oracle records the transitive inputs: {selection}"
    );

    let expected_text = fs::read_to_string(&plan.expected_file)?;
    let expected_json: serde_json::Value = serde_json::from_str(&expected_text)?;
    let (passed, report) =
        fixture.aggregate(&expected_text, &success_results_for(&expected_json)?)?;
    assert!(passed, "verify-only verdicts pass: {report}");
    assert!(
        report.contains("build input") && report.contains("app"),
        "the aggregate notes the out-of-plan input: {report}"
    );
    Ok(())
}

#[test]
fn s2_closed_contract_selects_no_work() -> Result<(), Box<dyn Error>> {
    let closed = CONFIG_S2.replacen(
        "watch = [\"site/**\"]",
        "watch = [\"site/**\"]\nreads_closed = true",
        1,
    );
    let fixture = Fixture::with_config(true, &closed, "AGENTS.md", "docs v2\n")?;
    let plan = fixture.plan()?;
    assert_eq!(
        plan.fields
            .get("planned_no_work")
            .cloned()
            .unwrap_or_default(),
        "true",
        "a closed contract excluding the path is zero work: {:?}",
        plan.fields
    );
    assert!(
        scheduled_ids(&plan).is_empty(),
        "nothing schedules: {:?}",
        plan.fields
    );
    Ok(())
}

#[test]
fn s2_changed_prereq_verifies_its_dependents() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(true, "crates/lib/src/lib.rs", "pub fn lib() {}\n// v2\n")?;
    let plan = fixture.plan()?;
    let mut scheduled = scheduled_ids(&plan);
    scheduled.sort();
    assert_eq!(
        scheduled,
        vec!["app".to_owned(), "lib".to_owned(), "site".to_owned()],
        "dependents of a changed crate still verify: {:?}",
        plan.fields
    );
    Ok(())
}

#[test]
fn s2_run_executes_only_the_verify_unit() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new(true, "AGENTS.md", "docs v2\n")?;
    let plan = fixture.plan()?;
    fixture.run_unit(&plan, "site")?;
    assert!(
        fixture.root.join("site.marker").exists(),
        "the verify unit ran its commands"
    );
    assert!(
        !fixture.root.join("lib.marker").exists() && !fixture.root.join("app.marker").exists(),
        "prerequisites ran nothing: no independent jobs, no commands"
    );
    Ok(())
}

#[test]
fn s1_closed_exclusion_is_visible_in_plan_log_and_select() -> Result<(), Box<dyn Error>> {
    let closed = CONFIG_S1.replacen(
        "watch = [\"site/**\"]",
        "watch = [\"site/**\"]\nreads_closed = true",
        1,
    );
    let fixture = Fixture::with_config(false, &closed, "AGENTS.md", "docs v2\n")?;
    let plan = fixture.plan()?;
    assert_eq!(
        plan.fields
            .get("planned_no_work")
            .cloned()
            .unwrap_or_default(),
        "true",
        "a closed contract excluding the path is zero work: {:?}",
        plan.fields
    );
    assert_eq!(
        plan.fields
            .get("no_work_reason")
            .cloned()
            .unwrap_or_default(),
        "no changed path selected a workload unit",
        "the no-work reason is unchanged: {:?}",
        plan.fields
    );
    assert!(
        scheduled_ids(&plan).is_empty(),
        "nothing schedules: {:?}",
        plan.fields
    );
    assert!(
        plan.stdout.contains("closed_excluded=site"),
        "the log names the excluded closed unit: {}",
        plan.stdout
    );
    assert!(
        !plan.fields.contains_key("closed_excluded"),
        "the record is log-only, like prereq_inputs: {:?}",
        plan.fields
    );

    let selection = fixture.select_json()?;
    assert_eq!(
        selection.get("required"),
        Some(&serde_json::json!([])),
        "the oracle still selects nothing: {selection}"
    );
    assert_eq!(
        selection.get("closed_excluded"),
        Some(&serde_json::json!(["site"])),
        "the oracle names the excluded closed unit: {selection}"
    );
    assert_eq!(
        selection.get("fallback_reason"),
        None,
        "no fallback fired: {selection}"
    );
    Ok(())
}

#[test]
fn s2_closed_exclusion_is_visible_in_plan_log_and_select() -> Result<(), Box<dyn Error>> {
    let closed = CONFIG_S2.replacen(
        "watch = [\"site/**\"]",
        "watch = [\"site/**\"]\nreads_closed = true",
        1,
    );
    let fixture = Fixture::with_config(true, &closed, "AGENTS.md", "docs v2\n")?;
    let plan = fixture.plan()?;
    assert_eq!(
        plan.fields
            .get("planned_no_work")
            .cloned()
            .unwrap_or_default(),
        "true",
        "a closed contract excluding the path is zero work: {:?}",
        plan.fields
    );
    assert_eq!(
        plan.fields
            .get("no_work_reason")
            .cloned()
            .unwrap_or_default(),
        "no changed path selected a workload unit",
        "the no-work reason is unchanged: {:?}",
        plan.fields
    );
    assert!(
        scheduled_ids(&plan).is_empty(),
        "nothing schedules: {:?}",
        plan.fields
    );
    assert!(
        plan.stdout.contains("closed_excluded=site"),
        "the log names the excluded closed unit: {}",
        plan.stdout
    );
    assert!(
        !plan.fields.contains_key("closed_excluded"),
        "the record is log-only, like prereq_inputs: {:?}",
        plan.fields
    );

    let selection = fixture.select_json()?;
    assert_eq!(
        selection.get("required"),
        Some(&serde_json::json!([])),
        "the oracle still selects nothing: {selection}"
    );
    assert_eq!(
        selection.get("closed_excluded"),
        Some(&serde_json::json!(["site"])),
        "the oracle names the excluded closed unit: {selection}"
    );
    assert_eq!(
        selection.get("fallback_reason"),
        None,
        "no fallback fired: {selection}"
    );
    Ok(())
}

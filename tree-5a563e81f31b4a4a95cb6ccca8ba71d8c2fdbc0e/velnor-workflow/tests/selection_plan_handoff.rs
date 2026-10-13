//! Flip bug #2: the s2 plan emits `needs.plan.outputs.units` as JSON while the
//! reusable materializes the selection file's CSV-only `units=` field from
//! it, so `run` fails closed in every executed leg (`invalid unit id in CI
//! selection artifact`). The plan now emits the same affected set as CSV
//! `unit_ids`, and the emitter threads that channel into `units=`.
//!
//! This test closes the structural gap with a live end-to-end chain: it runs
//! the real `plan` binary in a git fixture, materializes a selection file
//! from the live plan outputs exactly as the generated step does, and runs
//! the real `run` binary against it. Pre-fix there is no `unit_ids` output,
//! so the chain cannot close; a negative control pins the boundary by feeding
//! the plan JSON into `units=` and asserting the fail-closed error.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = env::temp_dir().join(format!(
            "velnor-selection-plan-handoff-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".github/ci"))?;
        fs::create_dir_all(root.join(".github-gen"))?;
        fs::create_dir_all(root.join(".velnor-ci-selection"))?;
        fs::create_dir_all(root.join("selected"))?;
        fs::create_dir_all(root.join("unselected"))?;
        // Schema-2 generation marker: runtime subcommands dispatch on the
        // working directory's marker, so `plan`/`run` take the s2 path.
        fs::write(
            root.join(".github-gen/velnor-workflow.toml"),
            "schema = 2\n",
        )?;
        fs::write(root.join(".github/ci/project.toml"), CONFIG)?;
        fs::write(root.join("selected/file.txt"), "initial\n")?;
        fs::write(root.join("unselected/file.txt"), "initial\n")?;
        let fixture = Self { root };
        fixture.git(&["init", "-q"])?;
        fixture.git(&["config", "user.email", "test@example.invalid"])?;
        fixture.git(&["config", "user.name", "Velnor test"])?;
        fixture.git(&["add", "."])?;
        fixture.git(&["commit", "-qm", "base"])?;
        fs::write(fixture.root.join("selected/file.txt"), "changed\n")?;
        fixture.git(&["commit", "-qam", "change selected"])?;
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

    fn plan(&self, base: &str, head: &str, github_output: &PathBuf) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"));
        command
            .current_dir(&self.root)
            .env("EVENT_NAME", "pull_request")
            .env("BASE_SHA", base)
            .env("HEAD_SHA", head)
            .env("GITHUB_OUTPUT", github_output)
            .args(["plan", "--config", ".github/ci/project.toml"]);
        command
    }

    fn run(&self, base: &str, head: &str, selection: &PathBuf, unit: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"));
        command
            .current_dir(&self.root)
            .env("EVENT_NAME", "pull_request")
            .env("BASE_SHA", base)
            .env("HEAD_SHA", head)
            .env("VELNOR_SELECTION_FILE", selection)
            .args([
                "run",
                "--config",
                ".github/ci/project.toml",
                "--scope",
                "affected",
                "--unit",
                unit,
            ]);
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Parse the `name=value` lines the plan appends to `GITHUB_OUTPUT`.
fn github_outputs(path: &PathBuf) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    let mut outputs = BTreeMap::new();
    for line in fs::read_to_string(path)?.lines() {
        let (name, value) = line
            .split_once('=')
            .ok_or_else(|| format!("malformed plan output line: {line}"))?;
        outputs.insert(name.to_owned(), value.to_owned());
    }
    Ok(outputs)
}

/// The unit ids named by the plan's JSON `units` output.
fn plan_json_unit_ids(units_json: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let parsed: serde_json::Value = serde_json::from_str(units_json)?;
    let units = parsed
        .as_array()
        .ok_or("plan units output is not a JSON array")?;
    units
        .iter()
        .map(|unit| {
            unit.get("unit_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "plan units record has no unit_id".into())
        })
        .collect()
}

/// Whether the value is shaped like one CSV selection id: lowercase-led
/// `[a-z0-9-]`, the contract `run` enforces per comma-separated element.
fn is_selection_id(value: &str) -> bool {
    !value.is_empty()
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[test]
fn live_plan_outputs_materialize_a_selection_file_run_executes() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let base = fixture.git(&["rev-parse", "HEAD~1"])?;
    let head = fixture.git(&["rev-parse", "HEAD"])?;
    let github_output = fixture.root.join("github-output");
    fs::write(&github_output, "")?;

    let output = fixture.plan(&base, &head, &github_output).output()?;
    assert!(output.status.success(), "{}", output_text(&output));
    let outputs = github_outputs(&github_output)?;
    let output_of = |name: &str| {
        outputs
            .get(name)
            .cloned()
            .ok_or_else(|| format!("live plan must emit the `{name}` output"))
    };
    // The CSV channel the fixed emitter threads into `units=`. Pre-fix this
    // output does not exist and the chain cannot close.
    let unit_ids = output_of("unit_ids")?;
    assert!(
        !unit_ids.is_empty() && unit_ids.split(',').all(is_selection_id),
        "unit_ids must be CSV selection ids, got: {unit_ids}"
    );
    let units_json = output_of("units")?;
    assert_eq!(
        unit_ids.split(',').collect::<Vec<_>>(),
        plan_json_unit_ids(&units_json)?,
        "the CSV channel must name exactly the JSON channel's affected set"
    );

    // Materialize the selection file from the live outputs exactly as the
    // generated step does: one `KEY=value` line per field, `units=` from the
    // CSV channel.
    let selection = fixture
        .root
        .join(".velnor-ci-selection/velnor-ci-selection");
    fs::write(
        &selection,
        format!(
            "version=2\nbase_sha={}\nhead_sha={}\nscope={}\nunits={unit_ids}\nfull_units={}\nplan_digest={}\n",
            output_of("base_sha")?,
            output_of("head_sha")?,
            output_of("scope")?,
            output_of("full_units")?,
            output_of("plan_digest")?,
        ),
    )?;
    let output = fixture.run(&base, &head, &selection, "selected").output()?;
    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(
        fs::read_to_string(fixture.root.join("selected.marker"))?,
        "selected"
    );
    assert!(!fixture.root.join("unselected.marker").exists());
    Ok(())
}

#[test]
fn plan_json_in_the_units_field_still_fails_closed() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let base = fixture.git(&["rev-parse", "HEAD~1"])?;
    let head = fixture.git(&["rev-parse", "HEAD"])?;
    let github_output = fixture.root.join("github-output");
    fs::write(&github_output, "")?;

    let output = fixture.plan(&base, &head, &github_output).output()?;
    assert!(output.status.success(), "{}", output_text(&output));
    let outputs = github_outputs(&github_output)?;
    let output_of = |name: &str| {
        outputs
            .get(name)
            .cloned()
            .ok_or_else(|| format!("live plan must emit the `{name}` output"))
    };
    // The pre-fix wiring: raw plan JSON in the CSV-only field. The parser
    // must keep rejecting it — this is the exact CI failure mode.
    let selection = fixture
        .root
        .join(".velnor-ci-selection/velnor-ci-selection");
    fs::write(
        &selection,
        format!(
            "version=2\nbase_sha={}\nhead_sha={}\nscope={}\nunits={}\nfull_units={}\nplan_digest={}\n",
            output_of("base_sha")?,
            output_of("head_sha")?,
            output_of("scope")?,
            output_of("units")?,
            output_of("full_units")?,
            output_of("plan_digest")?,
        ),
    )?;
    let output = fixture.run(&base, &head, &selection, "selected").output()?;
    assert!(!output.status.success());
    assert!(
        output_text(&output).contains("invalid unit id in CI selection artifact"),
        "{}",
        output_text(&output)
    );
    assert!(!fixture.root.join("selected.marker").exists());
    Ok(())
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const CONFIG: &str = r#"
schema = 3
repository = "example/selection-plan-handoff"
profile = "generic"
verified = true
default_branch = "main"
providers = ["github-hosted"]
automatic_providers = ["github-hosted"]
default_dispatch_providers = ["github-hosted"]

[[unit]]
id = "selected"
platform = "linux-x64"
trust = "untrusted-ok"
kind = "rust"
root = "."
watch = ["selected/**"]
pr_commands = ["printf selected > selected.marker"]
full_commands = ["printf selected > selected.marker"]

[[unit]]
id = "unselected"
platform = "linux-x64"
trust = "untrusted-ok"
kind = "rust"
root = "."
watch = ["unselected/**"]
pr_commands = ["printf unselected > unselected.marker"]
full_commands = ["printf unselected > unselected.marker"]
"#;

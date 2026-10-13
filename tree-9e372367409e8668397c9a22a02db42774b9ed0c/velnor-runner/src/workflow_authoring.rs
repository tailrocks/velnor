use std::{collections::BTreeSet, path::Path, process::Command};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::cli::{WorkflowArgs, WorkflowCheckArgs, WorkflowCommand};

pub fn workflow(args: WorkflowArgs) -> Result<()> {
    match args.command {
        WorkflowCommand::Check(args) => check(args),
    }
}

fn check(args: WorkflowCheckArgs) -> Result<()> {
    let value = evaluate_pkl_json(&args.pkl_bin, &args.path)?;
    let summary = validate_workflow_json(&value)?;
    println!(
        "Pkl workflow check passed: {} job(s), {} step(s).",
        summary.jobs, summary.steps
    );
    Ok(())
}

fn evaluate_pkl_json(pkl_bin: &Path, path: &Path) -> Result<Value> {
    let output = Command::new(pkl_bin)
        .arg("eval")
        .arg("--format")
        .arg("json")
        .arg(path)
        .output()
        .with_context(|| format!("run Pkl evaluator {}", pkl_bin.display()))?;

    if !output.status.success() {
        bail!(
            "Pkl evaluation failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    serde_json::from_slice(&output.stdout).context("parse Pkl JSON output")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkflowCheckSummary {
    jobs: usize,
    steps: usize,
}

fn validate_workflow_json(value: &Value) -> Result<WorkflowCheckSummary> {
    let root = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("workflow output must be a JSON object"))?;
    let jobs = root
        .get("jobs")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("workflow output must contain a jobs object"))?;
    if jobs.is_empty() {
        bail!("workflow jobs object must not be empty");
    }

    let mut steps = 0;
    for (job_id, job) in jobs {
        validate_job_id(job_id)?;
        let job = job
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("jobs.{job_id} must be an object"))?;
        validate_job_needs(job_id, job.get("needs"), jobs.keys().map(String::as_str))?;
        if let Some(job_steps) = job.get("steps") {
            let job_steps = job_steps
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("jobs.{job_id}.steps must be an array"))?;
            let mut step_ids = BTreeSet::new();
            for (index, step) in job_steps.iter().enumerate() {
                validate_step(job_id, index, step, &mut step_ids)?;
            }
            steps += job_steps.len();
        }
    }

    Ok(WorkflowCheckSummary {
        jobs: jobs.len(),
        steps,
    })
}

fn validate_job_id(job_id: &str) -> Result<()> {
    let mut chars = job_id.chars();
    let Some(first) = chars.next() else {
        bail!("job id must not be empty");
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        bail!("job id '{job_id}' must start with a letter or underscore");
    }
    if chars.any(|ch| !(ch == '_' || ch == '-' || ch.is_ascii_alphanumeric())) {
        bail!("job id '{job_id}' contains unsupported characters");
    }
    Ok(())
}

fn validate_job_needs<'a>(
    job_id: &str,
    needs: Option<&Value>,
    known_jobs: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let Some(needs) = needs else {
        return Ok(());
    };
    let known_jobs = known_jobs.collect::<BTreeSet<_>>();
    let validate_need = |value: &str| -> Result<()> {
        validate_job_id(value)
            .with_context(|| format!("jobs.{job_id}.needs contains invalid job id"))?;
        if value == job_id {
            bail!("jobs.{job_id}.needs must not reference itself");
        }
        if !known_jobs.contains(value) {
            bail!("jobs.{job_id}.needs references unknown job '{value}'");
        }
        Ok(())
    };
    match needs {
        Value::String(value) => validate_need(value),
        Value::Array(values) => {
            for value in values {
                let value = value.as_str().ok_or_else(|| {
                    anyhow::anyhow!("jobs.{job_id}.needs entries must be strings")
                })?;
                validate_need(value)?;
            }
            Ok(())
        }
        _ => bail!("jobs.{job_id}.needs must be a string or array of strings"),
    }
}

fn validate_step(
    job_id: &str,
    index: usize,
    step: &Value,
    seen_step_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let step = step
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("jobs.{job_id}.steps[{index}] must be an object"))?;
    let has_run = step.contains_key("run") || step.contains_key("command");
    let has_uses = step.contains_key("uses") || step.contains_key("action");
    if has_run && has_uses {
        bail!("jobs.{job_id}.steps[{index}] cannot define both run/command and uses/action");
    }
    if !has_run && !has_uses {
        bail!("jobs.{job_id}.steps[{index}] must define run/command or uses/action");
    }
    if let Some(step_id) = step.get("id").and_then(Value::as_str) {
        validate_job_id(step_id)
            .with_context(|| format!("jobs.{job_id}.steps[{index}].id is invalid"))?;
        if !seen_step_ids.insert(step_id.to_string()) {
            bail!("jobs.{job_id}.steps[{index}].id duplicates earlier step id '{step_id}'");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_github_like_workflow_json() {
        let value = json!({
            "name": "CI",
            "jobs": {
                "check": {
                    "runs-on": "ubuntu-latest",
                    "steps": [
                        { "uses": "actions/checkout@v6" },
                        { "id": "fmt", "run": "cargo fmt --check" }
                    ]
                },
                "required": {
                    "needs": ["check"],
                    "steps": [
                        { "run": "echo OK" }
                    ]
                }
            }
        });

        assert_eq!(
            validate_workflow_json(&value).unwrap(),
            WorkflowCheckSummary { jobs: 2, steps: 3 }
        );
    }

    #[test]
    fn rejects_step_with_run_and_uses() {
        let value = json!({
            "jobs": {
                "check": {
                    "steps": [
                        { "run": "echo no", "uses": "actions/checkout@v6" }
                    ]
                }
            }
        });

        let error = validate_workflow_json(&value).unwrap_err().to_string();
        assert!(error.contains("cannot define both run/command and uses/action"));
    }

    #[test]
    fn rejects_invalid_job_id() {
        let value = json!({
            "jobs": {
                "test api": {
                    "steps": [
                        { "run": "echo no" }
                    ]
                }
            }
        });

        let error = validate_workflow_json(&value).unwrap_err().to_string();
        assert!(error.contains("unsupported characters"));
    }

    #[test]
    fn rejects_unknown_needed_job() {
        let value = json!({
            "jobs": {
                "test": {
                    "needs": ["build"],
                    "steps": [
                        { "run": "echo no" }
                    ]
                }
            }
        });

        let error = validate_workflow_json(&value).unwrap_err().to_string();
        assert!(error.contains("references unknown job 'build'"));
    }

    #[test]
    fn rejects_duplicate_step_id_in_job() {
        let value = json!({
            "jobs": {
                "check": {
                    "steps": [
                        { "id": "same", "run": "echo one" },
                        { "id": "same", "run": "echo two" }
                    ]
                }
            }
        });

        let error = validate_workflow_json(&value).unwrap_err().to_string();
        assert!(error.contains("duplicates earlier step id 'same'"));
    }
}

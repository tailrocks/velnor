//! Source-bound GitHub Actions workflow expectation derivation.
//!
//! This parser consumes immutable workflow/action bytes captured by the G0
//! collector. It never derives expected work from run/job result rows. A
//! missing referenced source, dynamic runner target, empty job map, or
//! malformed YAML is an error; callers must fail closed.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_yaml::{Mapping, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::g0_contract::{G0WorkflowDependency, G0WorkflowSource};

const MAX_RECURSION: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedWorkflowPlan {
    pub jobs: Vec<DerivedWorkflowJob>,
    pub child_edges: Vec<DerivedChildEdge>,
    pub events: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedWorkflowJob {
    pub job_id: String,
    pub provider: String,
    pub platform: String,
    pub architecture: String,
    pub uses_reusable_workflow: bool,
    /// One concrete finite matrix assignment.  The logical `job_id` remains
    /// the source job key; the assignment prevents two matrix instances from
    /// being silently collapsed while validating their concrete target.
    pub matrix: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedChildEdge {
    pub workload_id: String,
    pub root_workload_id: String,
    pub repository: String,
    pub workflow_path: String,
    pub event: String,
    pub relation: String,
    pub source_sha: String,
    pub parent_repository: String,
    pub parent_workflow_path: String,
    pub parent_source_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceKey {
    repository: String,
    path: String,
}

/// Derive root workflow jobs and recursive child obligations from immutable
/// source blobs. Dependencies must contain every `uses:` source encountered.
pub(crate) fn derive_workflow_plan(
    root: &G0WorkflowSource,
    dependencies: &[G0WorkflowDependency],
) -> Result<DerivedWorkflowPlan> {
    let mut sources = BTreeMap::<SourceKey, &G0WorkflowSource>::new();
    for dependency in dependencies {
        let key = SourceKey {
            repository: dependency.source.repository.clone(),
            path: normalize_path(&dependency.source.path)?,
        };
        if sources.insert(key.clone(), &dependency.source).is_some() {
            bail!(
                "duplicate captured workflow dependency source {}/{}",
                key.repository,
                key.path
            );
        }
    }
    let root_key = SourceKey {
        repository: root.repository.clone(),
        path: normalize_path(&root.path)?,
    };
    let mut stack = BTreeSet::new();
    derive_source(root, &sources, &root_key, &mut stack, 0, None)
}

fn derive_source(
    source: &G0WorkflowSource,
    sources: &BTreeMap<SourceKey, &G0WorkflowSource>,
    source_key: &SourceKey,
    stack: &mut BTreeSet<SourceKey>,
    depth: usize,
    root_workload_id: Option<String>,
) -> Result<DerivedWorkflowPlan> {
    if depth > MAX_RECURSION {
        bail!(
            "workflow source recursion exceeded {} levels at {}/{}",
            MAX_RECURSION,
            source_key.repository,
            source_key.path
        );
    }
    if !stack.insert(source_key.clone()) {
        bail!(
            "workflow source dependency cycle at {}/{}",
            source_key.repository,
            source_key.path
        );
    }
    let bytes = BASE64
        .decode(&source.bytes_base64)
        .context("decode immutable workflow source bytes")?;
    let text = std::str::from_utf8(&bytes).context("workflow source is not UTF-8")?;
    let document: Value = serde_yaml::from_str(text).with_context(|| {
        format!(
            "parse workflow source {}/{}",
            source.repository, source.path
        )
    })?;
    let root_mapping = document
        .as_mapping()
        .ok_or_else(|| anyhow!("workflow source document must be a YAML mapping"))?;
    let events = workflow_events(root_mapping)?;
    let jobs = mapping_value(root_mapping, "jobs")
        .and_then(Value::as_mapping)
        .ok_or_else(|| anyhow!("workflow source has no jobs mapping"))?;
    if jobs.is_empty() {
        bail!("workflow source has an empty jobs mapping");
    }

    let mut plan = DerivedWorkflowPlan {
        jobs: Vec::new(),
        child_edges: Vec::new(),
        events,
    };
    for (job_key, job_value) in jobs {
        let job_id = job_key.as_str();
        if job_id.trim().is_empty() {
            bail!("workflow job key must be a non-empty string");
        }
        let job_id = job_id.to_owned();
        let root_for_job = root_workload_id.clone().unwrap_or_else(|| job_id.clone());
        let job = job_value
            .as_mapping()
            .ok_or_else(|| anyhow!("workflow job {job_id} must be a mapping"))?;
        reject_continue_on_error(job, &format!("workflow job {job_id}"))?;
        if let Some(condition) = mapping_value(job, "if")
            && !constant_condition(condition)?
        {
            continue;
        }
        let reusable = mapping_value(job, "uses").and_then(Value::as_str);
        let reusable_target = if let Some(uses) = reusable {
            let (target, pinned_ref) = resolve_source_target(uses, source)?;
            let dependency = sources.get(&target).ok_or_else(|| {
                anyhow!(
                    "workflow job {job_id} references uncaptured immutable source {}/{}",
                    target.repository,
                    target.path
                )
            })?;
            if dependency.revision != pinned_ref && dependency.source_sha != pinned_ref {
                bail!(
                    "workflow job {job_id} pins {pinned_ref}, but captured source {}/{} has revision {} and commit {}",
                    target.repository,
                    target.path,
                    dependency.revision,
                    dependency.source_sha
                );
            }
            let child_plan = derive_source(
                dependency,
                sources,
                &target,
                stack,
                depth + 1,
                Some(root_for_job.clone()),
            )?;
            if !child_plan.events.contains("workflow_call") {
                bail!(
                    "reusable workflow {}/{} must declare workflow_call",
                    target.repository,
                    target.path
                );
            }
            let mut child_job_ids = BTreeSet::new();
            if child_plan
                .jobs
                .iter()
                .any(|child| !child_job_ids.insert(child.job_id.clone()))
            {
                bail!(
                    "reusable workflow {}/{} expands a logical child job into multiple concrete matrix instances; the child graph has no concrete matrix identity",
                    target.repository,
                    target.path
                );
            }
            let child_job = child_plan.jobs.first().cloned().ok_or_else(|| {
                anyhow!(
                    "reusable workflow {}/{} has no derived jobs",
                    target.repository,
                    target.path
                )
            })?;
            if child_plan.jobs.iter().any(|job| {
                job.provider != child_job.provider
                    || job.platform != child_job.platform
                    || job.architecture != child_job.architecture
            }) {
                bail!(
                    "reusable workflow {}/{} has matrix instances with different targets",
                    target.repository,
                    target.path
                );
            }
            plan.child_edges.extend(child_plan.child_edges);
            plan.child_edges.push(DerivedChildEdge {
                workload_id: job_id.clone(),
                root_workload_id: root_for_job.clone(),
                repository: target.repository,
                workflow_path: target.path,
                event: "workflow_call".to_owned(),
                relation: "reusable_workflow".to_owned(),
                source_sha: dependency.source_sha.clone(),
                parent_repository: source.repository.clone(),
                parent_workflow_path: source.path.clone(),
                parent_source_sha: source.source_sha.clone(),
            });
            Some((
                child_job.provider,
                child_job.platform,
                child_job.architecture,
            ))
        } else {
            None
        };
        let matrix = job_matrix(job)
            .with_context(|| format!("derive finite matrix for workflow job {job_id}"))?;
        for assignment in matrix {
            let (provider, platform, architecture) = if let Some(target) = reusable_target.clone() {
                target
            } else {
                let runs_on = mapping_value(job, "runs-on")
                    .ok_or_else(|| anyhow!("workflow job {job_id} lacks source runs-on target"))?;
                let runs_on = resolve_matrix_value(runs_on, &assignment)?;
                runner_target(&runs_on)
                    .with_context(|| format!("derive runner target for workflow job {job_id}"))?
            };
            action_sources(job, source, sources)?;
            plan.jobs.push(DerivedWorkflowJob {
                job_id: job_id.clone(),
                provider,
                platform,
                architecture,
                uses_reusable_workflow: reusable.is_some(),
                matrix: assignment,
            });
        }
    }

    // These triggers are source-derived obligations. They are retained as
    // explicit child relations even when GitHub's run API does not expose a
    // parent link; the run collector must later prove that association.
    if plan.events.contains("workflow_run") {
        for job in &plan.jobs {
            plan.child_edges.push(DerivedChildEdge {
                workload_id: job.job_id.clone(),
                root_workload_id: root_workload_id
                    .clone()
                    .unwrap_or_else(|| job.job_id.clone()),
                repository: source.repository.clone(),
                workflow_path: source.path.clone(),
                event: "workflow_run".to_owned(),
                relation: "workflow_run".to_owned(),
                source_sha: source.source_sha.clone(),
                parent_repository: source.repository.clone(),
                parent_workflow_path: source.path.clone(),
                parent_source_sha: source.source_sha.clone(),
            });
        }
    }
    if plan.events.contains("workflow_dispatch") {
        for job in &plan.jobs {
            plan.child_edges.push(DerivedChildEdge {
                workload_id: job.job_id.clone(),
                root_workload_id: root_workload_id
                    .clone()
                    .unwrap_or_else(|| job.job_id.clone()),
                repository: source.repository.clone(),
                workflow_path: source.path.clone(),
                event: "workflow_dispatch".to_owned(),
                relation: "dispatch".to_owned(),
                source_sha: source.source_sha.clone(),
                parent_repository: source.repository.clone(),
                parent_workflow_path: source.path.clone(),
                parent_source_sha: source.source_sha.clone(),
            });
        }
    }
    stack.remove(source_key);
    Ok(plan)
}

fn action_sources(
    job: &Mapping,
    source: &G0WorkflowSource,
    sources: &BTreeMap<SourceKey, &G0WorkflowSource>,
) -> Result<()> {
    let Some(steps) = mapping_value(job, "steps").and_then(Value::as_sequence) else {
        return Ok(());
    };
    for (index, step) in steps.iter().enumerate() {
        let Some(step) = step.as_mapping() else {
            bail!("workflow step {index} must be a mapping");
        };
        reject_continue_on_error(step, &format!("workflow job step {index}"))?;
        if let Some(condition) = mapping_value(step, "if")
            && !constant_condition(condition)?
        {
            continue;
        }
        let Some(uses) = mapping_value(step, "uses").and_then(Value::as_str) else {
            continue;
        };
        let (target, pinned_ref) = resolve_source_target(uses, source)
            .with_context(|| format!("resolve immutable action source in workflow step {index}"))?;
        let Some(dependency) = sources.get(&target) else {
            bail!(
                "workflow step {index} references uncaptured immutable action source {}/{}",
                target.repository,
                target.path
            );
        };
        if dependency.revision != pinned_ref && dependency.source_sha != pinned_ref {
            bail!(
                "workflow step {index} pins {pinned_ref}, but captured action source {}/{} has revision {} and commit {}",
                target.repository,
                target.path,
                dependency.revision,
                dependency.source_sha
            );
        }
    }
    Ok(())
}

/// Return the finite literal matrix assignments for a job.  GitHub's
/// `include`, `exclude`, expressions, and object-valued matrix entries require
/// expression-context evaluation and are intentionally rejected here rather
/// than approximated from a result ledger.
fn job_matrix(job: &Mapping) -> Result<Vec<BTreeMap<String, String>>> {
    let Some(strategy) = mapping_value(job, "strategy") else {
        return Ok(vec![BTreeMap::new()]);
    };
    let strategy = strategy
        .as_mapping()
        .ok_or_else(|| anyhow!("workflow strategy must be a mapping"))?;
    let Some(matrix) = mapping_value(strategy, "matrix") else {
        return Ok(vec![BTreeMap::new()]);
    };
    let matrix = matrix
        .as_mapping()
        .ok_or_else(|| anyhow!("workflow matrix must be a mapping"))?;
    if matrix.contains_key("include") || matrix.contains_key("exclude") {
        bail!("workflow matrix include/exclude requires expression-aware derivation");
    }
    if matrix.is_empty() {
        bail!("workflow matrix cannot be empty");
    }
    let mut assignments = vec![BTreeMap::new()];
    for (key, values) in matrix {
        let key = key.as_str();
        if key.trim().is_empty() {
            bail!("workflow matrix key must be a non-empty string");
        }
        let values = values
            .as_sequence()
            .ok_or_else(|| anyhow!("workflow matrix key {key} must contain a literal sequence"))?;
        if values.is_empty() {
            bail!("workflow matrix key {key} has no values");
        }
        let mut scalar_values = BTreeSet::new();
        let base_assignments = assignments.clone();
        let mut expanded = Vec::with_capacity(base_assignments.len() * values.len());
        for value in values {
            let value = matrix_scalar(value)
                .with_context(|| format!("workflow matrix key {key} has a non-literal value"))?;
            if !scalar_values.insert(value.clone()) {
                bail!("workflow matrix key {key} repeats value {value}");
            }
            for assignment in &base_assignments {
                let mut assignment = assignment.clone();
                assignment.insert(key.to_owned(), value.clone());
                expanded.push(assignment);
            }
        }
        assignments = expanded;
    }
    Ok(assignments)
}

fn matrix_scalar(value: &Value) -> Result<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() && !value.contains("${{") => {
            Ok(value.clone())
        }
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        _ => bail!("matrix values must be non-empty literal strings, booleans, or numbers"),
    }
}

fn resolve_matrix_value(value: &Value, assignment: &BTreeMap<String, String>) -> Result<Value> {
    match value {
        Value::String(value) if value.contains("${{") => {
            let expression = value
                .trim()
                .strip_prefix("${{")
                .and_then(|value| value.strip_suffix("}}").map(str::trim))
                .ok_or_else(|| anyhow!("matrix expression is not a complete expression"))?;
            let key = expression
                .strip_prefix("matrix.")
                .filter(|key| !key.trim().is_empty())
                .ok_or_else(|| anyhow!("matrix expression is not a direct matrix lookup"))?;
            let resolved = assignment
                .get(key)
                .ok_or_else(|| anyhow!("matrix expression references unknown key {key}"))?;
            Ok(Value::String(resolved.clone()))
        }
        Value::String(_) => Ok(value.clone()),
        Value::Sequence(values) => Ok(Value::Sequence(
            values
                .iter()
                .map(|value| resolve_matrix_value(value, assignment))
                .collect::<Result<Vec<_>>>()?,
        )),
        _ => Ok(value.clone()),
    }
}

fn constant_condition(value: &Value) -> Result<bool> {
    match value {
        Value::Bool(value) => Ok(*value),
        Value::String(value) => match value.trim() {
            "true" | "${{ true }}" => Ok(true),
            "false" | "${{ false }}" => Ok(false),
            _ => bail!("workflow condition requires expression-aware derivation"),
        },
        _ => bail!("workflow condition must be a literal boolean"),
    }
}

fn reject_continue_on_error(mapping: &Mapping, subject: &str) -> Result<()> {
    let Some(value) = mapping_value(mapping, "continue-on-error") else {
        return Ok(());
    };
    match constant_condition(value) {
        Ok(false) => Ok(()),
        Ok(true) => bail!("{subject} enables continue-on-error"),
        Err(_) => bail!("{subject} has a dynamic continue-on-error expression"),
    }
}

fn resolve_source_target(
    reference: &str,
    current: &G0WorkflowSource,
) -> Result<(SourceKey, String)> {
    let (target, pinned_ref) = reference
        .split_once('@')
        .ok_or_else(|| anyhow!("workflow source reference {reference} lacks immutable @ref"))?;
    if target.trim().is_empty()
        || pinned_ref.trim().is_empty()
        || pinned_ref.len() != 40
        || !pinned_ref
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("workflow source reference {reference} has empty target/ref");
    }
    if target.starts_with("docker://") || target.starts_with("http://") {
        bail!("workflow source reference {reference} is not an immutable repository source");
    }
    if target.starts_with("./") {
        return Ok((
            SourceKey {
                repository: current.repository.clone(),
                path: normalize_path(target)?,
            },
            pinned_ref.to_owned(),
        ));
    }
    let mut parts = target.splitn(3, '/');
    let owner = parts.next().unwrap_or_default();
    let repository = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    if owner.is_empty() || repository.is_empty() || path.is_empty() {
        bail!("workflow source reference {reference} lacks owner/repository/path");
    }
    Ok((
        SourceKey {
            repository: format!("{owner}/{repository}"),
            path: normalize_path(path)?,
        },
        pinned_ref.to_owned(),
    ))
}

fn normalize_path(path: &str) -> Result<String> {
    let path = path.trim().trim_start_matches("./");
    if path.is_empty() || path.contains("..") || path.starts_with('/') {
        bail!("workflow source path is not a safe relative path: {path}");
    }
    Ok(path.to_owned())
}

fn workflow_events(mapping: &Mapping) -> Result<BTreeSet<String>> {
    let trigger =
        mapping_value(mapping, "on").ok_or_else(|| anyhow!("workflow source lacks on"))?;
    let mut events = BTreeSet::new();
    match trigger {
        Value::String(event) => {
            events.insert(event.clone());
        }
        Value::Sequence(values) => {
            for value in values {
                let event = value
                    .as_str()
                    .filter(|event| !event.trim().is_empty())
                    .ok_or_else(|| anyhow!("workflow trigger sequence contains non-string"))?;
                events.insert(event.to_owned());
            }
        }
        Value::Mapping(values) => {
            for (key, configuration) in values {
                let event = key.as_str();
                if event.trim().is_empty() {
                    bail!("workflow trigger key is not a non-empty string");
                }
                validate_trigger_configuration(event, configuration)?;
                events.insert(event.to_owned());
            }
        }
        _ => bail!("workflow trigger must be a string, sequence, or mapping"),
    }
    if events.is_empty() {
        bail!("workflow source has no trigger events");
    }
    Ok(events)
}

fn validate_trigger_configuration(event: &str, configuration: &Value) -> Result<()> {
    match configuration {
        Value::Null => Ok(()),
        Value::Mapping(values) if values.is_empty() => Ok(()),
        Value::Mapping(_) if matches!(event, "workflow_call" | "workflow_dispatch") => Ok(()),
        Value::Mapping(_) => bail!(
            "workflow trigger {event} has branch/path/type conditions that require event-aware derivation"
        ),
        _ => bail!("workflow trigger {event} has an unsupported configuration"),
    }
}

fn runner_target(value: &Value) -> Result<(String, String, String)> {
    let mut labels = Vec::new();
    match value {
        Value::String(label) => labels.push(label.as_str()),
        Value::Sequence(values) => {
            for value in values {
                labels.push(
                    value
                        .as_str()
                        .ok_or_else(|| anyhow!("runs-on label is not a string"))?,
                );
            }
        }
        _ => bail!("runs-on must be a string or string sequence"),
    }
    if labels.is_empty() || labels.iter().any(|label| label.contains("${{")) {
        bail!("runs-on must resolve to concrete source labels");
    }
    let joined = labels.join(" ").to_ascii_lowercase();
    let provider = if joined.contains("self-hosted") || joined.contains("velnor") {
        "velnor"
    } else {
        "github"
    };
    let platform = if joined.contains("ubuntu") || joined.contains("linux") {
        "linux"
    } else if joined.contains("macos") || joined.contains("darwin") {
        "macos"
    } else if joined.contains("windows") {
        "windows"
    } else {
        bail!("runs-on labels do not prove a supported platform")
    };
    let architecture = if joined.contains("arm64") || joined.contains("aarch64") {
        "arm64"
    } else if joined.contains("x64") || joined.contains("amd64") || joined.contains("x86_64") {
        "amd64"
    } else if provider == "github" && joined.contains("ubuntu-") {
        // GitHub's standard Ubuntu labels are x64 unless they carry an ARM
        // suffix. Keep this rule explicit and deterministic.
        "amd64"
    } else {
        bail!("runs-on labels do not prove a supported architecture")
    };
    Ok((
        provider.to_owned(),
        platform.to_owned(),
        architecture.to_owned(),
    ))
}

fn mapping_value<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(key)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "parser fixtures intentionally use direct assertions"
)]
mod tests {
    use super::*;

    fn source(repository: &str, path: &str, yaml: &str) -> G0WorkflowSource {
        G0WorkflowSource {
            repository: repository.to_owned(),
            path: path.to_owned(),
            revision: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            source_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            source_url: format!(
                "https://github.com/{repository}/blob/{}/{}",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", path
            ),
            media_type: "text/yaml".to_owned(),
            canonicalization: "raw-utf8".to_owned(),
            sha256: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_owned(),
            storage_ref:
                "sha256://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
            byte_length: yaml.len() as u64,
            bytes_base64: BASE64.encode(yaml.as_bytes()),
            raw_object_refs: vec!["raw-1".to_owned()],
        }
    }

    #[test]
    fn derives_jobs_recursive_reusable_workflow_and_triggers() {
        let root_yaml = r#"
on:
  workflow_run: {}
  workflow_dispatch: {}
jobs:
  scan:
    uses: ./.github/workflows/reusable.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  direct:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout/action.yml@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
"#;
        let reusable_yaml = r#"
on:
  workflow_call: {}
jobs:
  nested:
    uses: ./.github/workflows/deep.yml@cccccccccccccccccccccccccccccccccccccccc
"#;
        let deep_yaml = r#"
on:
  workflow_call: {}
jobs:
  deep:
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", root_yaml);
        let reusable = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            reusable_yaml,
        );
        let mut deep = source("tailrocks/velnor", ".github/workflows/deep.yml", deep_yaml);
        deep.revision = "cccccccccccccccccccccccccccccccccccccccc".to_owned();
        let action = source("actions/checkout", "action.yml", "name: checkout\n");
        let dependencies = vec![
            G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: reusable,
            },
            G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: deep,
            },
            G0WorkflowDependency {
                kind: "action".to_owned(),
                source: action,
            },
        ];
        let plan = derive_workflow_plan(&root, &dependencies).unwrap();
        assert_eq!(
            plan.jobs
                .iter()
                .map(|job| job.job_id.as_str())
                .collect::<Vec<_>>(),
            vec!["scan", "direct"]
        );
        assert!(plan.events.contains("workflow_run"));
        assert!(plan.events.contains("workflow_dispatch"));
        assert!(plan.child_edges.iter().any(|edge| {
            edge.relation == "reusable_workflow" && edge.workflow_path.ends_with("reusable.yml")
        }));
        assert!(plan.child_edges.iter().any(|edge| {
            edge.relation == "reusable_workflow"
                && edge.workflow_path.ends_with("deep.yml")
                && edge.source_sha == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                && edge.parent_workflow_path.ends_with("reusable.yml")
        }));
        assert!(plan
            .child_edges
            .iter()
            .any(|edge| edge.relation == "workflow_run"));
        assert!(plan
            .child_edges
            .iter()
            .any(|edge| edge.relation == "dispatch"));

        let mut stale_capture = dependencies;
        stale_capture[0].source.revision = "cccccccccccccccccccccccccccccccccccccccc".to_owned();
        assert!(derive_workflow_plan(&root, &stale_capture).is_err());
    }

    #[test]
    fn omitted_child_source_and_dynamic_job_are_rejected() {
        let yaml = r#"
on: [push]
jobs:
  child:
    uses: ./.github/workflows/missing.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        assert!(derive_workflow_plan(&root, &[]).is_err());

        let dynamic = r#"
on: [push]
jobs:
  scan:
    runs-on: ${{ matrix.os }}
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", dynamic);
        assert!(derive_workflow_plan(&root, &[]).is_err());
    }

    #[test]
    fn mutable_uses_ref_is_rejected() {
        let root_yaml = r#"
on: [push]
jobs:
  child:
    uses: ./.github/workflows/reusable.yml@main
"#;
        let child_yaml = r#"
on:
  workflow_call: {}
jobs:
  nested:
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", root_yaml);
        let child = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            child_yaml,
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: child,
        }];
        assert!(derive_workflow_plan(&root, &dependencies).is_err());
    }

    #[test]
    fn reusable_source_without_workflow_call_is_rejected() {
        let root_yaml = r#"
on: [push]
jobs:
  child:
    uses: ./.github/workflows/reusable.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
"#;
        let child_yaml = r#"
on: [push]
jobs:
  nested:
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", root_yaml);
        let child = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            child_yaml,
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: child,
        }];
        let error = derive_workflow_plan(&root, &dependencies)
            .expect_err("a job-level uses source must be reusable");
        assert!(error.to_string().contains("must declare workflow_call"));
    }

    #[test]
    fn reusable_child_matrix_instances_are_rejected_before_representative_selection() {
        let root_yaml = r#"
on: [push]
jobs:
  child:
    uses: ./.github/workflows/reusable.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
"#;
        let child_yaml = r#"
on:
  workflow_call: {}
jobs:
  nested:
    strategy:
      matrix:
        os: [ubuntu-24.04, ubuntu-22.04]
    runs-on: ${{ matrix.os }}
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", root_yaml);
        let child = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            child_yaml,
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: child,
        }];
        let error = derive_workflow_plan(&root, &dependencies)
            .expect_err("child matrix instances cannot be collapsed to the first job");
        assert!(error.to_string().contains("concrete matrix identity"));
    }

    #[test]
    fn conditional_trigger_filters_are_rejected() {
        let yaml = r#"
on:
  push:
    branches: [main]
jobs:
  scan:
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        let error = derive_workflow_plan(&root, &[])
            .expect_err("branch filters need event-aware source derivation");
        assert!(error.to_string().contains("event-aware derivation"));
    }

    #[test]
    fn continue_on_error_is_rejected_for_jobs_and_steps() {
        let job_yaml = r#"
on: [push]
jobs:
  scan:
    continue-on-error: true
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", job_yaml);
        assert!(derive_workflow_plan(&root, &[])
            .expect_err("required jobs cannot hide failures")
            .to_string()
            .contains("continue-on-error"));

        let step_yaml = r#"
on: [push]
jobs:
  scan:
    runs-on: ubuntu-24.04
    steps:
      - continue-on-error: true
        run: ./scan
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", step_yaml);
        assert!(derive_workflow_plan(&root, &[])
            .expect_err("required steps cannot hide failures")
            .to_string()
            .contains("continue-on-error"));
    }

    #[test]
    fn runtime_dependent_condition_is_rejected_but_finite_matrix_is_derived() {
        let conditional = r#"
on: [push]
jobs:
  scan:
    if: github.ref == 'refs/heads/main'
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", conditional);
        assert!(derive_workflow_plan(&root, &[]).is_err());

        let matrix = r#"
on: [push]
jobs:
  scan:
    strategy:
      matrix:
        os: [ubuntu-24.04, ubuntu-22.04]
        arch: [amd64, arm64]
    runs-on: [self-hosted, "${{ matrix.os }}", "${{ matrix.arch }}"]
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", matrix);
        let plan = derive_workflow_plan(&root, &[]).expect("finite matrix");
        assert_eq!(plan.jobs.len(), 4);
        assert!(plan.jobs.iter().all(|job| job.provider == "velnor"));
        assert!(plan.jobs.iter().any(|job| {
            job.matrix.get("os") == Some(&"ubuntu-24.04".to_owned())
                && job.matrix.get("arch") == Some(&"arm64".to_owned())
        }));

        let expression_matrix = r#"
on: [push]
jobs:
  scan:
    strategy:
      matrix:
        os: [ubuntu-24.04]
    runs-on: ${{ matrix.missing }}
    steps: []
"#;
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            expression_matrix,
        );
        assert!(derive_workflow_plan(&root, &[]).is_err());
    }

    #[test]
    fn literal_conditions_control_source_obligations() {
        let yaml = r#"
on: [push]
jobs:
  omitted:
    if: false
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/missing@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  kept:
    if: true
    runs-on: ubuntu-24.04
    steps:
      - if: false
        uses: actions/missing@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        let plan = derive_workflow_plan(&root, &[]).expect("literal conditions");
        assert_eq!(
            plan.jobs
                .iter()
                .map(|job| job.job_id.as_str())
                .collect::<Vec<_>>(),
            ["kept"]
        );
    }
}

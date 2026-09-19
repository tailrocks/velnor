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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedChildEdge {
    pub workload_id: String,
    pub repository: String,
    pub workflow_path: String,
    pub event: String,
    pub relation: String,
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
    derive_source(root, &sources, &root_key, &mut stack, 0)
}

fn derive_source(
    source: &G0WorkflowSource,
    sources: &BTreeMap<SourceKey, &G0WorkflowSource>,
    source_key: &SourceKey,
    stack: &mut BTreeSet<SourceKey>,
    depth: usize,
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
        jobs: Vec::with_capacity(jobs.len()),
        child_edges: Vec::new(),
        events,
    };
    for (job_key, job_value) in jobs {
        let job_id = job_key.as_str();
        if job_id.trim().is_empty() {
            bail!("workflow job key must be a non-empty string");
        }
        let job_id = job_id.to_owned();
        let job = job_value
            .as_mapping()
            .ok_or_else(|| anyhow!("workflow job {job_id} must be a mapping"))?;
        let reusable = mapping_value(job, "uses").and_then(Value::as_str);
        let (provider, platform, architecture) = if let Some(uses) = reusable {
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
            let child_plan = derive_source(dependency, sources, &target, stack, depth + 1)?;
            let child_job = child_plan.jobs.first().ok_or_else(|| {
                anyhow!(
                    "reusable workflow {}/{} has no derived jobs",
                    target.repository,
                    target.path
                )
            })?;
            plan.child_edges.push(DerivedChildEdge {
                workload_id: job_id.clone(),
                repository: target.repository,
                workflow_path: target.path,
                event: "workflow_call".to_owned(),
                relation: "reusable_workflow".to_owned(),
            });
            (
                child_job.provider.clone(),
                child_job.platform.clone(),
                child_job.architecture.clone(),
            )
        } else {
            let runs_on = mapping_value(job, "runs-on")
                .ok_or_else(|| anyhow!("workflow job {job_id} lacks source runs-on target"))?;
            runner_target(runs_on)
                .with_context(|| format!("derive runner target for workflow job {job_id}"))?
        };
        action_sources(job, source, sources)?;
        plan.jobs.push(DerivedWorkflowJob {
            job_id,
            provider,
            platform,
            architecture,
            uses_reusable_workflow: reusable.is_some(),
        });
    }

    // These triggers are source-derived obligations. They are retained as
    // explicit child relations even when GitHub's run API does not expose a
    // parent link; the run collector must later prove that association.
    if plan.events.contains("workflow_run") {
        for job in &plan.jobs {
            plan.child_edges.push(DerivedChildEdge {
                workload_id: job.job_id.clone(),
                repository: source.repository.clone(),
                workflow_path: source.path.clone(),
                event: "workflow_run".to_owned(),
                relation: "workflow_run".to_owned(),
            });
        }
    }
    if plan.events.contains("workflow_dispatch") {
        for job in &plan.jobs {
            plan.child_edges.push(DerivedChildEdge {
                workload_id: job.job_id.clone(),
                repository: source.repository.clone(),
                workflow_path: source.path.clone(),
                event: "workflow_dispatch".to_owned(),
                relation: "dispatch".to_owned(),
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
            for key in values.keys() {
                let event = key.as_str();
                if event.trim().is_empty() {
                    bail!("workflow trigger key is not a non-empty string");
                }
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
            source_url: format!("https://github.com/{repository}/blob/main/{path}"),
            media_type: "text/yaml".to_owned(),
            canonicalization: "raw-utf8".to_owned(),
            sha256: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
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
  workflow_run:
    workflows: [Build]
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
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", root_yaml);
        let reusable = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            reusable_yaml,
        );
        let action = source("actions/checkout", "action.yml", "name: checkout\n");
        let dependencies = vec![
            G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: reusable,
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
}

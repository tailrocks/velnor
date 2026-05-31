#![allow(dead_code)]

use crate::job_message::{ActionReferenceType, ActionStep};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Deserialize)]
pub struct ActionMetadata {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub runs: ActionRuns,
    #[serde(default)]
    pub inputs: BTreeMap<String, ActionInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionInput {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "default")]
    pub default_value: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionRuns {
    pub using: String,
    #[serde(default)]
    pub main: Option<String>,
    #[serde(default)]
    pub pre: Option<String>,
    #[serde(default)]
    pub post: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionRuntime {
    JavaScript { node: String, main: String },
    Composite,
    Docker { image: String },
}

impl ActionMetadata {
    pub fn runtime(&self) -> Result<ActionRuntime> {
        let using = self.runs.using.to_ascii_lowercase();
        if matches!(using.as_str(), "node12" | "node16" | "node20" | "node24") {
            let main =
                self.runs.main.clone().ok_or_else(|| {
                    anyhow::anyhow!("JavaScript action metadata missing runs.main")
                })?;
            return Ok(ActionRuntime::JavaScript { node: using, main });
        }
        if using == "composite" {
            return Ok(ActionRuntime::Composite);
        }
        if using == "docker" {
            let image = self
                .runs
                .image
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Docker action metadata missing runs.image"))?;
            return Ok(ActionRuntime::Docker { image });
        }
        bail!("unsupported action runtime '{}'", self.runs.using)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryActionPlan {
    pub repository: String,
    pub git_ref: String,
    pub source_path: Option<String>,
    pub action_dir: PathBuf,
    pub inputs: BTreeMap<String, String>,
}

pub fn parse_action_metadata(contents: &str) -> Result<ActionMetadata> {
    serde_yaml::from_str(contents).context("parse action metadata")
}

pub fn repository_action_plans(
    steps: &[ActionStep],
    actions_host: &Path,
) -> Result<Vec<RepositoryActionPlan>> {
    let mut plans = Vec::new();
    for step in steps {
        if !step.enabled || step.reference_type() != Some(ActionReferenceType::Repository) {
            continue;
        }
        let Some(reference) = step.reference.as_ref() else {
            continue;
        };
        let Some(repository) = reference.name.as_ref() else {
            continue;
        };
        if repository.eq_ignore_ascii_case("actions/checkout") {
            continue;
        }
        let git_ref = reference
            .git_ref
            .clone()
            .ok_or_else(|| anyhow::anyhow!("repository action '{repository}' missing ref"))?;
        let action_dir = action_dir(
            actions_host,
            repository,
            &git_ref,
            reference.path.as_deref(),
        )?;
        plans.push(RepositoryActionPlan {
            repository: repository.clone(),
            git_ref,
            source_path: reference.path.clone(),
            action_dir,
            inputs: string_inputs(step)?,
        });
    }
    Ok(plans)
}

fn action_dir(
    actions_host: &Path,
    repository: &str,
    git_ref: &str,
    source_path: Option<&str>,
) -> Result<PathBuf> {
    let mut dir = actions_host
        .join("_actions")
        .join(sanitize_segment(repository))
        .join(sanitize_segment(git_ref));
    if let Some(source_path) = source_path.filter(|path| !path.is_empty()) {
        if source_path.starts_with('/') || source_path.contains("..") {
            bail!("unsupported repository action path '{source_path}'")
        }
        dir = dir.join(source_path);
    }
    Ok(dir)
}

fn string_inputs(step: &ActionStep) -> Result<BTreeMap<String, String>> {
    let Some(inputs) = step.inputs.as_ref().and_then(|value| value.as_object()) else {
        return Ok(BTreeMap::new());
    };
    let mut result = BTreeMap::new();
    for (name, value) in inputs {
        let Some(value) = value.as_str() else {
            bail!("repository action input '{name}' is not a string")
        };
        result.insert(name.clone(), value.to_string());
    }
    Ok(result)
}

fn sanitize_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_javascript_action_metadata() {
        let metadata = parse_action_metadata(
            r#"
name: Setup Tool
inputs:
  version:
    required: true
    default: latest
runs:
  using: node20
  main: dist/index.js
  post: dist/cleanup.js
"#,
        )
        .unwrap();

        assert_eq!(
            metadata.inputs["version"].default_value.as_deref(),
            Some("latest")
        );
        assert_eq!(
            metadata.runtime().unwrap(),
            ActionRuntime::JavaScript {
                node: "node20".into(),
                main: "dist/index.js".into()
            }
        );
    }

    #[test]
    fn parses_composite_action_metadata() {
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps: []
"#,
        )
        .unwrap();

        assert_eq!(metadata.runtime().unwrap(), ActionRuntime::Composite);
    }

    #[test]
    fn builds_repository_action_plan() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            { "reference": { "type": "Repository", "name": "actions/checkout", "ref": "v4" } },
            {
                "id": "setup",
                "reference": {
                    "type": "Repository",
                    "name": "actions/setup-python",
                    "ref": "v5",
                    "path": "sub/action"
                },
                "inputs": { "python-version": "3.12" }
            }
        ]))
        .unwrap();

        let plans = repository_action_plans(&steps, Path::new("/tmp/actions")).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].repository, "actions/setup-python");
        assert_eq!(plans[0].git_ref, "v5");
        assert_eq!(plans[0].inputs["python-version"], "3.12");
        assert_eq!(
            plans[0].action_dir,
            Path::new("/tmp/actions")
                .join("_actions")
                .join("actions_setup-python")
                .join("v5")
                .join("sub/action")
        );
    }
}

#![allow(dead_code)]

use crate::{
    checkout::fetch_git_ref,
    executor::CommandRunner,
    job_message::{ActionReferenceType, ActionStep},
    script_step::ScriptStep,
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
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
    #[serde(default)]
    pub steps: Vec<CompositeActionStep>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CompositeActionStep {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub uses: Option<String>,
    #[serde(default)]
    pub with: BTreeMap<String, String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, rename = "working-directory", alias = "workingDirectory")]
    pub working_directory: Option<String>,
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
    pub step_id: String,
    pub repository: String,
    pub git_ref: String,
    pub source_path: Option<String>,
    pub repository_dir: PathBuf,
    pub action_dir: PathBuf,
    pub inputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalActionPlan {
    pub step_id: String,
    pub action_dir: PathBuf,
    pub inputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub enum CompositeActionInvocation {
    Script(ScriptStep),
    Repository(RepositoryActionPlan),
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
        if is_local_action_reference(reference.name.as_deref(), reference.path.as_deref()) {
            continue;
        }
        if repository.eq_ignore_ascii_case("actions/checkout") {
            continue;
        }
        let git_ref = reference
            .git_ref
            .clone()
            .ok_or_else(|| anyhow::anyhow!("repository action '{repository}' missing ref"))?;
        let repository_dir = repository_dir(actions_host, repository, &git_ref);
        let action_dir = action_dir(
            actions_host,
            repository,
            &git_ref,
            reference.path.as_deref(),
        )?;
        plans.push(RepositoryActionPlan {
            step_id: step_id(step, plans.len()),
            repository: repository.clone(),
            git_ref,
            source_path: reference.path.clone(),
            repository_dir,
            action_dir,
            inputs: string_inputs(step)?,
        });
    }
    Ok(plans)
}

pub fn is_local_action_step(step: &ActionStep) -> bool {
    step.reference
        .as_ref()
        .and_then(|reference| {
            local_action_path(reference.name.as_deref(), reference.path.as_deref())
        })
        .is_some()
}

pub fn local_action_plans(
    steps: &[ActionStep],
    workspace_host: &Path,
) -> Result<Vec<LocalActionPlan>> {
    let mut plans = Vec::new();
    for step in steps {
        if !step.enabled || step.reference_type() != Some(ActionReferenceType::Repository) {
            continue;
        }
        let Some(reference) = step.reference.as_ref() else {
            continue;
        };
        let Some(path) = local_action_path(reference.name.as_deref(), reference.path.as_deref())
        else {
            continue;
        };
        plans.push(LocalActionPlan {
            step_id: step_id(step, plans.len()),
            action_dir: local_action_dir(workspace_host, path)?,
            inputs: string_inputs(step)?,
        });
    }
    Ok(plans)
}

pub fn composite_repository_action_plans(
    local_actions: &[(LocalActionPlan, ActionMetadata)],
    actions_host: &Path,
) -> Result<Vec<RepositoryActionPlan>> {
    let mut plans = Vec::new();
    for (plan, metadata) in local_actions {
        for invocation in composite_action_invocations(plan, metadata, "/__w", actions_host)? {
            if let CompositeActionInvocation::Repository(repository_plan) = invocation {
                plans.push(repository_plan);
            }
        }
    }
    Ok(plans)
}

fn step_id(step: &ActionStep, index: usize) -> String {
    step.id
        .as_deref()
        .or(step.context_name.as_deref())
        .or(step.name.as_deref())
        .map(sanitize_segment)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("action{}", index + 1))
}

pub fn download_repository_actions<R>(
    runner: &mut R,
    plans: &[RepositoryActionPlan],
) -> Result<Vec<ResolvedAction>>
where
    R: CommandRunner,
{
    let mut resolved = Vec::new();
    for plan in plans {
        fetch_git_ref(
            runner,
            &repository_clone_url(&plan.repository),
            &plan.git_ref,
            &plan.repository_dir,
            None,
        )?;
        resolved.push(resolve_action(plan)?);
    }
    Ok(resolved)
}

#[derive(Debug, Clone)]
pub struct ResolvedAction {
    pub plan: RepositoryActionPlan,
    pub metadata_path: PathBuf,
    pub metadata: ActionMetadata,
    pub runtime: ActionRuntime,
}

pub fn resolve_action(plan: &RepositoryActionPlan) -> Result<ResolvedAction> {
    let metadata_path = action_metadata_path(&plan.action_dir)?;
    let metadata = parse_action_metadata(
        &fs::read_to_string(&metadata_path)
            .with_context(|| format!("read {}", metadata_path.display()))?,
    )?;
    let runtime = metadata.runtime()?;
    Ok(ResolvedAction {
        plan: plan.clone(),
        metadata_path,
        metadata,
        runtime,
    })
}

pub fn resolve_local_action(plan: &LocalActionPlan) -> Result<ActionMetadata> {
    let metadata_path = action_metadata_path(&plan.action_dir)?;
    parse_action_metadata(
        &fs::read_to_string(&metadata_path)
            .with_context(|| format!("read {}", metadata_path.display()))?,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaScriptActionInvocation {
    pub node: String,
    pub main_container_path: String,
    pub action_container_path: String,
    pub env: Vec<(String, String)>,
}

impl ResolvedAction {
    pub fn javascript_invocation(&self, actions_host: &Path) -> Result<JavaScriptActionInvocation> {
        let ActionRuntime::JavaScript { node, main } = &self.runtime else {
            bail!(
                "action '{}' is not a JavaScript action",
                self.plan.repository
            )
        };
        let action_container_path = container_path(actions_host, &self.plan.action_dir)?;
        let main_container_path =
            format!("{}/{}", action_container_path, main.trim_start_matches('/'));
        let mut env = vec![(
            "GITHUB_ACTION_PATH".to_string(),
            action_container_path.clone(),
        )];
        env.extend(
            self.plan
                .inputs
                .iter()
                .map(|(name, value)| (input_env_name(name), value.clone())),
        );

        Ok(JavaScriptActionInvocation {
            node: node.clone(),
            main_container_path,
            action_container_path,
            env,
        })
    }
}

pub fn composite_script_steps(
    plan: &LocalActionPlan,
    metadata: &ActionMetadata,
    workspace_container: &str,
) -> Result<Vec<ScriptStep>> {
    Ok(
        composite_action_invocations(plan, metadata, workspace_container, Path::new("/__a"))?
            .into_iter()
            .filter_map(|invocation| match invocation {
                CompositeActionInvocation::Script(step) => Some(step),
                CompositeActionInvocation::Repository(_) => None,
            })
            .collect(),
    )
}

pub fn composite_action_invocations(
    plan: &LocalActionPlan,
    metadata: &ActionMetadata,
    workspace_container: &str,
    actions_host: &Path,
) -> Result<Vec<CompositeActionInvocation>> {
    if metadata.runtime()? != ActionRuntime::Composite {
        bail!("local action '{}' is not a composite action", plan.step_id)
    }

    let action_path = workspace_container_path(workspace_container, &plan.action_dir)?;
    let mut invocations = Vec::new();
    for (index, step) in metadata.runs.steps.iter().enumerate() {
        let step_id = format!("{}-{}", plan.step_id, index + 1);
        if let Some(uses) = step.uses.as_deref() {
            let reference = parse_repository_uses(uses)?;
            let inputs = step
                .with
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        render_composite_value(value, plan, &action_path, workspace_container),
                    )
                })
                .collect();
            let repository_dir =
                repository_dir(actions_host, &reference.repository, &reference.git_ref);
            let action_dir = action_dir(
                actions_host,
                &reference.repository,
                &reference.git_ref,
                reference.source_path.as_deref(),
            )?;
            invocations.push(CompositeActionInvocation::Repository(
                RepositoryActionPlan {
                    step_id,
                    repository: reference.repository,
                    git_ref: reference.git_ref,
                    source_path: reference.source_path,
                    repository_dir,
                    action_dir,
                    inputs,
                },
            ));
            continue;
        }
        let Some(script) = step.run.as_deref() else {
            continue;
        };
        let shell = step
            .shell
            .as_deref()
            .map(crate::script_step::github_shell)
            .transpose()?
            .unwrap_or(crate::container::Shell::Bash);
        let mut rendered = render_composite_value(script, plan, &action_path, workspace_container);
        if !step.env.is_empty() {
            let exports = step
                .env
                .iter()
                .map(|(name, value)| {
                    Ok(format!(
                        "export {}={}\n",
                        shell_identifier(name)?,
                        shell_single_quote(&render_composite_value(
                            value,
                            plan,
                            &action_path,
                            workspace_container
                        ))
                    ))
                })
                .collect::<Result<String>>()?;
            rendered = format!("{exports}{rendered}");
        }
        let working_directory_container = step
            .working_directory
            .as_deref()
            .map(|path| workspace_path(workspace_container, path))
            .unwrap_or_else(|| workspace_container.to_string());
        invocations.push(CompositeActionInvocation::Script(ScriptStep {
            id: step_id,
            script: rendered,
            shell,
            working_directory_container,
            condition: None,
        }));
    }
    Ok(invocations)
}

#[derive(Debug, Clone)]
struct RepositoryUsesReference {
    repository: String,
    source_path: Option<String>,
    git_ref: String,
}

fn parse_repository_uses(uses: &str) -> Result<RepositoryUsesReference> {
    if uses.starts_with('.') {
        bail!("nested local composite uses '{uses}' are not implemented yet")
    }
    if uses.starts_with("docker://") {
        bail!("nested Docker composite uses '{uses}' are not implemented yet")
    }
    let Some((path, git_ref)) = uses.rsplit_once('@') else {
        bail!("repository action '{uses}' missing ref")
    };
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        bail!("unsupported repository action reference '{uses}'")
    }
    let repository = format!("{}/{}", parts[0], parts[1]);
    let source_path = if parts.len() > 2 {
        Some(parts[2..].join("/"))
    } else {
        None
    };
    Ok(RepositoryUsesReference {
        repository,
        source_path,
        git_ref: git_ref.to_string(),
    })
}

fn action_metadata_path(action_dir: &Path) -> Result<PathBuf> {
    for file_name in ["action.yml", "action.yaml"] {
        let path = action_dir.join(file_name);
        if path.exists() {
            return Ok(path);
        }
    }
    bail!("action metadata not found in {}", action_dir.display())
}

fn is_local_action_reference(name: Option<&str>, path: Option<&str>) -> bool {
    local_action_path(name, path).is_some()
}

fn local_action_path<'a>(name: Option<&'a str>, path: Option<&'a str>) -> Option<&'a str> {
    path.filter(|value| value.starts_with('.'))
        .or_else(|| name.filter(|value| value.starts_with('.')))
}

fn local_action_dir(workspace_host: &Path, source_path: &str) -> Result<PathBuf> {
    if source_path.starts_with('/') || source_path.contains("..") {
        bail!("unsupported local action path '{source_path}'")
    }
    Ok(workspace_host.join(source_path.trim_start_matches("./")))
}

fn workspace_container_path(workspace_container: &str, host_path: &Path) -> Result<String> {
    let relative = host_path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if let Some(index) = relative.find(".github/actions/") {
        return Ok(format!(
            "{}/{}",
            workspace_container.trim_end_matches('/'),
            &relative[index..]
        ));
    }
    bail!(
        "local action path {} is outside workspace action directory",
        host_path.display()
    )
}

fn container_path(actions_host: &Path, host_path: &Path) -> Result<String> {
    let relative = host_path.strip_prefix(actions_host).with_context(|| {
        format!(
            "action path {} is outside actions directory {}",
            host_path.display(),
            actions_host.display()
        )
    })?;
    let relative = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Ok(if relative.is_empty() {
        "/__a".to_string()
    } else {
        format!("/__a/{relative}")
    })
}

fn input_env_name(name: &str) -> String {
    format!(
        "INPUT_{}",
        name.chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
    )
}

fn repository_clone_url(repository: &str) -> String {
    format!("https://github.com/{repository}.git")
}

fn repository_dir(actions_host: &Path, repository: &str, git_ref: &str) -> PathBuf {
    actions_host
        .join("_actions")
        .join(sanitize_segment(repository))
        .join(sanitize_segment(git_ref))
}

fn action_dir(
    actions_host: &Path,
    repository: &str,
    git_ref: &str,
    source_path: Option<&str>,
) -> Result<PathBuf> {
    let mut dir = repository_dir(actions_host, repository, git_ref);
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

fn render_composite_value(
    value: &str,
    plan: &LocalActionPlan,
    action_path: &str,
    workspace_container: &str,
) -> String {
    let mut rendered = value
        .replace("${{ github.action_path }}", action_path)
        .replace("${{ github.workspace }}", workspace_container);
    for (name, value) in &plan.inputs {
        rendered = rendered.replace(&format!("${{{{ inputs.{name} }}}}"), value);
    }
    rendered
}

fn workspace_path(workspace_container: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!(
            "{}/{}",
            workspace_container.trim_end_matches('/'),
            path.trim_start_matches("./")
        )
    }
}

fn shell_identifier(name: &str) -> Result<&str> {
    if name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
    {
        Ok(name)
    } else {
        bail!("unsupported composite env name '{name}'")
    }
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
            { "reference": { "type": "Repository", "name": "./.github/actions/aggregate-needs" } },
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
        assert_eq!(plans[0].step_id, "setup");
        assert_eq!(plans[0].repository, "actions/setup-python");
        assert_eq!(plans[0].git_ref, "v5");
        assert_eq!(
            plans[0].repository_dir,
            Path::new("/tmp/actions")
                .join("_actions")
                .join("actions_setup-python")
                .join("v5")
        );
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

    #[test]
    fn builds_local_action_plan() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "aggregate",
                "reference": {
                    "type": "Repository",
                    "name": "./.github/actions/aggregate-needs"
                },
                "inputs": { "workflow-label": "CI" }
            },
            {
                "id": "setup",
                "reference": {
                    "type": "Repository",
                    "name": "actions/setup-python",
                    "ref": "v6"
                }
            }
        ]))
        .unwrap();

        let plans = local_action_plans(&steps, Path::new("/tmp/workspace")).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].step_id, "aggregate");
        assert_eq!(
            plans[0].action_dir,
            Path::new("/tmp/workspace").join(".github/actions/aggregate-needs")
        );
        assert_eq!(plans[0].inputs["workflow-label"], "CI");
    }

    #[test]
    fn expands_composite_run_steps() {
        let plan = LocalActionPlan {
            step_id: "aggregate".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/aggregate-needs"),
            inputs: [("workflow-label".to_string(), "CI".to_string())].into(),
        };
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - name: Aggregate
      shell: bash
      env:
        WORKFLOW_LABEL: ${{ inputs.workflow-label }}
      run: |
        echo "::error::${{ inputs.workflow-label }} failed"
        test -d "${{ github.action_path }}"
"#,
        )
        .unwrap();

        let steps = composite_script_steps(&plan, &metadata, "/__w").unwrap();

        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, "aggregate-1");
        assert!(steps[0].script.contains("export WORKFLOW_LABEL='CI'"));
        assert!(steps[0].script.contains("::error::CI failed"));
        assert!(steps[0]
            .script
            .contains("test -d \"/__w/.github/actions/aggregate-needs\""));
    }

    #[test]
    fn builds_nested_composite_repository_action_plan() {
        let plan = LocalActionPlan {
            step_id: "docs".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/docs"),
            inputs: [("github-token".to_string(), "ghs_token".to_string())].into(),
        };
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - uses: jdx/mise-action/sub/action@v4
      with:
        github_token: ${{ inputs.github-token }}
"#,
        )
        .unwrap();

        let plans =
            composite_repository_action_plans(&[(plan, metadata)], Path::new("/tmp/actions"))
                .unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].step_id, "docs-1");
        assert_eq!(plans[0].repository, "jdx/mise-action");
        assert_eq!(plans[0].git_ref, "v4");
        assert_eq!(plans[0].source_path.as_deref(), Some("sub/action"));
        assert_eq!(plans[0].inputs["github_token"], "ghs_token");
    }

    #[test]
    fn resolves_action_metadata_from_action_dir() {
        let temp = std::env::temp_dir().join(format!("velnor-action-test-{}", std::process::id()));
        let action_dir = temp.join("action");
        fs::create_dir_all(&action_dir).unwrap();
        fs::write(
            action_dir.join("action.yml"),
            "runs:\n  using: node20\n  main: dist/index.js\n",
        )
        .unwrap();
        let plan = RepositoryActionPlan {
            step_id: "setup".into(),
            repository: "actions/setup-node".into(),
            git_ref: "v4".into(),
            source_path: None,
            repository_dir: temp.clone(),
            action_dir,
            inputs: BTreeMap::new(),
        };

        let resolved = resolve_action(&plan).unwrap();

        assert_eq!(
            resolved.runtime,
            ActionRuntime::JavaScript {
                node: "node20".into(),
                main: "dist/index.js".into()
            }
        );
        assert_eq!(resolved.metadata_path.file_name().unwrap(), "action.yml");
        fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn builds_javascript_action_invocation() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "setup".into(),
            repository: "actions/setup-node".into(),
            git_ref: "v4".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/actions_setup-node/v4"),
            action_dir: actions_host.join("_actions/actions_setup-node/v4"),
            inputs: [("node-version".to_string(), "22".to_string())].into(),
        };
        let metadata =
            parse_action_metadata("runs:\n  using: node20\n  main: dist/index.js\n").unwrap();
        let runtime = metadata.runtime().unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host.join("_actions/actions_setup-node/v4/action.yml"),
            metadata,
            runtime,
        };

        let invocation = resolved.javascript_invocation(actions_host).unwrap();

        assert_eq!(invocation.node, "node20");
        assert_eq!(
            invocation.main_container_path,
            "/__a/_actions/actions_setup-node/v4/dist/index.js"
        );
        assert!(invocation
            .env
            .contains(&("INPUT_NODE_VERSION".into(), "22".into())));
        assert!(invocation.env.contains(&(
            "GITHUB_ACTION_PATH".into(),
            "/__a/_actions/actions_setup-node/v4".into()
        )));
    }
}

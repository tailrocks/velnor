#![allow(dead_code)]

use crate::{
    checkout::fetch_git_ref,
    executor::{render_context_expressions, CommandRunner},
    job_message::{ActionReferenceType, ActionStep},
    script_step::{step_environment, value_truthy, ScriptStep},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Deserializer};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    #[serde(default)]
    pub outputs: BTreeMap<String, ActionOutput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionInput {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(
        default,
        rename = "default",
        deserialize_with = "deserialize_optional_string_scalar"
    )]
    pub default_value: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionOutput {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionRuns {
    pub using: String,
    #[serde(default)]
    pub main: Option<String>,
    #[serde(default)]
    pub pre: Option<String>,
    #[serde(default, rename = "pre-if", alias = "preIf")]
    pub pre_if: Option<String>,
    #[serde(default)]
    pub post: Option<String>,
    #[serde(default, rename = "post-if", alias = "postIf")]
    pub post_if: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
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
    #[serde(default, deserialize_with = "deserialize_string_map")]
    pub with: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_string_map")]
    pub env: BTreeMap<String, String>,
    #[serde(default, rename = "if")]
    pub condition: Option<String>,
    #[serde(default, rename = "working-directory", alias = "workingDirectory")]
    pub working_directory: Option<String>,
    #[serde(
        default,
        rename = "continue-on-error",
        alias = "continueOnError",
        deserialize_with = "deserialize_optional_string_scalar"
    )]
    pub continue_on_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionRuntime {
    JavaScript { node: String, main: String },
    Composite,
    Docker { image: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeActionAdapter {
    Checkout,
    Cache,
    UploadArtifact,
    DownloadArtifact,
    UploadPagesArtifact,
    DeployPages,
    PathsFilter,
    Mise,
    Sccache,
    SetupMold,
    SetupJust,
    RustCache,
    GitHubRuntimeExport,
    Renovate,
    DockerSetupBuildx,
    DockerLogin,
    DockerMetadata,
    DockerBuildPush,
    DockerBake,
}

pub fn native_action_adapter(repository: &str) -> Option<NativeActionAdapter> {
    match repository.to_ascii_lowercase().as_str() {
        "actions/checkout" => Some(NativeActionAdapter::Checkout),
        "actions/cache" => Some(NativeActionAdapter::Cache),
        "actions/upload-artifact" => Some(NativeActionAdapter::UploadArtifact),
        "actions/download-artifact" => Some(NativeActionAdapter::DownloadArtifact),
        "actions/upload-pages-artifact" => Some(NativeActionAdapter::UploadPagesArtifact),
        "actions/deploy-pages" => Some(NativeActionAdapter::DeployPages),
        "dorny/paths-filter" => Some(NativeActionAdapter::PathsFilter),
        "jdx/mise-action" => Some(NativeActionAdapter::Mise),
        "mozilla-actions/sccache-action" => Some(NativeActionAdapter::Sccache),
        "rui314/setup-mold" => Some(NativeActionAdapter::SetupMold),
        "extractions/setup-just" => Some(NativeActionAdapter::SetupJust),
        "swatinem/rust-cache" => Some(NativeActionAdapter::RustCache),
        "crazy-max/ghaction-github-runtime" => Some(NativeActionAdapter::GitHubRuntimeExport),
        "renovatebot/github-action" => Some(NativeActionAdapter::Renovate),
        "docker/setup-buildx-action" => Some(NativeActionAdapter::DockerSetupBuildx),
        "docker/login-action" => Some(NativeActionAdapter::DockerLogin),
        "docker/metadata-action" => Some(NativeActionAdapter::DockerMetadata),
        "docker/build-push-action" => Some(NativeActionAdapter::DockerBuildPush),
        "docker/bake-action" => Some(NativeActionAdapter::DockerBake),
        _ => None,
    }
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
    pub env: Vec<(String, String)>,
    pub condition: Option<String>,
    pub continue_on_error: bool,
}

pub const NATIVE_ACTION_REF: &str = "__native";

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
    Outputs(CompositeActionOutputs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeActionOutputs {
    pub step_id: String,
    pub outputs: BTreeMap<String, String>,
}

pub fn parse_action_metadata(contents: &str) -> Result<ActionMetadata> {
    serde_yaml::from_str(contents).context("parse action metadata")
}

fn deserialize_optional_string_scalar<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.map(|value| input_value(&value)))
}

fn deserialize_string_map<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(object) = Option::<BTreeMap<String, serde_json::Value>>::deserialize(deserializer)?
    else {
        return Ok(BTreeMap::new());
    };
    Ok(object
        .into_iter()
        .map(|(name, value)| (name, input_value(&value)))
        .collect())
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
            .or_else(|| native_action_adapter(repository).map(|_| NATIVE_ACTION_REF.to_string()))
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
            env: step_environment(step)?,
            condition: step.condition.clone(),
            continue_on_error: crate::script_step::step_continue_on_error(step),
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
    local_action_plans_with_context(steps, workspace_host, &[])
}

pub fn local_action_plans_with_context(
    steps: &[ActionStep],
    workspace_host: &Path,
    context_data: &[(String, serde_json::Value)],
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
            inputs: render_inputs(&string_inputs(step)?, context_data),
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

pub fn composite_repository_action_plans_from_resolved(
    resolved_actions: &[ResolvedAction],
    actions_host: &Path,
) -> Result<Vec<RepositoryActionPlan>> {
    let mut plans = Vec::new();
    for action in resolved_actions {
        if action.runtime != ActionRuntime::Composite {
            continue;
        }
        for invocation in action.composite_invocations("/__w", actions_host)? {
            if let CompositeActionInvocation::Repository(repository_plan) = invocation {
                plans.push(repository_plan);
            }
        }
    }
    Ok(plans)
}

fn step_id(step: &ActionStep, index: usize) -> String {
    // Prefer context_name (YAML id:) over internal UUID for expression lookup.
    step.context_name
        .as_deref()
        .filter(|n| !n.is_empty() && !n.starts_with("__"))
        .or_else(|| step.id.as_deref())
        .or_else(|| step.name.as_deref())
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
    let mut fetched = BTreeSet::new();
    for plan in plans {
        if fetched.insert((plan.repository.clone(), plan.git_ref.clone())) {
            fetch_git_ref(
                runner,
                &repository_clone_url(&plan.repository),
                &plan.git_ref,
                &plan.repository_dir,
                None,
                Some(1),
                false,
                true,
                true,
                false, // lfs: action repos don't use LFS
            )?;
        }
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
    pub pre_container_path: Option<String>,
    pub pre_condition: Option<String>,
    pub main_container_path: String,
    pub post_container_path: Option<String>,
    pub post_condition: Option<String>,
    pub action_container_path: String,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerActionInvocation {
    pub image: String,
    pub build_context_host: Option<PathBuf>,
    pub dockerfile_host: Option<PathBuf>,
    pub action_container_path: String,
    pub env: Vec<(String, String)>,
    pub entrypoint: Option<String>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeActionInvocation {
    pub adapter: NativeActionAdapter,
    pub inputs: BTreeMap<String, String>,
    pub env: Vec<(String, String)>,
}

impl ResolvedAction {
    pub fn native_invocation(&self) -> Option<NativeActionInvocation> {
        native_invocation_from_plan(&self.plan)
    }

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
        let pre_container_path = self
            .metadata
            .runs
            .pre
            .as_ref()
            .map(|pre| format!("{}/{}", action_container_path, pre.trim_start_matches('/')));
        let post_container_path = self
            .metadata
            .runs
            .post
            .as_ref()
            .map(|post| format!("{}/{}", action_container_path, post.trim_start_matches('/')));
        let mut env = vec![
            ("GITHUB_ACTION".to_string(), self.plan.step_id.clone()),
            (
                "GITHUB_ACTION_PATH".to_string(),
                action_container_path.clone(),
            ),
            (
                "GITHUB_ACTION_REPOSITORY".to_string(),
                self.plan.repository.clone(),
            ),
            ("GITHUB_ACTION_REF".to_string(), self.plan.git_ref.clone()),
        ];
        let inputs = effective_inputs(&self.metadata, &self.plan.inputs);
        env.extend(
            self.plan
                .env
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        env.extend(
            inputs
                .iter()
                .map(|(name, value)| (input_env_name(name), value.clone())),
        );

        Ok(JavaScriptActionInvocation {
            node: node.clone(),
            pre_container_path,
            pre_condition: self.metadata.runs.pre_if.clone(),
            main_container_path,
            post_container_path,
            post_condition: self.metadata.runs.post_if.clone(),
            action_container_path,
            env,
        })
    }

    pub fn docker_invocation(&self, actions_host: &Path) -> Result<DockerActionInvocation> {
        let ActionRuntime::Docker { image } = &self.runtime else {
            bail!("action '{}' is not a Docker action", self.plan.repository)
        };
        let action_container_path = container_path(actions_host, &self.plan.action_dir)?;
        let inputs = effective_inputs(&self.metadata, &self.plan.inputs);
        let mut env = vec![
            ("GITHUB_ACTION".to_string(), self.plan.step_id.clone()),
            (
                "GITHUB_ACTION_PATH".to_string(),
                action_container_path.clone(),
            ),
            (
                "GITHUB_ACTION_REPOSITORY".to_string(),
                self.plan.repository.clone(),
            ),
            ("GITHUB_ACTION_REF".to_string(), self.plan.git_ref.clone()),
        ];
        env.extend(
            self.plan
                .env
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        env.extend(
            inputs
                .iter()
                .map(|(name, value)| (input_env_name(name), value.clone())),
        );

        let (image, build_context_host, dockerfile_host) =
            if let Some(image) = image.strip_prefix("docker://") {
                (image.to_string(), None, None)
            } else {
                let dockerfile_host = self.plan.action_dir.join(image);
                let tag = docker_action_tag(
                    &self.plan.repository,
                    &self.plan.git_ref,
                    self.plan.source_path.as_deref(),
                );
                (
                    tag,
                    Some(self.plan.action_dir.clone()),
                    Some(dockerfile_host),
                )
            };
        let entrypoint = self
            .metadata
            .runs
            .entrypoint
            .as_ref()
            .map(|value| render_action_scoped_value(value, &inputs, &action_container_path));
        let args = self
            .metadata
            .runs
            .args
            .iter()
            .map(|value| render_action_scoped_value(value, &inputs, &action_container_path))
            .collect();

        Ok(DockerActionInvocation {
            image,
            build_context_host,
            dockerfile_host,
            action_container_path,
            env,
            entrypoint,
            args,
        })
    }

    pub fn composite_invocations(
        &self,
        workspace_container: &str,
        actions_host: &Path,
    ) -> Result<Vec<CompositeActionInvocation>> {
        if self.runtime != ActionRuntime::Composite {
            bail!(
                "action '{}' is not a composite action",
                self.plan.repository
            )
        }
        let action_path = container_path(actions_host, &self.plan.action_dir)?;
        composite_action_invocations_with_path(
            &self.plan.step_id,
            &self.plan.inputs,
            &self.metadata,
            workspace_container,
            actions_host,
            &action_path,
        )
    }
}

pub fn native_invocation_from_plan(plan: &RepositoryActionPlan) -> Option<NativeActionInvocation> {
    native_action_adapter(&plan.repository).map(|adapter| NativeActionInvocation {
        adapter,
        inputs: plan.inputs.clone(),
        env: plan.env.clone(),
    })
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
                CompositeActionInvocation::Outputs(_) => None,
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
    composite_action_invocations_with_path(
        &plan.step_id,
        &plan.inputs,
        metadata,
        workspace_container,
        actions_host,
        &action_path,
    )
}

fn composite_action_invocations_with_path(
    step_id_prefix: &str,
    inputs: &BTreeMap<String, String>,
    metadata: &ActionMetadata,
    workspace_container: &str,
    actions_host: &Path,
    action_path: &str,
) -> Result<Vec<CompositeActionInvocation>> {
    let action_inputs = effective_inputs(metadata, inputs);
    let mut invocations = Vec::new();
    let mut step_ids = BTreeMap::new();
    for (index, step) in metadata.runs.steps.iter().enumerate() {
        let step_id = composite_step_id(step_id_prefix, step.id.as_deref(), index);
        if let Some(id) = step.id.as_deref() {
            step_ids.insert(id.to_string(), step_id.clone());
        }
        if let Some(uses) = step.uses.as_deref() {
            let reference = parse_repository_uses(uses)?;
            let inputs = step
                .with
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        render_composite_scoped_value(
                            value,
                            &action_inputs,
                            action_path,
                            workspace_container,
                            &step_ids,
                        ),
                    )
                })
                .collect();
            let env = step
                .env
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        render_composite_scoped_value(
                            value,
                            &action_inputs,
                            action_path,
                            workspace_container,
                            &step_ids,
                        ),
                    )
                })
                .collect();
            let condition = step.condition.as_ref().map(|condition| {
                render_composite_scoped_value(
                    condition,
                    &action_inputs,
                    action_path,
                    workspace_container,
                    &step_ids,
                )
            });
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
                    env,
                    condition,
                    continue_on_error: composite_continue_on_error(
                        step,
                        &action_inputs,
                        action_path,
                        workspace_container,
                        &step_ids,
                    ),
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
        let rendered = render_composite_scoped_value(
            script,
            &action_inputs,
            action_path,
            workspace_container,
            &step_ids,
        );
        let mut env = step
            .env
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    render_composite_scoped_value(
                        value,
                        &action_inputs,
                        action_path,
                        workspace_container,
                        &step_ids,
                    ),
                )
            })
            .collect::<Vec<_>>();
        env.push(("GITHUB_ACTION_PATH".to_string(), action_path.to_string()));
        let working_directory_container = step
            .working_directory
            .as_deref()
            .map(|path| {
                workspace_path(
                    workspace_container,
                    &render_composite_scoped_value(
                        path,
                        &action_inputs,
                        action_path,
                        workspace_container,
                        &step_ids,
                    ),
                )
            })
            .unwrap_or_else(|| workspace_container.to_string());
        let composite_display_name = step
            .name
            .clone()
            .filter(|n| !n.is_empty() && !n.starts_with("__"))
            .unwrap_or_else(|| {
                let first = rendered.lines().next().unwrap_or("").trim();
                if first.is_empty() {
                    String::new()
                } else {
                    format!("Run {first}")
                }
            });
        invocations.push(CompositeActionInvocation::Script(ScriptStep {
            id: step_id,
            display_name: composite_display_name,
            script: rendered,
            shell,
            working_directory_container,
            env,
            condition: step.condition.as_ref().map(|condition| {
                render_composite_scoped_value(
                    condition,
                    &action_inputs,
                    action_path,
                    workspace_container,
                    &step_ids,
                )
            }),
            continue_on_error: composite_continue_on_error(
                step,
                &action_inputs,
                action_path,
                workspace_container,
                &step_ids,
            ),
        }));
    }
    let outputs = metadata
        .outputs
        .iter()
        .filter_map(|(name, output)| {
            output.value.as_ref().map(|value| {
                (
                    name.clone(),
                    render_composite_scoped_value(
                        value,
                        &action_inputs,
                        action_path,
                        workspace_container,
                        &step_ids,
                    ),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    if !outputs.is_empty() {
        invocations.push(CompositeActionInvocation::Outputs(CompositeActionOutputs {
            step_id: step_id_prefix.to_string(),
            outputs,
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
    format!("INPUT_{}", name.replace(' ', "_").to_ascii_uppercase())
}

fn docker_action_tag(repository: &str, git_ref: &str, source_path: Option<&str>) -> String {
    let source = source_path.unwrap_or("root");
    format!(
        "velnor-action-{}-{}-{}",
        sanitize_segment(repository),
        sanitize_segment(git_ref),
        sanitize_segment(source)
    )
    .to_ascii_lowercase()
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
    string_input_map(step.inputs.as_ref())
}

fn string_input_map(value: Option<&serde_json::Value>) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    match value {
        serde_json::Value::Object(object) => {
            if object.get("type").or_else(|| object.get("Type")).is_some()
                && object.get("map").or_else(|| object.get("Map")).is_some()
            {
                return string_input_map(object.get("map").or_else(|| object.get("Map")));
            }
            Ok(object
                .iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case("type"))
                .map(|(name, value)| (name.clone(), input_value(value)))
                .collect())
        }
        serde_json::Value::Array(items) => {
            let mut result = BTreeMap::new();
            for item in items {
                if let Some((name, value)) = input_pair(item) {
                    result.insert(name.to_string(), input_value(value));
                }
            }
            Ok(result)
        }
        _ => Ok(BTreeMap::new()),
    }
}

fn input_pair(value: &serde_json::Value) -> Option<(&str, &serde_json::Value)> {
    match value {
        serde_json::Value::Object(object) => {
            let key = object.get("Key").or_else(|| object.get("key"))?;
            let value = object.get("Value").or_else(|| object.get("value"))?;
            Some((input_value_as_str(key)?, value))
        }
        serde_json::Value::Array(pair) if pair.len() == 2 => {
            Some((input_value_as_str(&pair[0])?, &pair[1]))
        }
        _ => None,
    }
}

fn input_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Object(object) => {
            // Prefer literal value first, then fall back to expression syntax.
            if let Some(v) = object
                .get("value")
                .or_else(|| object.get("Value"))
                .or_else(|| object.get("lit"))
                .or_else(|| object.get("Lit"))
            {
                return input_value(v);
            }
            // Expression type (type=3): wrap expr so resolve_expressions can evaluate it.
            if let Some(expr) = object
                .get("expr")
                .or_else(|| object.get("Expr"))
                .and_then(|v| v.as_str())
            {
                return format!("${{{{{expr}}}}}");
            }
            String::new()
        }
        _ => String::new(),
    }
}

fn input_value_as_str(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::String(value) => Some(value),
        serde_json::Value::Object(object) => object
            .get("value")
            .or_else(|| object.get("Value"))
            .or_else(|| object.get("lit"))
            .or_else(|| object.get("Lit"))
            .and_then(input_value_as_str),
        _ => None,
    }
}

fn render_inputs(
    inputs: &BTreeMap<String, String>,
    context_data: &[(String, serde_json::Value)],
) -> BTreeMap<String, String> {
    inputs
        .iter()
        .map(|(name, value)| {
            let rendered = if contains_step_output_expression(value) {
                value.clone()
            } else {
                render_context_expressions(value, context_data)
            };
            (name.clone(), rendered)
        })
        .collect()
}

fn contains_step_output_expression(value: &str) -> bool {
    value
        .match_indices("steps.")
        .any(|(index, _)| value[index..].contains(".outputs."))
}

fn render_composite_value(
    value: &str,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
) -> String {
    let mut rendered = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${{") {
        rendered.push_str(&replace_composite_bare_tokens(
            &rest[..start],
            inputs,
            action_path,
            workspace_container,
        ));
        let after_start = &rest[start + 3..];
        let Some(end) = after_start.find("}}") else {
            rendered.push_str(&rest[start..]);
            return rendered;
        };
        let expression = after_start[..end].trim();
        rendered.push_str(&render_composite_expression(
            expression,
            inputs,
            action_path,
            workspace_container,
        ));
        rest = &after_start[end + 2..];
    }
    rendered.push_str(&replace_composite_bare_tokens(
        rest,
        inputs,
        action_path,
        workspace_container,
    ));
    rendered
}

fn render_composite_expression(
    expression: &str,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
) -> String {
    if expression == "github.action_path" {
        return action_path.to_string();
    }
    if expression == "github.workspace" {
        return workspace_container.to_string();
    }
    if let Some(name) = expression.strip_prefix("inputs.") {
        if let Some(value) = inputs.get(name) {
            return value.clone();
        }
    }

    let mut rendered = expression
        .replace("github.action_path", &expression_single_quote(action_path))
        .replace(
            "github.workspace",
            &expression_single_quote(workspace_container),
        );
    for (name, value) in inputs_by_descending_name_len(inputs) {
        rendered = rendered.replace(&format!("inputs.{name}"), &expression_single_quote(value));
    }
    format!("${{{{ {rendered} }}}}")
}

fn replace_composite_bare_tokens(
    value: &str,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
) -> String {
    let mut rendered = value
        .replace("github.action_path", action_path)
        .replace("github.workspace", workspace_container);
    for (name, value) in inputs_by_descending_name_len(inputs) {
        rendered = rendered.replace(&format!("inputs.{name}"), value);
    }
    rendered
}

fn inputs_by_descending_name_len(inputs: &BTreeMap<String, String>) -> Vec<(&String, &String)> {
    let mut pairs = inputs.iter().collect::<Vec<_>>();
    pairs.sort_by(|(left, _), (right, _)| right.len().cmp(&left.len()));
    pairs
}

fn render_composite_scoped_value(
    value: &str,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
    step_ids: &BTreeMap<String, String>,
) -> String {
    rewrite_step_output_refs(
        &render_composite_value(value, inputs, action_path, workspace_container),
        step_ids,
    )
}

fn composite_continue_on_error(
    step: &CompositeActionStep,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
    step_ids: &BTreeMap<String, String>,
) -> bool {
    step.continue_on_error
        .as_ref()
        .map(|value| {
            let rendered = render_composite_scoped_value(
                value,
                inputs,
                action_path,
                workspace_container,
                step_ids,
            );
            value_truthy(&serde_json::Value::String(rendered))
        })
        .unwrap_or(false)
}

fn render_action_scoped_value(
    value: &str,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
) -> String {
    render_composite_value(value, inputs, action_path, "/__w")
}

fn rewrite_step_output_refs(value: &str, step_ids: &BTreeMap<String, String>) -> String {
    let mut rendered = value.to_string();
    for (source, target) in step_ids {
        rendered = rendered.replace(
            &format!("steps.{source}.outputs."),
            &format!("steps.{target}.outputs."),
        );
    }
    rendered
}

fn composite_step_id(prefix: &str, id: Option<&str>, index: usize) -> String {
    id.map(|id| format!("{prefix}-{}", sanitize_segment(id)))
        .filter(|value| !value.ends_with('-'))
        .unwrap_or_else(|| format!("{prefix}-{}", index + 1))
}

fn effective_inputs(
    metadata: &ActionMetadata,
    provided: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut inputs = metadata
        .inputs
        .iter()
        .filter_map(|(name, input)| {
            input
                .default_value
                .as_ref()
                .map(|value| (name.clone(), value.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    inputs.extend(provided.clone());
    inputs
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

fn expression_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
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
    use crate::executor::CommandResult;

    #[derive(Default)]
    struct RecordingRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn parses_javascript_action_metadata() {
        let metadata = parse_action_metadata(
            r#"
name: Setup Tool
inputs:
  version:
    required: true
    default: latest
  update-environment:
    default: true
  check-latest:
    default: false
runs:
  using: node20
  main: dist/index.js
  post: dist/cleanup.js
  post-if: success()
"#,
        )
        .unwrap();

        assert_eq!(
            metadata.inputs["version"].default_value.as_deref(),
            Some("latest")
        );
        assert_eq!(
            metadata.inputs["update-environment"]
                .default_value
                .as_deref(),
            Some("true")
        );
        assert_eq!(
            metadata.inputs["check-latest"].default_value.as_deref(),
            Some("false")
        );
        assert_eq!(
            metadata.runtime().unwrap(),
            ActionRuntime::JavaScript {
                node: "node20".into(),
                main: "dist/index.js".into()
            }
        );
        assert_eq!(metadata.runs.post_if.as_deref(), Some("success()"));
    }

    #[test]
    fn parses_docker_action_metadata() {
        let metadata = parse_action_metadata(
            r#"
inputs:
  image:
    default: alpine:3.20
runs:
  using: docker
  image: Dockerfile
  entrypoint: /entrypoint.sh
  args:
    - ${{ inputs.image }}
"#,
        )
        .unwrap();

        assert_eq!(
            metadata.runtime().unwrap(),
            ActionRuntime::Docker {
                image: "Dockerfile".into()
            }
        );
        assert_eq!(metadata.runs.entrypoint.as_deref(), Some("/entrypoint.sh"));
        assert_eq!(metadata.runs.args, vec!["${{ inputs.image }}"]);
    }

    #[test]
    fn parses_composite_action_metadata() {
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - id: run
      shell: bash
      env:
        BOOL_VALUE: false
        COUNT: 7
      run: echo hi
    - uses: actions/setup-buildx@v4
      with:
        cleanup: false
        retries: 3
"#,
        )
        .unwrap();

        assert_eq!(metadata.runtime().unwrap(), ActionRuntime::Composite);
        assert_eq!(metadata.runs.steps[0].env["BOOL_VALUE"], "false");
        assert_eq!(metadata.runs.steps[0].env["COUNT"], "7");
        assert_eq!(metadata.runs.steps[1].with["cleanup"], "false");
        assert_eq!(metadata.runs.steps[1].with["retries"], "3");
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
                    "name": "actions/cache",
                    "ref": "v5",
                    "path": "sub/action"
                },
                "inputs": { "key": "cargo-linux", "cache-on-failure": true, "fetch-depth": 0 },
                "environment": { "PIP_INDEX_URL": "${{ github.server_url }}" }
            }
        ]))
        .unwrap();

        let plans = repository_action_plans(&steps, Path::new("/tmp/actions")).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].step_id, "setup");
        assert_eq!(plans[0].repository, "actions/cache");
        assert_eq!(plans[0].git_ref, "v5");
        assert_eq!(
            plans[0].repository_dir,
            Path::new("/tmp/actions")
                .join("_actions")
                .join("actions_cache")
                .join("v5")
        );
        assert_eq!(plans[0].inputs["key"], "cargo-linux");
        assert_eq!(plans[0].inputs["cache-on-failure"], "true");
        assert_eq!(plans[0].inputs["fetch-depth"], "0");
        assert_eq!(
            plans[0].env,
            vec![("PIP_INDEX_URL".into(), "${{ github.server_url }}".into())]
        );
        assert_eq!(
            plans[0].action_dir,
            Path::new("/tmp/actions")
                .join("_actions")
                .join("actions_cache")
                .join("v5")
                .join("sub/action")
        );
    }

    #[test]
    fn builds_repository_action_plan_from_run_service_typed_inputs() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "cache",
                "reference": {
                    "type": "Repository",
                    "name": "actions/cache",
                    "ref": "v5"
                },
                "inputs": {
                    "type": "map",
                    "map": [
                        { "Key": { "lit": "path" }, "Value": { "lit": "~/.cargo/registry" } },
                        { "Key": { "lit": "key" }, "Value": { "lit": "cargo-${{ hashFiles('**/Cargo.lock') }}" } },
                        { "Key": { "lit": "fail-on-cache-miss" }, "Value": { "lit": "false" } },
                        { "Key": { "lit": "lookup-only" }, "Value": { "value": true } }
                    ]
                }
            }
        ]))
        .unwrap();

        let plans = repository_action_plans(&steps, Path::new("/tmp/actions")).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].inputs["path"], "~/.cargo/registry");
        assert_eq!(
            plans[0].inputs["key"],
            "cargo-${{ hashFiles('**/Cargo.lock') }}"
        );
        assert_eq!(plans[0].inputs["fail-on-cache-miss"], "false");
        assert_eq!(plans[0].inputs["lookup-only"], "true");
    }

    #[test]
    fn native_repository_action_plan_does_not_require_ref() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "cache",
                "reference": {
                    "type": "Repository",
                    "name": "actions/cache"
                },
                "inputs": {
                    "path": "~/.cargo",
                    "key": "cargo-linux"
                }
            }
        ]))
        .unwrap();

        let plans = repository_action_plans(&steps, Path::new("/tmp/actions")).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].repository, "actions/cache");
        assert_eq!(plans[0].git_ref, NATIVE_ACTION_REF);
        assert_eq!(
            native_invocation_from_plan(&plans[0]).unwrap().adapter,
            NativeActionAdapter::Cache
        );
    }

    #[test]
    fn builds_target_cache_action_plan_from_multiline_inputs() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "cache",
                "reference": {
                    "type": "Repository",
                    "name": "actions/cache",
                    "ref": "27d5ce7f107fe9357f9df03efb73ab90386fccae"
                },
                "inputs": {
                    "path": "~/.cache/rust-script",
                    "key": "rust-script-${{ runner.os }}-${{ hashFiles('kestra-docker-containers/**/*.rs', 'kestra-docker-containers/**/build.toml', 'kestra-docker-containers/justfile') }}",
                    "restore-keys": "rust-script-${{ runner.os }}-\n"
                }
            }
        ]))
        .unwrap();

        let plans = repository_action_plans(&steps, Path::new("/tmp/actions")).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].repository, "actions/cache");
        assert_eq!(plans[0].git_ref, "27d5ce7f107fe9357f9df03efb73ab90386fccae");
        assert_eq!(plans[0].inputs["path"], "~/.cache/rust-script");
        assert_eq!(
            plans[0].inputs["key"],
            "rust-script-${{ runner.os }}-${{ hashFiles('kestra-docker-containers/**/*.rs', 'kestra-docker-containers/**/build.toml', 'kestra-docker-containers/justfile') }}"
        );
        assert_eq!(
            plans[0].inputs["restore-keys"],
            "rust-script-${{ runner.os }}-\n"
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
                    "name": "actions/cache",
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
    fn builds_local_action_plan_from_run_service_typed_inputs() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "aggregate",
                "reference": {
                    "type": "Repository",
                    "name": "./.github/actions/aggregate-needs"
                },
                "inputs": {
                    "type": "map",
                    "map": [
                        { "Key": { "lit": "needs-json" }, "Value": { "lit": "${{ toJSON(needs) }}" } },
                        { "Key": { "lit": "workflow-label" }, "Value": { "lit": "CI" } }
                    ]
                }
            }
        ]))
        .unwrap();
        let context = vec![(
            "needs".to_string(),
            serde_json::json!({ "check": { "result": "success" } }),
        )];

        let plans =
            local_action_plans_with_context(&steps, Path::new("/tmp/workspace"), &context).unwrap();

        assert_eq!(
            plans[0].inputs["needs-json"],
            r#"{"check":{"result":"success"}}"#
        );
        assert_eq!(plans[0].inputs["workflow-label"], "CI");
    }

    #[test]
    fn renders_local_action_inputs_from_job_context() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "aggregate",
                "reference": {
                    "type": "Repository",
                    "name": "./.github/actions/aggregate-needs"
                },
                "inputs": {
                    "needs-json": "${{ toJSON(needs) }}",
                    "workflow-label": "CI"
                }
            }
        ]))
        .unwrap();
        let context = vec![(
            "needs".to_string(),
            serde_json::json!({
                "check": { "result": "success" },
                "build": { "result": "failure" }
            }),
        )];

        let plans =
            local_action_plans_with_context(&steps, Path::new("/tmp/workspace"), &context).unwrap();

        assert_eq!(
            plans[0].inputs["needs-json"],
            r#"{"build":{"result":"failure"},"check":{"result":"success"}}"#
        );
        assert_eq!(plans[0].inputs["workflow-label"], "CI");
    }

    #[test]
    fn preserves_step_output_inputs_for_runtime_resolution() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "check-deployed",
                "reference": {
                    "type": "Repository",
                    "name": "./.github/actions/check-deployed-docs"
                },
                "inputs": {
                    "sitemap-url": "${{ steps.sitemap.outputs.url }}",
                    "edit-url": "${{ env.JACKIN_REPO_EDIT_URL }}",
                    "github-token": "${{ github.token }}"
                }
            }
        ]))
        .unwrap();
        let context = vec![(
            "github".to_string(),
            serde_json::json!({ "token": "ghs_token" }),
        )];

        let plans =
            local_action_plans_with_context(&steps, Path::new("/tmp/workspace"), &context).unwrap();

        assert_eq!(
            plans[0].inputs["sitemap-url"],
            "${{ steps.sitemap.outputs.url }}"
        );
        assert_eq!(plans[0].inputs["github-token"], "ghs_token");
        assert_eq!(
            plans[0].inputs["edit-url"],
            "${{ env.JACKIN_REPO_EDIT_URL }}"
        );
    }

    #[test]
    fn renders_non_output_step_literals_in_local_action_inputs() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "local",
                "reference": {
                    "type": "Repository",
                    "name": "./.github/actions/check-deployed-docs"
                },
                "inputs": {
                    "label": "steps are documented for ${{ github.repository }}",
                    "status": "${{ steps.sccache.outcome }}"
                }
            }
        ]))
        .unwrap();
        let context = vec![(
            "github".to_string(),
            serde_json::json!({ "repository": "jackin-project/jackin" }),
        )];

        let plans =
            local_action_plans_with_context(&steps, Path::new("/tmp/workspace"), &context).unwrap();

        assert_eq!(
            plans[0].inputs["label"],
            "steps are documented for jackin-project/jackin"
        );
        assert_eq!(plans[0].inputs["status"], "${{ steps.sccache.outcome }}");
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
      if: ${{ inputs.workflow-label == 'CI' }}
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
        assert_eq!(steps[0].condition.as_deref(), Some("${{ 'CI' == 'CI' }}"));
        assert_eq!(
            steps[0].env,
            vec![
                ("WORKFLOW_LABEL".into(), "CI".into()),
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__w/.github/actions/aggregate-needs".into()
                )
            ]
        );
        assert!(steps[0].script.contains("::error::CI failed"));
        assert!(steps[0]
            .script
            .contains("test -d \"/__w/.github/actions/aggregate-needs\""));
    }

    #[test]
    fn target_aggregate_needs_expands_exact_failure_gate() {
        let plan = LocalActionPlan {
            step_id: "aggregate".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/aggregate-needs"),
            inputs: [
                (
                    "needs-json".to_string(),
                    r#"{"check":{"result":"success"},"build":{"result":"cancelled"}}"#.to_string(),
                ),
                ("workflow-label".to_string(), "CI".to_string()),
            ]
            .into(),
        };
        let metadata = parse_action_metadata(
            r#"
inputs:
  needs-json:
    required: true
  workflow-label:
    required: true
runs:
  using: composite
  steps:
    - name: Aggregate gated job results
      shell: bash
      env:
        NEEDS_RESULT: ${{ inputs.needs-json }}
        WORKFLOW_LABEL: ${{ inputs.workflow-label }}
      run: |
        set -euo pipefail
        printf '%s\n' "$NEEDS_RESULT"
        if printf '%s' "$NEEDS_RESULT" | jq -e 'to_entries | map(.value.result) | any(. == "failure" or . == "cancelled")' >/dev/null; then
          echo "::error::one or more gated ${WORKFLOW_LABEL} jobs failed or were cancelled"
          exit 1
        fi
"#,
        )
        .unwrap();

        let steps = composite_script_steps(&plan, &metadata, "/__w").unwrap();

        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, "aggregate-1");
        assert!(matches!(steps[0].shell, crate::container::Shell::Bash));
        assert!(steps[0].env.contains(&(
            "NEEDS_RESULT".into(),
            r#"{"check":{"result":"success"},"build":{"result":"cancelled"}}"#.into()
        )));
        assert!(steps[0]
            .env
            .contains(&("WORKFLOW_LABEL".into(), "CI".into())));
        assert!(steps[0].env.contains(&(
            "GITHUB_ACTION_PATH".into(),
            "/__w/.github/actions/aggregate-needs".into()
        )));
        assert!(steps[0].script.contains(
            r#"jq -e 'to_entries | map(.value.result) | any(. == "failure" or . == "cancelled")'"#
        ));
        assert!(steps[0].script.contains(
            "::error::one or more gated ${WORKFLOW_LABEL} jobs failed or were cancelled"
        ));
    }

    #[test]
    fn expands_composite_input_defaults() {
        let plan = LocalActionPlan {
            step_id: "docs".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/check-deployed-docs"),
            inputs: BTreeMap::new(),
        };
        let metadata = parse_action_metadata(
            r#"
inputs:
  external-links:
    default: "true"
runs:
  using: composite
  steps:
    - shell: bash
      env:
        EXTERNAL_LINKS: ${{ inputs.external-links }}
      run: echo "${{ inputs.external-links }}"
"#,
        )
        .unwrap();

        let steps = composite_script_steps(&plan, &metadata, "/__w").unwrap();

        assert_eq!(
            steps[0].env,
            vec![
                ("EXTERNAL_LINKS".into(), "true".into()),
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__w/.github/actions/check-deployed-docs".into()
                )
            ]
        );
        assert!(steps[0].script.contains("echo \"true\""));
    }

    #[test]
    fn target_check_deployed_docs_keeps_sitemap_step_output_input() {
        let plan = LocalActionPlan {
            step_id: "check-deployed".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/check-deployed-docs"),
            inputs: [
                (
                    "sitemap-url".to_string(),
                    "${{ steps.sitemap.outputs.url }}".to_string(),
                ),
                (
                    "edit-url".to_string(),
                    "${{ env.JACKIN_REPO_EDIT_URL }}".to_string(),
                ),
                (
                    "blob-url".to_string(),
                    "${{ env.JACKIN_REPO_BLOB_URL }}".to_string(),
                ),
                ("github-token".to_string(), "ghs_token".to_string()),
                ("external-links".to_string(), "false".to_string()),
            ]
            .into(),
        };
        let metadata = parse_action_metadata(
            r#"
inputs:
  sitemap-url:
    required: true
  edit-url:
    required: true
  blob-url:
    required: true
  github-token:
    required: true
  external-links:
    default: "true"
runs:
  using: composite
  steps:
    - shell: bash
      env:
        SITEMAP_URL: ${{ inputs.sitemap-url }}
        GITHUB_TOKEN: ${{ inputs.github-token }}
        EXTERNAL_LINKS: ${{ inputs.external-links }}
      run: |
        lychee --dump "${{ inputs.sitemap-url }}" > lychee/deployed-pages.txt
        lychee --remap "${{ inputs.edit-url }}/(.*) file://${{ github.workspace }}/\$1"
"#,
        )
        .unwrap();

        let steps = composite_script_steps(&plan, &metadata, "/__w").unwrap();

        assert!(steps[0].env.contains(&(
            "SITEMAP_URL".into(),
            "${{ steps.sitemap.outputs.url }}".into()
        )));
        assert!(steps[0]
            .env
            .contains(&("GITHUB_TOKEN".into(), "ghs_token".into())));
        assert!(steps[0]
            .env
            .contains(&("EXTERNAL_LINKS".into(), "false".into())));
        assert!(steps[0].env.contains(&(
            "GITHUB_ACTION_PATH".into(),
            "/__w/.github/actions/check-deployed-docs".into()
        )));
        assert!(steps[0]
            .script
            .contains(r#"lychee --dump "${{ steps.sitemap.outputs.url }}""#));
        assert!(steps[0].script.contains(r#"file:///__w/\$1"#));
    }

    #[test]
    fn expands_composite_outputs_from_inner_step_outputs() {
        let plan = LocalActionPlan {
            step_id: "pages".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/pages"),
            inputs: BTreeMap::new(),
        };
        let metadata = parse_action_metadata(
            r#"
outputs:
  artifact-id:
    value: ${{ steps.upload-artifact.outputs.artifact-id }}
runs:
  using: composite
  steps:
    - id: upload-artifact
      uses: actions/upload-artifact@v7
"#,
        )
        .unwrap();

        let invocations =
            composite_action_invocations(&plan, &metadata, "/__w", Path::new("/tmp/actions"))
                .unwrap();

        assert_eq!(invocations.len(), 2);
        let CompositeActionInvocation::Repository(plan) = &invocations[0] else {
            panic!("first composite invocation should be repository action")
        };
        assert_eq!(plan.step_id, "pages-upload-artifact");
        let CompositeActionInvocation::Outputs(outputs) = &invocations[1] else {
            panic!("second composite invocation should materialize outputs")
        };
        assert_eq!(outputs.step_id, "pages");
        assert_eq!(
            outputs.outputs["artifact-id"],
            "${{ steps.pages-upload-artifact.outputs.artifact-id }}"
        );
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
      if: ${{ inputs.github-token != '' }}
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
        assert_eq!(
            plans[0].condition.as_deref(),
            Some("${{ 'ghs_token' != '' }}")
        );
    }

    #[test]
    fn expands_repository_composite_run_steps() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "toolchain".into(),
            repository: "acme/toolchain".into(),
            git_ref: "stable".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/acme_toolchain/stable"),
            action_dir: actions_host.join("_actions/acme_toolchain/stable"),
            inputs: [("toolchain".to_string(), "stable".to_string())].into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            r#"
inputs:
  toolchain:
    default: nightly
runs:
  using: composite
  steps:
    - shell: bash
      if: runner.os == 'Linux'
      continue-on-error: true
      working-directory: ${{ github.action_path }}/fixtures
      run: echo "${{ github.action_path }} ${{ inputs.toolchain }}"
"#,
        )
        .unwrap();
        let runtime = metadata.runtime().unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host.join("_actions/acme_toolchain/stable/action.yml"),
            metadata,
            runtime,
        };

        let invocations = resolved
            .composite_invocations("/__w", actions_host)
            .unwrap();

        let CompositeActionInvocation::Script(step) = &invocations[0] else {
            panic!("repository composite should expand to script")
        };
        assert_eq!(step.id, "toolchain-1");
        assert_eq!(step.condition.as_deref(), Some("runner.os == 'Linux'"));
        assert!(step.continue_on_error);
        assert_eq!(
            step.working_directory_container,
            "/__a/_actions/acme_toolchain/stable/fixtures"
        );
        assert!(step
            .script
            .contains("/__a/_actions/acme_toolchain/stable stable"));
        assert!(step.env.contains(&(
            "GITHUB_ACTION_PATH".into(),
            "/__a/_actions/acme_toolchain/stable".into()
        )));
    }

    #[test]
    fn expands_composite_expressions_without_whitespace() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "toolchain".into(),
            repository: "acme/toolchain".into(),
            git_ref: "stable".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/acme_toolchain/stable"),
            action_dir: actions_host.join("_actions/acme_toolchain/stable"),
            inputs: [
                ("toolchain".to_string(), "stable".to_string()),
                ("target".to_string(), "x86_64-unknown-linux-gnu".to_string()),
                ("targets".to_string(), String::new()),
                ("components".to_string(), String::new()),
            ]
            .into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - id: parse
      shell: bash
      env:
        toolchain: ${{inputs.toolchain}}
      run: echo "toolchain=${{inputs.toolchain}}" >> "$GITHUB_OUTPUT"
    - id: flags
      shell: bash
      env:
        targets: ${{inputs.targets || inputs.target || ''}}
      run: echo "downgrade=${{steps.parse.outputs.toolchain == 'nightly' && inputs.components && ' --allow-downgrade' || ''}}" >> "$GITHUB_OUTPUT"
"#,
        )
        .unwrap();
        let runtime = metadata.runtime().unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host.join("_actions/acme_toolchain/stable/action.yml"),
            metadata,
            runtime,
        };

        let invocations = resolved
            .composite_invocations("/__w", actions_host)
            .unwrap();

        let CompositeActionInvocation::Script(parse) = &invocations[0] else {
            panic!("parse should expand to script")
        };
        assert!(parse.script.contains("toolchain=stable"));

        let CompositeActionInvocation::Script(flags) = &invocations[1] else {
            panic!("flags should expand to script")
        };
        assert_eq!(
            parse.env,
            vec![
                ("toolchain".into(), "stable".into()),
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__a/_actions/acme_toolchain/stable".into()
                )
            ]
        );
        assert!(flags.env.contains(&(
            "targets".into(),
            "${{ '' || 'x86_64-unknown-linux-gnu' || '' }}".into()
        )));
        assert!(flags.script.contains(
            "${{ steps.toolchain-parse.outputs.toolchain == 'nightly' && '' && ' --allow-downgrade' || '' }}"
        ));
    }

    #[test]
    fn expands_composite_continue_on_error_from_string_inputs() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "setup".into(),
            repository: "acme/setup".into(),
            git_ref: "v1".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/acme_setup/v1"),
            action_dir: actions_host.join("_actions/acme_setup/v1"),
            inputs: [("soft-fail".to_string(), "true".to_string())].into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            r#"
inputs:
  soft-fail:
    default: false
runs:
  using: composite
  steps:
    - shell: bash
      continue-on-error: ${{ inputs.soft-fail }}
      run: cargo install acme-cli
    - uses: actions/cache@v5
      continue-on-error: "true"
"#,
        )
        .unwrap();
        let runtime = metadata.runtime().unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host.join("_actions/acme_setup/v1/action.yml"),
            metadata,
            runtime,
        };

        let invocations = resolved
            .composite_invocations("/__w", actions_host)
            .unwrap();

        let CompositeActionInvocation::Script(script) = &invocations[0] else {
            panic!("first composite step should expand to script")
        };
        assert!(script.continue_on_error);
        let CompositeActionInvocation::Repository(repository) = &invocations[1] else {
            panic!("second composite step should expand to repository action")
        };
        assert!(repository.continue_on_error);
    }

    #[test]
    fn collects_nested_repository_actions_from_resolved_composites() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "pages".into(),
            repository: "actions/upload-pages-artifact".into(),
            git_ref: "v4".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/actions_upload-pages-artifact/v4"),
            action_dir: actions_host.join("_actions/actions_upload-pages-artifact/v4"),
            inputs: [("path".to_string(), "site".to_string())].into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - uses: actions/upload-artifact@v7
      with:
        path: ${{ inputs.path }}
"#,
        )
        .unwrap();
        let runtime = metadata.runtime().unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host
                .join("_actions/actions_upload-pages-artifact/v4/action.yml"),
            metadata,
            runtime,
        };

        let plans =
            composite_repository_action_plans_from_resolved(&[resolved], actions_host).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].step_id, "pages-1");
        assert_eq!(plans[0].repository, "actions/upload-artifact");
        assert_eq!(plans[0].git_ref, "v7");
        assert_eq!(plans[0].inputs["path"], "site");
    }

    #[test]
    fn downloads_same_repository_ref_once_for_multiple_action_paths() {
        let actions_host = std::env::temp_dir().join(format!(
            "velnor-action-path-fetch-test-{}",
            std::process::id()
        ));
        let repository_dir = actions_host.join("_actions/actions_cache/v5");
        let restore_dir = repository_dir.join("restore");
        let save_dir = repository_dir.join("save");
        fs::create_dir_all(&restore_dir).unwrap();
        fs::create_dir_all(&save_dir).unwrap();
        fs::write(
            restore_dir.join("action.yml"),
            "runs:\n  using: node20\n  main: dist/restore.js\n",
        )
        .unwrap();
        fs::write(
            save_dir.join("action.yml"),
            "runs:\n  using: node20\n  main: dist/save.js\n",
        )
        .unwrap();
        let plans = vec![
            RepositoryActionPlan {
                step_id: "cache-restore".into(),
                repository: "actions/cache".into(),
                git_ref: "v5".into(),
                source_path: Some("restore".into()),
                repository_dir: repository_dir.clone(),
                action_dir: restore_dir,
                inputs: BTreeMap::new(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            },
            RepositoryActionPlan {
                step_id: "cache-save".into(),
                repository: "actions/cache".into(),
                git_ref: "v5".into(),
                source_path: Some("save".into()),
                repository_dir,
                action_dir: save_dir,
                inputs: BTreeMap::new(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            },
        ];
        let mut runner = RecordingRunner::default();

        let resolved = download_repository_actions(&mut runner, &plans).unwrap();

        let fetches = runner
            .calls
            .iter()
            .filter(|(program, args)| program == "git" && args.contains(&"fetch".to_string()))
            .count();
        assert_eq!(resolved.len(), 2);
        assert_eq!(fetches, 1);
        std::fs::remove_dir_all(actions_host).ok();
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
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
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
            env: [(
                "NODE_AUTH_TOKEN".to_string(),
                "${{ github.token }}".to_string(),
            )]
            .into(),
            condition: None,
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            "runs:\n  using: node20\n  pre: dist/setup.js\n  pre-if: always()\n  main: dist/index.js\n  post: dist/cleanup.js\n  post-if: success()\n",
        )
        .unwrap();
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
            invocation.pre_container_path.as_deref(),
            Some("/__a/_actions/actions_setup-node/v4/dist/setup.js")
        );
        assert_eq!(invocation.pre_condition.as_deref(), Some("always()"));
        assert_eq!(
            invocation.main_container_path,
            "/__a/_actions/actions_setup-node/v4/dist/index.js"
        );
        assert_eq!(
            invocation.post_container_path.as_deref(),
            Some("/__a/_actions/actions_setup-node/v4/dist/cleanup.js")
        );
        assert_eq!(invocation.post_condition.as_deref(), Some("success()"));
        assert!(invocation
            .env
            .contains(&("INPUT_NODE-VERSION".into(), "22".into())));
        assert!(invocation.env.contains(&(
            "GITHUB_ACTION_PATH".into(),
            "/__a/_actions/actions_setup-node/v4".into()
        )));
        assert!(invocation
            .env
            .contains(&("GITHUB_ACTION".into(), "setup".into())));
        assert!(invocation.env.contains(&(
            "GITHUB_ACTION_REPOSITORY".into(),
            "actions/setup-node".into()
        )));
        assert!(invocation
            .env
            .contains(&("GITHUB_ACTION_REF".into(), "v4".into())));
        assert!(invocation
            .env
            .contains(&("NODE_AUTH_TOKEN".into(), "${{ github.token }}".into())));
    }

    #[test]
    fn builds_javascript_action_invocation_with_input_defaults() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "cache".into(),
            repository: "actions/cache".into(),
            git_ref: "v5".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/actions_cache/v5"),
            action_dir: actions_host.join("_actions/actions_cache/v5"),
            inputs: [("path".to_string(), "~/.cargo".to_string())].into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            r#"
inputs:
  path:
    required: true
  fail-on-cache-miss:
    default: "false"
runs:
  using: node20
  main: dist/index.js
"#,
        )
        .unwrap();
        let runtime = metadata.runtime().unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host.join("_actions/actions_cache/v5/action.yml"),
            metadata,
            runtime,
        };

        let invocation = resolved.javascript_invocation(actions_host).unwrap();

        assert!(invocation
            .env
            .contains(&("INPUT_PATH".into(), "~/.cargo".into())));
        assert!(invocation
            .env
            .contains(&("INPUT_FAIL-ON-CACHE-MISS".into(), "false".into())));
    }

    #[test]
    fn builds_target_download_artifact_invocation_inputs() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "download-platform-digests".into(),
            repository: "actions/download-artifact".into(),
            git_ref: "3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c".into(),
            source_path: None,
            repository_dir: actions_host.join(
                "_actions/actions_download-artifact/3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c",
            ),
            action_dir: actions_host.join(
                "_actions/actions_download-artifact/3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c",
            ),
            inputs: [
                ("pattern".to_string(), "construct-digest-*".to_string()),
                ("path".to_string(), "${{ env.DIGEST_DIR }}".to_string()),
                ("merge-multiple".to_string(), "true".to_string()),
            ]
            .into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            r#"
runs:
  using: node24
  main: dist/download/index.js
"#,
        )
        .unwrap();
        let runtime = metadata.runtime().unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host.join(
                "_actions/actions_download-artifact/3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c/action.yml",
            ),
            metadata,
            runtime,
        };

        let invocation = resolved.javascript_invocation(actions_host).unwrap();

        assert_eq!(invocation.node, "node24");
        assert_eq!(
            invocation.main_container_path,
            "/__a/_actions/actions_download-artifact/3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c/dist/download/index.js"
        );
        assert!(invocation
            .env
            .contains(&("INPUT_PATTERN".into(), "construct-digest-*".into())));
        assert!(invocation
            .env
            .contains(&("INPUT_PATH".into(), "${{ env.DIGEST_DIR }}".into())));
        assert!(invocation
            .env
            .contains(&("INPUT_MERGE-MULTIPLE".into(), "true".into())));
    }

    #[test]
    fn builds_target_upload_artifact_invocation_inputs() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "upload-platform-digest".into(),
            repository: "actions/upload-artifact".into(),
            git_ref: "043fb46d1a93c77aae656e7c1c64a875d1fc6a0a".into(),
            source_path: None,
            repository_dir: actions_host
                .join("_actions/actions_upload-artifact/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"),
            action_dir: actions_host
                .join("_actions/actions_upload-artifact/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"),
            inputs: [
                (
                    "name".to_string(),
                    "construct-digest-${{ matrix.platform }}".to_string(),
                ),
                (
                    "path".to_string(),
                    "${{ env.DIGEST_DIR }}/${{ matrix.platform }}.digest".to_string(),
                ),
                ("if-no-files-found".to_string(), "error".to_string()),
                ("retention-days".to_string(), "1".to_string()),
            ]
            .into(),
            env: Vec::new(),
            condition: Some("needs.changes.outputs.is_publish == 'true'".into()),
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            r#"
runs:
  using: node24
  main: dist/upload/index.js
"#,
        )
        .unwrap();
        let runtime = metadata.runtime().unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host
                .join("_actions/actions_upload-artifact/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/action.yml"),
            metadata,
            runtime,
        };

        let invocation = resolved.javascript_invocation(actions_host).unwrap();

        assert_eq!(invocation.node, "node24");
        assert_eq!(
            invocation.main_container_path,
            "/__a/_actions/actions_upload-artifact/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/dist/upload/index.js"
        );
        assert!(invocation.env.contains(&(
            "INPUT_NAME".into(),
            "construct-digest-${{ matrix.platform }}".into()
        )));
        assert!(invocation.env.contains(&(
            "INPUT_PATH".into(),
            "${{ env.DIGEST_DIR }}/${{ matrix.platform }}.digest".into()
        )));
        assert!(invocation
            .env
            .contains(&("INPUT_IF-NO-FILES-FOUND".into(), "error".into())));
        assert!(invocation
            .env
            .contains(&("INPUT_RETENTION-DAYS".into(), "1".into())));
    }

    #[test]
    fn builds_docker_action_invocation() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "renovate".into(),
            repository: "renovatebot/github-action".into(),
            git_ref: "v46.1.14".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/renovatebot_github-action/v46.1.14"),
            action_dir: actions_host.join("_actions/renovatebot_github-action/v46.1.14"),
            inputs: [(
                "renovate-image".to_string(),
                "ghcr.io/renovatebot/renovate".to_string(),
            )]
            .into(),
            env: [("LOG_LEVEL".to_string(), "debug".to_string())].into(),
            condition: None,
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            r#"
inputs:
  renovate-image:
    default: ghcr.io/renovatebot/renovate
runs:
  using: docker
  image: docker://alpine:3.20
  entrypoint: /entrypoint.sh
  args:
    - ${{ inputs.renovate-image }}
    - ${{ github.action_path }}/config.js
"#,
        )
        .unwrap();
        let runtime = metadata.runtime().unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host
                .join("_actions/renovatebot_github-action/v46.1.14/action.yml"),
            metadata,
            runtime,
        };

        let invocation = resolved.docker_invocation(actions_host).unwrap();

        assert_eq!(invocation.image, "alpine:3.20");
        assert!(invocation.build_context_host.is_none());
        assert!(invocation.dockerfile_host.is_none());
        assert_eq!(invocation.entrypoint.as_deref(), Some("/entrypoint.sh"));
        assert_eq!(
            invocation.args,
            vec![
                "ghcr.io/renovatebot/renovate",
                "/__a/_actions/renovatebot_github-action/v46.1.14/config.js",
            ]
        );
        assert!(invocation.env.contains(&(
            "INPUT_RENOVATE-IMAGE".into(),
            "ghcr.io/renovatebot/renovate".into()
        )));
        assert!(invocation
            .env
            .contains(&("LOG_LEVEL".into(), "debug".into())));
    }

    #[test]
    fn parses_fetched_target_action_metadata() {
        let roots = [
            Path::new("/tmp/velnor-actions"),
            Path::new("/tmp/velnor-targets/jackin/.github/actions"),
        ];
        if roots.iter().all(|root| !root.exists()) {
            return;
        }

        let mut parsed = 0;
        for root in roots.into_iter().filter(|root| root.exists()) {
            for path in action_metadata_files(root) {
                let contents = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                let metadata = parse_action_metadata(&contents)
                    .unwrap_or_else(|error| panic!("parse {}: {error:#}", path.display()));
                metadata
                    .runtime()
                    .unwrap_or_else(|error| panic!("runtime {}: {error:#}", path.display()));
                parsed += 1;
            }
        }

        let expected_minimum = if roots[0].exists() { 20 } else { 1 };
        assert!(
            parsed >= expected_minimum,
            "expected fetched target action metadata"
        );
    }

    #[test]
    fn fetched_target_composite_actions_have_repository_action_closure() {
        let actions_root = Path::new("/tmp/velnor-actions");
        if !actions_root.exists() {
            return;
        }
        let roots = [
            actions_root,
            Path::new("/tmp/velnor-targets/jackin/.github/actions"),
        ];
        if roots.iter().all(|root| !root.exists()) {
            return;
        }

        let mut checked = 0;
        let mut missing = Vec::new();
        for root in roots.into_iter().filter(|root| root.exists()) {
            for path in action_metadata_files(root) {
                let contents = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                let metadata = parse_action_metadata(&contents)
                    .unwrap_or_else(|error| panic!("parse {}: {error:#}", path.display()));
                if metadata.runtime().unwrap() != ActionRuntime::Composite {
                    continue;
                }
                for step in &metadata.runs.steps {
                    let Some(uses) = step.uses.as_deref() else {
                        continue;
                    };
                    let reference = parse_repository_uses(uses)
                        .unwrap_or_else(|error| panic!("parse uses {uses}: {error:#}"));
                    let action_dir = action_dir(
                        actions_root,
                        &reference.repository,
                        &reference.git_ref,
                        reference.source_path.as_deref(),
                    )
                    .unwrap_or_else(|error| panic!("resolve action dir for {uses}: {error:#}"));
                    checked += 1;
                    if action_metadata_path(&action_dir).is_err() {
                        missing.push(format!("{} -> {}", path.display(), uses));
                    }
                }
            }
        }

        assert!(
            checked >= 8,
            "expected target composite repository references to be checked"
        );
        assert!(
            missing.is_empty(),
            "missing fetched nested action metadata:\n{}",
            missing.join("\n")
        );
    }

    #[test]
    fn fetched_target_composite_actions_expand_to_supported_invocations() {
        let actions_root = Path::new("/tmp/velnor-actions");
        let roots = [
            actions_root,
            Path::new("/tmp/velnor-targets/jackin/.github/actions"),
        ];
        if roots.iter().all(|root| !root.exists()) {
            return;
        }

        let mut checked = 0;
        for root in roots.into_iter().filter(|root| root.exists()) {
            for path in action_metadata_files(root) {
                let contents = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                let metadata = parse_action_metadata(&contents)
                    .unwrap_or_else(|error| panic!("parse {}: {error:#}", path.display()));
                if metadata.runtime().unwrap() != ActionRuntime::Composite {
                    continue;
                }
                checked += 1;
                let invocations = if path.starts_with(actions_root) {
                    let action_dir = path.parent().unwrap().to_path_buf();
                    ResolvedAction {
                        plan: RepositoryActionPlan {
                            step_id: format!("composite-{checked}"),
                            repository: "target/composite".into(),
                            git_ref: "test".into(),
                            source_path: None,
                            repository_dir: action_dir.clone(),
                            action_dir,
                            inputs: BTreeMap::new(),
                            env: Vec::new(),
                            condition: None,
                            continue_on_error: false,
                        },
                        metadata_path: path.clone(),
                        runtime: metadata.runtime().unwrap(),
                        metadata,
                    }
                    .composite_invocations("/__w", actions_root)
                    .unwrap_or_else(|error| {
                        panic!("expand fetched composite {}: {error:#}", path.display())
                    })
                } else {
                    let action_dir = path.parent().unwrap().to_path_buf();
                    let plan = LocalActionPlan {
                        step_id: format!("local-composite-{checked}"),
                        action_dir,
                        inputs: BTreeMap::new(),
                    };
                    composite_action_invocations(&plan, &metadata, "/__w", actions_root)
                        .unwrap_or_else(|error| {
                            panic!("expand local composite {}: {error:#}", path.display())
                        })
                };
                assert!(
                    !invocations.is_empty(),
                    "expected composite {} to expand to invocations",
                    path.display()
                );
            }
        }

        assert!(
            checked >= 8,
            "expected target composite actions to be expanded"
        );
    }

    #[test]
    fn fetched_target_workflow_actions_have_metadata() {
        let actions_root = Path::new("/tmp/velnor-actions");
        let workflow_roots = [
            Path::new("/tmp/velnor-targets/jackin/.github/workflows"),
            Path::new("/tmp/velnor-targets/java-monorepo/.github/workflows"),
        ];
        if !actions_root.exists() || workflow_roots.iter().all(|root| !root.exists()) {
            return;
        }

        let mut checked = 0;
        let mut missing = Vec::new();
        for root in workflow_roots.into_iter().filter(|root| root.exists()) {
            for path in workflow_files(root) {
                let contents = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                let yaml = serde_yaml::from_str::<serde_yaml::Value>(&contents)
                    .unwrap_or_else(|error| panic!("parse {}: {error:#}", path.display()));
                for uses in workflow_uses_values(&yaml) {
                    if uses.starts_with('.') || uses.starts_with("docker://") {
                        continue;
                    }
                    let reference = parse_repository_uses(&uses)
                        .unwrap_or_else(|error| panic!("parse uses {uses}: {error:#}"));
                    if reference
                        .repository
                        .eq_ignore_ascii_case("actions/checkout")
                    {
                        continue;
                    }
                    let action_dir = action_dir(
                        actions_root,
                        &reference.repository,
                        &reference.git_ref,
                        reference.source_path.as_deref(),
                    )
                    .unwrap_or_else(|error| panic!("resolve action dir for {uses}: {error:#}"));
                    checked += 1;
                    if action_metadata_path(&action_dir).is_err() {
                        missing.push(format!("{} -> {}", path.display(), uses));
                    }
                }
            }
        }

        assert!(checked >= 40, "expected target workflow action references");
        assert!(
            missing.is_empty(),
            "missing fetched target action metadata:\n{}",
            missing.join("\n")
        );
    }

    #[test]
    fn target_marketplace_actions_map_to_native_adapters() {
        let adapters = [
            ("actions/checkout", NativeActionAdapter::Checkout),
            ("actions/cache", NativeActionAdapter::Cache),
            (
                "actions/upload-artifact",
                NativeActionAdapter::UploadArtifact,
            ),
            (
                "actions/download-artifact",
                NativeActionAdapter::DownloadArtifact,
            ),
            (
                "actions/upload-pages-artifact",
                NativeActionAdapter::UploadPagesArtifact,
            ),
            ("actions/deploy-pages", NativeActionAdapter::DeployPages),
            ("dorny/paths-filter", NativeActionAdapter::PathsFilter),
            ("jdx/mise-action", NativeActionAdapter::Mise),
            (
                "mozilla-actions/sccache-action",
                NativeActionAdapter::Sccache,
            ),
            ("rui314/setup-mold", NativeActionAdapter::SetupMold),
            ("extractions/setup-just", NativeActionAdapter::SetupJust),
            ("Swatinem/rust-cache", NativeActionAdapter::RustCache),
            (
                "crazy-max/ghaction-github-runtime",
                NativeActionAdapter::GitHubRuntimeExport,
            ),
            ("renovatebot/github-action", NativeActionAdapter::Renovate),
            (
                "docker/setup-buildx-action",
                NativeActionAdapter::DockerSetupBuildx,
            ),
            ("docker/login-action", NativeActionAdapter::DockerLogin),
            (
                "docker/metadata-action",
                NativeActionAdapter::DockerMetadata,
            ),
            (
                "docker/build-push-action",
                NativeActionAdapter::DockerBuildPush,
            ),
            ("docker/bake-action", NativeActionAdapter::DockerBake),
        ];

        for (repository, adapter) in adapters {
            assert_eq!(native_action_adapter(repository), Some(adapter));
        }
        assert_eq!(native_action_adapter("owner/unknown-action"), None);
    }

    fn action_metadata_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_action_metadata_files(root, &mut files);
        files.sort();
        files
    }

    fn collect_action_metadata_files(dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_action_metadata_files(&path, files);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| matches!(name, "action.yml" | "action.yaml"))
            {
                files.push(path);
            }
        }
    }

    fn workflow_files(root: &Path) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(root) else {
            return Vec::new();
        };
        let mut files = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| matches!(extension, "yml" | "yaml"))
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    fn workflow_uses_values(value: &serde_yaml::Value) -> Vec<String> {
        let mut values = Vec::new();
        collect_workflow_uses_values(value, &mut values);
        values.sort();
        values.dedup();
        values
    }

    fn collect_workflow_uses_values(value: &serde_yaml::Value, values: &mut Vec<String>) {
        match value {
            serde_yaml::Value::Mapping(map) => {
                for (key, value) in map {
                    if key.as_str() == Some("uses") {
                        if let Some(uses) = value.as_str() {
                            values.push(uses.to_string());
                        }
                    }
                    collect_workflow_uses_values(value, values);
                }
            }
            serde_yaml::Value::Sequence(sequence) => {
                for value in sequence {
                    collect_workflow_uses_values(value, values);
                }
            }
            _ => {}
        }
    }
}

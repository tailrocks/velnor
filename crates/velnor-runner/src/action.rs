#![allow(dead_code)]

use crate::{
    checkout::fetch_git_ref,
    executor::CommandRunner,
    job_message::{ActionReferenceType, ActionStep},
    script_step::{step_environment, ScriptStep},
};
use anyhow::{bail, Context, Result};
use serde::{
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
};
use velnor_action_manifest::{
    normalize_runner_yaml_numbers, normalized_runner_number, validate_template_expressions,
    ActionExpressionContext,
};

#[derive(Debug, Clone, Deserialize)]
pub struct ActionMetadata {
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub description: Option<String>,
    pub runs: ActionRuns,
    #[serde(default)]
    pub inputs: BTreeMap<String, ActionInput>,
    #[serde(default)]
    pub outputs: BTreeMap<String, ActionOutput>,
}

#[derive(Debug, Clone)]
pub struct ActionInput {
    pub description: Option<String>,
    pub default_value: Option<String>,
    pub deprecation_message: Option<String>,
}

impl<'de> Deserialize<'de> for ActionInput {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ActionInputVisitor;

        impl<'de> Visitor<'de> for ActionInputVisitor {
            type Value = ActionInput;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an action input mapping")
            }

            fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut description = None;
                let mut default_value = None;
                let mut deprecation_message = None;
                let mut seen = BTreeSet::new();
                while let Some(key) = map.next_key::<serde_yaml::Value>()? {
                    let key = action_manifest_scalar_string(&key).ok_or_else(|| {
                        <M::Error as serde::de::Error>::custom(
                            "action input definition keys must be YAML scalars",
                        )
                    })?;
                    let canonical = runner_ordinal_ignore_case_key(&key);
                    if matches!(canonical.as_str(), "default" | "deprecationmessage")
                        && !seen.insert(canonical.clone())
                    {
                        return Err(<M::Error as serde::de::Error>::custom(format!(
                            "duplicate action input field '{key}'"
                        )));
                    }
                    let value = map.next_value::<serde_yaml::Value>()?;
                    match canonical.as_str() {
                        // Runner's input schema gives non-default metadata
                        // loose `any` values and ConvertInputs ignores this
                        // property. Keep scalar descriptions for admission
                        // reporting, but never reject a value Runner ignores.
                        "description" => {
                            description = action_manifest_scalar_string(&value);
                        }
                        "default" => {
                            default_value =
                                Some(action_manifest_scalar_string(&value).ok_or_else(|| {
                                    <M::Error as serde::de::Error>::custom(
                                        "action input default must be a YAML scalar",
                                    )
                                })?);
                        }
                        "deprecationmessage" => {
                            deprecation_message = Some(
                                value
                                    .as_str()
                                    .ok_or_else(|| {
                                        <M::Error as serde::de::Error>::custom(
                                            "action input deprecationMessage must be a string",
                                        )
                                    })?
                                    .to_owned(),
                            );
                        }
                        _ => {}
                    }
                }
                Ok(ActionInput {
                    description,
                    default_value,
                    deprecation_message,
                })
            }
        }

        deserializer.deserialize_map(ActionInputVisitor)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionOutput {
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionRuns {
    #[serde(deserialize_with = "deserialize_string_scalar")]
    pub using: String,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub main: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub pre: Option<String>,
    #[serde(
        default,
        rename = "pre-if",
        deserialize_with = "deserialize_optional_string_scalar"
    )]
    pub pre_if: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub post: Option<String>,
    #[serde(
        default,
        rename = "post-if",
        deserialize_with = "deserialize_optional_string_scalar"
    )]
    pub post_if: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub image: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub entrypoint: Option<String>,
    /// Docker-action pre/post entrypoints (`runs.pre-entrypoint` /
    /// `runs.post-entrypoint`): upstream runs them as the Pre/Post stage
    /// with the same image and args
    /// (`ContainerActionHandler.cs: RunAsync(stage)`).
    #[serde(
        default,
        rename = "pre-entrypoint",
        deserialize_with = "deserialize_optional_string_scalar"
    )]
    pub pre_entrypoint: Option<String>,
    #[serde(
        default,
        rename = "post-entrypoint",
        deserialize_with = "deserialize_optional_string_scalar"
    )]
    pub post_entrypoint: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar_vec")]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: ActionTemplateMap,
    #[serde(default)]
    pub steps: Vec<CompositeActionStep>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CompositeActionStep {
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub shell: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub run: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    pub uses: Option<String>,
    #[serde(default)]
    pub with: ActionTemplateMap,
    #[serde(default)]
    pub env: ActionTemplateMap,
    #[serde(
        default,
        rename = "if",
        deserialize_with = "deserialize_optional_string_scalar"
    )]
    pub condition: Option<String>,
    #[serde(
        default,
        rename = "working-directory",
        deserialize_with = "deserialize_optional_string_scalar"
    )]
    pub working_directory: Option<String>,
    #[serde(
        default,
        rename = "continue-on-error",
        deserialize_with = "deserialize_optional_action_boolean"
    )]
    pub continue_on_error: Option<ActionBooleanValue>,
}

/// Pinned Runner's composite-step schema accepts a boolean literal or one
/// expression token. Preserve the expression until the step has failed, when
/// `ApplyContinueOnError` evaluates it against the then-current steps context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionBooleanValue {
    Literal(bool),
    Expression(String),
}

/// Ordered template entries from action metadata mappings. Runner evaluates
/// both keys and values, so keep the source expressions and which fields need
/// evaluation until the action's execution context exists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActionTemplateMap(Vec<ActionTemplateEntry>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionTemplateEntry {
    pub key: String,
    pub value: String,
    pub key_is_template: bool,
    pub value_is_template: bool,
}

impl ActionTemplateEntry {
    fn new(key: String, value: String) -> Self {
        let key_is_template = key.contains("${{");
        let value_is_template = value.contains("${{");
        Self {
            key,
            value,
            key_is_template,
            value_is_template,
        }
    }
}

impl ActionTemplateMap {
    pub(crate) fn from_entries(entries: impl IntoIterator<Item = ActionTemplateEntry>) -> Self {
        Self(entries.into_iter().collect())
    }

    pub(crate) fn from_btree_map(
        values: &BTreeMap<String, String>,
        expression_values: &BTreeSet<String>,
    ) -> Self {
        Self(
            values
                .iter()
                .map(|(key, value)| ActionTemplateEntry {
                    key: key.clone(),
                    value: value.clone(),
                    key_is_template: false,
                    value_is_template: expression_values.iter().any(|name| {
                        runner_ordinal_ignore_case_key(name) == runner_ordinal_ignore_case_key(key)
                    }),
                })
                .collect(),
        )
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &ActionTemplateEntry> {
        self.0.iter()
    }

    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.as_str())
    }

    pub(crate) fn has_template_keys(&self) -> bool {
        self.0.iter().any(|entry| entry.key_is_template)
    }

    pub(crate) fn static_key_values(&self) -> BTreeMap<String, String> {
        self.0
            .iter()
            .filter(|entry| !entry.key_is_template)
            .map(|entry| (entry.key.clone(), entry.value.clone()))
            .collect()
    }
}

impl<'de> Deserialize<'de> for ActionTemplateMap {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ActionTemplateMapVisitor;

        impl<'de> Visitor<'de> for ActionTemplateMapVisitor {
            type Value = ActionTemplateMap;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an action metadata mapping")
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ActionTemplateMap::default())
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ActionTemplateMap::default())
            }

            fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                deserializer.deserialize_map(self)
            }

            fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut entries = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some((key, value)) =
                    map.next_entry::<serde_yaml::Value, serde_yaml::Value>()?
                {
                    let key = action_manifest_scalar_string(&key).ok_or_else(|| {
                        <M::Error as serde::de::Error>::custom(
                            "action metadata mapping keys must be YAML scalars",
                        )
                    })?;
                    let value = action_manifest_scalar_string(&value).ok_or_else(|| {
                        <M::Error as serde::de::Error>::custom(
                            "action metadata mapping values must be YAML scalars",
                        )
                    })?;
                    entries.push(ActionTemplateEntry::new(key, value));
                }
                Ok(ActionTemplateMap::from_entries(entries))
            }
        }

        deserializer.deserialize_option(ActionTemplateMapVisitor)
    }
}

fn deserialize_optional_action_boolean<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<ActionBooleanValue>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_yaml::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_yaml::Value::Null) => Ok(None),
        Some(serde_yaml::Value::Bool(value)) => Ok(Some(ActionBooleanValue::Literal(value))),
        Some(serde_yaml::Value::String(value)) => {
            match runner_template_scalar(&value, ActionExpressionContext::CompositeBoolean)
                .map_err(serde::de::Error::custom)?
            {
                RunnerTemplateScalar::Expression(expression) => {
                    Ok(Some(ActionBooleanValue::Expression(expression)))
                }
                RunnerTemplateScalar::Literal(_) => Err(serde::de::Error::custom(
                    "continue-on-error string must contain a template expression",
                )),
            }
        }
        Some(_) => Err(serde::de::Error::custom(
            "continue-on-error must be a boolean or expression",
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionRuntime {
    JavaScript { node: String, main: String },
    Composite,
    Docker { image: String },
}

/// Dispatch class declared by the capability manifest. Generic action classes
/// are deliberately separate from native adapters so an admitted action names
/// the planner arm that can execute it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionAdapter {
    Composite,
    Docker,
    JavaScript,
    Native(NativeActionAdapter),
}

impl ActionAdapter {
    /// Stable name used by the exported capability manifest. Keep generic
    /// dispatch classes distinct from native adapter names.
    pub fn manifest_name(self) -> String {
        match self {
            Self::Composite => "Composite".to_string(),
            Self::Docker => "Docker".to_string(),
            Self::JavaScript => "JavaScript".to_string(),
            Self::Native(adapter) => format!("Native({adapter:?})"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeActionAdapter {
    Checkout,
    Cache,
    UploadArtifact,
    DownloadArtifact,
    UploadPagesArtifact,
    ConfigurePages,
    DeployPages,
    AttestBuildProvenance,
    CreateGitHubAppToken,
    PathsFilter,
    Mise,
    Sccache,
    SetupMold,
    SetupJust,
    RustCache,
    GitHubRuntimeExport,
    GitHubScript,
    Renovate,
    DockerSetupBuildx,
    DockerLogin,
    DockerMetadata,
    DockerBuildPush,
    DockerBake,
    Hadolint,
    SetupQemu,
    CosignInstaller,
}

/// GitHub cache lifecycle carried by an `actions/cache` invocation. The root
/// action restores in main and saves in post; `/restore` only restores; `/save`
/// only saves. Velnor must preserve the distinction the action subpath encodes
/// instead of collapsing every form into the root behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheActionKind {
    Root,
    Restore,
    Save,
}

/// Classify an `actions/cache` invocation from its action subpath. Absent or
/// empty = root; exact `restore`/`save` = the matching subaction. Any other
/// subpath is rejected rather than silently treated as root, so an unknown cache
/// form fails closed before it can restore/save with the wrong lifecycle.
pub fn cache_action_kind(source_path: Option<&str>) -> Result<CacheActionKind> {
    match source_path
        .map(|value| value.trim().trim_matches('/'))
        .filter(|value| !value.is_empty())
    {
        None => Ok(CacheActionKind::Root),
        Some("restore") => Ok(CacheActionKind::Restore),
        Some("save") => Ok(CacheActionKind::Save),
        Some(other) => bail!(
            "unsupported actions/cache subpath '{other}': only the root action, 'restore', and 'save' are recognized"
        ),
    }
}

pub fn native_action_adapter(repository: &str) -> Option<NativeActionAdapter> {
    match repository.to_ascii_lowercase().as_str() {
        "actions/checkout" => Some(NativeActionAdapter::Checkout),
        "actions/cache" => Some(NativeActionAdapter::Cache),
        "actions/upload-artifact" => Some(NativeActionAdapter::UploadArtifact),
        "actions/download-artifact" => Some(NativeActionAdapter::DownloadArtifact),
        "actions/upload-pages-artifact" => Some(NativeActionAdapter::UploadPagesArtifact),
        "actions/configure-pages" => Some(NativeActionAdapter::ConfigurePages),
        "actions/deploy-pages" => Some(NativeActionAdapter::DeployPages),
        "actions/attest-build-provenance" => Some(NativeActionAdapter::AttestBuildProvenance),
        "actions/create-github-app-token" => Some(NativeActionAdapter::CreateGitHubAppToken),
        "dorny/paths-filter" => Some(NativeActionAdapter::PathsFilter),
        "jdx/mise-action" => Some(NativeActionAdapter::Mise),
        "mozilla-actions/sccache-action" => Some(NativeActionAdapter::Sccache),
        "rui314/setup-mold" => Some(NativeActionAdapter::SetupMold),
        "extractions/setup-just" => Some(NativeActionAdapter::SetupJust),
        "swatinem/rust-cache" => Some(NativeActionAdapter::RustCache),
        "crazy-max/ghaction-github-runtime" => Some(NativeActionAdapter::GitHubRuntimeExport),
        "actions/github-script" => Some(NativeActionAdapter::GitHubScript),
        "renovatebot/github-action" => Some(NativeActionAdapter::Renovate),
        "docker/setup-buildx-action" => Some(NativeActionAdapter::DockerSetupBuildx),
        "docker/login-action" => Some(NativeActionAdapter::DockerLogin),
        "docker/metadata-action" => Some(NativeActionAdapter::DockerMetadata),
        "docker/build-push-action" => Some(NativeActionAdapter::DockerBuildPush),
        "docker/bake-action" => Some(NativeActionAdapter::DockerBake),
        "hadolint/hadolint-action" => Some(NativeActionAdapter::Hadolint),
        "docker/setup-qemu-action" => Some(NativeActionAdapter::SetupQemu),
        "sigstore/cosign-installer" => Some(NativeActionAdapter::CosignInstaller),
        _ => None,
    }
}

/// Canonical repository identity for a native adapter, sourced from the same
/// capability table that admits it.
pub(crate) fn native_action_repository(adapter: NativeActionAdapter) -> Option<&'static str> {
    crate::manifest::ACTIONS.iter().find_map(|capability| {
        matches!(capability.adapter, ActionAdapter::Native(candidate) if candidate == adapter)
            .then_some(capability.repository)
    })
}

/// Returns an error message for actions that are explicitly not supported on Velnor.
///
/// These actions run as node JavaScript in ephemeral sidecar containers. They rely on
/// tool binaries (e.g. `cargo`) being present in those containers, but Velnor's sidecar
/// images are plain node images with no Rust tooling. Velnor cannot silently emulate
/// this — it fails with a cryptic "executable not found" error. Fail fast instead.
pub fn unsupported_action_error(repository: &str) -> Option<&'static str> {
    match repository.to_ascii_lowercase().as_str() {
        "dtolnay/rust-toolchain" => Some(
            "dtolnay/rust-toolchain is not supported on Velnor: it installs Rust inside an \
             ephemeral node sidecar container, so the toolchain is lost when the container exits. \
             Use jdx/mise-action with a 'rust = \"stable\"' entry in mise.toml instead.",
        ),
        "baptiste0928/cargo-install" => Some(
            "baptiste0928/cargo-install is not supported on Velnor: it invokes cargo inside an \
             ephemeral node sidecar container that has no Rust tooling. \
             Use jdx/mise-action with a 'cargo:<crate> = \"latest\"' entry in mise.toml instead.",
        ),
        "embarkstudios/cargo-deny-action" => Some(
            "EmbarkStudios/cargo-deny-action is not supported on Velnor: use jdx/mise-action \
             with a pinned 'cargo:cargo-deny' tool and invoke cargo deny from a run step instead.",
        ),
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepositoryActionPlan {
    pub step_id: String,
    pub repository: String,
    pub git_ref: String,
    pub source_path: Option<String>,
    pub repository_dir: PathBuf,
    pub action_dir: PathBuf,
    pub inputs: BTreeMap<String, String>,
    /// Inputs whose expressions are deferred until the action invocation.
    pub expression_inputs: BTreeSet<String>,
    /// Original composite-step `with` mapping when one or more keys depend on
    /// the active embedded-step context. The executor evaluates it at step
    /// time, before dispatching the action.
    pub input_templates: Option<ActionTemplateMap>,
    pub env: Vec<(String, String)>,
    pub condition: Option<String>,
    pub continue_on_error: bool,
    pub timeout_minutes: Option<u64>,
}

pub const NATIVE_ACTION_REF: &str = "__native";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalActionPlan {
    pub step_id: String,
    pub action_dir: PathBuf,
    /// Workspace base used by both `uses: ./...` resolution and the matching
    /// container path. Local actions need not live below `.github/actions`.
    pub workspace_host: PathBuf,
    pub inputs: BTreeMap<String, String>,
    /// Inputs left for step-time rendering because they read runtime contexts.
    /// This keeps an evaluated literal such as `${{ secrets.X }}` from being
    /// parsed a second time after it is inserted into an action input.
    pub expression_inputs: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub enum CompositeActionInvocation {
    CompositeStart {
        step_id: String,
        display_name: String,
        inputs: BTreeMap<String, String>,
        input_diagnostics: Vec<String>,
        expression_inputs: BTreeSet<String>,
        input_templates: Option<ActionTemplateMap>,
        input_defaults: ActionTemplateMap,
        visible_step_ids: BTreeMap<String, String>,
        env: Vec<(String, String)>,
        condition: Option<String>,
        continue_on_error: bool,
        continue_on_error_expression: Option<ActionBooleanValue>,
        action_path: String,
    },
    CompositeEnd {
        step_id: String,
    },
    ContinueOnError {
        step_id: String,
        value: ActionBooleanValue,
    },
    LocalAction {
        step_id: String,
        invocation: LocalActionInvocation,
        display_name: String,
        condition: Option<String>,
        continue_on_error: bool,
    },
    Docker {
        step_id: String,
        display_name: String,
        invocation: DockerActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
    },
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
    let normalized_contents = normalize_runner_yaml_numbers(contents)
        .map_err(anyhow::Error::msg)
        .context("normalize Runner action-manifest scalar values")?;
    validate_runner_action_yaml_syntax(&normalized_contents)?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(&normalized_contents).context("parse action metadata")?;
    let mut value = normalize_runner_action_core_tags(value)?;
    validate_runner_manifest_schema(&value)?;
    validate_runner_action_template_memory(&value)?;
    normalize_runner_action_manifest_scalars(&mut value)?;
    ActionMetadata::deserialize(&value).context("parse action metadata")
}

/// ActionManifestManager reads manifests with Runner's YamlObjectReader, which
/// rejects anchors, aliases, collection keys, duplicate mapping keys, and
/// unsupported tags before schema validation. Serde YAML resolves or
/// normalizes some of those forms, so check the CST and streaming events first.
fn validate_runner_action_yaml_syntax(source: &str) -> Result<()> {
    let document = serde_yaml::cst::parse_document(source).context("parse action metadata YAML")?;
    if !document.anchors().is_empty() || !document.aliases().is_empty() {
        bail!("action metadata anchors and aliases are not supported by actions/runner");
    }
    if contains_runner_collection_mapping_key(document.syntax()) {
        bail!("action metadata mapping keys must be YAML scalars");
    }
    validate_runner_action_yaml_tags(source, document.syntax())?;
    let source = normalize_runner_boolean_mapping_key_tags(source, document.syntax())?;
    serde_yaml::from_str::<RunnerYamlValidation>(&source)
        .context("validate Runner action metadata mapping keys and limits")?;
    Ok(())
}

fn normalize_runner_boolean_mapping_key_tags(
    source: &str,
    root: &serde_yaml::cst::GreenNode,
) -> Result<String> {
    fn visit(
        source: &str,
        node: &serde_yaml::cst::GreenNode,
        offset: usize,
        tag_handles: &mut BTreeMap<String, String>,
        removals: &mut Vec<(usize, usize)>,
    ) -> Result<()> {
        use serde_yaml::cst::{GreenChild, SyntaxKind};

        let children = node.children().collect::<Vec<_>>();
        let mut in_key = matches!(
            node.kind(),
            SyntaxKind::MappingEntry | SyntaxKind::FlowMapping
        );
        let mut offset = offset;
        for child in children {
            match child {
                GreenChild::Token { kind, len } => {
                    let end = offset + *len as usize;
                    let text = &source[offset..end];
                    match kind {
                        SyntaxKind::Directive => {
                            let mut parts = text.split_whitespace();
                            if parts.next() == Some("%TAG")
                                && let (Some(handle), Some(prefix)) = (parts.next(), parts.next())
                            {
                                tag_handles.insert(handle.to_owned(), prefix.to_owned());
                            }
                        }
                        SyntaxKind::TagMark if in_key => {
                            if expand_runner_action_tag(text, tag_handles).as_deref()
                                == Some("tag:yaml.org,2002:bool")
                            {
                                removals.push((offset, end));
                            }
                        }
                        SyntaxKind::ColonIndicator
                            if matches!(
                                node.kind(),
                                SyntaxKind::MappingEntry | SyntaxKind::FlowMapping
                            ) =>
                        {
                            in_key = false;
                        }
                        SyntaxKind::Comma if node.kind() == SyntaxKind::FlowMapping => {
                            in_key = true;
                        }
                        _ => {}
                    }
                }
                GreenChild::Node(child) => {
                    visit(source, child, offset, tag_handles, removals)?;
                }
            }
            offset += child.text_len();
        }
        Ok(())
    }

    let mut tag_handles = BTreeMap::from([
        ("!".to_owned(), "!".to_owned()),
        ("!!".to_owned(), "tag:yaml.org,2002:".to_owned()),
    ]);
    let mut removals = Vec::new();
    visit(source, root, 0, &mut tag_handles, &mut removals)?;
    if removals.is_empty() {
        return Ok(source.to_owned());
    }
    let mut normalized = source.to_owned();
    for (start, end) in removals.into_iter().rev() {
        normalized.replace_range(start..end, "");
    }
    Ok(normalized)
}

fn validate_runner_action_yaml_tags(source: &str, root: &serde_yaml::cst::GreenNode) -> Result<()> {
    fn visit(
        source: &str,
        node: &serde_yaml::cst::GreenNode,
        offset: usize,
        tag_handles: &mut BTreeMap<String, String>,
    ) -> Result<()> {
        use serde_yaml::cst::{GreenChild, SyntaxKind};

        let children = node.children().collect::<Vec<_>>();
        let mut offset = offset;
        for (index, child) in children.iter().enumerate() {
            match child {
                GreenChild::Token { kind, len } => {
                    let end = offset + *len as usize;
                    let text = &source[offset..end];
                    match kind {
                        SyntaxKind::Directive => {
                            let mut parts = text.split_whitespace();
                            if parts.next() == Some("%TAG")
                                && let (Some(handle), Some(prefix)) = (parts.next(), parts.next())
                            {
                                tag_handles.insert(handle.to_owned(), prefix.to_owned());
                            }
                        }
                        SyntaxKind::TagMark => {
                            let tag =
                                expand_runner_action_tag(text, tag_handles).ok_or_else(|| {
                                    anyhow::anyhow!("invalid action metadata YAML tag '{text}'")
                                })?;
                            let target = children[index + 1..]
                                .iter()
                                .find(|child| !is_runner_yaml_trivia(child));
                            let tags_collection =
                                target.is_some_and(|child| matches!(child, GreenChild::Node(_)));
                            if !tags_collection {
                                if !is_runner_supported_action_scalar_tag(&tag) {
                                    bail!("action metadata YAML tag '{text}' is not supported by actions/runner");
                                }
                                if tag != "tag:yaml.org,2002:str"
                                    && tag != "!velnor-runner-number"
                                    && !target.is_some_and(|child| {
                                        matches!(
                                            child,
                                            GreenChild::Token {
                                                kind: SyntaxKind::PlainScalar,
                                                ..
                                            }
                                        )
                                    })
                                {
                                    bail!("action metadata numeric, boolean, and null tags require plain scalar style");
                                }
                            }
                        }
                        _ => {}
                    }
                }
                GreenChild::Node(child) => visit(source, child, offset, tag_handles)?,
            }
            offset += child.text_len();
        }
        Ok(())
    }

    let mut tag_handles = BTreeMap::from([
        ("!".to_owned(), "!".to_owned()),
        ("!!".to_owned(), "tag:yaml.org,2002:".to_owned()),
    ]);
    visit(source, root, 0, &mut tag_handles)
}

fn is_runner_yaml_trivia(child: &serde_yaml::cst::GreenChild) -> bool {
    matches!(
        child,
        serde_yaml::cst::GreenChild::Token {
            kind: serde_yaml::cst::SyntaxKind::Whitespace
                | serde_yaml::cst::SyntaxKind::Newline
                | serde_yaml::cst::SyntaxKind::Comment,
            ..
        }
    )
}

fn expand_runner_action_tag(tag: &str, tag_handles: &BTreeMap<String, String>) -> Option<String> {
    if let Some(tag) = tag.strip_prefix("!<").and_then(|tag| tag.strip_suffix('>')) {
        return Some(tag.to_owned());
    }

    let (handle, suffix) = if let Some(suffix) = tag.strip_prefix("!!") {
        ("!!", suffix)
    } else {
        let rest = tag.strip_prefix('!')?;
        if let Some(handle_end) = rest.find('!') {
            let handle_end = handle_end + 1;
            (&tag[..=handle_end], &tag[handle_end + 1..])
        } else {
            ("!", rest)
        }
    };

    tag_handles
        .get(handle)
        .map(|prefix| format!("{prefix}{suffix}"))
}

fn is_runner_supported_action_scalar_tag(tag: &str) -> bool {
    matches!(
        tag,
        "tag:yaml.org,2002:str"
            | "tag:yaml.org,2002:int"
            | "tag:yaml.org,2002:float"
            | "tag:yaml.org,2002:bool"
            | "tag:yaml.org,2002:null"
            | "!velnor-runner-number"
    )
}

fn normalize_runner_action_core_tags(value: serde_yaml::Value) -> Result<serde_yaml::Value> {
    match value {
        serde_yaml::Value::Tagged(tagged) => {
            let tag = tagged.tag().as_str().to_owned();
            if tag == "!velnor-runner-number" {
                if tagged.value().as_str().is_none()
                    || normalized_runner_number(&serde_yaml::Value::Tagged(tagged.clone()))
                        .is_none()
                {
                    bail!("invalid internal Runner numeric tag");
                }
                return Ok(serde_yaml::Value::Tagged(tagged));
            }
            let value = normalize_runner_action_core_tags(tagged.value().clone())?;
            if matches!(
                value,
                serde_yaml::Value::Mapping(_) | serde_yaml::Value::Sequence(_)
            ) {
                // Runner's YamlObjectReader ignores tags on collection-start events.
                return Ok(value);
            }

            match tag.as_str() {
                "tag:yaml.org,2002:str" => Ok(serde_yaml::Value::String(
                    action_manifest_scalar_string(&value).ok_or_else(|| {
                        anyhow::anyhow!("explicit YAML string tag must apply to a scalar")
                    })?,
                )),
                "tag:yaml.org,2002:bool" => {
                    match action_manifest_scalar_string(&value).as_deref() {
                        Some("true" | "True" | "TRUE") => Ok(serde_yaml::Value::Bool(true)),
                        Some("false" | "False" | "FALSE") => Ok(serde_yaml::Value::Bool(false)),
                        _ => bail!("invalid explicitly tagged YAML boolean"),
                    }
                }
                "tag:yaml.org,2002:null" => {
                    match action_manifest_scalar_string(&value).as_deref() {
                        Some("" | "null" | "Null" | "NULL" | "~") => Ok(serde_yaml::Value::Null),
                        _ => bail!("invalid explicitly tagged YAML null"),
                    }
                }
                "tag:yaml.org,2002:int" | "tag:yaml.org,2002:float" => {
                    bail!("explicitly tagged YAML number was not normalized by actions/runner")
                }
                "tag:yaml.org,2002:seq" | "tag:yaml.org,2002:map" => {
                    bail!("explicit YAML collection tag must apply to a collection")
                }
                _ => bail!("unsupported scalar YAML tag '{tag}' in action metadata"),
            }
        }
        serde_yaml::Value::Sequence(values) => values
            .into_iter()
            .map(normalize_runner_action_core_tags)
            .collect::<Result<Vec<_>>>()
            .map(serde_yaml::Value::Sequence),
        serde_yaml::Value::Mapping(values) => {
            let mut normalized = serde_yaml::Mapping::new();
            for (key, value) in values {
                normalized.insert(key, normalize_runner_action_core_tags(value)?);
            }
            Ok(serde_yaml::Value::Mapping(normalized))
        }
        value => Ok(value),
    }
}

struct RunnerYamlValidation<const MAX_EVENTS: usize = MAX_ACTION_METADATA_EVENTS>(usize);

impl<'de, const MAX_EVENTS: usize> Deserialize<'de> for RunnerYamlValidation<MAX_EVENTS> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        RunnerYamlValidationSeed::<MAX_EVENTS> { depth: 0 }
            .deserialize(deserializer)
            .map(Self)
    }
}

struct RunnerYamlValidationSeed<const MAX_EVENTS: usize> {
    depth: usize,
}

impl<'de, const MAX_EVENTS: usize> DeserializeSeed<'de> for RunnerYamlValidationSeed<MAX_EVENTS> {
    type Value = usize;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_newtype_struct(
            "RunnerYamlValidation",
            RunnerYamlValidationWrapperVisitor::<MAX_EVENTS> { depth: self.depth },
        )
    }
}

struct RunnerYamlValidationWrapperVisitor<const MAX_EVENTS: usize> {
    depth: usize,
}

impl<'de, const MAX_EVENTS: usize> Visitor<'de> for RunnerYamlValidationWrapperVisitor<MAX_EVENTS> {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Runner action metadata YAML value")
    }

    // noyalib surfaces custom tags to newtype visitors as a one-entry map.
    // The CST preflight already validates each tag; recurse through the value
    // without counting the synthetic wrapper as an additional YAML event.
    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let _: String = map
            .next_key()?
            .ok_or_else(|| <M::Error as serde::de::Error>::custom("tag map is empty"))?;
        let events =
            map.next_value_seed(RunnerYamlValidationSeed::<MAX_EVENTS> { depth: self.depth })?;
        if map.next_key::<serde_yaml::Value>()?.is_some() {
            return Err(<M::Error as serde::de::Error>::custom(
                "tag map has more than one value",
            ));
        }
        Ok(events)
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(RunnerYamlValueVisitor::<MAX_EVENTS> { depth: self.depth })
    }
}

struct RunnerYamlValueVisitor<const MAX_EVENTS: usize> {
    depth: usize,
}

impl<'de, const MAX_EVENTS: usize> Visitor<'de> for RunnerYamlValueVisitor<MAX_EVENTS> {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Runner action metadata YAML value")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let nested_depth = self.depth + 1;
        if nested_depth > MAX_METADATA_PARSE_NESTING {
            return Err(<M::Error as serde::de::Error>::custom(format!(
                "action metadata nesting exceeds the Runner limit of {MAX_METADATA_PARSE_NESTING}"
            )));
        }

        let mut events = 1;
        let mut seen = BTreeSet::new();
        while let Some(key) = map.next_key::<serde_yaml::Value>()? {
            let key = action_manifest_scalar_string(&key).ok_or_else(|| {
                <M::Error as serde::de::Error>::custom(
                    "action metadata mapping keys must be YAML scalars",
                )
            })?;
            let canonical = runner_ordinal_ignore_case_key(&key);
            if !seen.insert(canonical) {
                return Err(<M::Error as serde::de::Error>::custom(format!(
                    "action metadata mapping contains a duplicate key '{key}' ignoring case"
                )));
            }
            let child_events = map.next_value_seed(RunnerYamlValidationSeed::<MAX_EVENTS> {
                depth: nested_depth,
            })?;
            events = checked_runner_yaml_event_count::<MAX_EVENTS, M::Error>(events, child_events)?;
        }
        Ok(events)
    }

    fn visit_seq<S>(self, mut sequence: S) -> std::result::Result<Self::Value, S::Error>
    where
        S: SeqAccess<'de>,
    {
        let nested_depth = self.depth + 1;
        if nested_depth > MAX_METADATA_PARSE_NESTING {
            return Err(<S::Error as serde::de::Error>::custom(format!(
                "action metadata nesting exceeds the Runner limit of {MAX_METADATA_PARSE_NESTING}"
            )));
        }

        let mut events = 1;
        while let Some(child_events) =
            sequence.next_element_seed(RunnerYamlValidationSeed::<MAX_EVENTS> {
                depth: nested_depth,
            })?
        {
            events = checked_runner_yaml_event_count::<MAX_EVENTS, S::Error>(events, child_events)?;
        }
        Ok(events)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(1)
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(1)
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(1)
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(1)
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(1)
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(1)
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(1)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(1)
    }
}

fn checked_runner_yaml_event_count<const MAX_EVENTS: usize, E>(
    events: usize,
    child_events: usize,
) -> Result<usize, E>
where
    E: serde::de::Error,
{
    let events = events.saturating_add(child_events);
    if events > MAX_EVENTS {
        return Err(E::custom(format!(
            "action metadata exceeds the Runner limit of {MAX_EVENTS} YAML events"
        )));
    }
    Ok(events)
}

fn contains_runner_collection_mapping_key(node: &serde_yaml::cst::GreenNode) -> bool {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    let collection_key = match node.kind() {
        SyntaxKind::MappingEntry => {
            let mut in_key = true;
            node.children().any(|child| match child {
                GreenChild::Token {
                    kind: SyntaxKind::ColonIndicator,
                    ..
                } => {
                    in_key = false;
                    false
                }
                GreenChild::Node(child) if in_key => {
                    matches!(
                        child.kind(),
                        SyntaxKind::BlockMapping
                            | SyntaxKind::BlockSequence
                            | SyntaxKind::FlowMapping
                            | SyntaxKind::FlowSequence
                    )
                }
                _ => false,
            })
        }
        SyntaxKind::FlowMapping => {
            let mut in_key = true;
            node.children().any(|child| match child {
                GreenChild::Token {
                    kind: SyntaxKind::ColonIndicator,
                    ..
                } => {
                    in_key = false;
                    false
                }
                GreenChild::Token {
                    kind: SyntaxKind::Comma,
                    ..
                } => {
                    in_key = true;
                    false
                }
                GreenChild::Node(child) if in_key => {
                    matches!(
                        child.kind(),
                        SyntaxKind::BlockMapping
                            | SyntaxKind::BlockSequence
                            | SyntaxKind::FlowMapping
                            | SyntaxKind::FlowSequence
                    )
                }
                _ => false,
            })
        }
        _ => false,
    };
    collection_key
        || node.children().any(|child| match child {
            GreenChild::Node(child) => contains_runner_collection_mapping_key(child),
            GreenChild::Token { .. } => false,
        })
}

fn validate_runner_manifest_schema(value: &serde_yaml::Value) -> Result<()> {
    let root = value
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("action metadata must be a mapping"))?;
    validate_manifest_mapping_keys(root, "action metadata", true)?;
    for field in ["name", "description"] {
        if let Some(value) = root.get(field) {
            validate_manifest_string(value, field, false)?;
            validate_manifest_no_basic_expressions(value, field)?;
        }
    }
    for (field, value) in root {
        if !matches!(
            field.as_str(),
            "name" | "description" | "inputs" | "runs" | "outputs"
        ) {
            validate_manifest_no_basic_expressions(value, &format!("action metadata.{field}"))?;
        }
    }
    if let Some(inputs) = root.get("inputs") {
        let inputs = inputs
            .as_mapping()
            .ok_or_else(|| anyhow::anyhow!("action metadata inputs must be a mapping"))?;
        validate_manifest_mapping_keys(inputs, "inputs", true)?;
        for (name, definition) in inputs {
            let definition = definition.as_mapping().ok_or_else(|| {
                anyhow::anyhow!("action metadata inputs.{name} must be a mapping")
            })?;
            validate_manifest_mapping_keys(definition, &format!("inputs.{name}"), true)?;
            let defaults = definition
                .keys()
                .filter(|key| key.eq_ignore_ascii_case("default"))
                .collect::<Vec<_>>();
            if defaults.len() > 1 {
                bail!("action metadata inputs.{name} has duplicate default fields ignoring case");
            }
            if let Some(default) = defaults.first()
                && let Some(value) = definition.get(default.as_str())
            {
                let field = format!("inputs.{name}.default");
                validate_manifest_string(value, &field, false)?;
                validate_manifest_templates(value, &field, ActionExpressionContext::InputDefault)?;
            }
            for (key, value) in definition {
                if key.eq_ignore_ascii_case("default") {
                    continue;
                }
                let field = format!("inputs.{name}.{key}");
                if key.eq_ignore_ascii_case("deprecationMessage") && !value.is_string() {
                    bail!("action metadata inputs.{name}.deprecationMessage must be a string");
                }
                validate_manifest_no_basic_expressions(value, &field)?;
            }
        }
    }
    if let Some(outputs) = root.get("outputs") {
        let outputs = outputs
            .as_mapping()
            .ok_or_else(|| anyhow::anyhow!("action metadata outputs must be a mapping"))?;
        validate_manifest_mapping_keys(outputs, "outputs", true)?;
        for (name, definition) in outputs {
            let definition = definition.as_mapping().ok_or_else(|| {
                anyhow::anyhow!("action metadata outputs.{name} must be a mapping")
            })?;
            validate_manifest_mapping_keys(definition, &format!("outputs.{name}"), true)?;
            for (key, value) in definition {
                if !matches!(key.as_str(), "description" | "value") {
                    bail!(
                        "action metadata outputs.{name}.{key} is not supported by actions/runner"
                    );
                }
                let field = format!("outputs.{name}.{key}");
                validate_manifest_string(value, &field, false)?;
                if key == "value" {
                    validate_manifest_templates(
                        value,
                        &field,
                        ActionExpressionContext::OutputValue,
                    )?;
                } else {
                    validate_manifest_no_basic_expressions(value, &field)?;
                }
            }
        }
    }
    let runs = root
        .get("runs")
        .and_then(serde_yaml::Value::as_mapping)
        .ok_or_else(|| anyhow::anyhow!("action metadata runs must be a mapping"))?;
    validate_manifest_mapping_keys(runs, "runs", true)?;
    if let Some(plugin) = runs.get("plugin") {
        validate_manifest_runtime_keys(runs, "runs", &["plugin"])?;
        validate_manifest_string(plugin, "runs.plugin", true)?;
        validate_manifest_no_basic_expressions(plugin, "runs.plugin")?;
        bail!("plugin action runtime is unsupported by the Velnor executor");
    }
    let using_value = runs
        .get("using")
        .ok_or_else(|| anyhow::anyhow!("action metadata runs.using is required"))?;
    let using = validate_manifest_string(using_value, "runs.using", true)?;
    validate_manifest_no_basic_expressions(using_value, "runs.using")?;
    match using.to_ascii_lowercase().as_str() {
        "composite" => {
            validate_manifest_runtime_keys(runs, "runs", &["using", "steps"])?;
            let steps = runs
                .get("steps")
                .ok_or_else(|| {
                    anyhow::anyhow!("action metadata runs.steps is required for composite actions")
                })?
                .as_sequence()
                .ok_or_else(|| anyhow::anyhow!("action metadata runs.steps must be a sequence"))?;
            for (index, step) in steps.iter().enumerate() {
                validate_manifest_composite_step(step, index)?;
            }
        }
        "docker" => {
            validate_manifest_runtime_keys(
                runs,
                "runs",
                &[
                    "using",
                    "image",
                    "entrypoint",
                    "args",
                    "env",
                    "pre-entrypoint",
                    "pre-if",
                    "post-entrypoint",
                    "post-if",
                ],
            )?;
            for field in [
                "image",
                "entrypoint",
                "pre-entrypoint",
                "pre-if",
                "post-entrypoint",
                "post-if",
            ] {
                if let Some(value) = runs.get(field) {
                    validate_manifest_string(value, &format!("runs.{field}"), true)?;
                    validate_manifest_no_basic_expressions(value, &format!("runs.{field}"))?;
                }
            }
            if let Some(image) = runs.get("image") {
                validate_manifest_string(image, "runs.image", true)?;
                validate_manifest_no_basic_expressions(image, "runs.image")?;
            } else {
                bail!("action metadata runs.image is required for Docker actions");
            }
            if let Some(args) = runs.get("args") {
                let args = args.as_sequence().ok_or_else(|| {
                    anyhow::anyhow!("action metadata runs.args must be a sequence")
                })?;
                for (index, value) in args.iter().enumerate() {
                    let field = format!("runs.args[{index}]");
                    validate_manifest_string(value, &field, false)?;
                    validate_manifest_templates(
                        value,
                        &field,
                        ActionExpressionContext::ContainerRun,
                    )?;
                }
            }
            if let Some(env) = runs.get("env") {
                validate_manifest_string_map(
                    env,
                    "runs.env",
                    ActionExpressionContext::ContainerRun,
                )?;
            }
        }
        "node12" | "node16" | "node20" | "node24" => {
            validate_manifest_runtime_keys(
                runs,
                "runs",
                &["using", "main", "pre", "pre-if", "post", "post-if"],
            )?;
            for field in ["main", "pre", "pre-if", "post", "post-if"] {
                if let Some(value) = runs.get(field) {
                    validate_manifest_string(value, &format!("runs.{field}"), true)?;
                    validate_manifest_no_basic_expressions(value, &format!("runs.{field}"))?;
                }
            }
            if let Some(main) = runs.get("main") {
                validate_manifest_string(main, "runs.main", true)?;
                validate_manifest_no_basic_expressions(main, "runs.main")?;
            } else {
                bail!("action metadata runs.main is required for Node actions");
            }
        }
        _ => bail!("unsupported action runtime `{using}`"),
    }
    Ok(())
}

fn validate_manifest_composite_step(value: &serde_yaml::Value, index: usize) -> Result<()> {
    let field = format!("runs.steps[{index}]");
    let step = value
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("action metadata {field} must be a mapping"))?;
    validate_manifest_mapping_keys(step, &field, true)?;
    let run = step.get("run");
    let uses = step.get("uses");
    if run.is_some() == uses.is_some() {
        bail!("action metadata {field} must declare exactly one of run or uses");
    }
    let allowed = if run.is_some() {
        &[
            "run",
            "shell",
            "name",
            "id",
            "if",
            "env",
            "continue-on-error",
            "working-directory",
        ][..]
    } else {
        &[
            "uses",
            "name",
            "id",
            "if",
            "continue-on-error",
            "with",
            "env",
        ][..]
    };
    validate_manifest_runtime_keys(step, &field, allowed)?;
    if let Some(run) = run {
        let field_name = format!("{field}.run");
        validate_manifest_string(run, &field_name, false)?;
        validate_manifest_templates(run, &field_name, ActionExpressionContext::CompositeString)?;
        let shell = step.get("shell").ok_or_else(|| {
            anyhow::anyhow!("action metadata {field}.shell is required for run steps")
        })?;
        let field_name = format!("{field}.shell");
        validate_manifest_string(shell, &field_name, false)?;
        validate_manifest_templates(shell, &field_name, ActionExpressionContext::CompositeString)?;
    } else if let Some(uses) = uses {
        validate_manifest_string(uses, &format!("{field}.uses"), true)?;
        validate_manifest_no_basic_expressions(uses, &format!("{field}.uses"))?;
    }
    for key in ["name", "working-directory"] {
        if let Some(value) = step.get(key) {
            let field_name = format!("{field}.{key}");
            validate_manifest_string(value, &field_name, false)?;
            validate_manifest_templates(
                value,
                &field_name,
                ActionExpressionContext::CompositeString,
            )?;
        }
    }
    if let Some(value) = step.get("if") {
        let field_name = format!("{field}.if");
        validate_manifest_string(value, &field_name, false)?;
        validate_manifest_templates(value, &field_name, ActionExpressionContext::CompositeIf)?;
    }
    if let Some(id) = step.get("id") {
        validate_manifest_string(id, &format!("{field}.id"), true)?;
        validate_manifest_no_basic_expressions(id, &format!("{field}.id"))?;
    }
    if let Some(value) = step.get("continue-on-error") {
        validate_manifest_boolean_expression(value, ActionExpressionContext::CompositeBoolean)
            .with_context(|| format!("action metadata {field}.continue-on-error is invalid"))?;
    }
    for key in ["with", "env"] {
        if let Some(value) = step.get(key) {
            validate_manifest_string_map(
                value,
                &format!("{field}.{key}"),
                ActionExpressionContext::CompositeString,
            )?;
        }
    }
    Ok(())
}

fn validate_manifest_mapping_keys(
    mapping: &serde_yaml::Mapping,
    field: &str,
    nonempty: bool,
) -> Result<()> {
    validate_manifest_mapping_key_shape(mapping, field, nonempty)?;
    if mapping.keys().any(|key| key.contains("${{")) {
        bail!("action metadata {field} mapping keys cannot contain template expressions");
    }
    Ok(())
}

fn validate_manifest_expression_mapping_keys(
    mapping: &serde_yaml::Mapping,
    field: &str,
    context: ActionExpressionContext,
) -> Result<()> {
    validate_manifest_mapping_key_shape(mapping, field, true)?;
    for key in mapping.keys() {
        validate_template_expressions(key, context).map_err(|_| {
            anyhow::anyhow!(
                "action metadata {field} key has an invalid expression context or syntax"
            )
        })?;
    }
    Ok(())
}

fn validate_manifest_mapping_key_shape(
    mapping: &serde_yaml::Mapping,
    field: &str,
    nonempty: bool,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for key in mapping.keys() {
        if nonempty && key.is_empty() {
            bail!("action metadata {field} keys must not be empty");
        }
        if !seen.insert(runner_ordinal_ignore_case_key(key)) {
            bail!("action metadata {field} keys must be unique ignoring case");
        }
    }
    Ok(())
}

fn validate_manifest_runtime_keys(
    mapping: &serde_yaml::Mapping,
    field: &str,
    allowed: &[&str],
) -> Result<()> {
    for key in mapping.keys() {
        if !allowed.contains(&key.as_str()) {
            bail!("action metadata {field}.{key} is not valid for this action shape");
        }
    }
    Ok(())
}

fn validate_manifest_string_map(
    value: &serde_yaml::Value,
    field: &str,
    context: ActionExpressionContext,
) -> Result<()> {
    let mapping = value
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("action metadata {field} must be a mapping"))?;
    validate_manifest_expression_mapping_keys(mapping, field, context)?;
    for (key, value) in mapping {
        let field_name = format!("{field}.{key}");
        validate_manifest_string(value, &field_name, false)?;
        validate_manifest_templates(value, &field_name, context)?;
    }
    Ok(())
}

fn validate_manifest_string(
    value: &serde_yaml::Value,
    field: &str,
    nonempty: bool,
) -> Result<String> {
    let scalar = match value {
        serde_yaml::Value::String(value) => value.clone(),
        serde_yaml::Value::Number(value) => velnor_expression::value::format_number(value.as_f64()),
        serde_yaml::Value::Bool(value) => value.to_string(),
        serde_yaml::Value::Null => String::new(),
        serde_yaml::Value::Tagged(_) if normalized_runner_number(value).is_some() => {
            normalized_runner_number(value)
                .ok_or_else(|| anyhow::anyhow!("action metadata {field} number is invalid"))?
                .to_owned()
        }
        _ => bail!("action metadata {field} must be a scalar string"),
    };
    if nonempty && scalar.is_empty() {
        bail!("action metadata {field} must not be empty");
    }
    Ok(scalar)
}

fn validate_manifest_no_basic_expressions(value: &serde_yaml::Value, field: &str) -> Result<()> {
    match value {
        serde_yaml::Value::String(value) => {
            validate_manifest_no_basic_expression_text(value, field)?
        }
        serde_yaml::Value::Sequence(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_manifest_no_basic_expressions(value, &format!("{field}[{index}]"))?;
            }
        }
        serde_yaml::Value::Mapping(values) => {
            for (key, value) in values {
                validate_manifest_no_basic_expression_text(key, &format!("{field} key"))?;
                validate_manifest_no_basic_expressions(value, field)?;
            }
        }
        serde_yaml::Value::Tagged(tagged) => {
            validate_manifest_no_basic_expressions(tagged.value(), field)?;
        }
        serde_yaml::Value::Null | serde_yaml::Value::Bool(_) | serde_yaml::Value::Number(_) => {}
    }
    Ok(())
}

/// TemplateReader turns a single expression whose only content is a quoted
/// string into a literal before checking AllowedContext. Other interpolations
/// become BasicExpression tokens and fail for action fields with no context.
fn runner_expression_is_string_literal(value: &str) -> bool {
    let Some(expression) = value
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
    else {
        return false;
    };

    let mut in_string = false;
    for character in expression.trim().chars() {
        if character == '\'' {
            in_string = !in_string;
        } else if !in_string {
            return false;
        }
    }
    !in_string
}

fn validate_manifest_no_basic_expression_text(value: &str, field: &str) -> Result<()> {
    if value.contains("${{") && !runner_expression_is_string_literal(value) {
        bail!("action metadata {field} cannot contain a template expression");
    }
    Ok(())
}

fn normalize_runner_action_manifest_scalars(value: &mut serde_yaml::Value) -> Result<()> {
    let Some(root) = value.as_mapping_mut() else {
        return Ok(());
    };
    for key in ["name", "description"] {
        normalize_manifest_scalar_field(root, key)?;
    }
    if let Some(inputs) = root
        .get_mut("inputs")
        .and_then(serde_yaml::Value::as_mapping_mut)
    {
        for definition in inputs
            .values_mut()
            .filter_map(serde_yaml::Value::as_mapping_mut)
        {
            if let Some(source_key) = definition
                .keys()
                .find(|key| key.eq_ignore_ascii_case("default"))
                .cloned()
            {
                let mut value = definition
                    .remove(&source_key)
                    .context("action input default disappeared during normalization")?;
                value = serde_yaml::Value::String(validate_manifest_string(
                    &value,
                    "inputs.default",
                    false,
                )?);
                definition.insert("default".to_owned(), value);
            }
        }
    }
    if let Some(outputs) = root
        .get_mut("outputs")
        .and_then(serde_yaml::Value::as_mapping_mut)
    {
        for definition in outputs
            .values_mut()
            .filter_map(serde_yaml::Value::as_mapping_mut)
        {
            for key in ["description", "value"] {
                normalize_manifest_scalar_field(definition, key)?;
            }
        }
    }
    if let Some(runs) = root
        .get_mut("runs")
        .and_then(serde_yaml::Value::as_mapping_mut)
    {
        for key in [
            "using",
            "main",
            "pre",
            "pre-if",
            "post",
            "post-if",
            "image",
            "entrypoint",
            "pre-entrypoint",
            "post-entrypoint",
        ] {
            normalize_manifest_scalar_field(runs, key)?;
        }
        if let Some(args) = runs
            .get_mut("args")
            .and_then(serde_yaml::Value::as_sequence_mut)
        {
            for value in args {
                *value =
                    serde_yaml::Value::String(validate_manifest_string(value, "runs.args", false)?);
            }
        }
        if let Some(env) = runs
            .get_mut("env")
            .and_then(serde_yaml::Value::as_mapping_mut)
        {
            normalize_manifest_scalar_map(env)?;
        }
        if let Some(steps) = runs
            .get_mut("steps")
            .and_then(serde_yaml::Value::as_sequence_mut)
        {
            for step in steps {
                let Some(step) = step.as_mapping_mut() else {
                    continue;
                };
                for key in [
                    "id",
                    "name",
                    "shell",
                    "run",
                    "uses",
                    "if",
                    "working-directory",
                ] {
                    normalize_manifest_scalar_field(step, key)?;
                }
                for key in ["with", "env"] {
                    if let Some(mapping) = step
                        .get_mut(key)
                        .and_then(serde_yaml::Value::as_mapping_mut)
                    {
                        normalize_manifest_scalar_map(mapping)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn normalize_manifest_scalar_field(mapping: &mut serde_yaml::Mapping, field: &str) -> Result<()> {
    if let Some(value) = mapping.get_mut(field) {
        *value = serde_yaml::Value::String(validate_manifest_string(value, field, false)?);
    }
    Ok(())
}

fn normalize_manifest_scalar_map(mapping: &mut serde_yaml::Mapping) -> Result<()> {
    for (key, value) in mapping.iter_mut() {
        *value = serde_yaml::Value::String(validate_manifest_string(value, key, false)?);
    }
    Ok(())
}

fn validate_manifest_boolean_expression(
    value: &serde_yaml::Value,
    context: ActionExpressionContext,
) -> Result<()> {
    if value.is_bool() {
        return Ok(());
    }
    let expression = validate_manifest_string(value, "boolean expression", false)?;
    match runner_template_scalar(&expression, context)? {
        RunnerTemplateScalar::Expression(_) => Ok(()),
        RunnerTemplateScalar::Literal(_) => bail!("not a boolean or expression"),
    }
}

fn validate_manifest_templates(
    value: &serde_yaml::Value,
    field: &str,
    context: ActionExpressionContext,
) -> Result<()> {
    let value = validate_manifest_string(value, field, false)?;
    runner_template_scalar(&value, context)
        .map(|_| ())
        .map_err(|_| {
            anyhow::anyhow!("action metadata {field} has an invalid expression context or syntax")
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RunnerTemplateScalar {
    Literal(String),
    Expression(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RunnerInlineSegment {
    Literal(String),
    Expression(String),
}

fn runner_template_scalar(
    value: &str,
    context: ActionExpressionContext,
) -> Result<RunnerTemplateScalar> {
    let mut segments = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = value[cursor..].find("${{") {
        let start = cursor + relative_start;
        if start > cursor {
            push_runner_inline_literal(&mut segments, &value[cursor..start]);
        }
        let expression_start = start + 3;
        let expression_end = runner_template_expression_end(value, expression_start)
            .ok_or_else(|| anyhow::anyhow!("unterminated template expression"))?;
        let expression = value[expression_start..expression_end].trim();
        if expression.is_empty()
            || expression.encode_utf16().count() > velnor_expression::MAX_LENGTH
        {
            bail!("invalid template expression length or syntax");
        }
        let wrapped = format!("${{{{ {expression} }}}}");
        validate_template_expressions(&wrapped, context)
            .map_err(|_| anyhow::anyhow!("invalid expression context or syntax"))?;
        segments.push(RunnerInlineSegment::Expression(expression.to_owned()));
        cursor = expression_end + 2;
    }
    if cursor < value.len() {
        push_runner_inline_literal(&mut segments, &value[cursor..]);
    }

    let expression_count = segments
        .iter()
        .filter(|segment| matches!(segment, RunnerInlineSegment::Expression(_)))
        .count();
    if expression_count == 0 {
        return Ok(RunnerTemplateScalar::Literal(value.to_owned()));
    }
    if segments.len() == 1 {
        let [RunnerInlineSegment::Expression(expression)] = segments.as_slice() else {
            bail!("invalid single-expression template segment");
        };
        if let Some(literal) = runner_expression_string_literal_value(expression) {
            return Ok(RunnerTemplateScalar::Literal(literal));
        }
        return Ok(RunnerTemplateScalar::Expression(expression.clone()));
    }

    let expression = runner_inline_format_expression(&segments);
    if expression.encode_utf16().count() > velnor_expression::MAX_LENGTH {
        bail!("generated inline format expression exceeds the Runner limit");
    }
    let wrapped = format!("${{{{ {expression} }}}}");
    validate_template_expressions(&wrapped, context)
        .map_err(|_| anyhow::anyhow!("generated inline format expression is invalid"))?;
    Ok(RunnerTemplateScalar::Expression(expression))
}

fn push_runner_inline_literal(segments: &mut Vec<RunnerInlineSegment>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(RunnerInlineSegment::Literal(previous)) = segments.last_mut() {
        previous.push_str(value);
    } else {
        segments.push(RunnerInlineSegment::Literal(value.to_owned()));
    }
}

fn runner_template_expression_end(value: &str, start: usize) -> Option<usize> {
    let mut in_string = false;
    let mut offset = start;
    while offset < value.len() {
        let character = value[offset..].chars().next()?;
        if character == '\'' {
            in_string = !in_string;
        } else if !in_string && value[offset..].starts_with("}}") {
            return Some(offset);
        }
        offset += character.len_utf8();
    }
    None
}

fn runner_expression_string_literal_value(expression: &str) -> Option<String> {
    if !runner_expression_is_string_literal(&format!("${{{{ {expression} }}}}")) {
        return None;
    }
    expression
        .trim()
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .map(|value| value.replace("''", "'"))
}

fn runner_inline_format_expression(segments: &[RunnerInlineSegment]) -> String {
    let mut format = String::new();
    let mut arguments = Vec::new();
    for segment in segments {
        match segment {
            RunnerInlineSegment::Literal(value) => {
                format.push_str(
                    &value
                        .replace('\'', "''")
                        .replace('{', "{{")
                        .replace('}', "}}"),
                );
            }
            RunnerInlineSegment::Expression(expression) => {
                let index = arguments.len();
                format.push_str(&format!("{{{index}}}"));
                arguments.push(expression.as_str());
            }
        }
    }

    let mut expression = format!("format('{format}'");
    for argument in arguments {
        expression.push_str(", ");
        expression.push_str(argument);
    }
    expression.push(')');
    expression
}

#[derive(Clone, Copy)]
enum RunnerActionManifestValueKind {
    Root,
    Inputs,
    InputDefinition,
    Outputs,
    OutputDefinition,
    Runs,
    CompositeSteps,
    CompositeStep,
    DockerArguments,
    StringMap(ActionExpressionContext),
    StringNoContext,
    StringTemplate(ActionExpressionContext),
    BooleanTemplate(ActionExpressionContext),
    Any,
}

fn validate_runner_action_template_memory(value: &serde_yaml::Value) -> Result<()> {
    runner_action_manifest_value_memory(value, RunnerActionManifestValueKind::Root)?;
    Ok(())
}

fn runner_action_manifest_value_memory(
    value: &serde_yaml::Value,
    kind: RunnerActionManifestValueKind,
) -> Result<usize> {
    const MIN_OBJECT_SIZE: usize = 24;
    match kind {
        RunnerActionManifestValueKind::StringNoContext => {
            let string = validate_manifest_string(value, "action metadata string", false)?;
            Ok(runner_no_context_string_memory(&string))
        }
        RunnerActionManifestValueKind::StringTemplate(context) => {
            let string = validate_manifest_string(value, "action metadata template", false)?;
            runner_template_string_memory(&string, context)
        }
        RunnerActionManifestValueKind::BooleanTemplate(context) => {
            if value.is_bool() {
                return Ok(MIN_OBJECT_SIZE);
            }
            let string = validate_manifest_string(value, "action metadata boolean", false)?;
            match runner_template_scalar(&string, context)? {
                RunnerTemplateScalar::Expression(expression) => {
                    Ok(runner_expression_token_memory(&expression))
                }
                RunnerTemplateScalar::Literal(_) => {
                    bail!("action metadata boolean must be a YAML boolean or expression")
                }
            }
        }
        RunnerActionManifestValueKind::StringMap(context) => {
            let mapping = value
                .as_mapping()
                .ok_or_else(|| anyhow::anyhow!("action metadata string map must be a mapping"))?;
            runner_action_mapping_memory(
                mapping,
                |key| {
                    let key = runner_template_scalar(key, context)?;
                    Ok(match key {
                        RunnerTemplateScalar::Literal(value) => runner_string_token_memory(&value),
                        RunnerTemplateScalar::Expression(value) => {
                            runner_expression_token_memory(&value)
                        }
                    })
                },
                |_, _| RunnerActionManifestValueKind::StringTemplate(context),
            )
        }
        RunnerActionManifestValueKind::DockerArguments => {
            let values = value
                .as_sequence()
                .ok_or_else(|| anyhow::anyhow!("action metadata Docker args must be a sequence"))?;
            let mut bytes = MIN_OBJECT_SIZE;
            for value in values {
                bytes = runner_add_action_template_memory(
                    bytes,
                    runner_action_manifest_value_memory(
                        value,
                        RunnerActionManifestValueKind::StringTemplate(
                            ActionExpressionContext::ContainerRun,
                        ),
                    )?,
                )?;
            }
            Ok(bytes)
        }
        RunnerActionManifestValueKind::CompositeSteps => {
            let values = value.as_sequence().ok_or_else(|| {
                anyhow::anyhow!("action metadata composite steps must be a sequence")
            })?;
            let mut bytes = MIN_OBJECT_SIZE;
            for value in values {
                bytes = runner_add_action_template_memory(
                    bytes,
                    runner_action_manifest_value_memory(
                        value,
                        RunnerActionManifestValueKind::CompositeStep,
                    )?,
                )?;
            }
            Ok(bytes)
        }
        RunnerActionManifestValueKind::Any => match value {
            serde_yaml::Value::Null | serde_yaml::Value::Bool(_) | serde_yaml::Value::Number(_) => {
                Ok(MIN_OBJECT_SIZE)
            }
            serde_yaml::Value::Tagged(_) if normalized_runner_number(value).is_some() => {
                Ok(MIN_OBJECT_SIZE)
            }
            serde_yaml::Value::String(value) => Ok(runner_no_context_string_memory(value)),
            serde_yaml::Value::Sequence(values) => {
                let mut bytes = MIN_OBJECT_SIZE;
                for value in values {
                    bytes = runner_add_action_template_memory(
                        bytes,
                        runner_action_manifest_value_memory(
                            value,
                            RunnerActionManifestValueKind::Any,
                        )?,
                    )?;
                }
                Ok(bytes)
            }
            serde_yaml::Value::Mapping(mapping) => runner_action_mapping_memory(
                mapping,
                |key| Ok(runner_string_token_memory(key)),
                |_, _| RunnerActionManifestValueKind::Any,
            ),
            serde_yaml::Value::Tagged(_) => {
                bail!("unsupported tagged action metadata value after normalization")
            }
        },
        RunnerActionManifestValueKind::Root => {
            let mapping = value
                .as_mapping()
                .ok_or_else(|| anyhow::anyhow!("action metadata must be a mapping"))?;
            runner_action_mapping_memory(
                mapping,
                |key| Ok(runner_string_token_memory(key)),
                |key, _| match key {
                    "name" | "description" => RunnerActionManifestValueKind::StringNoContext,
                    "inputs" => RunnerActionManifestValueKind::Inputs,
                    "outputs" => RunnerActionManifestValueKind::Outputs,
                    "runs" => RunnerActionManifestValueKind::Runs,
                    _ => RunnerActionManifestValueKind::Any,
                },
            )
        }
        RunnerActionManifestValueKind::Inputs => {
            let mapping = value
                .as_mapping()
                .ok_or_else(|| anyhow::anyhow!("action metadata inputs must be a mapping"))?;
            runner_action_mapping_memory(
                mapping,
                |key| Ok(runner_string_token_memory(key)),
                |_, _| RunnerActionManifestValueKind::InputDefinition,
            )
        }
        RunnerActionManifestValueKind::InputDefinition => {
            let mapping = value.as_mapping().ok_or_else(|| {
                anyhow::anyhow!("action metadata input definition must be a mapping")
            })?;
            runner_action_mapping_memory(
                mapping,
                |key| Ok(runner_string_token_memory(key)),
                |key, _| {
                    if key.eq_ignore_ascii_case("default") {
                        RunnerActionManifestValueKind::StringTemplate(
                            ActionExpressionContext::InputDefault,
                        )
                    } else {
                        RunnerActionManifestValueKind::Any
                    }
                },
            )
        }
        RunnerActionManifestValueKind::Outputs => {
            let mapping = value
                .as_mapping()
                .ok_or_else(|| anyhow::anyhow!("action metadata outputs must be a mapping"))?;
            runner_action_mapping_memory(
                mapping,
                |key| Ok(runner_string_token_memory(key)),
                |_, _| RunnerActionManifestValueKind::OutputDefinition,
            )
        }
        RunnerActionManifestValueKind::OutputDefinition => {
            let mapping = value.as_mapping().ok_or_else(|| {
                anyhow::anyhow!("action metadata output definition must be a mapping")
            })?;
            runner_action_mapping_memory(
                mapping,
                |key| Ok(runner_string_token_memory(key)),
                |key, _| match key {
                    "description" => RunnerActionManifestValueKind::StringNoContext,
                    "value" => RunnerActionManifestValueKind::StringTemplate(
                        ActionExpressionContext::OutputValue,
                    ),
                    _ => RunnerActionManifestValueKind::Any,
                },
            )
        }
        RunnerActionManifestValueKind::Runs => {
            let mapping = value
                .as_mapping()
                .ok_or_else(|| anyhow::anyhow!("action metadata runs must be a mapping"))?;
            let using = mapping
                .get("using")
                .and_then(action_manifest_scalar_string)
                .ok_or_else(|| anyhow::anyhow!("action metadata runs.using must be a scalar"))?;
            let runtime = using.to_ascii_lowercase();
            runner_action_mapping_memory(
                mapping,
                |key| Ok(runner_string_token_memory(key)),
                |key, _| match (runtime.as_str(), key) {
                    ("composite", "steps") => RunnerActionManifestValueKind::CompositeSteps,
                    ("composite", "using") => RunnerActionManifestValueKind::StringNoContext,
                    ("docker", "args") => RunnerActionManifestValueKind::DockerArguments,
                    ("docker", "env") => RunnerActionManifestValueKind::StringMap(
                        ActionExpressionContext::ContainerRun,
                    ),
                    ("composite", _) => RunnerActionManifestValueKind::CompositeStep,
                    ("docker" | "node12" | "node16" | "node20" | "node24", _) => {
                        RunnerActionManifestValueKind::StringNoContext
                    }
                    _ => RunnerActionManifestValueKind::Any,
                },
            )
        }
        RunnerActionManifestValueKind::CompositeStep => {
            let mapping = value.as_mapping().ok_or_else(|| {
                anyhow::anyhow!("action metadata composite step must be a mapping")
            })?;
            runner_action_mapping_memory(
                mapping,
                |key| Ok(runner_string_token_memory(key)),
                |key, _| match key {
                    "name" | "run" | "shell" | "working-directory" => {
                        RunnerActionManifestValueKind::StringTemplate(
                            ActionExpressionContext::CompositeString,
                        )
                    }
                    "if" => RunnerActionManifestValueKind::StringTemplate(
                        ActionExpressionContext::CompositeIf,
                    ),
                    "id" | "uses" => RunnerActionManifestValueKind::StringNoContext,
                    "continue-on-error" => RunnerActionManifestValueKind::BooleanTemplate(
                        ActionExpressionContext::CompositeBoolean,
                    ),
                    "env" | "with" => RunnerActionManifestValueKind::StringMap(
                        ActionExpressionContext::CompositeString,
                    ),
                    _ => RunnerActionManifestValueKind::Any,
                },
            )
        }
    }
}

fn runner_action_mapping_memory(
    mapping: &serde_yaml::Mapping,
    mut key_memory: impl FnMut(&str) -> Result<usize>,
    mut value_kind: impl FnMut(&str, &serde_yaml::Value) -> RunnerActionManifestValueKind,
) -> Result<usize> {
    let mut bytes = 24;
    for (key, value) in mapping {
        bytes = runner_add_action_template_memory(bytes, key_memory(key)?)?;
        bytes = runner_add_action_template_memory(
            bytes,
            runner_action_manifest_value_memory(value, value_kind(key, value))?,
        )?;
    }
    Ok(bytes)
}

fn runner_template_string_memory(value: &str, context: ActionExpressionContext) -> Result<usize> {
    Ok(match runner_template_scalar(value, context)? {
        RunnerTemplateScalar::Literal(value) => runner_string_token_memory(&value),
        RunnerTemplateScalar::Expression(value) => runner_expression_token_memory(&value),
    })
}

fn runner_no_context_string_memory(value: &str) -> usize {
    value
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .and_then(runner_expression_string_literal_value)
        .map(|value| runner_string_token_memory(&value))
        .unwrap_or_else(|| runner_string_token_memory(value))
}

fn runner_string_token_memory(value: &str) -> usize {
    24 + 26 + value.encode_utf16().count().saturating_mul(2)
}

fn runner_expression_token_memory(value: &str) -> usize {
    24 + 26 + value.encode_utf16().count().saturating_mul(2)
}

fn runner_add_action_template_memory(current: usize, additional: usize) -> Result<usize> {
    let total = current
        .checked_add(additional)
        .ok_or_else(|| anyhow::anyhow!("action metadata template memory accounting overflowed"))?;
    if total > MAX_ACTION_TEMPLATE_MEMORY_BYTES {
        bail!("action metadata exceeds the Runner template memory limit");
    }
    Ok(total)
}

const MAX_METADATA_PARSE_NESTING: usize = 100;
const MAX_ACTION_METADATA_EVENTS: usize = 1_000_000;
const MAX_ACTION_TEMPLATE_MEMORY_BYTES: usize = 10 * 1024 * 1024;
const MAX_ACTION_INPUTS: usize = 256;
const MAX_ACTION_INPUT_NAME_BYTES: usize = 256;
const MAX_ACTION_INPUT_VALUE_BYTES: usize = 64 * 1024;
const MAX_ACTION_INPUT_BYTES: usize = 256 * 1024;
const MAX_ACTION_INPUT_VALUE_NESTING: usize = 64;

fn deserialize_optional_string_scalar<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(deserializer)?;
    action_manifest_scalar_string(&value)
        .map(Some)
        .ok_or_else(|| <D::Error as serde::de::Error>::custom("expected a YAML scalar"))
}

fn deserialize_string_scalar<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(deserializer)?;
    action_manifest_scalar_string(&value)
        .ok_or_else(|| <D::Error as serde::de::Error>::custom("expected a YAML scalar"))
}

fn deserialize_optional_string_scalar_vec<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<serde_yaml::Value>::deserialize(deserializer)?
        .iter()
        .map(|value| {
            action_manifest_scalar_string(value)
                .ok_or_else(|| <D::Error as serde::de::Error>::custom("expected a YAML scalar"))
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(Some)
}

fn action_manifest_scalar_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::Null => Some(String::new()),
        serde_yaml::Value::Bool(value) => Some(value.to_string()),
        serde_yaml::Value::Number(number) => normalized_runner_number(value)
            .map(str::to_owned)
            .or_else(|| Some(velnor_expression::value::format_number(number.as_f64()))),
        serde_yaml::Value::String(value) => Some(value.clone()),
        serde_yaml::Value::Tagged(tagged) => {
            if let Some(number) = normalized_runner_number(value) {
                return Some(number.to_owned());
            }
            let scalar = action_manifest_scalar_string(tagged.value())?;
            match tagged.tag().as_str() {
                "tag:yaml.org,2002:str" => {
                    tagged.value().as_str().map(str::to_owned).or(Some(scalar))
                }
                "tag:yaml.org,2002:bool" => match scalar.as_str() {
                    "true" | "True" | "TRUE" => Some("true".to_owned()),
                    "false" | "False" | "FALSE" => Some("false".to_owned()),
                    _ => None,
                },
                "tag:yaml.org,2002:null" => match scalar.as_str() {
                    "" | "null" | "Null" | "NULL" | "~" => Some(String::new()),
                    _ => None,
                },
                _ => Some(scalar),
            }
        }
        serde_yaml::Value::Sequence(_) | serde_yaml::Value::Mapping(_) => None,
    }
}

pub fn repository_action_plans(
    steps: &[ActionStep],
    actions_host: &Path,
) -> Result<Vec<RepositoryActionPlan>> {
    repository_action_plans_with_context(steps, actions_host, &[])
}

pub fn repository_action_plans_with_context(
    steps: &[ActionStep],
    actions_host: &Path,
    context_data: &[(String, serde_json::Value)],
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
        let (inputs, expression_inputs) = render_inputs(&string_inputs(step)?, context_data)?;
        plans.push(RepositoryActionPlan {
            step_id: step_id(step, plans.len()),
            repository: repository.clone(),
            git_ref,
            source_path: reference.path.clone(),
            repository_dir,
            action_dir,
            inputs,
            expression_inputs,
            input_templates: None,
            env: step_environment(step)?,
            condition: step.condition.clone(),
            continue_on_error: crate::script_step::step_continue_on_error(step),
            timeout_minutes: crate::script_step::step_timeout_minutes(step),
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
        let (inputs, expression_inputs) = render_inputs(&string_inputs(step)?, context_data)?;
        plans.push(LocalActionPlan {
            step_id: step_id(step, plans.len()),
            action_dir: local_action_dir(workspace_host, path)?,
            workspace_host: workspace_host.to_path_buf(),
            inputs,
            expression_inputs,
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
    workspace_host: &Path,
) -> Result<Vec<RepositoryActionPlan>> {
    let mut plans = Vec::new();
    for action in resolved_actions {
        if action.runtime != ActionRuntime::Composite {
            continue;
        }
        for invocation in action.composite_invocations("/__w", actions_host, workspace_host)? {
            if let CompositeActionInvocation::Repository(repository_plan) = invocation {
                plans.push(repository_plan);
            }
        }
    }
    Ok(plans)
}

pub(crate) fn step_id(step: &ActionStep, index: usize) -> String {
    // Prefer context_name (YAML id:) over internal UUID for expression lookup.
    step.context_name
        .as_deref()
        .filter(|n| !n.is_empty() && !n.starts_with("__"))
        .or(step.id.as_deref())
        .or(step.name.as_deref())
        .map(sanitize_segment)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("action{}", index + 1))
}

pub fn download_repository_actions<R>(
    runner: &mut R,
    plans: &[RepositoryActionPlan],
    actions_host: &Path,
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
                actions_host,
                None,
                Some(1),
                false,
                true,
                true,
                false, // preserve_target: action bundles are metadata-only, never built
                false, // lfs: action repos don't use LFS
                None,  // action bundles are not primary-repository mirrors
                &mut Vec::new(), // action-repo fetch trace is internal, not surfaced
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
    let (metadata_path, metadata) = action_definition_metadata(&plan.action_dir)?;
    let runtime = metadata.runtime()?;
    validate_javascript_main(&runtime, &plan.action_dir, &plan.repository)?;
    Ok(ResolvedAction {
        plan: plan.clone(),
        metadata_path,
        metadata,
        runtime,
    })
}

pub fn resolve_local_action(plan: &LocalActionPlan) -> Result<ActionMetadata> {
    let (_, metadata) = action_definition_metadata(&plan.action_dir)?;
    validate_javascript_main(&metadata.runtime()?, &plan.action_dir, &plan.step_id)?;
    Ok(metadata)
}

pub(crate) fn local_javascript_invocation(
    plan: &LocalActionPlan,
    metadata: &ActionMetadata,
    step_env: Vec<(String, String)>,
) -> Result<JavaScriptActionInvocation> {
    let ActionRuntime::JavaScript { node, main } = metadata.runtime()? else {
        bail!("local action '{}' is not a JavaScript action", plan.step_id);
    };
    validate_javascript_main(
        &ActionRuntime::JavaScript {
            node: node.clone(),
            main: main.clone(),
        },
        &plan.action_dir,
        &plan.step_id,
    )?;
    let action_container_path =
        workspace_container_path("/__w", &plan.workspace_host, &plan.action_dir)?;
    let container_file = |path: &str| {
        format!(
            "{}/{}",
            action_container_path.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    };
    let main_container_path = container_file(&main);
    let pre_container_path = metadata.runs.pre.as_deref().map(container_file);
    let post_container_path = metadata.runs.post.as_deref().map(container_file);
    let env = vec![
        ("GITHUB_ACTION".to_string(), plan.step_id.clone()),
        (
            "GITHUB_ACTION_PATH".to_string(),
            action_container_path.clone(),
        ),
    ];
    Ok(JavaScriptActionInvocation {
        node,
        pre_container_path,
        pre_condition: metadata.runs.pre_if.clone(),
        main_container_path,
        post_container_path,
        post_condition: metadata.runs.post_if.clone(),
        action_container_path,
        inputs: plan.inputs.clone(),
        input_diagnostics: action_input_diagnostics(
            metadata,
            &plan.inputs,
            ActionReferenceType::Script,
        ),
        input_expression_values: plan.expression_inputs.clone(),
        input_defaults: action_input_default_templates(metadata),
        step_env,
        env,
        ..Default::default()
    })
}

pub(crate) fn local_docker_invocation(
    plan: &LocalActionPlan,
    metadata: &ActionMetadata,
    step_env: Vec<(String, String)>,
) -> Result<DockerActionInvocation> {
    let ActionRuntime::Docker { image } = metadata.runtime()? else {
        bail!("local action '{}' is not a Docker action", plan.step_id);
    };
    let action_container_path =
        workspace_container_path("/__w", &plan.workspace_host, &plan.action_dir)?;
    let env = vec![
        ("GITHUB_ACTION".to_string(), plan.step_id.clone()),
        (
            "GITHUB_ACTION_PATH".to_string(),
            action_container_path.clone(),
        ),
    ];
    let (image, build_context_host, dockerfile_host) =
        if let Some(image) = docker_scheme_image(&image) {
            let image = crate::docker_argv::ImageReference::parse(image).map_err(|error| {
                anyhow::anyhow!(
                    "local action '{}' declares an invalid Docker image: {error}",
                    plan.step_id
                )
            })?;
            (image.as_str().to_string(), None, None)
        } else {
            let (context, dockerfile) = action_dockerfile_paths(&image, &plan.action_dir)?;
            (
                docker_action_tag("local", &plan.step_id, None),
                Some(context),
                Some(dockerfile),
            )
        };
    Ok(DockerActionInvocation {
        image,
        build_context_host,
        dockerfile_host,
        action_container_path,
        inputs: plan.inputs.clone(),
        input_diagnostics: action_input_diagnostics(
            metadata,
            &plan.inputs,
            ActionReferenceType::Script,
        ),
        input_expression_values: plan.expression_inputs.clone(),
        input_defaults: action_input_default_templates(metadata),
        step_env,
        runs_env: metadata.runs.env.clone(),
        env,
        entrypoint: metadata.runs.entrypoint.clone(),
        args: metadata.runs.args.clone(),
        pre_entrypoint: metadata.runs.pre_entrypoint.clone(),
        post_entrypoint: metadata.runs.post_entrypoint.clone(),
        pre_condition: metadata.runs.pre_if.clone(),
        post_condition: metadata.runs.post_if.clone(),
        ..Default::default()
    })
}

pub(crate) fn docker_registry_invocation(
    image: &str,
    inputs: BTreeMap<String, String>,
    input_templates: Option<ActionTemplateMap>,
    input_expression_values: BTreeSet<String>,
    step_env: Vec<(String, String)>,
    action_container_path: &str,
) -> Result<DockerActionInvocation> {
    let image = docker_scheme_image(image).unwrap_or(image);
    let image = crate::docker_argv::ImageReference::parse(image)
        .map_err(|error| anyhow::anyhow!("invalid Docker action image: {error}"))?;
    Ok(DockerActionInvocation {
        image: image.as_str().to_string(),
        action_container_path: action_container_path.to_string(),
        inputs,
        input_templates,
        input_expression_values,
        step_env,
        args: None,
        ..Default::default()
    })
}

fn action_definition_metadata(action_dir: &Path) -> Result<(PathBuf, ActionMetadata)> {
    if let Ok(metadata_path) = action_metadata_path(action_dir) {
        let metadata = parse_action_metadata(&read_bounded_file(&metadata_path)?)?;
        return Ok((metadata_path, metadata));
    }
    for file_name in ["Dockerfile", "dockerfile"] {
        let dockerfile = action_dir.join(file_name);
        if dockerfile.is_file() {
            let metadata =
                parse_action_metadata(&format!("runs:\n  using: docker\n  image: {file_name}\n"))?;
            return Ok((dockerfile, metadata));
        }
    }
    bail!(
        "action metadata or Dockerfile not found in {}",
        action_dir.display()
    )
}

fn validate_javascript_main(
    runtime: &ActionRuntime,
    action_dir: &Path,
    action_name: &str,
) -> Result<()> {
    if let ActionRuntime::JavaScript { main, .. } = runtime {
        let main_path = action_dir.join(main);
        if !main_path.is_file() {
            bail!(
                "JavaScript action '{action_name}' main file does not exist: {}",
                main_path.display()
            );
        }
    }
    Ok(())
}

fn read_bounded_file(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("read action metadata {}", path.display()))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JavaScriptActionInvocation {
    pub node: String,
    pub pre_container_path: Option<String>,
    pub pre_condition: Option<String>,
    pub main_container_path: String,
    pub post_container_path: Option<String>,
    pub post_condition: Option<String>,
    pub action_container_path: String,
    pub inputs: BTreeMap<String, String>,
    pub input_diagnostics: Vec<String>,
    pub input_templates: Option<ActionTemplateMap>,
    pub input_expression_values: BTreeSet<String>,
    pub input_defaults: ActionTemplateMap,
    pub step_env: Vec<(String, String)>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DockerActionInvocation {
    pub image: String,
    pub build_context_host: Option<PathBuf>,
    pub dockerfile_host: Option<PathBuf>,
    pub action_container_path: String,
    pub inputs: BTreeMap<String, String>,
    pub input_diagnostics: Vec<String>,
    pub input_templates: Option<ActionTemplateMap>,
    pub input_expression_values: BTreeSet<String>,
    pub input_defaults: ActionTemplateMap,
    pub step_env: Vec<(String, String)>,
    pub runs_env: ActionTemplateMap,
    pub env: Vec<(String, String)>,
    pub entrypoint: Option<String>,
    /// `None` preserves the distinction between an absent manifest field and
    /// an explicit empty sequence; Runner falls back to the `with.args` input
    /// only when the field is absent.
    pub args: Option<Vec<String>>,
    /// `runs.pre-entrypoint` / `runs.post-entrypoint`, rendered against
    /// the action scope like the main entrypoint. A docker pre/post runs
    /// the same image and args with the stage entrypoint substituted
    /// (`ContainerActionHandler.cs:116-123`).
    pub pre_entrypoint: Option<String>,
    pub post_entrypoint: Option<String>,
    /// `runs.pre-if` / `runs.post-if`, shared with the node lifecycle.
    pub pre_condition: Option<String>,
    pub post_condition: Option<String>,
}

#[derive(Debug, Clone)]
pub enum LocalActionInvocation {
    JavaScript(JavaScriptActionInvocation),
    Docker(DockerActionInvocation),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeActionInvocation {
    pub git_ref: String,
    pub adapter: NativeActionAdapter,
    /// Cache lifecycle for the `actions/cache` adapter (`None` for every other
    /// adapter). Preserving this through invocation is what keeps root/restore/
    /// save from collapsing into a single behavior.
    pub cache_kind: Option<CacheActionKind>,
    /// Action subpath retained from the plan (e.g. `restore`, `save`), so the
    /// invocation no longer drops the GitHub lifecycle identity.
    pub source_path: Option<String>,
    pub inputs: BTreeMap<String, String>,
    pub input_diagnostics: Vec<String>,
    pub input_templates: Option<ActionTemplateMap>,
    pub input_expression_values: BTreeSet<String>,
    pub input_defaults: ActionTemplateMap,
    pub step_env: Vec<(String, String)>,
    pub env: Vec<(String, String)>,
}

impl Default for NativeActionInvocation {
    fn default() -> Self {
        Self {
            git_ref: String::new(),
            adapter: NativeActionAdapter::Checkout,
            cache_kind: None,
            source_path: None,
            inputs: BTreeMap::new(),
            input_diagnostics: Vec::new(),
            input_templates: None,
            input_expression_values: BTreeSet::new(),
            input_defaults: ActionTemplateMap::default(),
            step_env: Vec::new(),
            env: Vec::new(),
        }
    }
}

impl ResolvedAction {
    pub fn native_invocation(&self) -> Result<Option<NativeActionInvocation>> {
        let Some(mut invocation) = native_invocation_from_plan(&self.plan)? else {
            return Ok(None);
        };
        invocation.input_diagnostics = action_input_diagnostics(
            &self.metadata,
            &self.plan.inputs,
            ActionReferenceType::Repository,
        );
        Ok(Some(invocation))
    }

    pub fn javascript_invocation(&self, actions_host: &Path) -> Result<JavaScriptActionInvocation> {
        self.javascript_invocation_with_action_path(actions_host, None)
    }

    pub fn javascript_invocation_with_action_path(
        &self,
        actions_host: &Path,
        enclosing_action_path: Option<&str>,
    ) -> Result<JavaScriptActionInvocation> {
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
        let github_action_path = enclosing_action_path.unwrap_or(&action_container_path);
        let env = vec![
            ("GITHUB_ACTION".to_string(), self.plan.step_id.clone()),
            (
                "GITHUB_ACTION_PATH".to_string(),
                github_action_path.to_owned(),
            ),
            (
                "GITHUB_ACTION_REPOSITORY".to_string(),
                self.plan.repository.clone(),
            ),
            ("GITHUB_ACTION_REF".to_string(), self.plan.git_ref.clone()),
        ];
        Ok(JavaScriptActionInvocation {
            node: node.clone(),
            pre_container_path,
            pre_condition: self.metadata.runs.pre_if.clone(),
            main_container_path,
            post_container_path,
            post_condition: self.metadata.runs.post_if.clone(),
            action_container_path,
            inputs: self.plan.inputs.clone(),
            input_diagnostics: action_input_diagnostics(
                &self.metadata,
                &self.plan.inputs,
                ActionReferenceType::Repository,
            ),
            input_templates: self.plan.input_templates.clone(),
            input_expression_values: self.plan.expression_inputs.clone(),
            input_defaults: action_input_default_templates(&self.metadata),
            step_env: self.plan.env.clone(),
            env,
        })
    }

    pub fn docker_invocation(&self, actions_host: &Path) -> Result<DockerActionInvocation> {
        self.docker_invocation_with_action_path(actions_host, None)
    }

    pub fn docker_invocation_with_action_path(
        &self,
        actions_host: &Path,
        enclosing_action_path: Option<&str>,
    ) -> Result<DockerActionInvocation> {
        let ActionRuntime::Docker { image } = &self.runtime else {
            bail!("action '{}' is not a Docker action", self.plan.repository)
        };
        let action_container_path = container_path(actions_host, &self.plan.action_dir)?;
        let github_action_path = enclosing_action_path.unwrap_or(&action_container_path);
        let env = vec![
            ("GITHUB_ACTION".to_string(), self.plan.step_id.clone()),
            (
                "GITHUB_ACTION_PATH".to_string(),
                github_action_path.to_owned(),
            ),
            (
                "GITHUB_ACTION_REPOSITORY".to_string(),
                self.plan.repository.clone(),
            ),
            ("GITHUB_ACTION_REF".to_string(), self.plan.git_ref.clone()),
        ];
        let (image, build_context_host, dockerfile_host) =
            if let Some(image) = docker_scheme_image(image) {
                // `runs.image` is repository content, so in the fork-PR case it
                // is attacker-controlled. Without a grammar check a value like
                // `docker://--privileged` reaches the host `docker run` as a
                // flag and hands the workflow root on a shared runner host.
                // Reject anything that is not an OCI reference here, at the
                // one place the scheme is stripped.
                let image = crate::docker_argv::ImageReference::parse(image).map_err(|error| {
                    anyhow::anyhow!(
                        "action '{}' declares an invalid Docker image: {error}",
                        self.plan.repository
                    )
                })?;
                (image.as_str().to_string(), None, None)
            } else {
                let (build_context_host, dockerfile_host) =
                    action_dockerfile_paths(image, &self.plan.action_dir)?;
                let tag = docker_action_tag(
                    &self.plan.repository,
                    &self.plan.git_ref,
                    self.plan.source_path.as_deref(),
                );
                (tag, Some(build_context_host), Some(dockerfile_host))
            };
        let entrypoint = self.metadata.runs.entrypoint.clone();
        let args = self.metadata.runs.args.clone();
        let pre_entrypoint = self.metadata.runs.pre_entrypoint.clone();
        let post_entrypoint = self.metadata.runs.post_entrypoint.clone();

        Ok(DockerActionInvocation {
            image,
            build_context_host,
            dockerfile_host,
            action_container_path,
            inputs: self.plan.inputs.clone(),
            input_diagnostics: action_input_diagnostics(
                &self.metadata,
                &self.plan.inputs,
                ActionReferenceType::Repository,
            ),
            input_templates: self.plan.input_templates.clone(),
            input_expression_values: self.plan.expression_inputs.clone(),
            input_defaults: action_input_default_templates(&self.metadata),
            step_env: self.plan.env.clone(),
            runs_env: self.metadata.runs.env.clone(),
            env,
            entrypoint,
            args,
            pre_entrypoint,
            post_entrypoint,
            pre_condition: self.metadata.runs.pre_if.clone(),
            post_condition: self.metadata.runs.post_if.clone(),
        })
    }

    pub fn composite_invocations(
        &self,
        workspace_container: &str,
        actions_host: &Path,
        workspace_host: &Path,
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
            workspace_host,
            &action_path,
            &mut BTreeSet::new(),
            0,
        )
    }
}

pub fn native_invocation_from_plan(
    plan: &RepositoryActionPlan,
) -> Result<Option<NativeActionInvocation>> {
    let Some(adapter) = native_action_adapter(&plan.repository) else {
        return Ok(None);
    };
    // Only the cache adapter carries a lifecycle; deriving it here (and failing
    // on an unknown subpath) keeps the classification at the single point where
    // the plan's `source_path` is turned into an invocation.
    let cache_kind = if adapter == NativeActionAdapter::Cache {
        Some(cache_action_kind(plan.source_path.as_deref())?)
    } else {
        None
    };
    Ok(Some(NativeActionInvocation {
        git_ref: plan.git_ref.clone(),
        adapter,
        cache_kind,
        source_path: plan.source_path.clone(),
        inputs: canonicalize_input_map(&plan.inputs)?,
        input_diagnostics: Vec::new(),
        input_templates: plan.input_templates.clone(),
        input_expression_values: plan.expression_inputs.clone(),
        input_defaults: ActionTemplateMap::default(),
        step_env: plan.env.clone(),
        env: plan.env.clone(),
    }))
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
                CompositeActionInvocation::CompositeStart { .. }
                | CompositeActionInvocation::CompositeEnd { .. }
                | CompositeActionInvocation::ContinueOnError { .. }
                | CompositeActionInvocation::LocalAction { .. }
                | CompositeActionInvocation::Docker { .. }
                | CompositeActionInvocation::Repository(_) => None,
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

    let action_path =
        workspace_container_path(workspace_container, &plan.workspace_host, &plan.action_dir)?;
    composite_action_invocations_with_path(
        &plan.step_id,
        &plan.inputs,
        metadata,
        workspace_container,
        actions_host,
        &plan.workspace_host,
        &action_path,
        &mut BTreeSet::new(),
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn composite_action_invocations_with_path(
    step_id_prefix: &str,
    inputs: &BTreeMap<String, String>,
    metadata: &ActionMetadata,
    workspace_container: &str,
    actions_host: &Path,
    local_root_host: &Path,
    action_path: &str,
    local_stack: &mut BTreeSet<PathBuf>,
    depth: usize,
) -> Result<Vec<CompositeActionInvocation>> {
    const MAX_LOCAL_COMPOSITE_DEPTH: usize = 16;
    if depth > MAX_LOCAL_COMPOSITE_DEPTH {
        bail!("nested local composite depth exceeds {MAX_LOCAL_COMPOSITE_DEPTH}")
    }
    let action_inputs = effective_inputs(metadata, inputs)?;
    let mut invocations = Vec::new();
    let step_ids = composite_step_id_map(step_id_prefix, metadata);
    for (index, step) in metadata.runs.steps.iter().enumerate() {
        let step_id = composite_step_id(step_id_prefix, step.id.as_deref(), index);
        if let Some(uses) = step.uses.as_deref() {
            if uses.starts_with('.') {
                let nested_dir = local_action_dir(local_root_host, uses)?;
                if !local_stack.insert(nested_dir.clone()) {
                    bail!("nested local composite cycle at '{}'", nested_dir.display())
                }
                let (_, nested_metadata) = action_definition_metadata(&nested_dir)?;
                let nested_runtime = nested_metadata.runtime()?;
                validate_javascript_main(&nested_runtime, &nested_dir, uses)?;
                let nested_input_templates = step.with.clone();
                let nested_provided_inputs = composite_static_inputs(
                    &step.with,
                    &action_inputs,
                    action_path,
                    workspace_container,
                    &step_ids,
                )?;
                let nested_deferred_inputs = expression_input_names(&nested_provided_inputs)?;
                let nested_env = composite_env_pairs(
                    &step.env,
                    &action_inputs,
                    action_path,
                    workspace_container,
                    &step_ids,
                )?;
                let nested_condition = step
                    .condition
                    .as_ref()
                    .map(|condition| {
                        render_composite_scoped_value(
                            condition,
                            &action_inputs,
                            action_path,
                            workspace_container,
                            &step_ids,
                        )
                    })
                    .transpose()?;
                let nested_action_path = if nested_dir.starts_with(actions_host) {
                    container_path(actions_host, &nested_dir)?
                } else {
                    workspace_container_path(workspace_container, local_root_host, &nested_dir)?
                };
                let (continue_on_error, continue_on_error_expression) =
                    composite_continue_on_error(step);
                let display_name = step
                    .name
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| uses.to_string());
                if nested_runtime == ActionRuntime::Composite {
                    let (nested_inputs, nested_expression_inputs) = composite_action_input_scope(
                        &nested_metadata,
                        &nested_provided_inputs,
                        &nested_deferred_inputs,
                    )?;
                    invocations.push(CompositeActionInvocation::CompositeStart {
                        step_id: step_id.clone(),
                        display_name,
                        expression_inputs: nested_expression_inputs,
                        input_templates: nested_input_templates
                            .has_template_keys()
                            .then_some(nested_input_templates),
                        input_defaults: action_input_default_templates(&nested_metadata),
                        visible_step_ids: composite_step_id_map(&step_id, &nested_metadata),
                        inputs: nested_inputs.clone(),
                        input_diagnostics: action_input_diagnostics(
                            &nested_metadata,
                            &nested_provided_inputs,
                            ActionReferenceType::Script,
                        ),
                        env: nested_env,
                        condition: nested_condition,
                        continue_on_error,
                        continue_on_error_expression,
                        action_path: nested_action_path.clone(),
                    });
                    invocations.extend(composite_action_invocations_with_path(
                        &step_id,
                        &nested_inputs,
                        &nested_metadata,
                        workspace_container,
                        actions_host,
                        local_root_host,
                        &nested_action_path,
                        local_stack,
                        depth + 1,
                    )?);
                    invocations.push(CompositeActionInvocation::CompositeEnd {
                        step_id: step_id.clone(),
                    });
                } else {
                    if let Some(expression) = continue_on_error_expression {
                        invocations.push(CompositeActionInvocation::ContinueOnError {
                            step_id: step_id.clone(),
                            value: expression,
                        });
                    }
                    let nested_plan = LocalActionPlan {
                        step_id: step_id.clone(),
                        action_dir: nested_dir.clone(),
                        workspace_host: local_root_host.to_path_buf(),
                        inputs: nested_provided_inputs,
                        expression_inputs: nested_deferred_inputs,
                    };
                    let input_templates = nested_input_templates
                        .has_template_keys()
                        .then_some(nested_input_templates);
                    let invocation = match nested_runtime {
                        ActionRuntime::JavaScript { .. } => {
                            let mut invocation = local_javascript_invocation(
                                &nested_plan,
                                &nested_metadata,
                                nested_env,
                            )?;
                            invocation.input_templates = input_templates;
                            LocalActionInvocation::JavaScript(invocation)
                        }
                        ActionRuntime::Docker { .. } => {
                            let mut invocation = local_docker_invocation(
                                &nested_plan,
                                &nested_metadata,
                                nested_env,
                            )?;
                            invocation.input_templates = input_templates;
                            LocalActionInvocation::Docker(invocation)
                        }
                        ActionRuntime::Composite => {
                            bail!("nested composite action runtime changed during expansion")
                        }
                    };
                    invocations.push(CompositeActionInvocation::LocalAction {
                        step_id: step_id.clone(),
                        invocation,
                        display_name,
                        condition: nested_condition,
                        continue_on_error,
                    });
                }
                local_stack.remove(&nested_dir);
                continue;
            }
            if docker_scheme_image(uses).is_some() {
                let inputs = composite_static_inputs(
                    &step.with,
                    &action_inputs,
                    action_path,
                    workspace_container,
                    &step_ids,
                )?;
                let env = composite_env_pairs(
                    &step.env,
                    &action_inputs,
                    action_path,
                    workspace_container,
                    &step_ids,
                )?;
                let condition = step
                    .condition
                    .as_ref()
                    .map(|condition| {
                        render_composite_scoped_value(
                            condition,
                            &action_inputs,
                            action_path,
                            workspace_container,
                            &step_ids,
                        )
                    })
                    .transpose()?;
                let (continue_on_error, continue_on_error_expression) =
                    composite_continue_on_error(step);
                if let Some(expression) = continue_on_error_expression {
                    invocations.push(CompositeActionInvocation::ContinueOnError {
                        step_id: step_id.clone(),
                        value: expression,
                    });
                }
                let input_templates = step.with.clone();
                let invocation = docker_registry_invocation(
                    uses,
                    inputs.clone(),
                    input_templates
                        .has_template_keys()
                        .then_some(input_templates),
                    expression_input_names(&inputs)?,
                    env,
                    action_path,
                )?;
                invocations.push(CompositeActionInvocation::Docker {
                    step_id: step_id.clone(),
                    display_name: step
                        .name
                        .clone()
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| format!("Run {uses}")),
                    invocation,
                    condition,
                    continue_on_error,
                });
                continue;
            }
            let reference = parse_repository_uses(uses)?;
            let input_templates = step.with.clone();
            let inputs = composite_static_inputs(
                &step.with,
                &action_inputs,
                action_path,
                workspace_container,
                &step_ids,
            )?;
            let env = composite_env_pairs(
                &step.env,
                &action_inputs,
                action_path,
                workspace_container,
                &step_ids,
            )?;
            let condition = step
                .condition
                .as_ref()
                .map(|condition| {
                    render_composite_scoped_value(
                        condition,
                        &action_inputs,
                        action_path,
                        workspace_container,
                        &step_ids,
                    )
                })
                .transpose()?;
            let repository_dir =
                repository_dir(actions_host, &reference.repository, &reference.git_ref);
            let action_dir = action_dir(
                actions_host,
                &reference.repository,
                &reference.git_ref,
                reference.source_path.as_deref(),
            )?;
            let (continue_on_error, continue_on_error_expression) =
                composite_continue_on_error(step);
            if let Some(expression) = continue_on_error_expression {
                invocations.push(CompositeActionInvocation::ContinueOnError {
                    step_id: step_id.clone(),
                    value: expression,
                });
            }
            invocations.push(CompositeActionInvocation::Repository(
                RepositoryActionPlan {
                    step_id,
                    repository: reference.repository,
                    git_ref: reference.git_ref,
                    source_path: reference.source_path,
                    repository_dir,
                    action_dir,
                    expression_inputs: expression_input_names(&inputs)?,
                    input_templates: input_templates
                        .has_template_keys()
                        .then_some(input_templates),
                    inputs,
                    env,
                    condition,
                    continue_on_error,
                    timeout_minutes: None,
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
            .map(|shell| crate::script_step::shell_from_template(shell.to_owned(), None))
            .transpose()?
            .unwrap_or(crate::container::Shell::BashDefault);
        let rendered = render_composite_scoped_value(
            script,
            &action_inputs,
            action_path,
            workspace_container,
            &step_ids,
        )?;
        let mut env = composite_env_pairs(
            &step.env,
            &action_inputs,
            action_path,
            workspace_container,
            &step_ids,
        )?;
        env.push(("GITHUB_ACTION_PATH".to_string(), action_path.to_string()));
        let working_directory_container = step
            .working_directory
            .as_deref()
            .map(|path| -> Result<String> {
                render_composite_scoped_value(
                    path,
                    &action_inputs,
                    action_path,
                    workspace_container,
                    &step_ids,
                )
            })
            .transpose()?
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
        let (continue_on_error, continue_on_error_expression) = composite_continue_on_error(step);
        if let Some(expression) = continue_on_error_expression {
            invocations.push(CompositeActionInvocation::ContinueOnError {
                step_id: step_id.clone(),
                value: expression,
            });
        }
        invocations.push(CompositeActionInvocation::Script(ScriptStep {
            id: step_id,
            display_name: composite_display_name,
            script: rendered,
            shell,
            working_directory_container,
            env,
            condition: step
                .condition
                .as_ref()
                .map(|condition| {
                    render_composite_scoped_value(
                        condition,
                        &action_inputs,
                        action_path,
                        workspace_container,
                        &step_ids,
                    )
                })
                .transpose()?,
            continue_on_error,
            timeout_minutes: None,
        }));
    }
    let outputs = metadata
        .outputs
        .iter()
        .filter_map(|(name, output)| {
            output.value.as_ref().map(|value| {
                Ok((
                    name.clone(),
                    render_composite_scoped_value(
                        value,
                        &action_inputs,
                        action_path,
                        workspace_container,
                        &step_ids,
                    )?,
                ))
            })
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
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
    if docker_scheme_image(uses).is_some() {
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

pub(crate) fn docker_scheme_image(value: &str) -> Option<&str> {
    value
        .get(.."docker://".len())
        .filter(|scheme| scheme.eq_ignore_ascii_case("docker://"))
        .and_then(|_| value.get("docker://".len()..))
}

fn action_metadata_path(action_dir: &Path) -> Result<PathBuf> {
    for file_name in ["action.yml", "action.yaml"] {
        let path = action_dir.join(file_name);
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!("action metadata not found in {}", action_dir.display())
}

fn is_local_action_reference(name: Option<&str>, path: Option<&str>) -> bool {
    local_action_path(name, path).is_some()
}

fn local_action_path<'a>(name: Option<&'a str>, path: Option<&'a str>) -> Option<&'a str> {
    path.filter(|value| has_local_action_prefix(value))
        .or_else(|| name.filter(|value| has_local_action_prefix(value)))
}

fn has_local_action_prefix(value: &str) -> bool {
    value.starts_with("./") || value.starts_with(".\\")
}

fn local_action_dir(workspace_host: &Path, source_path: &str) -> Result<PathBuf> {
    let normalized = source_path.replace('\\', "/");
    let Some(relative) = normalized.strip_prefix("./") else {
        bail!("unsupported local action path '{source_path}'")
    };
    if relative.starts_with('/')
        || relative.split('/').any(|component| component == "..")
        || relative
            .split('/')
            .next()
            .is_some_and(is_windows_drive_prefix)
        || source_path.chars().any(char::is_control)
    {
        bail!("unsupported local action path '{source_path}'")
    }
    Ok(workspace_host.join(relative))
}

fn is_windows_drive_prefix(value: &str) -> bool {
    value.as_bytes().get(1) == Some(&b':')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
}

pub(crate) fn workspace_container_path(
    workspace_container: &str,
    workspace_host: &Path,
    host_path: &Path,
) -> Result<String> {
    let relative = host_path.strip_prefix(workspace_host).with_context(|| {
        format!(
            "local action path {} is outside workspace {}",
            host_path.display(),
            workspace_host.display()
        )
    })?;
    let relative = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Ok(if relative.is_empty() {
        workspace_container.trim_end_matches('/').to_owned()
    } else {
        format!("{}/{}", workspace_container.trim_end_matches('/'), relative)
    })
}

pub(crate) fn container_path(actions_host: &Path, host_path: &Path) -> Result<String> {
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

pub(crate) fn input_env_name(name: &str) -> String {
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

fn action_dockerfile_paths(image: &str, action_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    if !is_dockerfile_image(image) {
        bail!("unsupported Docker action image '{image}'");
    }
    let relative = Path::new(image);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    {
        bail!("Dockerfile path '{image}' escapes the action directory");
    }
    let dockerfile = action_dir.join(relative);
    if !dockerfile.is_file() {
        bail!(
            "Docker action Dockerfile does not exist: {}",
            dockerfile.display()
        );
    }
    let context = dockerfile
        .parent()
        .context("Dockerfile path has no parent directory")?
        .to_path_buf();
    Ok((context, dockerfile))
}

fn is_dockerfile_image(image: &str) -> bool {
    let basename = image.rsplit('/').next().unwrap_or_default();
    let basename = basename.to_ascii_lowercase();
    basename.starts_with("dockerfile.") || basename.ends_with("dockerfile")
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

pub(crate) fn string_inputs(step: &ActionStep) -> Result<BTreeMap<String, String>> {
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
            let mut result = BTreeMap::new();
            let mut total_bytes = 0usize;
            for (name, value) in object
                .iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case("type"))
            {
                insert_bounded_input(
                    &mut result,
                    &mut total_bytes,
                    name,
                    input_value_bounded(value)?,
                )?;
            }
            Ok(result)
        }
        serde_json::Value::Array(items) => {
            let mut result = BTreeMap::new();
            let mut total_bytes = 0usize;
            for item in items {
                if let Some((name, value)) = input_pair(item) {
                    insert_bounded_input(
                        &mut result,
                        &mut total_bytes,
                        name,
                        input_value_bounded(value)?,
                    )?;
                }
            }
            Ok(result)
        }
        _ => Ok(BTreeMap::new()),
    }
}

fn insert_bounded_input(
    result: &mut BTreeMap<String, String>,
    total_bytes: &mut usize,
    name: &str,
    value: String,
) -> Result<()> {
    if result.len() >= MAX_ACTION_INPUTS {
        bail!("action input count exceeds {MAX_ACTION_INPUTS}");
    }
    if name.len() > MAX_ACTION_INPUT_NAME_BYTES {
        bail!("action input name exceeds {MAX_ACTION_INPUT_NAME_BYTES} bytes");
    }
    if value.len() > MAX_ACTION_INPUT_VALUE_BYTES {
        bail!("action input value exceeds {MAX_ACTION_INPUT_VALUE_BYTES} bytes");
    }
    *total_bytes = total_bytes
        .saturating_add(name.len())
        .saturating_add(value.len());
    if *total_bytes > MAX_ACTION_INPUT_BYTES {
        bail!("action inputs exceed {MAX_ACTION_INPUT_BYTES} bytes");
    }
    let canonical_name = runner_ordinal_ignore_case_key(name);
    if result
        .keys()
        .any(|existing| runner_ordinal_ignore_case_key(existing) == canonical_name)
    {
        bail!("action input names differ only by case");
    }
    result.insert(name.to_string(), value);
    Ok(())
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

fn input_value_bounded(value: &serde_json::Value) -> Result<String> {
    let mut current = value;
    for _ in 0..MAX_ACTION_INPUT_VALUE_NESTING {
        match current {
            serde_json::Value::String(value) => {
                if value.len() > MAX_ACTION_INPUT_VALUE_BYTES {
                    bail!("action input value exceeds {MAX_ACTION_INPUT_VALUE_BYTES} bytes");
                }
                return Ok(value.clone());
            }
            serde_json::Value::Null => return Ok(String::new()),
            serde_json::Value::Bool(value) => return Ok(value.to_string()),
            serde_json::Value::Number(value) => return Ok(value.to_string()),
            serde_json::Value::Object(object) => {
                if let Some(value) = object
                    .get("value")
                    .or_else(|| object.get("Value"))
                    .or_else(|| object.get("lit"))
                    .or_else(|| object.get("Lit"))
                {
                    current = value;
                    continue;
                }
                if let Some(expr) = object
                    .get("expr")
                    .or_else(|| object.get("Expr"))
                    .and_then(serde_json::Value::as_str)
                {
                    if expr.len() > MAX_ACTION_INPUT_VALUE_BYTES.saturating_sub(7) {
                        bail!(
                            "action input expression exceeds {MAX_ACTION_INPUT_VALUE_BYTES} bytes"
                        );
                    }
                    return Ok(format!("${{{{{expr}}}}}"));
                }
                return Ok(String::new());
            }
            serde_json::Value::Array(_) => return Ok(String::new()),
        }
    }
    bail!("action input value nesting exceeds {MAX_ACTION_INPUT_VALUE_NESTING} levels")
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

pub(crate) fn render_inputs(
    inputs: &BTreeMap<String, String>,
    context_data: &[(String, serde_json::Value)],
) -> Result<(BTreeMap<String, String>, BTreeSet<String>)> {
    let mut rendered_inputs = BTreeMap::new();
    let mut expression_inputs = BTreeSet::new();
    for (name, value) in inputs {
        let (rendered, deferred) =
            crate::executor::render_context_expressions_bounded_with_deferred(value, context_data)?;
        if deferred {
            expression_inputs.insert(name.clone());
        }
        rendered_inputs.insert(name.clone(), rendered);
    }
    Ok((rendered_inputs, expression_inputs))
}

fn render_composite_value(
    value: &str,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
) -> Result<String> {
    let spans = crate::executor::expression_template_spans(value)?;
    if spans.is_empty() {
        return Ok(value.to_string());
    }
    let mut rendered = String::with_capacity(value.len());
    let mut cursor = 0usize;
    for span in spans {
        rendered.push_str(&value[cursor..span.start()]);
        let expression = span.expression(value).trim();
        rendered.push_str(&render_composite_expression(
            expression,
            inputs,
            action_path,
            workspace_container,
        ));
        cursor = span.end();
    }
    rendered.push_str(&value[cursor..]);
    crate::executor::validate_deferred_expression_template(&rendered)?;
    Ok(rendered)
}

fn render_composite_expression(
    expression: &str,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
) -> String {
    if expression.eq_ignore_ascii_case("github.action_path") {
        return action_path.to_string();
    }
    if expression.eq_ignore_ascii_case("github.workspace") {
        return workspace_container.to_string();
    }
    if let Some(name) = ascii_strip_prefix_case_insensitive(expression, "inputs.")
        && let Some(value) = input_value_case_insensitive(inputs, name)
    {
        return value.clone();
    }

    let rendered =
        replace_composite_expression_tokens(expression, inputs, action_path, workspace_container);
    format!("${{{{ {rendered} }}}}")
}

fn replace_composite_expression_tokens(
    expression: &str,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
) -> String {
    let mut rendered = String::with_capacity(expression.len());
    let mut cursor = 0usize;
    let mut quote = None;
    while cursor < expression.len() {
        let byte = expression.as_bytes()[cursor];
        if let Some(quote_char) = quote {
            let character = expression[cursor..].chars().next().unwrap_or_default();
            rendered.push(character);
            cursor += character.len_utf8();
            if character == quote_char as char {
                quote = None;
            }
            continue;
        }
        if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
            rendered.push(byte as char);
            cursor += 1;
            continue;
        }

        let at_boundary = cursor == 0
            || !expression.as_bytes()[cursor - 1].is_ascii_alphanumeric()
                && expression.as_bytes()[cursor - 1] != b'_'
                && expression.as_bytes()[cursor - 1] != b'-';
        if at_boundary
            && let Some((length, replacement)) = composite_expression_token_at(
                &expression[cursor..],
                inputs,
                action_path,
                workspace_container,
            )
        {
            rendered.push_str(&replacement);
            cursor += length;
            continue;
        }
        let ch = expression[cursor..].chars().next().unwrap_or_default();
        rendered.push(ch);
        cursor += ch.len_utf8();
    }
    rendered
}

fn composite_expression_token_at(
    value: &str,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
) -> Option<(usize, String)> {
    if let Some(input_name) = ascii_strip_prefix_case_insensitive(value, "inputs.") {
        let name_length = input_name
            .bytes()
            .position(|byte| !byte.is_ascii_alphanumeric() && byte != b'_' && byte != b'-')
            .unwrap_or(input_name.len());
        let name_end = "inputs.".len() + name_length;
        if name_length > 0
            && let Some(name) = input_name.get(..name_length)
            && let Some(input) = input_value_case_insensitive(inputs, name)
            && token_boundary_after(value, name_end)
        {
            return Some((name_end, expression_single_quote(input)));
        }
    }
    for (token, replacement) in [
        ("github.action_path", expression_single_quote(action_path)),
        (
            "github.workspace",
            expression_single_quote(workspace_container),
        ),
    ] {
        if ascii_starts_with_case_insensitive(value, token)
            && token_boundary_after(value, token.len())
        {
            return Some((token.len(), replacement));
        }
    }
    None
}

fn ascii_starts_with_case_insensitive(value: &str, prefix: &str) -> bool {
    ascii_strip_prefix_case_insensitive(value, prefix).is_some()
}

fn ascii_strip_prefix_case_insensitive<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let candidate = value.get(..prefix.len())?;
    if candidate.eq_ignore_ascii_case(prefix) {
        value.get(prefix.len()..)
    } else {
        None
    }
}

fn token_boundary_after(value: &str, length: usize) -> bool {
    value
        .as_bytes()
        .get(length)
        .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_' && *byte != b'-')
}

fn render_composite_scoped_value(
    value: &str,
    _inputs: &BTreeMap<String, String>,
    _action_path: &str,
    _workspace_container: &str,
    _step_ids: &BTreeMap<String, String>,
) -> Result<String> {
    // CompositeActionHandler evaluates templates against the active embedded
    // StepsContext. Keep them intact until execution; substituting inputs here
    // re-parsed returned strings such as a literal `${{ secrets.X }}`.
    crate::executor::validate_deferred_expression_template(value)?;
    Ok(value.to_owned())
}

fn composite_static_inputs(
    mapping: &ActionTemplateMap,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
    step_ids: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    mapping
        .iter()
        .filter(|entry| !entry.key_is_template)
        .map(|entry| {
            Ok((
                entry.key.clone(),
                render_composite_scoped_value(
                    &entry.value,
                    inputs,
                    action_path,
                    workspace_container,
                    step_ids,
                )?,
            ))
        })
        .collect()
}

fn composite_env_pairs(
    mapping: &ActionTemplateMap,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
    step_ids: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>> {
    mapping
        .iter()
        .map(|entry| {
            Ok((
                entry.key.clone(),
                render_composite_scoped_value(
                    &entry.value,
                    inputs,
                    action_path,
                    workspace_container,
                    step_ids,
                )?,
            ))
        })
        .collect()
}

fn composite_continue_on_error(step: &CompositeActionStep) -> (bool, Option<ActionBooleanValue>) {
    match step.continue_on_error.as_ref() {
        None => (false, None),
        Some(ActionBooleanValue::Literal(value)) => (*value, None),
        Some(ActionBooleanValue::Expression(expression)) => (
            false,
            Some(ActionBooleanValue::Expression(expression.clone())),
        ),
    }
}

fn render_action_scoped_value(
    value: &str,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
) -> Result<String> {
    render_composite_value(value, inputs, action_path, "/__w")
}

fn composite_step_id_map(prefix: &str, metadata: &ActionMetadata) -> BTreeMap<String, String> {
    metadata
        .runs
        .steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| {
            step.id
                .as_deref()
                .map(|id| (id.to_string(), composite_step_id(prefix, Some(id), index)))
        })
        .collect()
}

pub(crate) fn composite_visible_step_ids(
    prefix: &str,
    metadata: &ActionMetadata,
) -> BTreeMap<String, String> {
    composite_step_id_map(prefix, metadata)
}

pub(crate) fn composite_action_input_scope(
    metadata: &ActionMetadata,
    provided: &BTreeMap<String, String>,
    deferred_inputs: &BTreeSet<String>,
) -> Result<(BTreeMap<String, String>, BTreeSet<String>)> {
    let inputs = effective_inputs(metadata, provided)?;
    // This set carries expression provenance from the plan builder or the
    // composite `with` template. Do not infer it from rendered strings: an
    // evaluated input may itself contain text that looks like `${{ ... }}`.
    let mut expression_inputs: BTreeSet<String> = deferred_inputs
        .iter()
        .map(|name| runner_ordinal_ignore_case_key(name))
        .collect();
    for (name, input) in &metadata.inputs {
        if input
            .default_value
            .as_deref()
            .is_some_and(|value| value.contains("${{"))
        {
            expression_inputs.insert(runner_ordinal_ignore_case_key(name));
        }
    }
    Ok((inputs, expression_inputs))
}

pub(crate) fn expression_input_names(
    inputs: &BTreeMap<String, String>,
) -> Result<BTreeSet<String>> {
    inputs
        .iter()
        .filter_map(
            |(name, value)| match crate::executor::expression_template_spans(value) {
                Ok(spans) if !spans.is_empty() => Some(Ok(name.clone())),
                Ok(_) => None,
                Err(error) => Some(Err(anyhow::Error::from(error))),
            },
        )
        .collect()
}

fn composite_step_id(prefix: &str, id: Option<&str>, index: usize) -> String {
    // Flattened composite steps are addressed as `steps.<id>` in rendered
    // expressions, so the synthesized id has to lex as an identifier. The
    // prefix falls back to the internal job-message GUID when the parent step
    // has no YAML `id:`, and a GUID both starts with a digit and may end in a
    // dotted suffix — neither survives `ExpressionUtility.IsLegalKeyword`.
    let prefix = expression_legal_segment(prefix);
    id.map(|id| format!("{prefix}-{}", expression_legal_segment(id)))
        .filter(|value| !value.ends_with('-'))
        .unwrap_or_else(|| format!("{prefix}-{}", index + 1))
}

/// Map `value` onto a segment the expression lexer accepts
/// (`ExpressionUtility.IsLegalKeyword`): keep `[A-Za-z0-9_-]`, replace every
/// other character — dots included, unlike [`sanitize_segment`] — with `_`,
/// and prefix a digit-leading segment with `_` so the first character is a
/// letter or underscore.
fn expression_legal_segment(value: &str) -> String {
    let segment: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    match segment.chars().next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => segment,
        Some(_) => format!("_{segment}"),
        None => segment,
    }
}

pub fn canonicalize_input_map(
    inputs: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut canonical = BTreeMap::new();
    for (name, value) in inputs {
        let canonical_name = runner_ordinal_ignore_case_key(name);
        if canonical
            .insert(canonical_name.clone(), value.clone())
            .is_some()
        {
            bail!("action input names differ only by case: {canonical_name}");
        }
    }
    Ok(canonical)
}

fn effective_inputs(
    metadata: &ActionMetadata,
    provided: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut inputs = BTreeMap::new();
    let mut declared_names = BTreeSet::new();
    for (name, input) in &metadata.inputs {
        let canonical_name = runner_ordinal_ignore_case_key(name);
        if canonical_name.is_empty() {
            bail!("action input names must not be empty");
        }
        if !declared_names.insert(canonical_name.clone()) {
            bail!("metadata input names differ only by case: {canonical_name}");
        }
        if let Some(value) = &input.default_value {
            inputs.insert(canonical_name, value.clone());
        } else {
            inputs.insert(canonical_name, String::new());
        }
    }
    inputs.extend(canonicalize_input_map(provided)?);
    Ok(inputs)
}

/// Match `ActionRunner`'s input diagnostics. `provided` must contain only the
/// caller's `with:` values; metadata defaults are merged after these warnings
/// are emitted upstream.
pub(crate) fn action_input_diagnostics(
    metadata: &ActionMetadata,
    provided: &BTreeMap<String, String>,
    reference_type: ActionReferenceType,
) -> Vec<String> {
    let mut warnings = Vec::new();

    for name in provided.keys() {
        // Runner's deprecated-input dictionary uses its default, case-sensitive
        // comparer. The valid/unexpected input sets below use OrdinalIgnoreCase.
        if let Some(message) = metadata
            .inputs
            .get(name)
            .and_then(|input| input.deprecation_message.as_deref())
        {
            warnings.push(format!(
                "Input '{name}' has been deprecated with message: {message}"
            ));
        }
    }

    if reference_type != ActionReferenceType::Repository {
        return warnings;
    }

    let mut valid_inputs = Vec::new();
    let mut valid_input_names = BTreeSet::new();
    if metadata.runs.using.eq_ignore_ascii_case("docker") {
        for name in ["entryPoint", "args"] {
            valid_input_names.insert(runner_ordinal_ignore_case_key(name));
            valid_inputs.push(name.to_owned());
        }
    }
    for name in metadata.inputs.keys() {
        let canonical_name = runner_ordinal_ignore_case_key(name);
        if valid_input_names.insert(canonical_name) {
            valid_inputs.push(name.clone());
        }
    }

    let mut seen_user_inputs = BTreeSet::new();
    let unexpected_inputs = provided
        .keys()
        .filter(|name| {
            let canonical_name = runner_ordinal_ignore_case_key(name);
            seen_user_inputs.insert(canonical_name.clone())
                && !valid_input_names.contains(&canonical_name)
        })
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected_inputs.is_empty() {
        warnings.push(format!(
            "Unexpected input(s) '{}', valid inputs are ['{}']",
            unexpected_inputs.join("', '"),
            valid_inputs.join("', '")
        ));
    }

    warnings
}

pub(crate) fn action_input_default_templates(metadata: &ActionMetadata) -> ActionTemplateMap {
    let mut defaults = BTreeMap::new();
    let mut expression_values = BTreeSet::new();
    for (name, input) in &metadata.inputs {
        let value = input.default_value.as_deref().unwrap_or_default();
        if value.contains("${{") {
            expression_values.insert(name.clone());
        }
        defaults.insert(name.clone(), value.to_owned());
    }
    ActionTemplateMap::from_btree_map(&defaults, &expression_values)
}

/// Merge an evaluated action `with` mapping over evaluated manifest defaults.
/// Runner assigns each item into its destination dictionary, so a key that
/// evaluates to an existing key overwrites the earlier value.
pub(crate) fn effective_action_inputs_from_pairs(
    defaults: &[(String, String)],
    provided: &[(String, String)],
) -> BTreeMap<String, String> {
    let mut inputs = BTreeMap::new();
    for (name, value) in defaults.iter().chain(provided) {
        inputs.insert(runner_ordinal_ignore_case_key(name), value.clone());
    }
    inputs
}

fn input_value_case_insensitive<'a>(
    inputs: &'a BTreeMap<String, String>,
    name: &str,
) -> Option<&'a String> {
    inputs.get(&runner_ordinal_ignore_case_key(name))
}

pub(crate) fn runner_ordinal_ignore_case_key(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character == '\u{03c2}' {
                return '\u{03c3}';
            }
            let mut lowercase = character.to_lowercase();
            match (lowercase.next(), lowercase.next()) {
                (Some(lowercase), None) => lowercase,
                _ => character,
            }
        })
        .collect()
}

pub(crate) fn workspace_path(workspace_container: &str, path: &str) -> String {
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
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
        assert_eq!(
            metadata.runs.args.as_deref(),
            Some(["${{ inputs.image }}".to_string()].as_slice())
        );
    }

    /// F4: docker `runs.pre-entrypoint` / `runs.post-entrypoint` (plus
    /// their `pre-if` / `post-if` conditions) parse — serde drops
    /// misspelled keys silently, so pin the wire names.
    #[test]
    fn parses_docker_action_pre_post_entrypoints() {
        let metadata = parse_action_metadata(
            r#"
runs:
  using: docker
  image: Dockerfile
  pre-entrypoint: /setup.sh
  entrypoint: /main.sh
  post-entrypoint: /cleanup.sh
  pre-if: success()
  post-if: always()
"#,
        )
        .unwrap();

        assert_eq!(metadata.runs.pre_entrypoint.as_deref(), Some("/setup.sh"));
        assert_eq!(
            metadata.runs.post_entrypoint.as_deref(),
            Some("/cleanup.sh")
        );
        assert_eq!(metadata.runs.pre_if.as_deref(), Some("success()"));
        assert_eq!(metadata.runs.post_if.as_deref(), Some("always()"));
    }

    #[test]
    fn metadata_parser_rejects_camel_case_runner_schema_aliases() {
        for metadata in [
            "runs:\n  using: node20\n  main: index.js\n  preIf: success()\n",
            "runs:\n  using: node20\n  main: index.js\n  postIf: always()\n",
            "runs:\n  using: docker\n  image: ubuntu\n  preEntrypoint: setup.sh\n",
            "runs:\n  using: docker\n  image: ubuntu\n  postEntrypoint: cleanup.sh\n",
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      workingDirectory: scripts\n",
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continueOnError: true\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_err(),
                "camelCase manifest alias must be rejected: {metadata}"
            );
        }
    }

    #[test]
    fn metadata_parser_distinguishes_missing_from_empty_composite_steps() {
        let metadata = parse_action_metadata("runs:\n  using: composite\n  steps: []\n").unwrap();
        assert!(metadata.runs.steps.is_empty());
        assert_eq!(metadata.runtime().unwrap(), ActionRuntime::Composite);

        for metadata in [
            "runs:\n  using: composite\n",
            "runs:\n  using: composite\n  steps: null\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_err(),
                "missing or null composite steps must be rejected: {metadata}"
            );
        }
    }

    #[test]
    fn metadata_parser_matches_case_insensitive_input_default_schema() {
        let metadata = parse_action_metadata(
            "inputs:\n  token:\n    Default: 7\n  enabled:\n    default: true\n  empty:\n    default: null\nruns:\n  using: node20\n  main: index.js\n",
        )
        .unwrap();
        assert_eq!(metadata.inputs["token"].default_value.as_deref(), Some("7"));
        assert_eq!(
            metadata.inputs["enabled"].default_value.as_deref(),
            Some("true")
        );
        assert_eq!(metadata.inputs["empty"].default_value.as_deref(), Some(""));

        for metadata in [
            "inputs: null\nruns:\n  using: node20\n  main: index.js\n",
            "outputs: null\nruns:\n  using: node20\n  main: index.js\n",
            "inputs:\n  token:\n    Default: []\nruns:\n  using: node20\n  main: index.js\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_err(),
                "malformed schema value must be rejected: {metadata}"
            );
        }
    }

    #[test]
    fn metadata_parser_rejects_runner_ordinal_ignore_case_input_duplicates() {
        for (first, second) in [("Σ", "ς"), ("ᾀ", "ᾈ")] {
            let metadata = format!(
                "inputs:\n  {first}:\n    default: first\n  {second}:\n    default: second\nruns:\n  using: node20\n  main: index.js\n"
            );
            assert!(
                parse_action_metadata(&metadata).is_err(),
                "Runner OrdinalIgnoreCase duplicate must be rejected: {first} / {second}"
            );
        }
    }

    #[test]
    fn metadata_parser_rejects_duplicate_mapping_keys_before_value_collapse() {
        for metadata in [
            "runs:\n  using: node20\n  main: index.js\nruns:\n  using: docker\n  image: ubuntu\n",
            "runs:\n  using: node20\n  main: index.js\nRUNS:\n  using: docker\n  image: ubuntu\n",
            "inputs:\n  token:\n    default: first\n  TOKEN:\n    default: second\nruns:\n  using: node20\n  main: index.js\n",
            "inputs:\n  token:\n    default: first\n    DEFAULT: second\nruns:\n  using: node20\n  main: index.js\n",
            "inputs:\n  token:\n    custom: {field: first, FIELD: second}\nruns:\n  using: node20\n  main: index.js\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_err(),
                "Runner must reject same-case and OrdinalIgnoreCase duplicate keys: {metadata}"
            );
        }
    }

    #[test]
    fn metadata_parser_rejects_numeric_equivalent_mapping_keys() {
        for metadata in [
            "extra: {1: first, 1.0: second}\nruns:\n  using: node20\n  main: index.js\n",
            "extra: {0x1: first, 1: second}\nruns:\n  using: node20\n  main: index.js\n",
            "extra: {!!int 1: first, 1.0: second}\nruns:\n  using: node20\n  main: index.js\n",
            "extra: {!!bool TRUE: first, true: second}\nruns:\n  using: node20\n  main: index.js\n",
            "extra: {!!str 1: first, 1: second}\nruns:\n  using: node20\n  main: index.js\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_err(),
                "Runner must reject numeric-equivalent keys before mapping collapse: {metadata}"
            );
        }
    }

    #[test]
    fn metadata_parser_rejects_unknown_tags_and_accepts_runner_tags() {
        for metadata in [
            "extra: !custom value\nruns:\n  using: node20\n  main: index.js\n",
            "inputs:\n  token:\n    extra: !custom value\nruns:\n  using: node20\n  main: index.js\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_err(),
                "Runner must reject unsupported tags in loose fields: {metadata}"
            );
        }

        assert!(parse_action_metadata(
            "extra: !!str value\nruns:\n  using: node20\n  main: index.js\n"
        )
        .is_ok());
        assert!(parse_action_metadata(
            "extra: [!!str text, !!int 7, !!float 1.5, !!bool true, !!null null, !!map {inner: value}, !!seq [value]]\nruns:\n  using: node20\n  main: index.js\n"
        )
        .is_ok());
        assert!(parse_action_metadata(
            "extra: !custom [value]\nruns:\n  using: node20\n  main: index.js\n"
        )
        .is_ok());
        assert!(parse_action_metadata(
            "inputs:\n  token:\n    extra: !custom {nested: value}\nruns:\n  using: node20\n  main: index.js\n"
        )
        .is_ok());
        assert!(
            parse_action_metadata("runs: !custom\n  using: node20\n  main: index.js\n").is_ok()
        );
        let explicit_core_tags = parse_action_metadata(
            "inputs:\n  text:\n    default: !!str 7\n  flag:\n    default: !!bool TRUE\n  empty:\n    default: !!null null\nruns:\n  using: !!str node20\n  main: !!str index.js\n",
        )
        .unwrap();
        assert_eq!(
            explicit_core_tags.inputs["text"].default_value.as_deref(),
            Some("7")
        );
        assert_eq!(
            explicit_core_tags.inputs["flag"].default_value.as_deref(),
            Some("true")
        );
        assert_eq!(
            explicit_core_tags.inputs["empty"].default_value.as_deref(),
            Some("")
        );
        for metadata in [
            "extra: !!seq value\nruns:\n  using: node20\n  main: index.js\n",
            "extra: !!map value\nruns:\n  using: node20\n  main: index.js\n",
            "extra: !!bool \"true\"\nruns:\n  using: node20\n  main: index.js\n",
            "extra: !!null \"null\"\nruns:\n  using: node20\n  main: index.js\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_err(),
                "Runner must reject unsupported scalar tag/style: {metadata}"
            );
        }

        let normalized = normalize_runner_yaml_numbers(
            "runs:\n  using: node20\n  main: index.js\nextra: 1.25\n",
        )
        .unwrap();
        assert!(normalized.contains("!velnor-runner-number"));
        assert!(validate_runner_action_yaml_syntax(&normalized).is_ok());
    }

    #[test]
    fn metadata_parser_matches_runner_depth_event_and_size_limits() {
        let depth_100 = format!(
            "{}null{}",
            "[".repeat(MAX_METADATA_PARSE_NESTING),
            "]".repeat(MAX_METADATA_PARSE_NESTING)
        );
        let depth_101 = format!(
            "{}null{}",
            "[".repeat(MAX_METADATA_PARSE_NESTING + 1),
            "]".repeat(MAX_METADATA_PARSE_NESTING + 1)
        );
        assert!(serde_yaml::from_str::<RunnerYamlValidation>(&depth_100).is_ok());
        assert!(serde_yaml::from_str::<RunnerYamlValidation>(&depth_101).is_err());

        assert_eq!(MAX_ACTION_METADATA_EVENTS, 1_000_000);
        const SMALL_EVENT_LIMIT: usize = 4;
        type SmallEventValidation = RunnerYamlValidation<SMALL_EVENT_LIMIT>;
        let at_limit = "[[null, null]]";
        assert_eq!(
            serde_yaml::from_str::<SmallEventValidation>(at_limit)
                .unwrap()
                .0,
            SMALL_EVENT_LIMIT
        );
        let over_limit = "[null, null, null, null]";
        assert!(serde_yaml::from_str::<SmallEventValidation>(over_limit).is_err());

        let normalized = normalize_runner_yaml_numbers(
            "runs:\n  using: node20\n  main: index.js\nextra: \"\"\n",
        )
        .unwrap();
        let value =
            normalize_runner_action_core_tags(serde_yaml::from_str(&normalized).unwrap()).unwrap();
        let base_bytes =
            runner_action_manifest_value_memory(&value, RunnerActionManifestValueKind::Root)
                .unwrap();
        let available_bytes = MAX_ACTION_TEMPLATE_MEMORY_BYTES - base_bytes;
        assert_eq!(available_bytes % 2, 0);
        let payload_len = available_bytes / 2;
        let mut at_limit = value.clone();
        at_limit.as_mapping_mut().unwrap().insert(
            "extra".to_owned(),
            serde_yaml::Value::String("x".repeat(payload_len)),
        );
        assert_eq!(
            runner_action_manifest_value_memory(&at_limit, RunnerActionManifestValueKind::Root)
                .unwrap(),
            MAX_ACTION_TEMPLATE_MEMORY_BYTES
        );
        let mut over_limit = at_limit;
        over_limit.as_mapping_mut().unwrap().insert(
            "extra".to_owned(),
            serde_yaml::Value::String("x".repeat(payload_len + 1)),
        );
        assert!(runner_action_manifest_value_memory(
            &over_limit,
            RunnerActionManifestValueKind::Root
        )
        .is_err());

        let comment_only_bytes = MAX_ACTION_TEMPLATE_MEMORY_BYTES + 1;
        let large_comment = format!(
            "runs:\n  using: node20\n  main: index.js\n#{}",
            "x".repeat(comment_only_bytes)
        );
        assert!(parse_action_metadata(&large_comment).is_ok());
    }

    #[test]
    fn metadata_parser_matches_runner_inline_format_limits_and_boolean_templates() {
        let context = ActionExpressionContext::CompositeString;
        let empty_format = runner_inline_format_expression(&[RunnerInlineSegment::Expression(
            "inputs.value".to_owned(),
        )]);
        let literal_at_limit = "x".repeat(velnor_expression::MAX_LENGTH - empty_format.len());
        let at_limit = format!("{literal_at_limit}${{{{ inputs.value }}}}");
        let RunnerTemplateScalar::Expression(expression) =
            runner_template_scalar(&at_limit, context).unwrap()
        else {
            panic!("mixed template must compile to an inline format expression")
        };
        assert_eq!(
            expression.encode_utf16().count(),
            velnor_expression::MAX_LENGTH
        );

        let over_limit = format!("x{at_limit}");
        assert!(runner_template_scalar(&over_limit, context).is_err());

        let max_arguments = "${{ inputs.value }}".repeat(254);
        let too_many_arguments = "${{ inputs.value }}".repeat(255);
        assert!(runner_template_scalar(&max_arguments, context).is_ok());
        assert!(runner_template_scalar(&too_many_arguments, context).is_err());

        let mixed_boolean = parse_action_metadata(
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: \"value: ${{ inputs.enabled }}\"\n",
        )
        .unwrap();
        assert!(matches!(
            mixed_boolean.runs.steps[0].continue_on_error,
            Some(ActionBooleanValue::Expression(ref expression)) if expression.starts_with("format('")
        ));
        assert!(parse_action_metadata(
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: \"${{ inputs.enabled }}\"\n"
        )
        .is_ok());
        assert!(parse_action_metadata(
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: \"${{ 'constant' }}\"\n"
        )
        .is_err());
    }

    #[test]
    fn metadata_parser_rejects_basic_expressions_without_runner_context() {
        for (field, metadata) in [
            (
                "root name",
                "name: \"${{ github.ref }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "root loose value",
                "extra: {nested: \"${{ github.ref }}\"}\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "input loose metadata",
                "inputs:\n  token:\n    custom: \"${{ github.ref }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "input deprecation message",
                "inputs:\n  token:\n    deprecationMessage: \"${{ github.ref }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "output description",
                "outputs:\n  result:\n    description: \"${{ github.ref }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "node main",
                "runs:\n  using: node20\n  main: \"${{ github.ref }}\"\n",
            ),
            (
                "docker image",
                "runs:\n  using: docker\n  image: \"${{ github.ref }}\"\n",
            ),
            (
                "composite uses",
                "runs:\n  using: composite\n  steps:\n    - uses: \"${{ github.ref }}\"\n",
            ),
            (
                "composite id",
                "runs:\n  using: composite\n  steps:\n    - uses: owner/action@v1\n      id: \"${{ github.ref }}\"\n",
            ),
        ] {
            let error = parse_action_metadata(metadata)
                .expect_err("basic expression must fail with an empty AllowedContext");
            assert!(
                format!("{error:#}").contains("cannot contain a template expression"),
                "wrong diagnostic for {field}: {error:#}"
            );
        }

        assert!(parse_action_metadata(
            "name: \"${{ 'constant string' }}\"\nruns:\n  using: node20\n  main: index.js\n"
        )
        .is_ok());
    }

    #[test]
    fn metadata_parser_uses_runner_g15_for_numeric_string_fields() {
        let metadata = parse_action_metadata(
            "inputs:\n  precise:\n    Default: 1.2345678901234567\n  exponent:\n    default: 1e15\n  tiny:\n    default: 1e-5\n  negative-zero:\n    default: -0\n  hex:\n    default: 0xFFFFFFFF\n  octal:\n    default: 0o10\n  binary:\n    default: 0b101\nruns:\n  using: node20\n  main: index.js\n",
        )
        .unwrap();
        assert_eq!(
            metadata.inputs["precise"].default_value.as_deref(),
            Some("1.23456789012346")
        );
        assert_eq!(
            metadata.inputs["exponent"].default_value.as_deref(),
            Some("1E+15")
        );
        assert_eq!(
            metadata.inputs["tiny"].default_value.as_deref(),
            Some("1E-05")
        );
        assert_eq!(
            metadata.inputs["negative-zero"].default_value.as_deref(),
            Some("-0")
        );
        assert_eq!(metadata.inputs["hex"].default_value.as_deref(), Some("-1"));
        assert_eq!(metadata.inputs["octal"].default_value.as_deref(), Some("8"));
        assert_eq!(
            metadata.inputs["binary"].default_value.as_deref(),
            Some("0b101")
        );
    }

    #[test]
    fn input_metadata_fields_follow_runner_loose_value_schema() {
        let metadata = parse_action_metadata(
            "inputs:\n  token:\n    description: []\n    required: {unexpected: shape}\n    custom-field: [ignored]\n    DeprecationMessage: Upgrade to token-v2\n    default: value\nruns:\n  using: node20\n  main: index.js\n",
        )
        .expect("Runner ignores non-default input metadata fields");
        assert_eq!(metadata.inputs["token"].description, None);
        assert_eq!(
            metadata.inputs["token"].default_value.as_deref(),
            Some("value")
        );
        assert_eq!(
            metadata.inputs["token"].deprecation_message.as_deref(),
            Some("Upgrade to token-v2")
        );
    }

    #[test]
    fn action_input_diagnostics_match_runner_deprecation_and_unexpected_warnings() {
        let metadata = parse_action_metadata(
            "inputs:\n  token:\n    default: fallback\n    deprecationMessage: Upgrade to token-v2\nruns:\n  using: node20\n  main: index.js\n",
        )
        .unwrap();
        let provided = BTreeMap::from([
            ("Surprise".to_owned(), "ignored".to_owned()),
            ("token".to_owned(), "deprecated input".to_owned()),
        ]);

        assert_eq!(
            action_input_diagnostics(&metadata, &provided, ActionReferenceType::Repository),
            [
                "Input 'token' has been deprecated with message: Upgrade to token-v2",
                "Unexpected input(s) 'Surprise', valid inputs are ['token']"
            ]
        );
        assert!(action_input_diagnostics(
            &metadata,
            &BTreeMap::from([(
                "TOKEN".to_owned(),
                "case-insensitive valid input".to_owned()
            )]),
            ActionReferenceType::Repository
        )
        .is_empty());
        assert_eq!(
            action_input_diagnostics(&metadata, &provided, ActionReferenceType::ContainerRegistry),
            ["Input 'token' has been deprecated with message: Upgrade to token-v2"]
        );
    }

    #[test]
    fn docker_action_input_diagnostics_accept_runner_reserved_inputs() {
        let metadata = parse_action_metadata(
            "inputs:\n  token:\n    default: ''\nruns:\n  using: docker\n  image: Dockerfile\n",
        )
        .unwrap();
        let provided = BTreeMap::from([
            ("args".to_owned(), "--flag".to_owned()),
            ("entryPoint".to_owned(), "/tool".to_owned()),
            ("unknown".to_owned(), "ignored".to_owned()),
        ]);

        assert_eq!(
            action_input_diagnostics(&metadata, &provided, ActionReferenceType::Repository),
            ["Unexpected input(s) 'unknown', valid inputs are ['entryPoint', 'args', 'token']"]
        );
    }

    #[test]
    fn invocation_builders_attach_input_diagnostics_to_caller_inputs() {
        let actions_host = Path::new("/tmp/actions");
        let action_dir = actions_host.join("_actions/acme/tool/v1");
        let provided = BTreeMap::from([
            ("Surprise".to_owned(), "ignored".to_owned()),
            ("token".to_owned(), "caller value".to_owned()),
        ]);
        let metadata = parse_action_metadata(
            "inputs:\n  token:\n    default: fallback\n    deprecationMessage: use token-v2\nruns:\n  using: node20\n  main: index.js\n",
        )
        .unwrap();
        let runtime = metadata.runtime().unwrap();
        let resolved = ResolvedAction {
            plan: RepositoryActionPlan {
                step_id: "tool".to_owned(),
                repository: "acme/tool".to_owned(),
                git_ref: "v1".to_owned(),
                repository_dir: action_dir.clone(),
                action_dir: action_dir.clone(),
                inputs: provided.clone(),
                ..Default::default()
            },
            metadata_path: action_dir.join("action.yml"),
            metadata,
            runtime,
        };
        let javascript = resolved.javascript_invocation(actions_host).unwrap();
        assert_eq!(
            javascript.input_diagnostics,
            [
                "Input 'token' has been deprecated with message: use token-v2",
                "Unexpected input(s) 'Surprise', valid inputs are ['token']",
            ]
        );

        let docker_metadata = parse_action_metadata(
            "inputs:\n  token:\n    default: fallback\n    deprecationMessage: use token-v2\nruns:\n  using: docker\n  image: docker://alpine:3.20\n",
        )
        .unwrap();
        let docker_runtime = docker_metadata.runtime().unwrap();
        let docker_resolved = ResolvedAction {
            plan: RepositoryActionPlan {
                step_id: "container".to_owned(),
                repository: "acme/container".to_owned(),
                git_ref: "v1".to_owned(),
                repository_dir: action_dir.clone(),
                action_dir: action_dir.clone(),
                inputs: BTreeMap::from([
                    ("args".to_owned(), "--flag".to_owned()),
                    ("entryPoint".to_owned(), "/tool".to_owned()),
                    ("Surprise".to_owned(), "ignored".to_owned()),
                    ("token".to_owned(), "caller value".to_owned()),
                ]),
                ..Default::default()
            },
            metadata_path: action_dir.join("action.yml"),
            metadata: docker_metadata.clone(),
            runtime: docker_runtime,
        };
        let docker = docker_resolved.docker_invocation(actions_host).unwrap();
        assert_eq!(
            docker.input_diagnostics,
            [
                "Input 'token' has been deprecated with message: use token-v2",
                "Unexpected input(s) 'Surprise', valid inputs are ['entryPoint', 'args', 'token']",
            ]
        );

        let local = local_docker_invocation(
            &LocalActionPlan {
                step_id: "local".to_owned(),
                action_dir: PathBuf::from("/tmp/workspace/.github/actions/local"),
                workspace_host: PathBuf::from("/tmp/workspace"),
                inputs: provided.clone(),
                expression_inputs: BTreeSet::new(),
            },
            &docker_metadata,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            local.input_diagnostics,
            ["Input 'token' has been deprecated with message: use token-v2"]
        );

        let native_metadata = parse_action_metadata(
            "inputs:\n  token:\n    deprecationMessage: use token-v2\nruns:\n  using: node20\n  main: index.js\n",
        )
        .unwrap();
        let native_runtime = native_metadata.runtime().unwrap();
        let native_resolved = ResolvedAction {
            plan: RepositoryActionPlan {
                step_id: "cache".to_owned(),
                repository: "actions/cache".to_owned(),
                git_ref: "v5".to_owned(),
                inputs: provided,
                ..Default::default()
            },
            metadata_path: PathBuf::from("/tmp/actions/action.yml"),
            metadata: native_metadata,
            runtime: native_runtime,
        };
        let native = native_resolved.native_invocation().unwrap().unwrap();
        assert_eq!(
            native.input_diagnostics,
            [
                "Input 'token' has been deprecated with message: use token-v2",
                "Unexpected input(s) 'Surprise', valid inputs are ['token']",
            ]
        );
    }

    #[test]
    fn metadata_parser_enforces_field_specific_runner_expression_contexts() {
        for metadata in [
            "inputs:\n  token:\n    default: \"${{ inputs.token }}\"\nruns:\n  using: node20\n  main: index.js\n",
            "runs:\n  using: docker\n  image: ubuntu\n  args: [\"${{ env.PATH }}\"]\n",
            "runs:\n  using: docker\n  image: ubuntu\n  env:\n    TOKEN: \"${{ env.PATH }}\"\n",
            "runs:\n  using: composite\n  steps:\n    - run: \"${{ secrets.token }}\"\n      shell: bash\n",
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: \"${{ secrets.token }}\"\n",
            "runs:\n  using: composite\n  steps:\n    - name: \"${{ secrets.token }}\"\n      run: echo ok\n      shell: bash\n",
            "runs:\n  using: composite\n  steps:\n    - if: \"${{ secrets.token }}\"\n      run: echo ok\n      shell: bash\n",
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      working-directory: \"${{ secrets.token }}\"\n",
            "runs:\n  using: composite\n  steps:\n    - uses: owner/repo@0123456789012345678901234567890123456789\n      with:\n        value: \"${{ secrets.token }}\"\n",
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      env:\n        TOKEN: \"${{ secrets.token }}\"\n",
            "inputs:\n  \"${{ runner.os }}\":\n    default: value\nruns:\n  using: node20\n  main: index.js\n",
            "runs:\n  using: docker\n  image: ubuntu\n  env:\n    \"${{ env.PATH }}\": value\n",
            "runs:\n  using: composite\n  steps:\n    - uses: owner/repo@0123456789012345678901234567890123456789\n      with:\n        \"${{ secrets.token }}\": value\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_err(),
                "expression must fail in the field's Runner context: {metadata}"
            );
        }

        for metadata in [
            "runs:\n  using: docker\n  image: ubuntu\n  env:\n    \"${{ inputs.name }}\": value\n",
            "runs:\n  using: composite\n  steps:\n    - uses: owner/repo@0123456789012345678901234567890123456789\n      with:\n        \"${{ inputs.name }}\": value\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_ok(),
                "Runner-valid dynamic mapping keys must remain in the parsed template map"
            );
        }

        for metadata in [
            "inputs:\n  token:\n    default: \"${{ runner.os }}-${{ hashFiles('**') }}\"\nruns:\n  using: node20\n  main: index.js\n",
            "runs:\n  using: docker\n  image: ubuntu\n  args: [\"${{ inputs.tag }}\"]\n  env:\n    TAG: \"${{ inputs.tag }}\"\n",
            "runs:\n  using: composite\n  steps:\n    - run: \"${{ inputs.command }}\"\n      shell: \"${{ runner.os }}\"\n      name: \"${{ inputs.label }}\"\n      working-directory: \"${{ inputs.directory }}\"\n      env:\n        VALUE: \"${{ inputs.value }}\"\n",
            "runs:\n  using: composite\n  steps:\n    - if: \"${{ success() && hashFiles('**') == '' }}\"\n      run: echo ok\n      shell: bash\n",
            "outputs:\n  result:\n    value: \"${{ steps.build.outputs.result }}-${{ env.PATH }}\"\nruns:\n  using: composite\n  steps:\n    - id: build\n      run: echo ok\n      shell: bash\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_ok(),
                "expression must pass in the field's Runner context: {metadata}"
            );
        }
    }

    #[test]
    fn dynamic_action_mapping_keys_keep_order_and_expression_provenance() {
        let docker = parse_action_metadata(
            "runs:\n  using: docker\n  image: ubuntu\n  env:\n    FIRST: one\n    \"${{ inputs.name }}_TOKEN\": \"${{ inputs.value }}\"\n    LAST: three\n",
        )
        .unwrap();
        let entries = docker.runs.env.iter().collect::<Vec<_>>();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].key, "FIRST");
        assert!(!entries[0].key_is_template);
        assert_eq!(entries[1].key, "${{ inputs.name }}_TOKEN");
        assert!(entries[1].key_is_template);
        assert!(entries[1].value_is_template);
        assert_eq!(entries[2].key, "LAST");

        let composite = parse_action_metadata(
            "runs:\n  using: composite\n  steps:\n    - uses: owner/repo@0123456789012345678901234567890123456789\n      with:\n        \"${{ inputs.name }}\": \"${{ inputs.value }}\"\n      env:\n        \"${{ inputs.env_name }}\": \"${{ inputs.env_value }}\"\n",
        )
        .unwrap();
        let step = &composite.runs.steps[0];
        assert!(step.with.iter().next().unwrap().key_is_template);
        assert!(step.with.iter().next().unwrap().value_is_template);
        assert!(step.env.iter().next().unwrap().key_is_template);
        assert!(step.env.iter().next().unwrap().value_is_template);

        let repeated_expression_key = parse_action_metadata(
            "runs:\n  using: docker\n  image: ubuntu\n  env:\n    \"${{ inputs.name }}\": first\n    \"${{ inputs.name }}\": second\n",
        )
        .unwrap();
        assert_eq!(
            repeated_expression_key.runs.env.iter().count(),
            2,
            "the ordered template map must preserve duplicate expression keys until runtime collision validation"
        );
    }

    #[test]
    fn metadata_parser_explains_plugin_runtime_is_unsupported() {
        let error = parse_action_metadata("runs:\n  plugin: checkout\n").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("plugin action runtime is unsupported by the Velnor executor"),
            "{error}"
        );
        assert!(parse_action_metadata(
            "runs:\n  plugin: checkout\n  using: node20\n  main: index.js\n"
        )
        .is_err());
    }

    #[test]
    fn metadata_parser_rejects_runner_schema_mismatches() {
        for metadata in [
            "name: []\nruns:\n  using: node20\n  main: index.js\n",
            "inputs:\n  token: value\nruns:\n  using: node20\n  main: index.js\n",
            "outputs:\n  result: value\nruns:\n  using: node20\n  main: index.js\n",
            "outputs:\n  result:\n    Description: result\nruns:\n  using: node20\n  main: index.js\n",
            "runs:\n  using: node20\n  main: index.js\n  args: []\n",
            "runs:\n  using: node20\n  main: index.js\n  env: { VALUE: ok }\n",
            "runs:\n  using: docker\n  image: null\n",
            "runs:\n  using: docker\n  image: alpine\n  args: false\n",
            "runs:\n  using: docker\n  image: alpine\n  env: null\n",
            "runs:\n  using: node20\n  main: index.js\n  pre-if: ''\n",
            "runs:\n  using: node20\n  main: index.js\n  post-if: null\n",
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      unexpected: value\n",
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n",
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      env:\n        VALUE: []\n",
            "runs:\n  using: composite\n  steps:\n    - uses: ./child\n      with: value\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_err(),
                "Runner schema mismatch must be rejected: {metadata}"
            );
        }
    }

    #[test]
    fn metadata_parser_rejects_runner_yaml_aliases_and_collection_keys() {
        for metadata in [
            "other: &value shared\nruns:\n  using: node20\n  main: index.js\n",
            "other: &value shared\nalias: *value\nruns:\n  using: node20\n  main: index.js\n",
            "? [complex, key]\n: value\nruns:\n  using: node20\n  main: index.js\n",
        ] {
            assert!(
                parse_action_metadata(metadata).is_err(),
                "Runner YAML reader must reject anchors, aliases, and collection keys: {metadata}"
            );
        }
    }

    #[test]
    fn action_runtime_does_not_trim_runs_using() {
        let error =
            parse_action_metadata("runs:\n  using: ' node20 '\n  main: index.js\n").unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported action runtime ` node20 `"));
    }

    #[test]
    fn metadata_parser_matches_boolean_steps_context_for_continue_on_error() {
        for value in [
            "'maybe'",
            "'false'",
            "\"${{ inputs.flag + }}\"",
            "\"${{ success() }}\"",
            "\"${{ secrets.token }}\"",
            "\"${{ mystery() }}\"",
        ] {
            let metadata = format!(
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: {value}\n"
            );
            assert!(
                parse_action_metadata(&metadata).is_err(),
                "invalid continue-on-error must be rejected: {value}"
            );
        }

        for value in [
            "true",
            "false",
            "\"${{ inputs.flag }}\"",
            "\"${{ hashFiles('**') }}\"",
        ] {
            let metadata = format!(
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: {value}\n"
            );
            assert!(
                parse_action_metadata(&metadata).is_ok(),
                "valid continue-on-error must be accepted: {value}"
            );
        }
    }

    #[test]
    fn metadata_parser_rejects_excessive_nesting_before_yaml_parse() {
        let nested = format!("{}true{}", "[".repeat(65), "]".repeat(65));
        assert!(parse_action_metadata(&nested).is_err());
    }

    #[test]
    fn metadata_parser_ignores_quoted_and_block_scalar_delimiters() {
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - shell: bash
      run: |
        echo "{ this is shell text }"
        echo '[ still shell text ]'
"#,
        )
        .unwrap();
        assert_eq!(metadata.runs.steps.len(), 1);
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
        assert_eq!(metadata.runs.steps[0].env.get("BOOL_VALUE"), Some("false"));
        assert_eq!(metadata.runs.steps[0].env.get("COUNT"), Some("7"));
        assert_eq!(metadata.runs.steps[1].with.get("cleanup"), Some("false"));
        assert_eq!(metadata.runs.steps[1].with.get("retries"), Some("3"));
    }

    #[test]
    fn composite_inputs_are_canonicalized_for_case_insensitive_rendering() {
        let metadata = parse_action_metadata(
            r#"
inputs:
  lookup-only:
    default: false
runs:
  using: composite
  steps:
    - run: echo ${{ inputs.LOOKUP-ONLY }}
      shell: bash
"#,
        )
        .unwrap();
        let provided = [("LOOKUP-ONLY".to_string(), "true".to_string())]
            .into_iter()
            .collect();
        let inputs = effective_inputs(&metadata, &provided).unwrap();
        assert_eq!(inputs.get("lookup-only").map(String::as_str), Some("true"));
        assert_eq!(
            render_composite_value("${{ inputs.LOOKUP-ONLY }}", &inputs, "/__a", "/__w").unwrap(),
            "true"
        );
    }

    #[test]
    fn canonical_input_map_rejects_case_collisions() {
        let inputs = [
            ("lookup-only".to_string(), "true".to_string()),
            ("LOOKUP-ONLY".to_string(), "false".to_string()),
        ]
        .into_iter()
        .collect();
        assert!(canonicalize_input_map(&inputs).is_err());
    }

    #[test]
    fn canonical_input_map_matches_runner_ordinal_ignore_case() {
        for (left, right) in [("ΟΣ", "Ος"), ("ᾀ", "ᾈ")] {
            let inputs = [
                (left.to_owned(), "first".to_owned()),
                (right.to_owned(), "second".to_owned()),
            ]
            .into_iter()
            .collect();
            assert!(
                canonicalize_input_map(&inputs).is_err(),
                "Runner OrdinalIgnoreCase must collide {left:?} with {right:?}"
            );
        }
    }

    #[test]
    fn cache_subpaths_are_case_sensitive() {
        assert_eq!(
            cache_action_kind(Some("restore")).unwrap(),
            CacheActionKind::Restore
        );
        assert!(cache_action_kind(Some("Restore")).is_err());
    }

    #[test]
    fn composite_replacement_preserves_shell_literals_and_quoted_strings() {
        let inputs = [("name".to_string(), "velnor".to_string())]
            .into_iter()
            .collect();
        assert_eq!(
            render_composite_value(
                "echo inputs.name; echo ${{ format('inputs.name', inputs.name) }}",
                &inputs,
                "/__a",
                "/__w"
            )
            .unwrap(),
            "echo inputs.name; echo ${{ format('inputs.name', 'velnor') }}"
        );
    }

    #[test]
    fn composite_expression_rendering_is_utf8_safe() {
        let inputs = [("name".to_string(), "velnor".to_string())]
            .into_iter()
            .collect();

        for expression in ["'日本語です'", "'αααα'", "'ünïcödé'"] {
            assert_eq!(
                render_composite_value(
                    &format!("${{{{ {expression} }}}}"),
                    &inputs,
                    "/__a",
                    "/__w"
                )
                .unwrap(),
                format!("${{{{ {expression} }}}}")
            );
        }
        assert_eq!(
            render_composite_value("日本語 ${{ inputs.name }}", &inputs, "/__a", "/__w").unwrap(),
            "日本語 velnor"
        );
        assert!(render_composite_value("${{ 日本語です }}", &inputs, "/__a", "/__w").is_err());
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
    fn native_repository_action_plan_requires_ref() {
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

        let error = repository_action_plans(&steps, Path::new("/tmp/actions")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "repository action 'actions/cache' missing ref"
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
    fn local_action_directory_accepts_runner_dot_backslash_reference() {
        let workspace = Path::new("/tmp/workspace");
        let expected = workspace.join(".github/actions/local");
        assert_eq!(
            local_action_dir(workspace, "./.github/actions/local").unwrap(),
            expected
        );
        assert_eq!(
            local_action_dir(workspace, ".\\.github\\actions\\local").unwrap(),
            expected
        );
        assert!(local_action_dir(workspace, "./../outside").is_err());
    }

    #[test]
    fn local_action_plan_anchors_dot_backslash_to_workspace() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([{
            "id": "local",
            "reference": {
                "type": "Repository",
                "name": ".\\.github\\actions\\local"
            }
        }]))
        .unwrap();
        let workspace = Path::new("/tmp/workspace");

        let plans = local_action_plans(&steps, workspace).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].workspace_host, workspace);
        assert_eq!(plans[0].action_dir, workspace.join(".github/actions/local"));
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
            "{\n  \"check\": {\n    \"result\": \"success\"\n  }\n}"
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
            "{\n  \"build\": {\n    \"result\": \"failure\"\n  },\n  \"check\": {\n    \"result\": \"success\"\n  }\n}"
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
            workspace_host: Path::new("/tmp/workspace").into(),
            inputs: [("workflow-label".to_string(), "CI".to_string())].into(),
            expression_inputs: BTreeSet::new(),
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
        assert_eq!(
            steps[0].condition.as_deref(),
            Some("${{ inputs.workflow-label == 'CI' }}")
        );
        assert_eq!(
            steps[0].env,
            vec![
                (
                    "WORKFLOW_LABEL".into(),
                    "${{ inputs.workflow-label }}".into()
                ),
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__w/.github/actions/aggregate-needs".into()
                )
            ]
        );
        assert!(steps[0]
            .script
            .contains("::error::${{ inputs.workflow-label }} failed"));
        assert!(steps[0]
            .script
            .contains("test -d \"${{ github.action_path }}\""));
    }

    struct ExpressionParseEnv;

    impl velnor_expression::ParseEnvironment for ExpressionParseEnv {
        fn is_named_value(&self, name: &str) -> bool {
            crate::expression::ROOT_CONTEXTS
                .iter()
                .any(|root| root.eq_ignore_ascii_case(name))
        }

        fn function_arity(&self, name: &str) -> Option<(usize, usize)> {
            crate::expression::RUNNER_FUNCTIONS
                .iter()
                .find(|(known, _, _)| known.eq_ignore_ascii_case(name))
                .map(|(_, min, max)| (*min, *max))
        }
    }

    /// A composite step id is only usable if rendered references such as
    /// `steps.<id>.outputs.checked` survive `ExpressionUtility.IsLegalKeyword`
    /// and the parser behind it.
    fn assert_expression_identifier(id: &str) {
        assert!(
            velnor_expression::is_legal_keyword(id),
            "`{id}` must lex as an expression identifier"
        );
        assert!(
            matches!(
                velnor_expression::parse(
                    &format!("steps.{id}.outputs.checked"),
                    &ExpressionParseEnv
                ),
                Ok(Some(_))
            ),
            "`steps.{id}.outputs.checked` must parse"
        );
    }

    #[test]
    fn composite_step_ids_stay_expression_legal_without_a_yaml_step_id() {
        // No ContextName, so step_id() falls back to the internal job-message
        // GUID. A GUID starts with a digit, which used to render references
        // like `steps.26f576d4-...-check.outputs.checked` that fail expression
        // parsing at byte 0.
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "26f576d4-0368-407e-817d-c34c2f1e8103",
                "reference": {
                    "type": "Repository",
                    "name": "./.github/actions/aggregate-needs"
                }
            }
        ]))
        .unwrap();

        let plans = local_action_plans(&steps, Path::new("/tmp/workspace")).unwrap();
        assert_eq!(plans.len(), 1);

        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - id: check
      shell: bash
      run: echo checked
"#,
        )
        .unwrap();

        let expanded = composite_script_steps(&plans[0], &metadata, "/__w").unwrap();
        assert_eq!(expanded.len(), 1);
        assert_eq!(
            expanded[0].id,
            "_26f576d4-0368-407e-817d-c34c2f1e8103-check"
        );
        assert_expression_identifier(&expanded[0].id);
    }

    #[test]
    fn composite_step_ids_map_illegal_characters_and_digit_leading_ids() {
        // Digit-leading prefix (GUID fallback for the parent step).
        assert_eq!(
            composite_step_id("0abc1234", Some("check"), 0),
            "_0abc1234-check"
        );
        // Dots are not legal inside identifiers, unlike in sanitize_segment.
        assert_eq!(
            composite_step_id("aggregate", Some("a.b"), 0),
            "aggregate-a_b"
        );
        // Already-legal ids and the positional fallback stay untouched.
        assert_eq!(
            composite_step_id("aggregate", Some("check"), 0),
            "aggregate-check"
        );
        assert_eq!(composite_step_id("aggregate", None, 2), "aggregate-3");
        // A trailing dash still falls back to the positional id.
        assert_eq!(
            composite_step_id("aggregate", Some("check-"), 0),
            "aggregate-1"
        );

        for id in ["_0abc1234-check", "aggregate-a_b", "aggregate-3"] {
            assert_expression_identifier(id);
        }
    }

    #[test]
    fn target_aggregate_needs_expands_exact_failure_gate() {
        let plan = LocalActionPlan {
            step_id: "aggregate".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/aggregate-needs"),
            workspace_host: Path::new("/tmp/workspace").into(),
            inputs: [
                (
                    "needs-json".to_string(),
                    r#"{"check":{"result":"success"},"build":{"result":"cancelled"}}"#.to_string(),
                ),
                ("workflow-label".to_string(), "CI".to_string()),
            ]
            .into(),
            expression_inputs: BTreeSet::new(),
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
        assert!(steps[0]
            .env
            .contains(&("NEEDS_RESULT".into(), "${{ inputs.needs-json }}".into())));
        assert!(steps[0].env.contains(&(
            "WORKFLOW_LABEL".into(),
            "${{ inputs.workflow-label }}".into()
        )));
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
            workspace_host: Path::new("/tmp/workspace").into(),
            inputs: BTreeMap::new(),
            expression_inputs: BTreeSet::new(),
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
                (
                    "EXTERNAL_LINKS".into(),
                    "${{ inputs.external-links }}".into()
                ),
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__w/.github/actions/check-deployed-docs".into()
                )
            ]
        );
        assert!(steps[0]
            .script
            .contains("echo \"${{ inputs.external-links }}\""));
    }

    #[test]
    fn target_check_deployed_docs_keeps_sitemap_step_output_input() {
        let plan = LocalActionPlan {
            step_id: "check-deployed".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/check-deployed-docs"),
            workspace_host: Path::new("/tmp/workspace").into(),
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
            expression_inputs: BTreeSet::new(),
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

        assert!(steps[0]
            .env
            .contains(&("SITEMAP_URL".into(), "${{ inputs.sitemap-url }}".into())));
        assert!(steps[0]
            .env
            .contains(&("GITHUB_TOKEN".into(), "${{ inputs.github-token }}".into())));
        assert!(steps[0].env.contains(&(
            "EXTERNAL_LINKS".into(),
            "${{ inputs.external-links }}".into()
        )));
        assert!(steps[0].env.contains(&(
            "GITHUB_ACTION_PATH".into(),
            "/__w/.github/actions/check-deployed-docs".into()
        )));
        assert!(steps[0]
            .script
            .contains(r#"lychee --dump "${{ inputs.sitemap-url }}""#));
        assert!(steps[0]
            .script
            .contains(r#"file://${{ github.workspace }}/\$1"#));
    }

    #[test]
    fn expands_composite_outputs_from_inner_step_outputs() {
        let plan = LocalActionPlan {
            step_id: "pages".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/pages"),
            workspace_host: Path::new("/tmp/workspace").into(),
            inputs: BTreeMap::new(),
            expression_inputs: BTreeSet::new(),
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
            "${{ steps.upload-artifact.outputs.artifact-id }}"
        );
    }

    #[test]
    fn builds_nested_composite_repository_action_plan() {
        let plan = LocalActionPlan {
            step_id: "docs".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/docs"),
            workspace_host: Path::new("/tmp/workspace").into(),
            inputs: [("github-token".to_string(), "ghs_token".to_string())].into(),
            expression_inputs: BTreeSet::new(),
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
        assert_eq!(
            plans[0].inputs["github_token"],
            "${{ inputs.github-token }}"
        );
        assert!(plans[0].expression_inputs.contains("github_token"));
        assert_eq!(
            plans[0].condition.as_deref(),
            Some("${{ inputs.github-token != '' }}")
        );
    }

    #[test]
    fn expands_nested_local_composite_before_execution() {
        let workspace = std::env::temp_dir().join(format!(
            "velnor-nested-local-composite-{}",
            std::process::id()
        ));
        let root = workspace.join(".github/actions/root");
        let nested = workspace.join(".github/actions/nested");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("action.yml"),
            r#"
inputs:
  token:
    description: Value forwarded by the parent action
runs:
  using: composite
  steps:
    - id: same
      shell: bash
      run: echo "closure=${{ inputs.token }}" >> "$GITHUB_OUTPUT"
outputs:
  closure:
    value: ${{ steps.same.outputs.closure }}
"#,
        )
        .unwrap();
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - id: same
      shell: bash
      run: echo "ready=yes" >> "$GITHUB_OUTPUT"
    - id: nested
      uses: ./.github/actions/nested
      if: ${{ steps.same.outputs.ready == 'yes' }}
      env:
        NESTED_VALUE: ${{ steps.same.outputs.ready }}
      continue-on-error: ${{ steps.same.outputs.ready == 'yes' }}
      with:
        token: ${{ steps.same.outputs.ready }}
outputs:
  closure:
    value: ${{ steps.nested.outputs.closure }}
"#,
        )
        .unwrap();
        let plan = LocalActionPlan {
            step_id: "root".into(),
            action_dir: root,
            workspace_host: workspace.clone(),
            inputs: BTreeMap::new(),
            expression_inputs: BTreeSet::new(),
        };

        let invocations =
            composite_action_invocations(&plan, &metadata, "/__w", Path::new("/__a")).unwrap();

        let CompositeActionInvocation::Script(parent_step) = &invocations[0] else {
            panic!("first parent step should remain in the parent action scope")
        };
        assert_eq!(parent_step.id, "root-same");
        let CompositeActionInvocation::CompositeStart {
            step_id,
            inputs,
            expression_inputs,
            visible_step_ids,
            env,
            condition,
            continue_on_error_expression,
            ..
        } = &invocations[1]
        else {
            panic!("nested local action should retain its wrapper invocation")
        };
        assert_eq!(step_id, "root-nested");
        assert_eq!(visible_step_ids.len(), 1);
        assert_eq!(visible_step_ids.get("same").unwrap(), "root-nested-same");
        assert!(!visible_step_ids.contains_key("root-same"));
        assert_eq!(inputs["token"], "${{ steps.same.outputs.ready }}");
        assert!(expression_inputs.contains("token"));
        assert_eq!(
            env,
            &vec![(
                "NESTED_VALUE".into(),
                "${{ steps.same.outputs.ready }}".into()
            )]
        );
        assert_eq!(
            condition.as_deref(),
            Some("${{ steps.same.outputs.ready == 'yes' }}")
        );
        assert_eq!(
            continue_on_error_expression,
            &Some(ActionBooleanValue::Expression(
                "steps.same.outputs.ready == 'yes'".into()
            ))
        );
        assert!(matches!(
            &invocations[2],
            CompositeActionInvocation::Script(step)
                if step.id == "root-nested-same"
                    && step.script.contains("${{ inputs.token }}")
        ));
        assert!(matches!(
            &invocations[3],
            CompositeActionInvocation::Outputs(outputs)
                if outputs.step_id == "root-nested"
                    && outputs.outputs["closure"]
                        == "${{ steps.same.outputs.closure }}"
        ));
        assert!(matches!(
            &invocations[4],
            CompositeActionInvocation::CompositeEnd { step_id } if step_id == "root-nested"
        ));
        assert!(matches!(
            &invocations[5],
            CompositeActionInvocation::Outputs(outputs)
                if outputs.step_id == "root"
                    && outputs.outputs["closure"]
                        == "${{ steps.nested.outputs.closure }}"
        ));
        fs::remove_dir_all(workspace).unwrap();
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
            expression_inputs: BTreeSet::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
            ..Default::default()
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
            .composite_invocations("/__w", actions_host, Path::new("/tmp/workspace"))
            .unwrap();

        let CompositeActionInvocation::Script(step) = &invocations[0] else {
            panic!("repository composite should expand to script")
        };
        assert_eq!(step.id, "toolchain-1");
        assert_eq!(step.condition.as_deref(), Some("runner.os == 'Linux'"));
        assert!(step.continue_on_error);
        assert_eq!(
            step.working_directory_container,
            "${{ github.action_path }}/fixtures"
        );
        assert!(step
            .script
            .contains("${{ github.action_path }} ${{ inputs.toolchain }}"));
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
            expression_inputs: BTreeSet::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
            ..Default::default()
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
            .composite_invocations("/__w", actions_host, Path::new("/tmp/workspace"))
            .unwrap();

        let CompositeActionInvocation::Script(parse) = &invocations[0] else {
            panic!("parse should expand to script")
        };
        assert!(parse.script.contains("toolchain=${{inputs.toolchain}}"));

        let CompositeActionInvocation::Script(flags) = &invocations[1] else {
            panic!("flags should expand to script")
        };
        assert!(parse
            .env
            .contains(&("toolchain".into(), "${{inputs.toolchain}}".into())));
        assert!(parse.env.contains(&(
            "GITHUB_ACTION_PATH".into(),
            "/__a/_actions/acme_toolchain/stable".into()
        )));
        assert!(flags.env.contains(&(
            "targets".into(),
            "${{inputs.targets || inputs.target || ''}}".into()
        )));
        assert!(flags.script.contains(
            "${{steps.parse.outputs.toolchain == 'nightly' && inputs.components && ' --allow-downgrade' || ''}}"
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
            expression_inputs: BTreeSet::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
            ..Default::default()
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
      continue-on-error: true
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
            .composite_invocations("/__w", actions_host, Path::new("/tmp/workspace"))
            .unwrap();

        let CompositeActionInvocation::ContinueOnError {
            step_id,
            value: ActionBooleanValue::Expression(expression),
        } = &invocations[0]
        else {
            panic!("expression continue-on-error should remain deferred")
        };
        assert_eq!(step_id, "setup-1");
        assert_eq!(expression, "inputs.soft-fail");
        let CompositeActionInvocation::Script(script) = &invocations[1] else {
            panic!("script should follow its deferred continue-on-error")
        };
        assert!(!script.continue_on_error);
        let CompositeActionInvocation::Repository(repository) = &invocations[2] else {
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
            expression_inputs: BTreeSet::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
            ..Default::default()
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

        let plans = composite_repository_action_plans_from_resolved(
            &[resolved],
            actions_host,
            Path::new("/tmp/workspace"),
        )
        .unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].step_id, "pages-1");
        assert_eq!(plans[0].repository, "actions/upload-artifact");
        assert_eq!(plans[0].git_ref, "v7");
        assert_eq!(plans[0].inputs["path"], "${{ inputs.path }}");
        assert!(plans[0].expression_inputs.contains("path"));
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
        fs::create_dir_all(restore_dir.join("dist")).unwrap();
        fs::write(restore_dir.join("dist/restore.js"), "").unwrap();
        fs::write(
            save_dir.join("action.yml"),
            "runs:\n  using: node20\n  main: dist/save.js\n",
        )
        .unwrap();
        fs::create_dir_all(save_dir.join("dist")).unwrap();
        fs::write(save_dir.join("dist/save.js"), "").unwrap();
        let plans = vec![
            RepositoryActionPlan {
                step_id: "cache-restore".into(),
                repository: "actions/cache".into(),
                git_ref: "v5".into(),
                source_path: Some("restore".into()),
                repository_dir: repository_dir.clone(),
                action_dir: restore_dir,
                inputs: BTreeMap::new(),
                expression_inputs: BTreeSet::new(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
                ..Default::default()
            },
            RepositoryActionPlan {
                step_id: "cache-save".into(),
                repository: "actions/cache".into(),
                git_ref: "v5".into(),
                source_path: Some("save".into()),
                repository_dir,
                action_dir: save_dir,
                inputs: BTreeMap::new(),
                expression_inputs: BTreeSet::new(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
                ..Default::default()
            },
        ];
        let mut runner = RecordingRunner::default();

        let resolved = download_repository_actions(&mut runner, &plans, &actions_host).unwrap();

        let fetches = runner
            .calls
            .iter()
            .filter(|(program, args)| program == "git" && args.contains(&"fetch".to_string()))
            .count();
        assert_eq!(resolved.len(), 2);
        assert_eq!(fetches, 1);
        std::fs::remove_dir_all(actions_host).ok();
    }

    fn cache_plan(step_id: &str, source_path: Option<&str>) -> RepositoryActionPlan {
        RepositoryActionPlan {
            step_id: step_id.into(),
            repository: "actions/cache".into(),
            git_ref: "v4".into(),
            source_path: source_path.map(str::to_string),
            repository_dir: PathBuf::from("/tmp/_actions/actions_cache/v4"),
            action_dir: PathBuf::from("/tmp/_actions/actions_cache/v4"),
            inputs: BTreeMap::new(),
            expression_inputs: BTreeSet::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
            ..Default::default()
        }
    }

    #[test]
    fn native_invocation_preserves_cache_lifecycle() {
        let root = native_invocation_from_plan(&cache_plan("cache", None))
            .unwrap()
            .unwrap();
        assert_eq!(root.adapter, NativeActionAdapter::Cache);
        assert_eq!(root.cache_kind, Some(CacheActionKind::Root));
        assert_eq!(root.source_path, None);

        // An empty subpath is the root form, not an error.
        let empty = native_invocation_from_plan(&cache_plan("cache", Some("")))
            .unwrap()
            .unwrap();
        assert_eq!(empty.cache_kind, Some(CacheActionKind::Root));

        let restore = native_invocation_from_plan(&cache_plan("cache-restore", Some("restore")))
            .unwrap()
            .unwrap();
        assert_eq!(restore.cache_kind, Some(CacheActionKind::Restore));
        assert_eq!(restore.source_path.as_deref(), Some("restore"));

        let save = native_invocation_from_plan(&cache_plan("cache-save", Some("save")))
            .unwrap()
            .unwrap();
        assert_eq!(save.cache_kind, Some(CacheActionKind::Save));
        assert_eq!(save.source_path.as_deref(), Some("save"));
    }

    #[test]
    fn native_invocation_survives_nested_composite_cache_plan() {
        // A composite action expands each nested step into a
        // `RepositoryActionPlan`; the subpath it carries must classify the same
        // way through `native_invocation_from_plan` as a top-level reference.
        let mut nested = cache_plan("composite__cache-save", Some("save"));
        nested.env = vec![("GITHUB_ACTION".into(), "outer-composite".into())];
        let invocation = native_invocation_from_plan(&nested).unwrap().unwrap();
        assert_eq!(invocation.cache_kind, Some(CacheActionKind::Save));
        assert_eq!(invocation.source_path.as_deref(), Some("save"));
    }

    #[test]
    fn native_invocation_rejects_unknown_cache_subpath() {
        let error = native_invocation_from_plan(&cache_plan("cache", Some("delete"))).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported actions/cache subpath"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn native_invocation_leaves_non_cache_adapters_without_cache_kind() {
        let mut plan = cache_plan("upload", None);
        plan.repository = "actions/upload-artifact".into();
        plan.source_path = Some("ignored".into());
        let invocation = native_invocation_from_plan(&plan).unwrap().unwrap();
        assert_eq!(invocation.adapter, NativeActionAdapter::UploadArtifact);
        // Non-cache adapters keep their prior behavior: no lifecycle, and a
        // non-cache subpath is never rejected here.
        assert_eq!(invocation.cache_kind, None);
        assert_eq!(invocation.source_path.as_deref(), Some("ignored"));
    }

    #[test]
    fn native_invocation_is_none_for_unknown_repository() {
        let mut plan = cache_plan("setup", None);
        plan.repository = "owner/unknown-action".into();
        assert!(native_invocation_from_plan(&plan).unwrap().is_none());
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
        fs::create_dir_all(action_dir.join("dist")).unwrap();
        fs::write(action_dir.join("dist/index.js"), "").unwrap();
        let plan = RepositoryActionPlan {
            step_id: "setup".into(),
            repository: "actions/setup-node".into(),
            git_ref: "v4".into(),
            source_path: None,
            repository_dir: temp.clone(),
            action_dir,
            inputs: BTreeMap::new(),
            expression_inputs: BTreeSet::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
            ..Default::default()
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
    fn resolves_dockerfile_only_action_and_manifest_takes_precedence() {
        let temp = std::env::temp_dir().join(format!(
            "velnor-dockerfile-action-test-{}",
            std::process::id()
        ));
        let action_dir = temp.join("action");
        fs::create_dir_all(&action_dir).unwrap();
        fs::write(action_dir.join("Dockerfile"), "FROM alpine:3.20\n").unwrap();
        let plan = RepositoryActionPlan {
            step_id: "docker-only".into(),
            repository: "acme/docker-only".into(),
            git_ref: "v1".into(),
            repository_dir: temp.clone(),
            action_dir: action_dir.clone(),
            ..Default::default()
        };

        let resolved = resolve_action(&plan).unwrap();
        assert_eq!(resolved.metadata_path, action_dir.join("Dockerfile"));
        assert_eq!(
            resolved.runtime,
            ActionRuntime::Docker {
                image: "Dockerfile".into()
            }
        );
        let invocation = resolved.docker_invocation(&temp).unwrap();
        assert_eq!(
            invocation.build_context_host.as_deref(),
            Some(action_dir.as_path())
        );
        assert_eq!(
            invocation.dockerfile_host.as_deref(),
            Some(action_dir.join("Dockerfile").as_path())
        );

        fs::write(
            action_dir.join("action.yml"),
            "runs:\n  using: docker\n  image: docker://alpine:3.20\n",
        )
        .unwrap();
        let resolved = resolve_action(&plan).unwrap();
        assert_eq!(resolved.metadata_path, action_dir.join("action.yml"));
        assert_eq!(
            resolved.runtime,
            ActionRuntime::Docker {
                image: "docker://alpine:3.20".into()
            }
        );
        let invocation = resolved.docker_invocation(&temp).unwrap();
        assert_eq!(invocation.image, "alpine:3.20");
        assert!(invocation.build_context_host.is_none());
        assert!(invocation.dockerfile_host.is_none());
        fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn javascript_action_requires_its_main_file() {
        let temp = std::env::temp_dir().join(format!(
            "velnor-js-main-validation-test-{}",
            std::process::id()
        ));
        let action_dir = temp.join("action");
        fs::create_dir_all(&action_dir).unwrap();
        fs::write(
            action_dir.join("action.yml"),
            "runs:\n  using: node20\n  main: dist/index.js\n",
        )
        .unwrap();
        let plan = RepositoryActionPlan {
            step_id: "js".into(),
            repository: "acme/js".into(),
            git_ref: "v1".into(),
            repository_dir: temp.clone(),
            action_dir: action_dir.clone(),
            ..Default::default()
        };

        let error = resolve_action(&plan).unwrap_err().to_string();
        assert!(error.contains("main file does not exist"), "{error}");
        fs::create_dir_all(action_dir.join("dist")).unwrap();
        fs::write(action_dir.join("dist/index.js"), "").unwrap();
        assert_eq!(
            resolve_action(&plan).unwrap().runtime,
            ActionRuntime::JavaScript {
                node: "node20".into(),
                main: "dist/index.js".into()
            }
        );
        fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn nested_local_dockerfile_uses_workspace_root_and_its_own_build_context() {
        let workspace = std::env::temp_dir().join(format!(
            "velnor-nested-local-docker-test-{}",
            std::process::id()
        ));
        let outer = workspace.join(".github/actions/outer");
        let child = workspace.join(".github/actions/child/docker");
        fs::create_dir_all(&outer).unwrap();
        fs::create_dir_all(&child).unwrap();
        fs::write(
            outer.join("action.yml"),
            "runs:\n  using: composite\n  steps:\n    - uses: ./.github/actions/child\n",
        )
        .unwrap();
        fs::write(child.join("Dockerfile.test"), "FROM alpine:3.20\n").unwrap();
        fs::write(
            workspace.join(".github/actions/child/action.yml"),
            "runs:\n  using: docker\n  image: docker/Dockerfile.test\n",
        )
        .unwrap();
        let plan = LocalActionPlan {
            step_id: "outer".into(),
            action_dir: outer,
            workspace_host: workspace.clone(),
            inputs: BTreeMap::new(),
            expression_inputs: BTreeSet::new(),
        };
        let metadata = resolve_local_action(&plan).unwrap();

        let invocations =
            composite_action_invocations(&plan, &metadata, "/__w", Path::new("/tmp/actions"))
                .unwrap();
        let CompositeActionInvocation::LocalAction { invocation, .. } = &invocations[0] else {
            panic!("nested local Docker action should remain an executable invocation")
        };
        let LocalActionInvocation::Docker(invocation) = invocation else {
            panic!("nested local Docker action should retain its Docker runtime")
        };
        assert_eq!(invocation.image, "velnor-action-local-outer-1-root");
        assert_eq!(
            invocation.action_container_path,
            "/__w/.github/actions/child"
        );
        assert_eq!(
            invocation.build_context_host.as_deref(),
            Some(child.as_path())
        );
        assert_eq!(
            invocation.dockerfile_host.as_deref(),
            Some(child.join("Dockerfile.test").as_path())
        );
        fs::remove_dir_all(workspace).ok();
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
            expression_inputs: BTreeSet::new(),
            env: [(
                "NODE_AUTH_TOKEN".to_string(),
                "${{ github.token }}".to_string(),
            )]
            .into(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
            ..Default::default()
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
        assert_eq!(
            invocation.inputs.get("node-version").map(String::as_str),
            Some("22")
        );
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
            .step_env
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
            expression_inputs: BTreeSet::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
            ..Default::default()
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

        assert_eq!(
            invocation.inputs.get("path").map(String::as_str),
            Some("~/.cargo")
        );
        assert_eq!(
            invocation.input_defaults.get("fail-on-cache-miss"),
            Some("false")
        );
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
            expression_inputs: ["path".to_string()].into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
            ..Default::default()
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
        assert_eq!(
            invocation.inputs.get("pattern").map(String::as_str),
            Some("construct-digest-*")
        );
        assert_eq!(
            invocation.inputs.get("path").map(String::as_str),
            Some("${{ env.DIGEST_DIR }}")
        );
        assert_eq!(
            invocation.inputs.get("merge-multiple").map(String::as_str),
            Some("true")
        );
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
            expression_inputs: ["name".to_string(), "path".to_string()].into(),
            env: Vec::new(),
            condition: Some("needs.changes.outputs.is_publish == 'true'".into()),
            continue_on_error: false,
            timeout_minutes: None,
            ..Default::default()
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
        assert_eq!(
            invocation.inputs.get("name").map(String::as_str),
            Some("construct-digest-${{ matrix.platform }}")
        );
        assert_eq!(
            invocation.inputs.get("path").map(String::as_str),
            Some("${{ env.DIGEST_DIR }}/${{ matrix.platform }}.digest")
        );
        assert_eq!(
            invocation
                .inputs
                .get("if-no-files-found")
                .map(String::as_str),
            Some("error")
        );
        assert_eq!(
            invocation.inputs.get("retention-days").map(String::as_str),
            Some("1")
        );
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
            inputs: [
                (
                    "renovate-image".to_string(),
                    "ghcr.io/renovatebot/renovate".to_string(),
                ),
                (
                    "config-file".to_string(),
                    "${{ github.action_path }}/config.js".to_string(),
                ),
            ]
            .into(),
            expression_inputs: ["config-file".to_string()].into(),
            env: [("LOG_LEVEL".to_string(), "debug".to_string())].into(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
            ..Default::default()
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
    - ${{ inputs.config-file }}
  env:
    ACTION_RUNTIME: ${{ inputs.renovate-image }}
    LOG_LEVEL: action-default
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
            Some(vec![
                "${{ inputs.renovate-image }}".to_string(),
                "${{ inputs.config-file }}".to_string(),
            ])
        );
        assert_eq!(
            invocation.inputs.get("renovate-image").map(String::as_str),
            Some("ghcr.io/renovatebot/renovate")
        );
        assert_eq!(
            invocation.inputs.get("config-file").map(String::as_str),
            Some("${{ github.action_path }}/config.js")
        );
        assert_eq!(
            invocation.input_defaults.get("renovate-image"),
            Some("ghcr.io/renovatebot/renovate")
        );
        assert!(invocation
            .step_env
            .contains(&("LOG_LEVEL".into(), "debug".into())));
        assert_eq!(
            invocation.runs_env.get("ACTION_RUNTIME"),
            Some("${{ inputs.renovate-image }}")
        );
    }

    /// Fork-PR payload: `runs.image` is repository content. Without a grammar
    /// check `docker://--privileged` becomes a flag of the host `docker run`,
    /// which is root on a shared runner host.
    #[test]
    fn docker_action_image_that_would_be_read_as_a_flag_is_refused() {
        let actions_host = Path::new("/tmp/actions");
        for image in [
            "docker://--privileged",
            "docker://-v/:/host",
            "docker://--user=0:0",
            "docker://",
        ] {
            let plan = RepositoryActionPlan {
                step_id: "evil".into(),
                repository: "attacker/action".into(),
                git_ref: "v1".into(),
                source_path: None,
                repository_dir: actions_host.join("_actions/attacker_action/v1"),
                action_dir: actions_host.join("_actions/attacker_action/v1"),
                inputs: Default::default(),
                expression_inputs: BTreeSet::new(),
                env: Default::default(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
                ..Default::default()
            };
            let metadata = parse_action_metadata(&format!(
                "runs:\n  using: docker\n  image: {image}\n  args:\n    - --privileged\n"
            ))
            .unwrap();
            let runtime = metadata.runtime().unwrap();
            let resolved = ResolvedAction {
                plan,
                metadata_path: actions_host.join("_actions/attacker_action/v1/action.yml"),
                metadata,
                runtime,
            };

            let error = resolved
                .docker_invocation(actions_host)
                .expect_err("a flag-shaped image must never build an invocation");
            assert!(
                error.to_string().contains("invalid Docker image"),
                "{error}"
            );
        }
    }

    #[test]
    fn parses_fetched_target_action_metadata() {
        let roots = [
            Path::new("/tmp/velnor-runner-action-scratch"),
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
        let actions_root = Path::new("/tmp/velnor-runner-action-scratch");
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
        let actions_root = Path::new("/tmp/velnor-runner-action-scratch");
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
                            expression_inputs: BTreeSet::new(),
                            env: Vec::new(),
                            condition: None,
                            continue_on_error: false,
                            timeout_minutes: None,
                            ..Default::default()
                        },
                        metadata_path: path.clone(),
                        runtime: metadata.runtime().unwrap(),
                        metadata,
                    }
                    .composite_invocations(
                        "/__w",
                        actions_root,
                        Path::new("/tmp/velnor-targets/jackin"),
                    )
                    .unwrap_or_else(|error| {
                        panic!("expand fetched composite {}: {error:#}", path.display())
                    })
                } else {
                    let action_dir = path.parent().unwrap().to_path_buf();
                    let plan = LocalActionPlan {
                        step_id: format!("local-composite-{checked}"),
                        action_dir,
                        workspace_host: Path::new("/tmp/velnor-targets/jackin").into(),
                        inputs: BTreeMap::new(),
                        expression_inputs: BTreeSet::new(),
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
        let actions_root = Path::new("/tmp/velnor-runner-action-scratch");
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
                    if uses.starts_with('.') || docker_scheme_image(&uses).is_some() {
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
            (
                "actions/attest-build-provenance",
                NativeActionAdapter::AttestBuildProvenance,
            ),
            (
                "actions/create-github-app-token",
                NativeActionAdapter::CreateGitHubAppToken,
            ),
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
            ("hadolint/hadolint-action", NativeActionAdapter::Hadolint),
            ("docker/setup-qemu-action", NativeActionAdapter::SetupQemu),
            (
                "sigstore/cosign-installer",
                NativeActionAdapter::CosignInstaller,
            ),
        ];

        for (repository, adapter) in adapters {
            assert_eq!(native_action_adapter(repository), Some(adapter));
            assert!(native_action_repository(adapter)
                .is_some_and(|canonical| canonical.eq_ignore_ascii_case(repository)));
        }
        assert_eq!(native_action_adapter("owner/unknown-action"), None);
    }

    #[test]
    fn unsupported_actions_return_error_message() {
        assert!(unsupported_action_error("dtolnay/rust-toolchain").is_some());
        assert!(unsupported_action_error("DTOLNAY/RUST-TOOLCHAIN").is_some());
        assert!(unsupported_action_error("baptiste0928/cargo-install").is_some());
        assert!(unsupported_action_error("Baptiste0928/Cargo-Install").is_some());
        assert!(unsupported_action_error("EmbarkStudios/cargo-deny-action").is_some());
        assert!(unsupported_action_error("actions/attest-build-provenance").is_none());
        assert!(unsupported_action_error("jdx/mise-action").is_none());
        assert!(unsupported_action_error("owner/unknown-action").is_none());
        assert!(unsupported_action_error("dtolnay/rust-toolchain")
            .unwrap()
            .contains("jdx/mise-action"));
        assert!(unsupported_action_error("baptiste0928/cargo-install")
            .unwrap()
            .contains("jdx/mise-action"));
        assert!(unsupported_action_error("EmbarkStudios/cargo-deny-action")
            .unwrap()
            .contains("cargo:cargo-deny"));
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
                    if key == "uses"
                        && let Some(uses) = value.as_str()
                    {
                        values.push(uses.to_string());
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

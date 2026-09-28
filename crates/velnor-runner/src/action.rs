#![allow(dead_code)]

use crate::{
    checkout::fetch_git_ref,
    executor::CommandRunner,
    job_message::{ActionReferenceType, ActionStep},
    script_step::{step_environment, value_truthy, ScriptStep},
};
use anyhow::{bail, Context, Result};
use serde::de::{Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
use velnor_model::action_reference::{
    is_runner_local_action_reference, resolve_action_path, ActionImageReference,
    RepositoryActionReference, SafeActionPath,
};

#[derive(Debug, Clone, Deserialize)]
pub struct ActionMetadata {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_action_metadata_string"
    )]
    pub name: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_action_metadata_string"
    )]
    pub description: Option<String>,
    pub runs: ActionRuns,
    #[serde(default, deserialize_with = "deserialize_action_input_map")]
    pub inputs: BTreeMap<String, ActionInput>,
    #[serde(default, deserialize_with = "deserialize_action_output_map")]
    pub outputs: BTreeMap<String, ActionOutput>,
}

#[derive(Debug, Clone)]
pub struct ActionInput {
    pub description: Option<String>,
    pub default_value: Option<String>,
    pub required: bool,
}

impl<'de> Deserialize<'de> for ActionInput {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ActionInputVisitor)
    }
}

fn deserialize_action_input_map<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, ActionInput>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_map(ActionInputMapVisitor)
}

fn deserialize_action_output_map<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, ActionOutput>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_map(ActionOutputMapVisitor)
}

struct ActionInputMapVisitor;

impl<'de> Visitor<'de> for ActionInputMapVisitor {
    type Value = BTreeMap<String, ActionInput>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an action inputs mapping with non-empty keys")
    }

    fn visit_map<A>(self, mut mapping: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut inputs = BTreeMap::new();
        let mut seen_keys = BTreeSet::new();
        while let Some(ActionMetadataMapKey(key)) = mapping.next_key()? {
            if key.is_empty() {
                return Err(A::Error::custom(
                    "action input names must be non-empty strings",
                ));
            }
            if !seen_keys.insert(runner_ordinal_ignore_case_key(&key)) {
                return Err(A::Error::custom(format!(
                    "duplicate action input name after case-insensitive matching: `{key}`"
                )));
            }
            inputs.insert(key, mapping.next_value()?);
        }
        Ok(inputs)
    }
}

struct ActionOutputMapVisitor;

impl<'de> Visitor<'de> for ActionOutputMapVisitor {
    type Value = BTreeMap<String, ActionOutput>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an action outputs mapping with non-empty keys")
    }

    fn visit_map<A>(self, mut mapping: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut outputs = BTreeMap::new();
        let mut seen_keys = BTreeSet::new();
        while let Some(ActionMetadataMapKey(key)) = mapping.next_key()? {
            if key.is_empty() {
                return Err(A::Error::custom(
                    "action output names must be non-empty strings",
                ));
            }
            if !seen_keys.insert(runner_ordinal_ignore_case_key(&key)) {
                return Err(A::Error::custom(format!(
                    "duplicate action output name after case-insensitive matching: `{key}`"
                )));
            }
            outputs.insert(key, mapping.next_value()?);
        }
        Ok(outputs)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionOutput {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_action_metadata_string"
    )]
    pub description: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_action_metadata_string"
    )]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRuns {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub plugin: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_non_empty_action_metadata_string"
    )]
    pub using: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub main: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub pre: Option<String>,
    #[serde(
        default,
        rename = "pre-if",
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub pre_if: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub post: Option<String>,
    #[serde(
        default,
        rename = "post-if",
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub post_if: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub image: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub entrypoint: Option<String>,
    /// Docker-action pre/post entrypoints (`runs.pre-entrypoint` /
    /// `runs.post-entrypoint`): upstream runs them as the Pre/Post stage
    /// with the same image and args
    /// (`ContainerActionHandler.cs: RunAsync(stage)`).
    #[serde(
        default,
        rename = "pre-entrypoint",
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub pre_entrypoint: Option<String>,
    #[serde(
        default,
        rename = "post-entrypoint",
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub post_entrypoint: Option<String>,
    #[serde(default, deserialize_with = "deserialize_action_metadata_string_list")]
    pub args: Vec<String>,
    /// Environment declared by the Docker action itself (`runs.env`).
    /// Keep it separate from workflow step env until invocation construction,
    /// where the latter can retain the runner's override precedence.
    #[serde(default, deserialize_with = "deserialize_string_map")]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub steps: Vec<CompositeActionStep>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompositeActionStep {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_action_metadata_string"
    )]
    pub name: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_action_metadata_string"
    )]
    pub shell: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_action_metadata_string"
    )]
    pub run: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_action_metadata_string"
    )]
    pub uses: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_map")]
    pub with: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_string_map")]
    pub env: BTreeMap<String, String>,
    #[serde(
        default,
        rename = "if",
        deserialize_with = "deserialize_optional_action_metadata_string"
    )]
    pub condition: Option<String>,
    #[serde(
        default,
        rename = "working-directory",
        deserialize_with = "deserialize_optional_action_metadata_string"
    )]
    pub working_directory: Option<String>,
    #[serde(
        default,
        rename = "continue-on-error",
        deserialize_with = "deserialize_optional_boolean_or_expression"
    )]
    pub continue_on_error: Option<String>,
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
        if let Some(plugin) = self.runs.plugin.as_deref() {
            bail!("plugin action runtime '{plugin}' is not supported by Velnor");
        }
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
    pub timeout_minutes: Option<u64>,
}

pub const NATIVE_ACTION_REF: &str = "__native";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalActionPlan {
    pub step_id: String,
    /// Canonical trust boundary for this action's repository files.
    pub workspace_root: PathBuf,
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
    if contents.len() as u64 > MAX_LOCAL_ACTION_METADATA_BYTES {
        bail!("action metadata exceeds {MAX_LOCAL_ACTION_METADATA_BYTES} bytes");
    }
    if !metadata_document_within_budget(contents) {
        bail!("action metadata nesting exceeds the admission parser budget");
    }
    let tag_directives = runner_action_tag_directives(contents)?;
    serde_yaml::from_str_with_config::<serde_yaml::Value>(contents, &runner_action_parser_config())
        .context("parse action metadata within the admission parser budget")?;
    let tag_syntax = action_metadata_tag_syntax(contents)?;
    let scalar_lexemes = action_metadata_scalar_lexemes(contents)?;
    let config = runner_action_parser_config().with_policy(ActionMetadataTagPolicy {
        tags: tag_syntax,
        directives: tag_directives.clone(),
        next_tag: AtomicUsize::new(0),
    });
    let value = serde_yaml::from_str_with_config::<serde_yaml::Value>(contents, &config)
        .context("parse action metadata")?;
    let value = normalize_runner_scalar_lexemes(value, &scalar_lexemes, &tag_directives)?;
    validate_nested_action_metadata_map_keys(&value)?;
    let value = normalize_action_metadata_document(value)?;
    ActionMetadata::deserialize(&value).context("parse action metadata")
}

#[derive(Debug, Clone)]
struct ActionMetadataScalarLexeme {
    kind: serde_yaml::cst::SyntaxKind,
    text: String,
    tag: Option<String>,
}

#[derive(Default)]
struct ActionMetadataCstBudget {
    event_count: usize,
    node_count: usize,
}

fn validate_action_metadata_cst_budget(
    node: &serde_yaml::cst::GreenNode,
    depth: usize,
    budget: &mut ActionMetadataCstBudget,
    source_len: usize,
) -> Result<()> {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    budget.event_count = budget
        .event_count
        .checked_add(1)
        .context("action metadata CST event budget overflow")?;
    if budget.event_count > MAX_METADATA_PARSE_EVENTS {
        bail!("action metadata exceeds the parser event budget");
    }

    let collection = matches!(
        node.kind(),
        SyntaxKind::BlockMapping
            | SyntaxKind::BlockSequence
            | SyntaxKind::FlowMapping
            | SyntaxKind::FlowSequence
    );
    let depth = if collection {
        let depth = depth
            .checked_add(1)
            .context("action metadata CST depth budget overflow")?;
        if depth > MAX_METADATA_PARSE_NESTING {
            bail!("action metadata nesting exceeds {MAX_METADATA_PARSE_NESTING} levels");
        }
        budget.node_count = budget
            .node_count
            .checked_add(1)
            .context("action metadata CST node budget overflow")?;
        if budget.node_count > MAX_METADATA_PARSE_NODES {
            bail!("action metadata exceeds the parser node budget");
        }
        depth
    } else {
        depth
    };

    for child in node.children() {
        match child {
            GreenChild::Node(child) => {
                validate_action_metadata_cst_budget(child, depth, budget, source_len)?;
            }
            GreenChild::Token { kind, .. } => {
                budget.event_count = budget
                    .event_count
                    .checked_add(1)
                    .context("action metadata CST event budget overflow")?;
                if budget.event_count > MAX_METADATA_PARSE_EVENTS {
                    bail!("action metadata exceeds the parser event budget");
                }
                if matches!(
                    kind,
                    SyntaxKind::PlainScalar
                        | SyntaxKind::SingleQuotedScalar
                        | SyntaxKind::DoubleQuotedScalar
                        | SyntaxKind::LiteralScalar
                        | SyntaxKind::FoldedScalar
                ) {
                    budget.node_count = budget
                        .node_count
                        .checked_add(1)
                        .context("action metadata CST node budget overflow")?;
                    if budget.node_count > MAX_METADATA_PARSE_NODES {
                        bail!("action metadata exceeds the parser node budget");
                    }
                }
            }
        }
    }

    let estimated_bytes = source_len
        .checked_add(budget.node_count.saturating_mul(64))
        .and_then(|bytes| bytes.checked_add(budget.event_count.saturating_mul(8)))
        .unwrap_or(usize::MAX);
    if estimated_bytes > MAX_METADATA_PARSE_ESTIMATED_BYTES {
        bail!(
            "estimated action metadata parser memory exceeds {MAX_METADATA_PARSE_ESTIMATED_BYTES} bytes"
        );
    }
    Ok(())
}

fn action_metadata_scalar_lexemes(contents: &str) -> Result<Vec<ActionMetadataScalarLexeme>> {
    use serde_yaml::cst::{GreenChild, GreenNode, SyntaxKind};

    fn is_scalar(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::PlainScalar
                | SyntaxKind::SingleQuotedScalar
                | SyntaxKind::DoubleQuotedScalar
                | SyntaxKind::LiteralScalar
                | SyntaxKind::FoldedScalar
        )
    }

    fn is_trivia(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::Whitespace | SyntaxKind::Newline | SyntaxKind::Comment
        )
    }

    fn child_has_value(child: &GreenChild) -> bool {
        match child {
            GreenChild::Node(_) => true,
            GreenChild::Token { kind, .. } => is_scalar(*kind),
        }
    }

    fn lexeme(kind: SyntaxKind, text: String, tag: Option<String>) -> ActionMetadataScalarLexeme {
        ActionMetadataScalarLexeme { kind, text, tag }
    }

    fn collect(
        node: &GreenNode,
        source: &str,
        start: usize,
        output: &mut Vec<ActionMetadataScalarLexeme>,
    ) -> Result<usize> {
        use serde_yaml::cst::SyntaxKind as K;

        let children: Vec<_> = node.children().collect();
        let kind = node.kind();
        let mut empty_key_before = BTreeSet::new();
        let mut empty_value_after = BTreeSet::new();
        let mut empty_sequence_item_before = BTreeSet::new();

        if kind == K::MappingEntry {
            if let Some(colon) = children.iter().position(|child| {
                matches!(
                    child,
                    GreenChild::Token {
                        kind: K::ColonIndicator,
                        ..
                    }
                )
            }) {
                if !children[..colon].iter().any(|child| child_has_value(child)) {
                    empty_key_before.insert(colon);
                }
                if !children[colon + 1..]
                    .iter()
                    .any(|child| child_has_value(child))
                {
                    empty_value_after.insert(children.len());
                }
            }
        } else if kind == K::SequenceItem {
            if let Some(dash) = children.iter().position(|child| {
                matches!(
                    child,
                    GreenChild::Token {
                        kind: K::DashIndicator,
                        ..
                    }
                )
            }) && !children[dash + 1..]
                .iter()
                .any(|child| child_has_value(child))
            {
                empty_sequence_item_before.insert(children.len());
            }
        } else if kind == K::FlowMapping {
            let mut segment_start = 0usize;
            for (index, child) in children.iter().enumerate() {
                match child {
                    GreenChild::Token {
                        kind: K::OpenBrace | K::Comma,
                        ..
                    } => {
                        segment_start = index + 1;
                    }
                    GreenChild::Token {
                        kind: K::ColonIndicator,
                        ..
                    } => {
                        if !children[segment_start..index]
                            .iter()
                            .any(|child| child_has_value(child))
                        {
                            empty_key_before.insert(index);
                        }
                        let mut end = index + 1;
                        while end < children.len()
                            && !matches!(
                                children[end],
                                GreenChild::Token {
                                    kind: K::Comma | K::CloseBrace,
                                    ..
                                }
                            )
                        {
                            end += 1;
                        }
                        if !children[index + 1..end]
                            .iter()
                            .any(|child| child_has_value(child))
                        {
                            empty_value_after.insert(end);
                        }
                        segment_start = end;
                    }
                    _ => {}
                }
            }
        } else if kind == K::FlowSequence {
            let mut segment_start = 1usize;
            for (index, child) in children.iter().enumerate() {
                if matches!(child, GreenChild::Token { kind: K::Comma, .. }) {
                    if !children[segment_start..index]
                        .iter()
                        .any(|child| child_has_value(child))
                    {
                        empty_sequence_item_before.insert(index);
                    }
                    segment_start = index + 1;
                }
            }
        }

        let mut offset = start;
        let mut pending_tag = None;
        for (index, child) in children.iter().enumerate() {
            if empty_key_before.contains(&index) {
                output.push(lexeme(K::PlainScalar, String::new(), pending_tag.take()));
            }
            if empty_sequence_item_before.contains(&index) {
                output.push(lexeme(K::PlainScalar, String::new(), pending_tag.take()));
            }
            match child {
                GreenChild::Node(child_node) => {
                    collect(child_node, source, offset, output)?;
                }
                GreenChild::Token { kind, len } if is_scalar(*kind) => {
                    let end = offset + *len as usize;
                    let text = source
                        .get(offset..end)
                        .context("action metadata scalar range is not on a UTF-8 boundary")?;
                    output.push(ActionMetadataScalarLexeme {
                        kind: *kind,
                        text: text.to_owned(),
                        tag: pending_tag.take(),
                    });
                }
                GreenChild::Token {
                    kind: K::TagMark,
                    len,
                } => {
                    let end = offset + *len as usize;
                    pending_tag = Some(
                        source
                            .get(offset..end)
                            .context("action metadata tag range is not on a UTF-8 boundary")?
                            .to_owned(),
                    );
                }
                GreenChild::Token { .. } => {}
            }
            offset += child.text_len();
            if empty_value_after.contains(&(index + 1)) {
                output.push(lexeme(K::PlainScalar, String::new(), pending_tag.take()));
            }
        }
        if empty_sequence_item_before.contains(&children.len()) {
            output.push(lexeme(K::PlainScalar, String::new(), pending_tag.take()));
        }
        Ok(offset)
    }

    let document =
        serde_yaml::cst::parse_document(contents).context("parse action metadata scalar syntax")?;
    fn reject_collection_keys(node: &GreenNode) -> Result<()> {
        let children: Vec<_> = node.children().collect();
        match node.kind() {
            SyntaxKind::MappingEntry => {
                if let Some(colon) = children.iter().position(|child| {
                    matches!(
                        child,
                        GreenChild::Token {
                            kind: SyntaxKind::ColonIndicator,
                            ..
                        }
                    )
                }) && children[..colon]
                    .iter()
                    .any(|child| matches!(child, GreenChild::Node(_)))
                {
                    bail!("actions/runner does not accept collection mapping keys");
                }
            }
            SyntaxKind::FlowMapping => {
                let mut in_key = true;
                for child in &children {
                    match child {
                        GreenChild::Token {
                            kind: SyntaxKind::OpenBrace | SyntaxKind::Comma,
                            ..
                        } => in_key = true,
                        GreenChild::Token {
                            kind: SyntaxKind::ColonIndicator,
                            ..
                        } if in_key => in_key = false,
                        GreenChild::Token {
                            kind: SyntaxKind::CloseBrace,
                            ..
                        } => break,
                        GreenChild::Node(_) if in_key => {
                            bail!("actions/runner does not accept collection mapping keys");
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        for child in children {
            if let GreenChild::Node(child_node) = child {
                reject_collection_keys(child_node)?;
            }
        }
        Ok(())
    }

    reject_collection_keys(document.syntax())?;
    let mut lexemes = Vec::new();
    collect(document.syntax(), contents, 0, &mut lexemes)?;
    Ok(lexemes)
}

fn normalize_runner_scalar_lexemes(
    value: serde_yaml::Value,
    lexemes: &[ActionMetadataScalarLexeme],
    tag_directives: &BTreeMap<String, String>,
) -> Result<serde_yaml::Value> {
    fn next_lexeme<'a>(
        lexemes: &'a [ActionMetadataScalarLexeme],
        cursor: &mut usize,
    ) -> Option<&'a ActionMetadataScalarLexeme> {
        let lexeme = lexemes.get(*cursor)?;
        *cursor += 1;
        Some(lexeme)
    }

    fn walk(
        value: serde_yaml::Value,
        lexemes: &[ActionMetadataScalarLexeme],
        tag_directives: &BTreeMap<String, String>,
        cursor: &mut usize,
        preserve_number: bool,
    ) -> Result<serde_yaml::Value> {
        match value {
            serde_yaml::Value::Mapping(mapping) => {
                let mut normalized = serde_yaml::Mapping::new();
                for (key, value) in mapping {
                    let Some(lexeme) = next_lexeme(lexemes, cursor) else {
                        bail!("action metadata scalar syntax does not match its parsed mapping");
                    };
                    let key = runner_mapping_key_from_lexeme(key, lexeme, tag_directives)?;
                    let preserve_value_number =
                        runner_ordinal_ignore_case_eq(&key, "deprecationMessage");
                    let value = walk(
                        value,
                        lexemes,
                        tag_directives,
                        cursor,
                        preserve_value_number,
                    )?;
                    if normalized.insert(key.clone(), value).is_some() {
                        bail!("action metadata keys collide after Runner scalar coercion: `{key}`");
                    }
                }
                Ok(serde_yaml::Value::Mapping(normalized))
            }
            serde_yaml::Value::Sequence(sequence) => sequence
                .into_iter()
                .map(|value| walk(value, lexemes, tag_directives, cursor, preserve_number))
                .collect::<Result<Vec<_>>>()
                .map(serde_yaml::Value::Sequence),
            serde_yaml::Value::Tagged(value) => {
                if matches!(
                    value.value(),
                    serde_yaml::Value::Mapping(_) | serde_yaml::Value::Sequence(_)
                ) {
                    return walk(
                        value.value().clone(),
                        lexemes,
                        tag_directives,
                        cursor,
                        preserve_number,
                    );
                }
                let lexeme = next_lexeme(lexemes, cursor)
                    .context("action metadata scalar syntax does not match its tagged value")?;
                normalize_explicit_scalar_tag(
                    serde_yaml::Value::Tagged(value),
                    lexeme,
                    preserve_number,
                    tag_directives,
                )
            }
            serde_yaml::Value::Number(number) => {
                let Some(lexeme) = next_lexeme(lexemes, cursor) else {
                    bail!("action metadata scalar syntax does not match its parsed number");
                };
                if lexeme.tag.is_some() {
                    return normalize_explicit_scalar_tag(
                        serde_yaml::Value::Number(number),
                        lexeme,
                        preserve_number,
                        tag_directives,
                    );
                }
                if !preserve_number && lexeme.kind == serde_yaml::cst::SyntaxKind::PlainScalar {
                    let raw = lexeme.text.trim();
                    if let Some(value) = runner_numeric_lexeme_to_string(raw) {
                        return Ok(serde_yaml::Value::String(value));
                    }
                    if is_runner_radix_integer_lexeme(raw) {
                        bail!("Runner rejects out-of-range action metadata integer `{raw}`");
                    }
                    return Ok(serde_yaml::Value::String(raw.to_owned()));
                }
                let _ = number;
                Ok(serde_yaml::Value::Number(number))
            }
            serde_yaml::Value::String(value) => {
                let Some(lexeme) = next_lexeme(lexemes, cursor) else {
                    bail!("action metadata scalar syntax does not match its parsed string");
                };
                if lexeme.tag.is_some() {
                    return normalize_explicit_scalar_tag(
                        serde_yaml::Value::String(value),
                        lexeme,
                        preserve_number,
                        tag_directives,
                    );
                }
                if lexeme.kind == serde_yaml::cst::SyntaxKind::PlainScalar {
                    let raw = lexeme.text.trim();
                    if is_runner_radix_integer_lexeme(raw) {
                        bail!("Runner rejects out-of-range action metadata integer `{raw}`");
                    }
                    if let Some(value) = runner_numeric_lexeme_to_string(raw) {
                        return Ok(serde_yaml::Value::String(value));
                    }
                }
                Ok(serde_yaml::Value::String(value))
            }
            scalar => {
                let lexeme = next_lexeme(lexemes, cursor)
                    .context("action metadata scalar syntax does not match its parsed value")?;
                if lexeme.tag.is_some() {
                    normalize_explicit_scalar_tag(scalar, lexeme, preserve_number, tag_directives)
                } else {
                    Ok(scalar)
                }
            }
        }
    }

    let mut cursor = 0usize;
    let normalized = walk(value, lexemes, tag_directives, &mut cursor, false)?;
    if cursor != lexemes.len() {
        bail!("action metadata scalar syntax does not match its parsed document");
    }
    Ok(normalized)
}

fn runner_mapping_key_from_lexeme(
    key: String,
    lexeme: &ActionMetadataScalarLexeme,
    tag_directives: &BTreeMap<String, String>,
) -> Result<String> {
    if let Some(tag) = lexeme
        .tag
        .as_deref()
        .and_then(|tag| runner_core_scalar_tag_from_source(tag, tag_directives))
    {
        let raw = lexeme.text.trim();
        return match tag {
            ActionMetadataScalarTag::String => {
                if lexeme.kind == serde_yaml::cst::SyntaxKind::PlainScalar {
                    Ok(raw.to_owned())
                } else {
                    Ok(key)
                }
            }
            ActionMetadataScalarTag::Bool => match raw {
                "true" | "True" | "TRUE" => Ok("true".to_owned()),
                "false" | "False" | "FALSE" => Ok("false".to_owned()),
                _ => bail!("invalid boolean action metadata key"),
            },
            ActionMetadataScalarTag::Int => runner_action_integer_number(raw)
                .map(runner_number_to_string)
                .context("invalid integer action metadata key"),
            ActionMetadataScalarTag::Float => runner_action_float_number(raw)
                .map(runner_number_to_string)
                .context("invalid float action metadata key"),
            ActionMetadataScalarTag::Null => Ok(String::new()),
        };
    }
    if lexeme.kind != serde_yaml::cst::SyntaxKind::PlainScalar {
        return Ok(key);
    }
    let value = lexeme.text.trim();
    if matches!(value, "" | "null" | "Null" | "NULL" | "~") {
        return Ok(String::new());
    }
    if let Some(value) = runner_numeric_lexeme_to_string(value) {
        return Ok(value);
    }
    if is_runner_radix_integer_lexeme(value) {
        bail!("Runner rejects out-of-range action metadata integer `{value}`");
    }
    Ok(value.to_owned())
}

fn normalize_explicit_scalar_tag(
    value: serde_yaml::Value,
    lexeme: &ActionMetadataScalarLexeme,
    preserve_number: bool,
    tag_directives: &BTreeMap<String, String>,
) -> Result<serde_yaml::Value> {
    let tag = lexeme
        .tag
        .as_deref()
        .and_then(|tag| runner_core_scalar_tag_from_source(tag, tag_directives))
        .or_else(|| match &value {
            serde_yaml::Value::Tagged(tagged) => runner_core_scalar_tag(tagged.tag().as_ref()),
            _ => None,
        })
        .context("unsupported action metadata scalar tag")?;
    let raw = lexeme.text.trim();
    let original = value.clone();
    let inner = match value {
        serde_yaml::Value::Tagged(tagged) => tagged.value().clone(),
        scalar => scalar,
    };
    match tag {
        ActionMetadataScalarTag::String => {
            let text = match inner {
                serde_yaml::Value::String(value) => value,
                _ if lexeme.kind == serde_yaml::cst::SyntaxKind::PlainScalar => raw.to_owned(),
                _ => bail!("tagged string action metadata value is not a scalar string"),
            };
            Ok(serde_yaml::Value::String(text))
        }
        ActionMetadataScalarTag::Bool => match raw {
            "true" | "True" | "TRUE" => Ok(serde_yaml::Value::Bool(true)),
            "false" | "False" | "FALSE" => Ok(serde_yaml::Value::Bool(false)),
            _ => bail!("invalid boolean action metadata value"),
        },
        ActionMetadataScalarTag::Int => {
            if preserve_number {
                return Ok(original);
            }
            runner_action_integer_number(raw)
                .map(runner_number_to_string)
                .map(serde_yaml::Value::String)
                .context("invalid integer action metadata value")
        }
        ActionMetadataScalarTag::Float => {
            if preserve_number {
                return Ok(original);
            }
            runner_action_float_number(raw)
                .map(runner_number_to_string)
                .map(serde_yaml::Value::String)
                .context("invalid float action metadata value")
        }
        ActionMetadataScalarTag::Null => Ok(serde_yaml::Value::Null),
    }
}

fn runner_numeric_lexeme_to_string(value: &str) -> Option<String> {
    runner_action_integer_number(value)
        .or_else(|| runner_action_float_number(value))
        .map(runner_number_to_string)
}

fn is_runner_radix_integer_lexeme(value: &str) -> bool {
    let Some((digits, radix)) = value
        .strip_prefix("0x")
        .map(|digits| (digits, 16))
        .or_else(|| value.strip_prefix("0o").map(|digits| (digits, 8)))
    else {
        return false;
    };
    !digits.is_empty()
        && match radix {
            16 => digits.bytes().all(|byte| byte.is_ascii_hexdigit()),
            _ => digits.bytes().all(|byte| (b'0'..=b'7').contains(&byte)),
        }
}

fn validate_nested_action_metadata_map_keys(value: &serde_yaml::Value) -> Result<()> {
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            let mut seen = BTreeSet::new();
            for (key, value) in mapping {
                if !seen.insert(runner_ordinal_ignore_case_key(key)) {
                    bail!("action metadata contains nested keys that differ only by case: `{key}`");
                }
                validate_nested_action_metadata_map_keys(value)?;
            }
        }
        serde_yaml::Value::Sequence(sequence) => {
            for value in sequence {
                validate_nested_action_metadata_map_keys(value)?;
            }
        }
        serde_yaml::Value::Tagged(tagged) => {
            validate_nested_action_metadata_map_keys(tagged.value())?;
        }
        _ => {}
    }
    Ok(())
}

fn normalize_action_metadata_document(value: serde_yaml::Value) -> Result<serde_yaml::Value> {
    let mut root = normalize_action_metadata_mapping(value, "action metadata root", None)?;
    if let Some(runs) = root.get("runs").cloned() {
        root.insert("runs", normalize_action_runs(runs)?);
    }

    if let Some(outputs) = root.get("outputs").cloned() {
        let outputs = normalize_action_metadata_mapping(outputs, "action outputs", None)?;
        let mut normalized_outputs = serde_yaml::Mapping::new();
        for (name, definition) in outputs {
            let definition = normalize_action_metadata_mapping(
                definition,
                "action output definition",
                Some(&["description", "value"]),
            )?;
            normalized_outputs.insert(name, serde_yaml::Value::Mapping(definition));
        }
        root.insert("outputs", serde_yaml::Value::Mapping(normalized_outputs));
    }

    Ok(serde_yaml::Value::Mapping(root))
}

fn normalize_action_runs(value: serde_yaml::Value) -> Result<serde_yaml::Value> {
    let mapping = normalize_action_metadata_mapping(value, "action runs", None)?;
    let plugin = mapping.contains_key("plugin");
    let allowed = if plugin {
        &["plugin"][..]
    } else {
        let using = mapping
            .get("using")
            .cloned()
            .context("action runs metadata is missing `using` or `plugin`")?;
        let using = action_metadata_scalar_to_string(using)
            .map_err(anyhow::Error::msg)?
            .to_ascii_lowercase();
        let has_node_only_field = ["main", "pre", "post"]
            .iter()
            .any(|key| mapping.contains_key(key));
        let has_container_only_field = [
            "image",
            "entrypoint",
            "args",
            "env",
            "pre-entrypoint",
            "post-entrypoint",
        ]
        .iter()
        .any(|key| mapping.contains_key(key));
        if matches!(using.as_str(), "node12" | "node16" | "node20" | "node24")
            && !has_node_only_field
            && !has_container_only_field
            && (mapping.contains_key("pre-if") || mapping.contains_key("post-if"))
        {
            bail!("action runs metadata is ambiguous between node and container variants");
        }
        match using.as_str() {
            "docker" => &[
                "using",
                "image",
                "entrypoint",
                "args",
                "env",
                "pre-entrypoint",
                "pre-if",
                "post-entrypoint",
                "post-if",
            ][..],
            "node12" | "node16" | "node20" | "node24" => {
                &["using", "main", "pre", "pre-if", "post", "post-if"][..]
            }
            "composite" => {
                if !mapping.contains_key("steps") {
                    bail!("composite action `runs` metadata is missing `steps`");
                }
                &["using", "steps"][..]
            }
            _ if ["main", "pre", "post"]
                .iter()
                .any(|key| mapping.contains_key(key)) =>
            {
                &["using", "main", "pre", "pre-if", "post", "post-if"][..]
            }
            _ if [
                "image",
                "entrypoint",
                "args",
                "env",
                "pre-entrypoint",
                "post-entrypoint",
            ]
            .iter()
            .any(|key| mapping.contains_key(key)) =>
            {
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
                ][..]
            }
            _ if mapping.contains_key("steps") => &["using", "steps"][..],
            _ => bail!("unsupported action runs variant `{using}`"),
        }
    };

    let mut normalized = normalize_action_metadata_mapping(
        serde_yaml::Value::Mapping(mapping),
        "action runs",
        Some(allowed),
    )?;
    if !plugin && let Some(steps) = normalized.get_mut("steps") {
        let serde_yaml::Value::Sequence(steps) = steps else {
            bail!("composite action `runs.steps` must be a sequence");
        };
        for step in steps {
            *step =
                normalize_composite_action_step(std::mem::replace(step, serde_yaml::Value::Null))?;
        }
    }
    Ok(serde_yaml::Value::Mapping(normalized))
}

fn normalize_composite_action_step(value: serde_yaml::Value) -> Result<serde_yaml::Value> {
    let mapping = normalize_action_metadata_mapping(value, "composite action step", None)?;
    let has_run = mapping.contains_key("run");
    let has_uses = mapping.contains_key("uses");
    if has_run == has_uses {
        bail!("composite action step must define exactly one of `run` or `uses`");
    }

    let allowed = if has_run {
        if !mapping.contains_key("shell") {
            bail!("composite run step requires `shell`");
        }
        &[
            "name",
            "id",
            "if",
            "run",
            "env",
            "continue-on-error",
            "working-directory",
            "shell",
        ][..]
    } else {
        &[
            "name",
            "id",
            "if",
            "uses",
            "continue-on-error",
            "with",
            "env",
        ][..]
    };
    let normalized = normalize_action_metadata_mapping(
        serde_yaml::Value::Mapping(mapping),
        "composite action step",
        Some(allowed),
    )?;
    Ok(serde_yaml::Value::Mapping(normalized))
}

fn normalize_action_metadata_mapping(
    value: serde_yaml::Value,
    context: &str,
    allowed: Option<&[&str]>,
) -> Result<serde_yaml::Mapping> {
    let serde_yaml::Value::Mapping(mapping) = value else {
        bail!("{context} must be a mapping");
    };
    let mut normalized = serde_yaml::Mapping::new();
    let mut seen = BTreeSet::new();
    for (key, value) in mapping {
        if key.is_empty() {
            bail!("{context} keys must be non-empty");
        }
        if !seen.insert(runner_ordinal_ignore_case_key(&key)) {
            bail!("{context} contains duplicate keys ignoring case: `{key}`");
        }
        if allowed.is_some_and(|allowed| !allowed.contains(&key.as_str())) {
            bail!("{context} contains unsupported field `{key}`");
        }
        normalized.insert(key, value);
    }
    Ok(normalized)
}

#[derive(Debug)]
struct ActionMetadataTagSyntax {
    source: String,
    style: Option<serde_yaml::cst::SyntaxKind>,
}

fn action_metadata_tag_syntax(contents: &str) -> Result<Vec<ActionMetadataTagSyntax>> {
    use serde_yaml::cst::{GreenChild, GreenNode, SyntaxKind};

    fn collect(
        node: &GreenNode,
        source: &str,
        mut offset: usize,
        output: &mut Vec<ActionMetadataTagSyntax>,
    ) -> Result<usize> {
        let children: Vec<_> = node.children().collect();
        for (index, child) in children.iter().enumerate() {
            match child {
                GreenChild::Node(child_node) => {
                    offset = collect(child_node, source, offset, output)?;
                    continue;
                }
                GreenChild::Token {
                    kind: SyntaxKind::TagMark,
                    len,
                } => {
                    let end = offset + *len as usize;
                    let tag_source = source
                        .get(offset..end)
                        .context("action metadata tag range is not on a UTF-8 boundary")?;
                    let style = children[index + 1..]
                        .iter()
                        .find_map(|next| match next {
                            GreenChild::Node(_) => Some(None),
                            GreenChild::Token {
                                kind:
                                    SyntaxKind::Whitespace
                                    | SyntaxKind::Newline
                                    | SyntaxKind::Comment
                                    | SyntaxKind::AnchorMark,
                                ..
                            } => None,
                            GreenChild::Token { kind, .. } => Some(Some(*kind)),
                        })
                        .flatten();
                    output.push(ActionMetadataTagSyntax {
                        source: tag_source.to_owned(),
                        style,
                    });
                }
                GreenChild::Token { .. } => {}
            }
            offset += child.text_len();
        }
        Ok(offset)
    }

    let document =
        serde_yaml::cst::parse_document(contents).context("parse action metadata syntax")?;
    let mut budget = ActionMetadataCstBudget::default();
    validate_action_metadata_cst_budget(document.syntax(), 0, &mut budget, contents.len())?;
    let mut tags = Vec::new();
    collect(document.syntax(), contents, 0, &mut tags)?;
    Ok(tags)
}

#[derive(Debug)]
struct ActionMetadataTagPolicy {
    tags: Vec<ActionMetadataTagSyntax>,
    directives: BTreeMap<String, String>,
    next_tag: AtomicUsize,
}

impl serde_yaml::policy::Policy for ActionMetadataTagPolicy {
    fn check_event(&self, event: serde_yaml::policy::PolicyEvent<'_>) -> serde_yaml::Result<()> {
        if event.anchor.is_some() || event.kind == serde_yaml::policy::PolicyEventKind::Alias {
            return Err(serde_yaml::Error::Deserialize(
                "actions/runner does not support YAML anchors or aliases in action metadata"
                    .to_owned(),
            ));
        }
        let Some(tag) = event.tag else {
            return Ok(());
        };
        let index = self.next_tag.fetch_add(1, Ordering::Relaxed);
        let syntax = self.tags.get(index);
        if event.kind != serde_yaml::policy::PolicyEventKind::Scalar {
            return Ok(());
        }
        let source_tag = syntax.map_or(tag, |syntax| syntax.source.as_str());
        let style = syntax.and_then(|syntax| syntax.style);
        let Some(scalar_tag) = runner_core_scalar_tag_from_source(source_tag, &self.directives)
        else {
            return Err(serde_yaml::Error::Deserialize(format!(
                "actions/runner does not accept YAML tag `{source_tag}` in action metadata"
            )));
        };
        let plain_required = !matches!(scalar_tag, ActionMetadataScalarTag::String);
        let empty_null = scalar_tag == ActionMetadataScalarTag::Null
            && event.scalar.unwrap_or_default().is_empty();
        let empty_tagged_scalar = empty_null
            || (scalar_tag == ActionMetadataScalarTag::String
                && event.scalar.unwrap_or_default().is_empty());
        if (style.is_none() && !empty_tagged_scalar)
            || (plain_required
                && style != Some(serde_yaml::cst::SyntaxKind::PlainScalar)
                && !empty_null)
            || !runner_tagged_scalar_lexeme_is_valid(scalar_tag, event.scalar.unwrap_or_default())
        {
            return Err(serde_yaml::Error::Deserialize(format!(
                "invalid scalar for actions/runner YAML tag `{source_tag}`"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionMetadataScalarTag {
    String,
    Bool,
    Int,
    Float,
    Null,
}

fn runner_core_scalar_tag(tag: &str) -> Option<ActionMetadataScalarTag> {
    match tag {
        "!!str" | "tag:yaml.org,2002:str" => Some(ActionMetadataScalarTag::String),
        "!!bool" | "tag:yaml.org,2002:bool" => Some(ActionMetadataScalarTag::Bool),
        "!!int" | "tag:yaml.org,2002:int" => Some(ActionMetadataScalarTag::Int),
        "!!float" | "tag:yaml.org,2002:float" => Some(ActionMetadataScalarTag::Float),
        "!!null" | "tag:yaml.org,2002:null" => Some(ActionMetadataScalarTag::Null),
        _ => None,
    }
}

fn runner_action_tag_directives(contents: &str) -> Result<BTreeMap<String, String>> {
    let mut directives = BTreeMap::new();
    for (line_number, line) in contents.lines().enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(marker_tail) = line.strip_prefix("---") {
            let marker_tail = marker_tail.trim_start();
            if marker_tail.is_empty() || marker_tail.starts_with('#') {
                break;
            }
        }
        if !line.starts_with('%') {
            break;
        }

        let mut fields = line.split_whitespace();
        let directive = fields.next().unwrap_or_default();
        if directive != "%TAG" {
            // YAML permits other directives (for example `%YAML`) in the
            // prolog. The YAML parser validates their syntax; they do not
            // affect Runner's scalar-tag classification here.
            continue;
        }
        let handle = fields.next().ok_or_else(|| {
            anyhow::anyhow!(
                "YAML %TAG directive on line {} is missing its handle",
                line_number + 1
            )
        })?;
        let prefix = fields.next().ok_or_else(|| {
            anyhow::anyhow!(
                "YAML %TAG directive on line {} is missing its prefix",
                line_number + 1
            )
        })?;
        if fields.next().is_some_and(|field| field != "#") {
            bail!(
                "YAML %TAG directive on line {} has unexpected trailing fields",
                line_number + 1
            );
        }
        if !(handle == "!"
            || handle == "!!"
            || (handle.starts_with('!') && handle.ends_with('!') && handle.len() > 2))
        {
            bail!("invalid YAML %TAG handle `{handle}`");
        }
        if directives
            .insert(handle.to_owned(), prefix.to_owned())
            .is_some()
        {
            bail!("duplicate YAML %TAG directive for handle `{handle}`");
        }
    }
    Ok(directives)
}

fn decode_yaml_tag_uri(value: &str) -> Option<String> {
    fn hex_digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_digit(*bytes.get(index + 1)?)?;
            let low = hex_digit(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn runner_core_scalar_tag_from_source(
    tag: &str,
    directives: &BTreeMap<String, String>,
) -> Option<ActionMetadataScalarTag> {
    let expanded = if let Some(uri) = tag.strip_prefix("!<").and_then(|tag| tag.strip_suffix('>')) {
        uri.to_owned()
    } else if let Some(tag_suffix) = tag.strip_prefix("!!") {
        // An explicit %TAG directive may override YAML's default secondary
        // handle. Runner uses the expanded tag URI, so preserve that override.
        let prefix = directives
            .get("!!")
            .map(String::as_str)
            .unwrap_or("tag:yaml.org,2002:");
        format!("{prefix}{tag_suffix}")
    } else {
        let separator = tag[1..].find('!').map(|index| index + 1);
        let (handle, suffix) = separator.map_or(("!", &tag[1..]), |index| {
            (&tag[..=index], &tag[index + 1..])
        });
        format!("{}{}", directives.get(handle)?, suffix)
    };
    let expanded = decode_yaml_tag_uri(&expanded)?;
    runner_core_scalar_tag(&expanded)
}

fn runner_tagged_scalar_lexeme_is_valid(tag: ActionMetadataScalarTag, value: &str) -> bool {
    match tag {
        ActionMetadataScalarTag::String => true,
        ActionMetadataScalarTag::Bool => matches!(
            value,
            "true" | "True" | "TRUE" | "false" | "False" | "FALSE"
        ),
        ActionMetadataScalarTag::Int => runner_action_integer_number(value).is_some(),
        ActionMetadataScalarTag::Float => runner_action_float_number(value).is_some(),
        ActionMetadataScalarTag::Null => {
            matches!(value, "" | "null" | "Null" | "NULL" | "~")
        }
    }
}

fn runner_action_integer_number(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    if !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit) {
        return value.parse::<f64>().ok();
    }
    if bytes.len() > 1
        && matches!(bytes[0], b'+' | b'-')
        && bytes[1..].iter().all(u8::is_ascii_digit)
    {
        return value.parse::<f64>().ok();
    }
    if let Some(digits) = value.strip_prefix("0x") {
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let raw = u32::from_str_radix(digits, 16).ok()?;
        return Some((raw as i32) as f64);
    }
    if let Some(digits) = value.strip_prefix("0o") {
        if digits.is_empty() || !digits.bytes().all(|byte| (b'0'..=b'7').contains(&byte)) {
            return None;
        }
        return i32::from_str_radix(digits, 8).ok().map(f64::from);
    }
    None
}

fn runner_action_float_number(value: &str) -> Option<f64> {
    match value {
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
            return Some(f64::INFINITY);
        }
        "-.inf" | "-.Inf" | "-.INF" => return Some(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => return Some(f64::NAN),
        _ => {}
    }

    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut index = usize::from(matches!(bytes[0], b'+' | b'-'));
    let mut has_integer = false;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        has_integer = true;
        index += 1;
    }
    let mut has_dot = false;
    if bytes.get(index) == Some(&b'.') {
        has_dot = true;
        index += 1;
    }
    let mut has_fraction = false;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        has_fraction = true;
        index += 1;
    }
    if !(has_integer || (has_dot && has_fraction)) {
        return None;
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return None;
        }
    }
    if index != bytes.len() {
        return None;
    }
    value.parse::<f64>().ok()
}

const MAX_METADATA_PARSE_NESTING: usize = 64;
const MAX_METADATA_PARSE_NODES: usize = 50_000;
const MAX_METADATA_PARSE_EVENTS: usize = 1_000_000;
const MAX_METADATA_PARSE_ESTIMATED_BYTES: usize = 10 * 1024 * 1024;
const MAX_LOCAL_ACTION_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_ACTION_INPUTS: usize = 256;
const MAX_ACTION_INPUT_NAME_BYTES: usize = 256;
const MAX_ACTION_INPUT_VALUE_BYTES: usize = 64 * 1024;
const MAX_ACTION_INPUT_BYTES: usize = 256 * 1024;

fn runner_action_parser_config() -> serde_yaml::ParserConfig {
    serde_yaml::ParserConfig::new()
        .max_document_length(MAX_LOCAL_ACTION_METADATA_BYTES as usize)
        .max_depth(MAX_METADATA_PARSE_NESTING)
        .max_nodes(MAX_METADATA_PARSE_NODES)
        .max_events(MAX_METADATA_PARSE_EVENTS)
        .max_total_scalar_bytes(MAX_LOCAL_ACTION_METADATA_BYTES as usize)
        .max_mapping_keys(MAX_METADATA_PARSE_NODES)
        .max_sequence_length(MAX_METADATA_PARSE_NODES)
        .max_alias_expansions(0)
        .max_documents(1)
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error)
        .with_policy(serde_yaml::policy::DenyAnchors)
}

fn metadata_document_within_budget(contents: &str) -> bool {
    let mut flow_depth = 0usize;
    let mut block_scalar_indent = None;
    for line in contents.lines() {
        let indentation = line
            .bytes()
            .take_while(|byte| *byte == b' ' || *byte == b'\t')
            .count();
        if indentation > MAX_METADATA_PARSE_NESTING * 2 {
            return false;
        }
        if block_scalar_indent.is_some_and(|base| indentation > base) {
            continue;
        }
        block_scalar_indent = None;
        let mut single_quoted = false;
        let mut double_quoted = false;
        let mut escaped = false;
        for byte in line.bytes() {
            if double_quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    double_quoted = false;
                }
                continue;
            }
            if single_quoted {
                if byte == b'\'' {
                    single_quoted = false;
                }
                continue;
            }
            match byte {
                b'#' => break,
                b'\'' => single_quoted = true,
                b'"' => double_quoted = true,
                b'[' | b'{' => {
                    flow_depth = flow_depth.saturating_add(1);
                    if flow_depth > MAX_METADATA_PARSE_NESTING {
                        return false;
                    }
                }
                b']' | b'}' => flow_depth = flow_depth.saturating_sub(1),
                _ => {}
            }
        }
        let trimmed = line.trim_end();
        if trimmed.ends_with('|') || trimmed.ends_with('>') {
            block_scalar_indent = Some(indentation);
        }
    }
    true
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
    deserializer.deserialize_map(ActionMetadataStringMapVisitor)
}

fn deserialize_optional_action_metadata_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(deserializer)?;
    action_metadata_scalar_to_string(value)
        .map(Some)
        .map_err(D::Error::custom)
}

fn deserialize_optional_non_empty_action_metadata_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(deserializer)?;
    let value = action_metadata_scalar_to_string(value).map_err(D::Error::custom)?;
    if value.is_empty() {
        return Err(D::Error::custom(
            "action metadata value must be a non-empty string",
        ));
    }
    Ok(Some(value))
}

fn deserialize_optional_boolean_or_expression<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(deserializer)?;
    match value {
        serde_yaml::Value::Bool(value) => Ok(Some(value.to_string())),
        serde_yaml::Value::Tagged(tagged)
            if runner_core_scalar_tag(tagged.tag().as_ref())
                == Some(ActionMetadataScalarTag::Bool) =>
        {
            action_metadata_scalar_to_string(serde_yaml::Value::Tagged(tagged))
                .map(Some)
                .map_err(D::Error::custom)
        }
        serde_yaml::Value::String(value) => {
            let spans =
                crate::executor::expression_template_spans(&value).map_err(D::Error::custom)?;
            if spans.len() != 1
                || spans[0].start() != 0
                || spans[0].end() != value.len()
                || spans[0].expression(&value).trim().is_empty()
            {
                return Err(D::Error::custom(
                    "continue-on-error must be a boolean or one complete expression",
                ));
            }
            crate::executor::validate_deferred_expression_template(&value)
                .map_err(D::Error::custom)?;
            Ok(Some(value))
        }
        _ => Err(D::Error::custom(
            "continue-on-error must be a boolean or one complete expression",
        )),
    }
}

fn deserialize_non_empty_action_metadata_string<'de, D>(
    deserializer: D,
) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_optional_non_empty_action_metadata_string(deserializer)?
        .ok_or_else(|| D::Error::custom("action metadata value must be a non-empty string"))
}

fn deserialize_action_metadata_string_list<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<serde_yaml::Value>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|value| action_metadata_scalar_to_string(value).map_err(D::Error::custom))
        .collect()
}

struct ActionMetadataStringMapVisitor;

impl<'de> Visitor<'de> for ActionMetadataStringMapVisitor {
    type Value = BTreeMap<String, String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an action metadata mapping with non-empty scalar keys and values")
    }

    fn visit_map<A>(self, mut mapping: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = BTreeMap::new();
        let mut seen_keys = BTreeSet::new();
        while let Some(ActionMetadataMapKey(key)) = mapping.next_key()? {
            if key.is_empty() {
                return Err(A::Error::custom(
                    "action metadata mapping keys must be non-empty strings",
                ));
            }
            if !seen_keys.insert(runner_ordinal_ignore_case_key(&key)) {
                return Err(A::Error::custom(format!(
                    "duplicate action metadata mapping key after case-insensitive matching: `{key}`"
                )));
            }
            let value = action_metadata_scalar_to_string(mapping.next_value()?)
                .map_err(A::Error::custom)?;
            values.insert(key, value);
        }
        Ok(values)
    }
}

struct ActionInputVisitor;

impl<'de> Visitor<'de> for ActionInputVisitor {
    type Value = ActionInput;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an action input metadata mapping")
    }

    fn visit_map<A>(self, mut mapping: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut input = ActionInput {
            description: None,
            default_value: None,
            required: false,
        };
        let mut seen_keys = BTreeSet::new();
        while let Some(ActionMetadataMapKey(key)) = mapping.next_key()? {
            if key.is_empty() {
                return Err(A::Error::custom(
                    "action input metadata keys must be non-empty strings",
                ));
            }
            if !seen_keys.insert(runner_ordinal_ignore_case_key(&key)) {
                return Err(A::Error::custom(format!(
                    "duplicate action input metadata key after case-insensitive matching: `{key}`"
                )));
            }
            if runner_ordinal_ignore_case_eq(&key, "default") {
                input.default_value = Some(
                    action_metadata_scalar_to_string(mapping.next_value()?)
                        .map_err(A::Error::custom)?,
                );
            } else if key == "description" {
                input.description = match mapping.next_value::<serde_yaml::Value>()? {
                    serde_yaml::Value::String(value) => Some(value),
                    _ => None,
                };
            } else if key == "required" {
                input.required = matches!(
                    mapping.next_value::<serde_yaml::Value>()?,
                    serde_yaml::Value::Bool(true)
                );
            } else if runner_ordinal_ignore_case_eq(&key, "deprecationMessage") {
                let _: String = mapping.next_value()?;
            } else {
                let _: serde::de::IgnoredAny = mapping.next_value()?;
            }
        }
        Ok(input)
    }
}

struct ActionMetadataMapKey(String);

impl<'de> Deserialize<'de> for ActionMetadataMapKey {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        action_metadata_scalar_to_string(value)
            .map(Self)
            .map_err(D::Error::custom)
    }
}

fn action_metadata_scalar_to_string(
    value: serde_yaml::Value,
) -> std::result::Result<String, &'static str> {
    match value {
        serde_yaml::Value::Null => Ok(String::new()),
        serde_yaml::Value::Bool(value) => Ok(value.to_string()),
        serde_yaml::Value::Number(value) => Ok(runner_number_to_string(value.as_f64())),
        serde_yaml::Value::String(value) => Ok(value),
        serde_yaml::Value::Tagged(tagged) => {
            let tag = tagged.tag().to_string();
            let scalar = match tagged.value().clone() {
                serde_yaml::Value::String(value) => value,
                _ => return Err("tagged action metadata values must be scalar strings"),
            };
            match runner_core_scalar_tag(&tag) {
                Some(ActionMetadataScalarTag::String) => Ok(scalar),
                Some(ActionMetadataScalarTag::Bool) => match scalar.as_str() {
                    "true" | "True" | "TRUE" => Ok("true".to_owned()),
                    "false" | "False" | "FALSE" => Ok("false".to_owned()),
                    _ => Err("invalid boolean action metadata value"),
                },
                Some(ActionMetadataScalarTag::Int) => runner_action_integer_number(&scalar)
                    .map(runner_number_to_string)
                    .ok_or("invalid integer action metadata value"),
                Some(ActionMetadataScalarTag::Float) => runner_action_float_number(&scalar)
                    .map(runner_number_to_string)
                    .ok_or("invalid float action metadata value"),
                Some(ActionMetadataScalarTag::Null) => Ok(String::new()),
                None => Err("actions/runner does not accept this action metadata tag"),
            }
        }
        serde_yaml::Value::Sequence(_) | serde_yaml::Value::Mapping(_) => {
            Err("action metadata values must be YAML scalars")
        }
    }
}

/// Match Actions Runner's `NumberToken.ToString()` (`G15`, invariant culture).
fn runner_number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == f64::INFINITY {
        return "Infinity".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }

    let sign = if value.is_sign_negative() { "-" } else { "" };
    let scientific = format!("{:.14e}", value.abs());
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return value.to_string();
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return value.to_string();
    };
    let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');

    if !(-4..15).contains(&exponent) {
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{mantissa}E{exponent_sign}{:02}", exponent.abs());
    }

    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    let decimal_index = exponent + 1;
    let mut result = if decimal_index <= 0 {
        format!(
            "0.{}{}",
            "0".repeat(decimal_index.unsigned_abs() as usize),
            digits
        )
    } else if decimal_index as usize >= digits.len() {
        format!(
            "{}{}",
            digits,
            "0".repeat(decimal_index as usize - digits.len())
        )
    } else {
        let decimal_index = decimal_index as usize;
        format!("{}.{}", &digits[..decimal_index], &digits[decimal_index..])
    };
    if result.contains('.') {
        while result.ends_with('0') {
            result.pop();
        }
        if result.ends_with('.') {
            result.pop();
        }
    }
    format!("{sign}{result}")
}

fn runner_ordinal_ignore_case_key(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if matches!(character, '\u{0131}' | '\u{017f}') {
                return character;
            }
            let mut uppercase = character.to_uppercase();
            match (uppercase.next(), uppercase.next()) {
                (Some(single), None) => single,
                _ => character,
            }
        })
        .collect()
}

fn runner_ordinal_ignore_case_eq(left: &str, right: &str) -> bool {
    runner_ordinal_ignore_case_key(left) == runner_ordinal_ignore_case_key(right)
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
        plans.push(LocalActionPlan {
            step_id: step_id(step, plans.len()),
            workspace_root: workspace_host.to_path_buf(),
            action_dir: local_action_dir(workspace_host, path)?,
            inputs: render_inputs(&string_inputs(step)?, context_data)?,
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
    validate_remote_action_root(&plan.repository_dir, &plan.action_dir)?;
    let metadata_path = action_metadata_path(&plan.action_dir)?;
    let metadata = parse_action_metadata(&read_bounded_file(&metadata_path)?)?;
    let runtime = metadata.runtime()?;
    Ok(ResolvedAction {
        plan: plan.clone(),
        metadata_path,
        metadata,
        runtime,
    })
}

pub fn resolve_local_action(plan: &LocalActionPlan) -> Result<ActionMetadata> {
    validate_action_path_beneath(&plan.workspace_root, &plan.action_dir)?;
    let metadata_path = action_metadata_path(&plan.action_dir)?;
    parse_action_metadata(&read_bounded_file(&metadata_path)?)
}

fn read_bounded_file(path: &Path) -> Result<String> {
    let file = fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    let length = file.metadata()?.len();
    if length > MAX_LOCAL_ACTION_METADATA_BYTES {
        bail!(
            "action metadata {} exceeds {MAX_LOCAL_ACTION_METADATA_BYTES} bytes",
            path.display()
        );
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(MAX_LOCAL_ACTION_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LOCAL_ACTION_METADATA_BYTES {
        bail!(
            "action metadata {} exceeds {MAX_LOCAL_ACTION_METADATA_BYTES} bytes",
            path.display()
        );
    }
    String::from_utf8(bytes).context("action metadata is not UTF-8")
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
    pub inputs: BTreeMap<String, String>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerActionInvocation {
    pub image: String,
    pub build_context_host: Option<PathBuf>,
    pub dockerfile_host: Option<PathBuf>,
    pub action_container_path: String,
    pub inputs: BTreeMap<String, String>,
    /// `runs.env` from the action manifest, kept separate to preserve its
    /// precedence below workflow/step environment and effective inputs.
    pub runs_env: Vec<(String, String)>,
    pub env: Vec<(String, String)>,
    pub entrypoint: Option<String>,
    pub args: Vec<String>,
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
    pub env: Vec<(String, String)>,
}

impl ResolvedAction {
    pub fn native_invocation(&self) -> Result<Option<NativeActionInvocation>> {
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
        let main_container_path = container_path(
            actions_host,
            &resolve_metadata_action_path(&self.plan.action_dir, main)?,
        )?;
        let pre_container_path = self
            .metadata
            .runs
            .pre
            .as_ref()
            .map(|pre| {
                resolve_metadata_action_path(&self.plan.action_dir, pre)
                    .and_then(|path| container_path(actions_host, &path))
            })
            .transpose()?;
        let post_container_path = self
            .metadata
            .runs
            .post
            .as_ref()
            .map(|post| {
                resolve_metadata_action_path(&self.plan.action_dir, post)
                    .and_then(|path| container_path(actions_host, &path))
            })
            .transpose()?;
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
        let inputs = effective_inputs(&self.metadata, &self.plan.inputs)?;
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
            inputs: self.plan.inputs.clone(),
            env,
        })
    }

    pub fn docker_invocation(&self, actions_host: &Path) -> Result<DockerActionInvocation> {
        let ActionRuntime::Docker { image } = &self.runtime else {
            bail!("action '{}' is not a Docker action", self.plan.repository)
        };
        let action_container_path = container_path(actions_host, &self.plan.action_dir)?;
        let inputs = effective_inputs(&self.metadata, &self.plan.inputs)?;
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

        let (image, build_context_host, dockerfile_host) = match ActionImageReference::parse(image)
            .map_err(|error| {
                anyhow::anyhow!(
                    "action '{}' declares an invalid Docker image: {error}",
                    self.plan.repository
                )
            })? {
            ActionImageReference::DockerImage(image) => (image.as_str().to_owned(), None, None),
            ActionImageReference::Dockerfile(path) => {
                let dockerfile_host = resolve_metadata_action_path(&self.plan.action_dir, &path)?;
                let build_context_host = dockerfile_host
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("Dockerfile path has no parent directory"))?
                    .to_path_buf();
                let tag = docker_action_tag(
                    &self.plan.repository,
                    &self.plan.git_ref,
                    self.plan.source_path.as_deref(),
                );
                (tag, Some(build_context_host), Some(dockerfile_host))
            }
        };
        let entrypoint = self
            .metadata
            .runs
            .entrypoint
            .as_ref()
            .map(|value| render_action_scoped_value(value, &inputs, &action_container_path))
            .transpose()?;
        let args = self
            .metadata
            .runs
            .args
            .iter()
            .map(|value| render_action_scoped_value(value, &inputs, &action_container_path))
            .collect::<Result<Vec<_>>>()?;
        let render_stage_entrypoint = |value: Option<&String>| {
            value
                .map(|value| render_action_scoped_value(value, &inputs, &action_container_path))
                .transpose()
        };
        let pre_entrypoint = render_stage_entrypoint(self.metadata.runs.pre_entrypoint.as_ref())?;
        let post_entrypoint = render_stage_entrypoint(self.metadata.runs.post_entrypoint.as_ref())?;

        Ok(DockerActionInvocation {
            image,
            build_context_host,
            dockerfile_host,
            action_container_path,
            inputs,
            runs_env: self
                .metadata
                .runs
                .env
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
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
            &self.plan.repository_dir,
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
        &plan.workspace_root,
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
    let mut step_ids = BTreeMap::new();
    for (index, step) in metadata.runs.steps.iter().enumerate() {
        let step_id = composite_step_id(step_id_prefix, step.id.as_deref(), index);
        if let Some(id) = step.id.as_deref() {
            step_ids.insert(id.to_string(), step_id.clone());
        }
        if let Some(uses) = step.uses.as_deref() {
            if is_runner_local_action_reference(uses) {
                let nested_dir = local_action_dir(local_root_host, uses)?;
                validate_action_path_beneath(local_root_host, &nested_dir)?;
                if !local_stack.insert(nested_dir.clone()) {
                    bail!("nested local composite cycle at '{}'", nested_dir.display())
                }
                let metadata_path = action_metadata_path(&nested_dir)?;
                let nested_metadata = parse_action_metadata(&read_bounded_file(&metadata_path)?)?;
                if nested_metadata.runtime()? != ActionRuntime::Composite {
                    bail!("nested local action '{uses}' is not a composite action")
                }
                let nested_inputs = step
                    .with
                    .iter()
                    .map(|(name, value)| -> Result<(String, String)> {
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
                    .collect::<Result<BTreeMap<_, _>>>()?;
                let nested_action_path = if nested_dir.starts_with(actions_host) {
                    container_path(actions_host, &nested_dir)?
                } else {
                    workspace_container_path(workspace_container, &nested_dir)?
                };
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
                local_stack.remove(&nested_dir);
                continue;
            }
            let reference = parse_repository_uses(uses)?;
            let inputs = step
                .with
                .iter()
                .map(|(name, value)| -> Result<(String, String)> {
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
                .collect::<Result<BTreeMap<_, _>>>()?;
            let env = step
                .env
                .iter()
                .map(|(name, value)| -> Result<(String, String)> {
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
                .collect::<Result<Vec<_>>>()?;
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
                    )?,
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
            .map(crate::script_step::github_shell)
            .transpose()?
            .unwrap_or(crate::container::Shell::Bash);
        let rendered = render_composite_scoped_value(
            script,
            &action_inputs,
            action_path,
            workspace_container,
            &step_ids,
        )?;
        let mut env = step
            .env
            .iter()
            .map(|(name, value)| -> Result<(String, String)> {
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
            .collect::<Result<Vec<_>>>()?;
        env.push(("GITHUB_ACTION_PATH".to_string(), action_path.to_string()));
        let working_directory_container = step
            .working_directory
            .as_deref()
            .map(|path| -> Result<String> {
                let path = render_composite_scoped_value(
                    path,
                    &action_inputs,
                    action_path,
                    workspace_container,
                    &step_ids,
                )?;
                Ok(workspace_path(workspace_container, &path))
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
            continue_on_error: composite_continue_on_error(
                step,
                &action_inputs,
                action_path,
                workspace_container,
                &step_ids,
            )?,
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

fn parse_repository_uses(uses: &str) -> Result<RepositoryActionReference> {
    if is_runner_local_action_reference(uses) {
        bail!("nested local composite uses '{uses}' are not implemented yet")
    }
    if uses.starts_with("docker://") {
        bail!("nested Docker composite uses '{uses}' are not implemented yet")
    }
    RepositoryActionReference::parse(uses)
        .map_err(|error| anyhow::anyhow!("invalid repository action reference: {error}"))
}

fn action_metadata_path(action_dir: &Path) -> Result<PathBuf> {
    for file_name in ["action.yml", "action.yaml"] {
        let path = resolve_action_path(action_dir, file_name)
            .map_err(|error| anyhow::anyhow!("unsafe action metadata path: {error}"))?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => return Ok(path),
            Ok(_) => bail!("action metadata {} is not a regular file", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", path.display()));
            }
        }
    }
    bail!("action metadata not found in {}", action_dir.display())
}

/// Prove the selected remote action directory is a physical directory beneath
/// its fetched repository checkout. Repository and action path components may
/// come from remote metadata, so every component after the runner-owned
/// `_actions` root must be a real directory, never a symlink.
fn validate_remote_action_root(repository_dir: &Path, action_dir: &Path) -> Result<()> {
    let checkout_root = repository_dir
        .parent()
        .and_then(Path::parent)
        .context("repository action path is outside the fetched action cache")?;
    if checkout_root
        .file_name()
        .is_none_or(|name| name != "_actions")
    {
        bail!("repository action path is outside the fetched action cache")
    }
    let _repository_relative = repository_dir
        .strip_prefix(checkout_root)
        .context("repository action path is outside the fetched action cache")?;
    let _action_relative = action_dir
        .strip_prefix(repository_dir)
        .context("repository action subpath escapes its checkout")?;

    let root_metadata = fs::symlink_metadata(checkout_root)
        .with_context(|| format!("inspect action cache {}", checkout_root.display()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!("action cache root is not a real directory")
    }

    // Establish containment against the actual cache root, then inspect every
    // repository/ref/subpath component without following links. Canonicalizing
    // the runner-owned boundary accommodates host aliases such as macOS
    // `/tmp` -> `/private/tmp`; the cache root itself remains no-follow above.
    validate_action_path_beneath(checkout_root, action_dir)
}

/// Validate an action root relative to its trusted workspace/cache boundary.
/// Canonicalize the boundary once (so OS-level aliases do not look like an
/// escape), then reject every symlink below it before a caller reads action
/// metadata or payload paths.
fn validate_action_path_beneath(boundary_root: &Path, action_root: &Path) -> Result<()> {
    let relative = action_root
        .strip_prefix(boundary_root)
        .context("action path escapes its trusted root")?;
    let canonical_boundary = fs::canonicalize(boundary_root)
        .with_context(|| format!("resolve action root {}", boundary_root.display()))?;
    if !canonical_boundary.is_dir() {
        bail!("action root boundary is not a directory")
    }
    let mut existing = canonical_boundary;
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            bail!("action path contains a non-normal component")
        };
        existing.push(component);
        match fs::symlink_metadata(&existing) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("action path traverses a symlink: {}", existing.display())
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                break
            }
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", existing.display()));
            }
        }
    }
    Ok(())
}

fn resolve_metadata_action_path(action_dir: &Path, value: &str) -> Result<PathBuf> {
    resolve_action_path(action_dir, value)
        .map_err(|error| anyhow::anyhow!("unsafe action metadata path: {error}"))
}

fn is_local_action_reference(name: Option<&str>, path: Option<&str>) -> bool {
    local_action_path(name, path).is_some()
}

fn local_action_path<'a>(name: Option<&'a str>, path: Option<&'a str>) -> Option<&'a str> {
    if name.is_some_and(|n| !is_runner_local_action_reference(n) && n.contains('/')) {
        return None;
    }
    path.filter(|value| is_runner_local_action_reference(value))
        .or_else(|| name.filter(|value| is_runner_local_action_reference(value)))
}

fn local_action_dir(workspace_host: &Path, source_path: &str) -> Result<PathBuf> {
    let normalized_source_path = source_path.replace('\\', "/");
    let relative = SafeActionPath::parse(&normalized_source_path).map_err(|error| {
        anyhow::anyhow!("unsupported local action path '{source_path}': {error}")
    })?;
    Ok(workspace_host.join(relative.as_path()))
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
        let relative = SafeActionPath::parse(source_path).map_err(|error| {
            anyhow::anyhow!("unsupported repository action path '{source_path}': {error}")
        })?;
        dir = dir.join(relative.as_path());
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
    let canonical_name = name.to_ascii_lowercase();
    if result
        .keys()
        .any(|existing| existing.eq_ignore_ascii_case(&canonical_name))
    {
        bail!("action input names differ only by ASCII case");
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
    for _ in 0..MAX_METADATA_PARSE_NESTING {
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
    bail!("action input value nesting exceeds {MAX_METADATA_PARSE_NESTING} levels")
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
) -> Result<BTreeMap<String, String>> {
    inputs
        .iter()
        .map(|(name, value)| -> Result<(String, String)> {
            let rendered = if contains_step_output_expression(value) {
                crate::executor::validate_deferred_expression_template(value)?;
                value.clone()
            } else {
                crate::executor::render_context_expressions_bounded(value, context_data)?
            };
            Ok((name.clone(), rendered))
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
            return Some((name_end, expression_input_value(input)));
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
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
    step_ids: &BTreeMap<String, String>,
) -> Result<String> {
    let rendered = render_composite_value(value, inputs, action_path, workspace_container)?;
    Ok(rewrite_step_output_refs(&rendered, step_ids))
}

fn composite_continue_on_error(
    step: &CompositeActionStep,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
    workspace_container: &str,
    step_ids: &BTreeMap<String, String>,
) -> Result<bool> {
    step.continue_on_error
        .as_ref()
        .map(|value| -> Result<bool> {
            let rendered = render_composite_scoped_value(
                value,
                inputs,
                action_path,
                workspace_container,
                step_ids,
            )?;
            Ok(value_truthy(&serde_json::Value::String(rendered)))
        })
        .transpose()
        .map(|value| value.unwrap_or(false))
}

fn render_action_scoped_value(
    value: &str,
    inputs: &BTreeMap<String, String>,
    action_path: &str,
) -> Result<String> {
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
        let canonical_name = name.to_ascii_lowercase();
        if canonical
            .insert(canonical_name.clone(), value.clone())
            .is_some()
        {
            bail!("action input names differ only by ASCII case: {canonical_name}");
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
        let canonical_name = name.to_ascii_lowercase();
        if !declared_names.insert(canonical_name.clone()) {
            bail!("metadata input names differ only by ASCII case: {canonical_name}");
        }
        inputs.insert(
            canonical_name,
            input.default_value.clone().unwrap_or_default(),
        );
    }
    inputs.extend(canonicalize_input_map(provided)?);
    Ok(inputs)
}

fn input_value_case_insensitive<'a>(
    inputs: &'a BTreeMap<String, String>,
    name: &str,
) -> Option<&'a String> {
    inputs.get(&name.to_ascii_lowercase())
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

/// Keep a caller expression live when a nested composite action uses its input
/// inside another expression. Literal inputs still use the runner's quoted
/// form.
fn expression_input_value(value: &str) -> String {
    let value = value.trim();
    value
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map_or_else(
            || expression_single_quote(value),
            |expression| expression.trim().to_owned(),
        )
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
    default: 'true'
  check-latest:
    default: 'false'
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
    fn metadata_parser_rejects_excessive_nesting_before_yaml_parse() {
        let nested = format!("{}true{}", "[".repeat(65), "]".repeat(65));
        assert!(parse_action_metadata(&nested).is_err());
    }

    #[test]
    fn metadata_parser_enforces_document_and_node_budgets() {
        let oversized = "x".repeat(MAX_LOCAL_ACTION_METADATA_BYTES as usize + 1);
        assert!(parse_action_metadata(&oversized).is_err());

        let many_nodes = format!(
            "runs:\n  using: composite\n  steps: [{}]\n",
            "null,".repeat(MAX_METADATA_PARSE_NODES)
        );
        assert!(parse_action_metadata(&many_nodes).is_err());
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
        BOOL_VALUE: 'false'
        COUNT: '7'
      run: echo hi
    - uses: actions/setup-buildx@v4
      with:
        cleanup: 'false'
        retries: '3'
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
    fn action_metadata_scalar_fields_follow_runner_string_coercion() {
        let input_metadata = parse_action_metadata(
            "inputs:\n  boolean:\n    default: false\n  number:\n    default: 7\n  'null':\n    default: null\n  uppercase:\n    DEFAULT: 'value'\n  true:\n    default: boolean-key\n  7:\n    default: number-key\nruns:\n  using: composite\n  steps: []\n",
        )
        .unwrap();
        assert_eq!(
            input_metadata.inputs["boolean"].default_value.as_deref(),
            Some("false")
        );
        assert_eq!(
            input_metadata.inputs["number"].default_value.as_deref(),
            Some("7")
        );
        assert_eq!(
            input_metadata.inputs["null"].default_value.as_deref(),
            Some("")
        );
        assert_eq!(
            input_metadata.inputs["uppercase"].default_value.as_deref(),
            Some("value")
        );
        assert_eq!(
            input_metadata.inputs["true"].default_value.as_deref(),
            Some("boolean-key")
        );
        assert_eq!(
            input_metadata.inputs["7"].default_value.as_deref(),
            Some("number-key")
        );

        let docker_metadata = parse_action_metadata(
            "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    BOOL: false\n    COUNT: 7\n    EMPTY: null\n    true: value\n",
        )
        .unwrap();
        assert_eq!(docker_metadata.runs.env["BOOL"], "false");
        assert_eq!(docker_metadata.runs.env["COUNT"], "7");
        assert_eq!(docker_metadata.runs.env["EMPTY"], "");
        assert_eq!(docker_metadata.runs.env["true"], "value");

        let composite_metadata = parse_action_metadata(
            "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      with:\n        BOOL: false\n        COUNT: 7\n        EMPTY: null\n        true: value\n    - shell: bash\n      run: echo ok\n      env:\n        BOOL: false\n        COUNT: 7\n        EMPTY: null\n",
        )
        .unwrap();
        assert_eq!(composite_metadata.runs.steps[0].with["BOOL"], "false");
        assert_eq!(composite_metadata.runs.steps[0].with["COUNT"], "7");
        assert_eq!(composite_metadata.runs.steps[0].with["EMPTY"], "");
        assert_eq!(composite_metadata.runs.steps[0].with["true"], "value");
        assert_eq!(composite_metadata.runs.steps[1].env["BOOL"], "false");
        assert_eq!(composite_metadata.runs.steps[1].env["COUNT"], "7");
        assert_eq!(composite_metadata.runs.steps[1].env["EMPTY"], "");

        for (name, metadata) in [
            (
                "input-default-sequence",
                "inputs:\n  value:\n    default: []\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-default-mapping",
                "inputs:\n  value:\n    default: {nested: value}\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-default-duplicate",
                "inputs:\n  value:\n    default: first\n    DEFAULT: second\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-name-duplicate",
                "inputs:\n  Foo:\n    default: first\n  foo:\n    default: second\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-null-name",
                "inputs:\n  null:\n    default: value\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "docker-env-null-map",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env: null\n",
            ),
            (
                "docker-env-sequence-value",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    VALUE: []\n",
            ),
            (
                "docker-env-mapping-value",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    VALUE: {nested: value}\n",
            ),
            (
                "docker-env-duplicate-key",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    Foo: first\n    foo: second\n",
            ),
            (
                "docker-env-empty-key",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    null: value\n",
            ),
            (
                "composite-with-null-map",
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      with: null\n",
            ),
            (
                "composite-with-mapping-value",
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      with:\n        VALUE: {nested: value}\n",
            ),
            (
                "composite-with-sequence-value",
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      with:\n        VALUE: []\n",
            ),
            (
                "composite-with-duplicate-key",
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      with:\n        Foo: first\n        foo: second\n",
            ),
            (
                "composite-env-sequence-value",
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n      env:\n        VALUE: []\n",
            ),
            (
                "composite-env-mapping-value",
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n      env:\n        VALUE: {nested: value}\n",
            ),
            (
                "composite-env-null-map",
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n      env: null\n",
            ),
        ] {
            assert!(
                parse_action_metadata(metadata).is_err(),
                "Runner-incompatible action metadata passed the runtime parser: {name}"
            );
        }

        for (value, expected) in [
            (1.0, "1"),
            (1e14, "100000000000000"),
            (1e15, "1E+15"),
            (1e-4, "0.0001"),
            (1e-5, "1E-05"),
            (1.234_567_890_123_456_7, "1.23456789012346"),
        ] {
            assert_eq!(runner_number_to_string(value), expected);
        }
    }

    #[test]
    fn action_metadata_preserves_runner_numeric_lexemes_and_null_keys() {
        let metadata = parse_action_metadata(
            "inputs:\n  1e15:\n    default: 1e15\n  radix:\n    default: 0xFFFFFFFF\n  negative-zero:\n    default: -0\n  huge-float:\n    default: 1e999\n  negative-huge-float:\n    default: -1e999\nruns:\n  using: composite\n  steps: []\n",
        )
        .unwrap();
        assert_eq!(
            metadata.inputs["1E+15"].default_value.as_deref(),
            Some("1E+15")
        );
        assert_eq!(
            metadata.inputs["radix"].default_value.as_deref(),
            Some("-1")
        );
        assert_eq!(
            metadata.inputs["negative-zero"].default_value.as_deref(),
            Some("-0")
        );
        assert_eq!(
            metadata.inputs["huge-float"].default_value.as_deref(),
            Some("Infinity")
        );
        assert_eq!(
            metadata.inputs["negative-huge-float"]
                .default_value
                .as_deref(),
            Some("-Infinity")
        );
        let huge_integer = "9".repeat(400);
        let huge_integer_metadata = parse_action_metadata(&format!(
            "inputs:\n  large:\n    default: {huge_integer}\nruns:\n  using: composite\n  steps: []\n"
        ))
        .unwrap();
        assert_eq!(
            huge_integer_metadata.inputs["large"]
                .default_value
                .as_deref(),
            Some("Infinity")
        );

        for source in [
            "inputs:\n  null:\n    default: invalid\nruns:\n  using: composite\n  steps: []\n",
            "inputs:\n  value:\n    default: 0x100000000\nruns:\n  using: composite\n  steps: []\n",
            "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    0o40000000000: invalid\n",
        ] {
            assert!(parse_action_metadata(source).is_err(), "accepted {source}");
        }
    }

    #[test]
    fn action_metadata_enforces_closed_runner_variants() {
        for source in [
            "runs:\n  using: node20\n  main: dist/index.js\n  image: Dockerfile\n",
            "runs:\n  using: node20\n  pre-if: success()\n",
            "runs:\n  using: composite\n",
            "runs:\n  using: docker\n  image: Dockerfile\n  preIf: success()\n",
            "outputs:\n  result:\n    description: output\n    unknown: value\nruns:\n  using: composite\n  steps: []\n",
            "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n      with:\n        key: value\n",
            "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      shell: bash\n",
            "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      working-directory: .\n",
            "runs:\n  using: composite\n  steps:\n    - run: echo missing-shell\n",
            "runs:\n  using: composite\n  steps:\n    - run: echo both\n      shell: bash\n      uses: actions/example@0123456789abcdef0123456789abcdef01234567\n",
            "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      continueOnError: true\n",
        ] {
            assert!(parse_action_metadata(source).is_err(), "accepted {source}");
        }

        let plugin = parse_action_metadata("runs:\n  plugin: marketplace\n").unwrap();
        assert_eq!(plugin.runs.plugin.as_deref(), Some("marketplace"));
        assert!(plugin
            .runtime()
            .unwrap_err()
            .to_string()
            .contains("plugin action runtime 'marketplace' is not supported"));
        assert!(parse_action_metadata("runs:\n  using: composite\n  steps: []\n").is_ok());
    }

    #[test]
    fn action_metadata_ignores_unknown_values_but_rejects_empty_schema_keys() {
        let metadata = parse_action_metadata(
            "unknown-root:\n  nested:\n    - false\n    - {count: 7}\nunknown-scalar: false\ninputs:\n  value:\n    custom: {nested: [false, 7]}\nruns:\n  using: composite\n  steps: []\n",
        )
        .unwrap();
        assert!(metadata.inputs.contains_key("value"));

        for source in [
            "\"\": {nested: value}\nruns:\n  using: composite\n  steps: []\n",
            "inputs:\n  value:\n    \"\": {nested: value}\nruns:\n  using: composite\n  steps: []\n",
        ] {
            assert!(parse_action_metadata(source).is_err(), "accepted {source}");
        }
    }

    #[test]
    fn action_metadata_rejects_collection_mapping_keys() {
        for source in [
            "? [complex, key]\n: ignored\nruns:\n  using: composite\n  steps: []\n",
            "? {complex: key}\n: ignored\nruns:\n  using: composite\n  steps: []\n",
            "{? [complex, key] : ignored, runs: {using: composite, steps: []}}\n",
            "{? {complex: key} : ignored, runs: {using: composite, steps: []}}\n",
        ] {
            assert!(parse_action_metadata(source).is_err(), "accepted {source}");
        }
    }

    #[test]
    fn composite_continue_on_error_accepts_only_boolean_or_expression() {
        let metadata = parse_action_metadata(
            "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo native\n      continue-on-error: false\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      continue-on-error: ${{ inputs.soft-fail }}\n    - shell: bash\n      run: echo tagged\n      continue-on-error: !!bool false\n    - shell: bash\n      run: echo verbatim-tagged\n      continue-on-error: !<tag:yaml.org,2002:bool> true\n",
        )
        .unwrap();
        assert_eq!(
            metadata.runs.steps[0].continue_on_error.as_deref(),
            Some("false")
        );
        assert_eq!(
            metadata.runs.steps[1].continue_on_error.as_deref(),
            Some("${{ inputs.soft-fail }}")
        );
        assert_eq!(
            metadata.runs.steps[2].continue_on_error.as_deref(),
            Some("false")
        );
        assert_eq!(
            metadata.runs.steps[3].continue_on_error.as_deref(),
            Some("true")
        );

        for value in ["'true'", "1", "null", "[]", "{value: true}"] {
            let source = format!(
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo invalid\n      continue-on-error: {value}\n"
            );
            assert!(parse_action_metadata(&source).is_err(), "accepted {source}");
        }

        for value in ["!!bool 'false'", "!!string false"] {
            let source = format!(
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo invalid\n      continue-on-error: {value}\n"
            );
            assert!(parse_action_metadata(&source).is_err(), "accepted {source}");
        }
    }

    #[test]
    fn action_metadata_accepts_only_runner_core_scalar_tags() {
        let metadata = parse_action_metadata(
            "name: !!str false\ninputs:\n  string:\n    default: !<tag:yaml.org,2002:str> true\n  hex:\n    default: !!int 0xFFFFFFFF\n  literal-null:\n    default: !!str null\n  literal-hex:\n    default: !!str 0xFFFFFFFF\n  empty-tag:\n    default: !!str ''\n  !!str null:\n    default: kept\n  empty-null:\n    default: !!null\nruns:\n  using: composite\n  steps: []\n",
        )
        .unwrap();
        assert_eq!(metadata.name.as_deref(), Some("false"));
        assert_eq!(
            metadata.inputs["string"].default_value.as_deref(),
            Some("true")
        );
        assert_eq!(metadata.inputs["hex"].default_value.as_deref(), Some("-1"));
        assert_eq!(
            metadata.inputs["literal-null"].default_value.as_deref(),
            Some("null")
        );
        assert_eq!(
            metadata.inputs["literal-hex"].default_value.as_deref(),
            Some("0xFFFFFFFF")
        );
        assert_eq!(
            metadata.inputs["empty-tag"].default_value.as_deref(),
            Some("")
        );
        assert_eq!(
            metadata.inputs["empty-null"].default_value.as_deref(),
            Some("")
        );
        assert_eq!(
            metadata.inputs["null"].default_value.as_deref(),
            Some("kept")
        );
        let collection_tags =
            parse_action_metadata("runs: !!map {using: composite, steps: !!seq []}\n").unwrap();
        assert!(matches!(
            collection_tags.runtime().unwrap(),
            ActionRuntime::Composite
        ));
        let directive_tagged = parse_action_metadata(
            "%TAG !core! tag:yaml.org,2002:\n---\nname: !core!str false\nruns:\n  using: composite\n  steps: []\n",
        )
        .unwrap();
        assert_eq!(directive_tagged.name.as_deref(), Some("false"));
        let percent_encoded_directive_tagged = parse_action_metadata(
            "%TAG !core! tag:yaml.org,2002:\n---\nname: !core!%73tr false\nruns:\n  using: composite\n  steps: []\n",
        )
        .unwrap();
        assert_eq!(
            percent_encoded_directive_tagged.name.as_deref(),
            Some("false")
        );
        let percent_encoded_directive_prefix = parse_action_metadata(
            "%TAG !! tag:yaml.org,2002%3A\n---\nname: !!str false\nruns:\n  using: composite\n  steps: []\n",
        )
        .unwrap();
        assert_eq!(
            percent_encoded_directive_prefix.name.as_deref(),
            Some("false")
        );
        let directive_text_in_block_scalar = parse_action_metadata(
            "name: !!str false\ndescription: |\n  %TAG !! tag:custom.example,2026:\nruns:\n  using: composite\n  steps: []\n",
        )
        .unwrap();
        assert_eq!(
            directive_text_in_block_scalar.name.as_deref(),
            Some("false")
        );
        for source in [
            "name: !!string value\nruns:\n  using: composite\n  steps: []\n",
            "name: !tag:yaml.org,2002:str value\nruns:\n  using: composite\n  steps: []\n",
            "%TAG !core! tag:yaml.org,2002:\n---\nname: !missing!str false\nruns:\n  using: composite\n  steps: []\n",
            "%TAG !! tag:custom.example,2026:\n---\nname: !!str false\nruns:\n  using: composite\n  steps: []\n",
            "%TAG !! !custom-\n---\nname: !!str false\nruns:\n  using: composite\n  steps: []\n",
            "%TAG ! !custom-\n---\nname: !str false\nruns:\n  using: composite\n  steps: []\n",
            "%TAG !core! tag:yaml.org,2002:\n%TAG !core! tag:yaml.org,2002:\n---\nname: !core!str false\nruns:\n  using: composite\n  steps: []\n",
            "inputs:\n  value:\n    default: !!bool 'false'\nruns:\n  using: composite\n  steps: []\n",
            "inputs:\n  value:\n    default: !!int 0x100000000\nruns:\n  using: composite\n  steps: []\n",
            "name: &action-name value\nruns:\n  using: docker\n  image: *action-name\n",
        ] {
            assert!(parse_action_metadata(source).is_err(), "accepted {source}");
        }
    }

    #[test]
    fn missing_input_defaults_are_materialized_as_empty_strings() {
        let metadata = parse_action_metadata(
            "inputs:\n  absent:\n    description: no default\n  present:\n    default: false\nruns:\n  using: composite\n  steps: []\n",
        )
        .unwrap();
        let inputs = effective_inputs(&metadata, &BTreeMap::new()).unwrap();
        assert_eq!(inputs.get("absent").map(String::as_str), Some(""));
        assert_eq!(inputs.get("present").map(String::as_str), Some("false"));
    }

    #[test]
    fn implicit_empty_scalars_keep_following_lexemes_aligned() {
        let metadata = parse_action_metadata(
            "inputs:\n  empty:\n    default:\nruns:\n  using: docker\n  image: docker://ubuntu\n  args:\n    -\n    - 7\n  env:\n    EMPTY:\n    COUNT: 8\n",
        )
        .unwrap();
        assert_eq!(metadata.inputs["empty"].default_value.as_deref(), Some(""));
        assert_eq!(metadata.runs.args, ["", "7"]);
        assert_eq!(metadata.runs.env.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(
            metadata.runs.env.get("COUNT").map(String::as_str),
            Some("8")
        );

        for source in [
            "inputs:\n  : invalid\nruns:\n  using: composite\n  steps: []\n",
            "runs:\n  using: composite\n  steps:\n    -\n",
        ] {
            assert!(parse_action_metadata(source).is_err(), "accepted {source}");
        }
    }

    #[test]
    fn case_insensitive_duplicates_are_rejected_in_nested_loose_metadata() {
        for source in [
            "extensions:\n  Foo: one\n  foo: two\nruns:\n  using: composite\n  steps: []\n",
            "inputs:\n  value:\n    custom:\n      Foo: one\n      foo: two\nruns:\n  using: composite\n  steps: []\n",
            "extensions: {Foo: one, Foo: two}\nruns:\n  using: composite\n  steps: []\n",
        ] {
            assert!(parse_action_metadata(source).is_err(), "accepted {source}");
        }
        parse_action_metadata(
            "inputs:\n  value:\n    custom:\n      \"\": allowed\nruns:\n  using: composite\n  steps: []\n",
        )
        .unwrap();
    }

    #[test]
    fn action_metadata_string_schema_fields_follow_runner_coercion() {
        let loose_input_metadata = parse_action_metadata(
            "inputs:\n  value:\n    description: {ignored: metadata}\n    required: [ignored]\n    default: false\nruns:\n  using: composite\n  steps: []\n",
        )
        .unwrap();
        assert_eq!(
            loose_input_metadata.inputs["value"].description, None,
            "Runner ignores loose input description values"
        );
        assert!(
            !loose_input_metadata.inputs["value"].required,
            "Runner ignores loose input required values"
        );
        assert_eq!(
            loose_input_metadata.inputs["value"]
                .default_value
                .as_deref(),
            Some("false")
        );

        let root_and_node_metadata = parse_action_metadata(
            "name: false\ndescription: null\noutputs:\n  7:\n    description: false\n    value: null\nruns:\n  using: true\n  main: 7\n  pre: false\n  pre-if: 8\n  post: 9\n  post-if: true\n",
        )
        .unwrap();
        assert_eq!(root_and_node_metadata.name.as_deref(), Some("false"));
        assert_eq!(root_and_node_metadata.description.as_deref(), Some(""));
        assert_eq!(root_and_node_metadata.runs.using, "true");
        assert_eq!(root_and_node_metadata.runs.main.as_deref(), Some("7"));
        assert_eq!(root_and_node_metadata.runs.pre.as_deref(), Some("false"));
        assert_eq!(root_and_node_metadata.runs.pre_if.as_deref(), Some("8"));
        assert_eq!(root_and_node_metadata.runs.post.as_deref(), Some("9"));
        assert_eq!(root_and_node_metadata.runs.post_if.as_deref(), Some("true"));
        assert_eq!(
            root_and_node_metadata.outputs["7"].description.as_deref(),
            Some("false")
        );
        assert_eq!(
            root_and_node_metadata.outputs["7"].value.as_deref(),
            Some("")
        );

        let docker_metadata = parse_action_metadata(
            "runs:\n  using: docker\n  image: 7\n  entrypoint: false\n  pre-entrypoint: 8\n  post-entrypoint: 9\n  args: [false, 7, null]\n",
        )
        .unwrap();
        assert_eq!(docker_metadata.runs.image.as_deref(), Some("7"));
        assert_eq!(docker_metadata.runs.entrypoint.as_deref(), Some("false"));
        assert_eq!(docker_metadata.runs.pre_entrypoint.as_deref(), Some("8"));
        assert_eq!(docker_metadata.runs.post_entrypoint.as_deref(), Some("9"));
        assert_eq!(docker_metadata.runs.args, ["false", "7", ""]);

        let composite_metadata = parse_action_metadata(
            "runs:\n  using: composite\n  steps:\n    - id: 7\n      name: null\n      uses: true\n    - id: 8\n      shell: false\n      run: 9\n      if: true\n      working-directory: null\n",
        )
        .unwrap();
        assert_eq!(composite_metadata.runs.steps[0].id.as_deref(), Some("7"));
        assert_eq!(composite_metadata.runs.steps[0].name.as_deref(), Some(""));
        assert_eq!(
            composite_metadata.runs.steps[0].uses.as_deref(),
            Some("true")
        );
        assert_eq!(composite_metadata.runs.steps[1].id.as_deref(), Some("8"));
        assert_eq!(
            composite_metadata.runs.steps[1].shell.as_deref(),
            Some("false")
        );
        assert_eq!(composite_metadata.runs.steps[1].run.as_deref(), Some("9"));
        assert_eq!(
            composite_metadata.runs.steps[1].condition.as_deref(),
            Some("true")
        );
        assert_eq!(
            composite_metadata.runs.steps[1]
                .working_directory
                .as_deref(),
            Some("")
        );

        for (field, value) in [
            ("root name sequence", "name: []\n"),
            ("root description mapping", "description: {nested: value}\n"),
            ("runs using sequence", "using: []\n"),
            ("node main mapping", "main: {nested: value}\n"),
            ("node pre sequence", "pre: []\n"),
            ("node pre-if mapping", "pre-if: {nested: value}\n"),
            ("node post sequence", "post: []\n"),
            ("node post-if mapping", "post-if: {nested: value}\n"),
            ("docker image sequence", "image: []\n"),
            ("docker entrypoint mapping", "entrypoint: {nested: value}\n"),
            ("docker pre-entrypoint sequence", "pre-entrypoint: []\n"),
            ("docker post-entrypoint mapping", "post-entrypoint: {nested: value}\n"),
            ("docker args sequence item", "args: [[]]\n"),
            ("output description mapping", "outputs:\n  out:\n    description: {nested: value}\n"),
            ("output value sequence", "outputs:\n  out:\n    value: []\n"),
            (
                "composite step id mapping",
                "steps:\n  - id: {nested: value}\n    shell: bash\n    run: echo ok\n",
            ),
            (
                "composite step name sequence",
                "steps:\n  - name: []\n    shell: bash\n    run: echo ok\n",
            ),
            (
                "composite step shell mapping",
                "steps:\n  - shell: {nested: value}\n    run: echo ok\n",
            ),
            (
                "composite step run sequence",
                "steps:\n  - shell: bash\n    run: []\n",
            ),
            (
                "composite step uses mapping",
                "steps:\n  - uses: {nested: value}\n",
            ),
            (
                "composite step if sequence",
                "steps:\n  - shell: bash\n    run: echo ok\n    if: []\n",
            ),
            (
                "composite step working-directory mapping",
                "steps:\n  - shell: bash\n    run: echo ok\n    working-directory: {nested: value}\n",
            ),
            (
                "deprecationMessage boolean",
                "inputs:\n  value:\n    deprecationMessage: false\n",
            ),
            (
                "deprecationMessage number",
                "inputs:\n  value:\n    deprecationMessage: 7\n",
            ),
            (
                "deprecationMessage null",
                "inputs:\n  value:\n    deprecationMessage: null\n",
            ),
        ] {
            let metadata = if field.starts_with("docker ") {
                format!("runs:\n  using: docker\n  image: docker://ubuntu\n  {value}")
            } else if field.starts_with("runs ") {
                format!("runs:\n  {value}")
            } else if field.starts_with("node ") {
                format!("runs:\n  using: node20\n  {value}")
            } else if field.starts_with("output ") {
                format!("{value}runs:\n  using: composite\n  steps: []\n")
            } else if field.starts_with("composite ") {
                format!("runs:\n  using: composite\n  {value}")
            } else {
                format!("{value}runs:\n  using: composite\n  steps: []\n")
            };
            assert!(
                parse_action_metadata(&metadata).is_err(),
                "Runner-incompatible string metadata passed the runtime parser: {field}"
            );
        }
    }

    #[test]
    fn composite_inputs_are_canonicalized_for_case_insensitive_rendering() {
        let metadata = parse_action_metadata(
            r#"
inputs:
  lookup-only:
    default: 'false'
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
    fn remote_repository_action_with_dot_subpath_not_treated_as_local() {
        assert_eq!(
            local_action_path(
                Some("tailrocks/velnor"),
                Some(".github/actions/report-velnor-ci-outcomes")
            ),
            None
        );
        assert!(!is_local_action_reference(
            Some("tailrocks/velnor"),
            Some(".github/actions/report-velnor-ci-outcomes")
        ));
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "report",
                "reference": {
                    "type": "Repository",
                    "name": "tailrocks/velnor",
                    "ref": "8b8f1cbe03427227e9d04301de530b3e744110f4",
                    "path": ".github/actions/report-velnor-ci-outcomes"
                }
            }
        ]))
        .unwrap();
        let local_plans = local_action_plans(&steps, Path::new("/tmp/workspace")).unwrap();
        assert!(local_plans.is_empty());

        let repo_plans = repository_action_plans(&steps, Path::new("/tmp/actions")).unwrap();
        assert_eq!(repo_plans.len(), 1);
        assert_eq!(repo_plans[0].repository, "tailrocks/velnor");
        assert_eq!(
            repo_plans[0].source_path.as_deref(),
            Some(".github/actions/report-velnor-ci-outcomes")
        );
    }

    #[test]
    fn local_action_classification_requires_runner_path_prefix() {
        for value in ["./action", ".\\action"] {
            assert!(
                local_action_path(Some(value), None).is_some(),
                "accepted local action prefix {value}"
            );
            assert!(is_runner_local_action_reference(value));
        }
        for value in [".hidden/action", ".../action", "..", ".", "hidden/action"] {
            assert_eq!(
                local_action_path(Some(value), None),
                None,
                "misclassified local action reference {value}"
            );
            assert!(!is_runner_local_action_reference(value));
        }
        assert_eq!(
            local_action_path(Some("tailrocks/velnor"), Some(".hidden/action")),
            None
        );
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
            workspace_root: Path::new("/tmp/workspace").into(),
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

    struct ExpressionParseEnv;

    impl crate::expression::parser::ParseEnvironment for ExpressionParseEnv {
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
            crate::expression::lexer::is_legal_keyword(id),
            "`{id}` must lex as an expression identifier"
        );
        assert!(
            matches!(
                crate::expression::parse(
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
            workspace_root: Path::new("/tmp/workspace").into(),
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
            workspace_root: Path::new("/tmp/workspace").into(),
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
            workspace_root: Path::new("/tmp/workspace").into(),
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
            workspace_root: Path::new("/tmp/workspace").into(),
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
      uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
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
            workspace_root: Path::new("/tmp/workspace").into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/docs"),
            inputs: [("github-token".to_string(), "ghs_token".to_string())].into(),
        };
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - uses: jdx/mise-action/sub/action@c2a87611a18de5b3828c5652fe268e992400cb5c
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
        assert_eq!(plans[0].git_ref, "c2a87611a18de5b3828c5652fe268e992400cb5c");
        assert_eq!(plans[0].source_path.as_deref(), Some("sub/action"));
        assert_eq!(plans[0].inputs["github_token"], "ghs_token");
        assert_eq!(
            plans[0].condition.as_deref(),
            Some("${{ 'ghs_token' != '' }}")
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
runs:
  using: composite
  steps:
    - id: prove
      shell: bash
      run: echo "closure=nested" >> "$GITHUB_OUTPUT"
outputs:
  closure:
    value: ${{ steps.prove.outputs.closure }}
"#,
        )
        .unwrap();
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - id: nested
      uses: ./.github/actions/nested
outputs:
  closure:
    value: ${{ steps.nested.outputs.closure }}
"#,
        )
        .unwrap();
        let plan = LocalActionPlan {
            step_id: "root".into(),
            workspace_root: workspace.clone(),
            action_dir: root,
            inputs: BTreeMap::new(),
        };

        let invocations =
            composite_action_invocations(&plan, &metadata, "/__w", Path::new("/__a")).unwrap();

        assert!(matches!(
            invocations[0],
            CompositeActionInvocation::Script(_)
        ));
        assert!(matches!(
            &invocations[1],
            CompositeActionInvocation::Outputs(outputs) if outputs.step_id == "root-nested"
        ));
        assert!(matches!(
            &invocations[2],
            CompositeActionInvocation::Outputs(outputs) if outputs.step_id == "root"
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
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
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
            timeout_minutes: None,
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
            timeout_minutes: None,
        };
        let metadata = parse_action_metadata(
            r#"
inputs:
  soft-fail:
    default: 'false'
runs:
  using: composite
  steps:
    - shell: bash
      continue-on-error: ${{ inputs.soft-fail }}
      run: cargo install acme-cli
    - uses: actions/cache@6849a6489940f00c2f30c0fb92c6274307ccb58a
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
            timeout_minutes: None,
        };
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
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
        assert_eq!(plans[0].git_ref, "043fb46d1a93c77aae656e7c1c64a875d1fc6a0a");
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
                timeout_minutes: None,
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
                timeout_minutes: None,
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
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
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
        let actions_host = temp.join("actions-host");
        let action_root = repository_dir(&actions_host, "actions/setup-node", "v4");
        let action_dir = action_root.clone();
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
            repository_dir: action_root,
            action_dir,
            inputs: BTreeMap::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
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

    #[cfg(unix)]
    #[test]
    fn remote_action_subpath_cannot_escape_checkout_through_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = std::env::temp_dir().join(format!(
            "velnor-remote-action-symlink-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        let actions_host = temp.join("actions-host");
        let repository_dir = repository_dir(&actions_host, "owner/example", "0123456789abcdef");
        let outside = temp.join("outside");
        fs::create_dir_all(repository_dir.join("nested")).unwrap();
        fs::create_dir_all(outside.join("nested")).unwrap();
        fs::write(
            outside.join("nested/action.yml"),
            "runs:\n  using: node20\n  main: index.js\n",
        )
        .unwrap();
        symlink(&outside, repository_dir.join("escape")).unwrap();
        let action_dir = repository_dir.join("escape/nested");
        let plan = RepositoryActionPlan {
            step_id: "escape".into(),
            repository: "owner/example".into(),
            git_ref: "0123456789abcdef".into(),
            source_path: Some("escape/nested".into()),
            repository_dir: repository_dir.clone(),
            action_dir,
            inputs: BTreeMap::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };

        let error = resolve_action(&plan).unwrap_err();

        assert!(error.to_string().contains("symlink"));
        assert!(super::action_dir(
            &actions_host,
            "owner/example",
            "0123456789abcdef",
            Some("../outside")
        )
        .is_err());
        fs::remove_dir_all(temp).ok();
    }

    #[cfg(unix)]
    #[test]
    fn local_action_root_cannot_escape_workspace_through_ancestor_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = std::env::temp_dir().join(format!(
            "velnor-local-action-symlink-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        let workspace = temp.join("workspace");
        let outside = temp.join("outside");
        fs::create_dir_all(workspace.join(".github/actions")).unwrap();
        fs::create_dir_all(outside.join("action")).unwrap();
        fs::write(
            outside.join("action/action.yml"),
            "runs:\n  using: node20\n  main: index.js\n",
        )
        .unwrap();
        symlink(&outside, workspace.join(".github/actions/escape")).unwrap();
        let plan = LocalActionPlan {
            step_id: "escape".into(),
            workspace_root: workspace.clone(),
            action_dir: workspace.join(".github/actions/escape/action"),
            inputs: BTreeMap::new(),
        };

        let error = resolve_local_action(&plan).unwrap_err();

        assert!(error.to_string().contains("symlink"));
        fs::remove_dir_all(temp).ok();
    }

    #[cfg(unix)]
    #[test]
    fn remote_action_checkout_roots_reject_symlinked_cache_and_repository_dirs() {
        use std::os::unix::fs::symlink;

        let temp = std::env::temp_dir().join(format!(
            "velnor-action-checkout-root-symlink-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);

        let linked_cache_host = temp.join("linked-cache-host");
        let cache_target = temp.join("cache-target");
        fs::create_dir_all(cache_target.join("owner/ref")).unwrap();
        fs::create_dir_all(&linked_cache_host).unwrap();
        symlink(&cache_target, linked_cache_host.join("_actions")).unwrap();
        let linked_repository = linked_cache_host.join("_actions/owner/ref");
        assert!(validate_remote_action_root(&linked_repository, &linked_repository).is_err());

        let actions_host = temp.join("actions-host");
        let action_cache = actions_host.join("_actions");
        let repository_target = temp.join("repository-target");
        fs::create_dir_all(&action_cache).unwrap();
        fs::create_dir_all(repository_target.join("ref")).unwrap();
        symlink(&repository_target, action_cache.join("owner")).unwrap();
        let linked_repository = action_cache.join("owner/ref");
        assert!(validate_remote_action_root(&linked_repository, &linked_repository).is_err());

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
            timeout_minutes: None,
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
            timeout_minutes: None,
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
            timeout_minutes: None,
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
            timeout_minutes: None,
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
            timeout_minutes: None,
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
        assert_eq!(
            invocation.inputs.get("renovate-image").map(String::as_str),
            Some("ghcr.io/renovatebot/renovate")
        );
        assert!(invocation
            .env
            .contains(&("LOG_LEVEL".into(), "debug".into())));
    }

    #[test]
    fn dockerfile_action_uses_dockerfile_parent_context_and_preserves_runs_env() {
        let root = std::env::temp_dir().join(format!(
            "velnor-docker-action-context-{}",
            std::process::id()
        ));
        let repository_dir = root.join("repository");
        let action_dir = repository_dir.join("actions/docker");
        let _ = fs::remove_dir_all(&root);
        let dockerfile_dir = action_dir.join("docker");
        fs::create_dir_all(&dockerfile_dir).unwrap();
        fs::write(
            dockerfile_dir.join("Dockerfile"),
            "FROM alpine:3.20\nCOPY payload /payload\n",
        )
        .unwrap();
        fs::write(dockerfile_dir.join("payload"), "nested payload\n").unwrap();
        let plan = RepositoryActionPlan {
            step_id: "docker".into(),
            repository: "octo/action".into(),
            git_ref: "0123456789abcdef0123456789abcdef01234567".into(),
            source_path: Some("actions/docker".into()),
            repository_dir: repository_dir.clone(),
            action_dir: action_dir.clone(),
            inputs: BTreeMap::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let metadata = parse_action_metadata(
            "runs:\n  using: docker\n  image: docker/Dockerfile\n  env:\n    RUNS_ENV: action-value\n",
        )
        .unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: action_dir.join("action.yml"),
            runtime: metadata.runtime().unwrap(),
            metadata,
        };

        let invocation = resolved.docker_invocation(&root).unwrap();

        assert_eq!(invocation.build_context_host, Some(dockerfile_dir.clone()));
        assert_eq!(
            invocation.dockerfile_host,
            Some(dockerfile_dir.join("Dockerfile"))
        );
        assert!(invocation
            .runs_env
            .contains(&("RUNS_ENV".into(), "action-value".into())));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn downloaded_javascript_entrypoints_reject_unsafe_paths() {
        let actions_host = Path::new("/tmp/actions");
        for main in [
            "../escape.js",
            "/absolute.js",
            "C:/drive.js",
            "nested\\entry.js",
        ] {
            let plan = RepositoryActionPlan {
                step_id: "unsafe".into(),
                repository: "octo/action".into(),
                git_ref: "0123456789abcdef0123456789abcdef01234567".into(),
                source_path: None,
                repository_dir: actions_host.join("_actions/octo_action/sha"),
                action_dir: actions_host.join("_actions/octo_action/sha"),
                inputs: BTreeMap::new(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            };
            let metadata =
                parse_action_metadata(&format!("runs:\n  using: node20\n  main: '{main}'\n"))
                    .unwrap();
            let resolved = ResolvedAction {
                plan,
                metadata_path: actions_host.join("_actions/octo_action/sha/action.yml"),
                runtime: metadata.runtime().unwrap(),
                metadata,
            };
            let error = resolved
                .javascript_invocation(actions_host)
                .expect_err("unsafe downloaded JavaScript path passed runner");
            assert!(error.to_string().contains("unsafe action metadata path"));
        }

        for (field, value) in [
            ("pre", "../pre.js"),
            ("post", "/post.js"),
            ("pre", "C:/pre.js"),
            ("post", "nested\\post.js"),
        ] {
            let plan = RepositoryActionPlan {
                step_id: "unsafe-stage".into(),
                repository: "octo/action".into(),
                git_ref: "0123456789abcdef0123456789abcdef01234567".into(),
                source_path: None,
                repository_dir: actions_host.join("_actions/octo_action/sha"),
                action_dir: actions_host.join("_actions/octo_action/sha"),
                inputs: BTreeMap::new(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            };
            let metadata = parse_action_metadata(&format!(
                "runs:\n  using: node20\n  main: safe.js\n  {field}: '{value}'\n"
            ))
            .unwrap();
            let resolved = ResolvedAction {
                plan,
                metadata_path: actions_host.join("_actions/octo_action/sha/action.yml"),
                runtime: metadata.runtime().unwrap(),
                metadata,
            };
            let error = resolved
                .javascript_invocation(actions_host)
                .expect_err("unsafe downloaded JavaScript stage path passed runner");
            assert!(error.to_string().contains("unsafe action metadata path"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn downloaded_javascript_entrypoints_reject_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "velnor-runner-action-symlink-{}",
            std::process::id()
        ));
        let action_dir = root.join("repository/actions/tool");
        let outside = root.join("outside");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&action_dir).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, action_dir.join("link")).unwrap();

        let plan = RepositoryActionPlan {
            step_id: "symlink".into(),
            repository: "octo/action".into(),
            git_ref: "0123456789abcdef0123456789abcdef01234567".into(),
            source_path: None,
            repository_dir: root.join("repository"),
            action_dir: action_dir.clone(),
            inputs: BTreeMap::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let metadata =
            parse_action_metadata("runs:\n  using: node20\n  main: link/entry.js\n").unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: action_dir.join("action.yml"),
            runtime: metadata.runtime().unwrap(),
            metadata,
        };

        let error = resolved
            .javascript_invocation(root.join("repository").as_path())
            .expect_err("symlink escape passed runner");
        assert!(error.to_string().contains("unsafe action metadata path"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dockerfile_metadata_paths_reject_unsafe_paths() {
        let actions_host = Path::new("/tmp/actions");
        for image in [
            "/Dockerfile",
            "../Dockerfile",
            "C:/Dockerfile",
            "nested\\Dockerfile",
        ] {
            let plan = RepositoryActionPlan {
                step_id: "unsafe-dockerfile".into(),
                repository: "octo/action".into(),
                git_ref: "0123456789abcdef0123456789abcdef01234567".into(),
                source_path: None,
                repository_dir: actions_host.join("_actions/octo_action/sha"),
                action_dir: actions_host.join("_actions/octo_action/sha"),
                inputs: BTreeMap::new(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            };
            let metadata =
                parse_action_metadata(&format!("runs:\n  using: docker\n  image: '{image}'\n"))
                    .unwrap();
            let resolved = ResolvedAction {
                plan,
                metadata_path: actions_host.join("_actions/octo_action/sha/action.yml"),
                runtime: metadata.runtime().unwrap(),
                metadata,
            };
            let error = resolved
                .docker_invocation(actions_host)
                .expect_err("unsafe Dockerfile path passed runner");
            assert!(error.to_string().contains("unsafe action metadata path"));
        }
    }

    #[test]
    fn docker_scheme_surrounding_whitespace_is_rejected_by_runner() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "whitespace".into(),
            repository: "octo/action".into(),
            git_ref: "0123456789abcdef0123456789abcdef01234567".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/octo_action/sha"),
            action_dir: actions_host.join("_actions/octo_action/sha"),
            inputs: BTreeMap::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let metadata =
            parse_action_metadata("runs:\n  using: docker\n  image: 'docker://alpine:3.20 '\n")
                .unwrap();
        let runtime = metadata.runtime().unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host.join("_actions/octo_action/sha/action.yml"),
            metadata,
            runtime,
        };
        let error = resolved
            .docker_invocation(actions_host)
            .expect_err("surrounding image whitespace passed runner");
        assert!(error.to_string().contains("invalid Docker image"));
    }

    #[test]
    fn planner_uses_strict_shared_repository_reference_parser() {
        let parsed = parse_repository_uses(
            "octo/example/sub/action@0123456789abcdef0123456789abcdef01234567",
        )
        .unwrap();
        assert_eq!(parsed.repository, "octo/example");
        assert_eq!(parsed.source_path.as_deref(), Some("sub/action"));
        assert!(parse_repository_uses("octo/example@v1").is_err());
        assert!(parse_repository_uses(
            "octo/example/../action@0123456789abcdef0123456789abcdef01234567"
        )
        .is_err());
    }

    #[test]
    fn docker_action_image_scheme_is_case_insensitive() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "renovate".into(),
            repository: "renovatebot/github-action".into(),
            git_ref: "v46.1.14".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/renovatebot_github-action/v46.1.14"),
            action_dir: actions_host.join("_actions/renovatebot_github-action/v46.1.14"),
            inputs: Default::default(),
            env: Default::default(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let metadata =
            parse_action_metadata("runs:\n  using: docker\n  image: DOCKER://alpine:3.20\n")
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
    }

    #[test]
    fn docker_action_uppercase_scheme_flag_shaped_image_is_refused() {
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "evil".into(),
            repository: "attacker/action".into(),
            git_ref: "v1".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/attacker_action/v1"),
            action_dir: actions_host.join("_actions/attacker_action/v1"),
            inputs: Default::default(),
            env: Default::default(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let metadata =
            parse_action_metadata("runs:\n  using: docker\n  image: DOCKER://--privileged\n")
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
                env: Default::default(),
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
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
                            env: Vec::new(),
                            condition: None,
                            continue_on_error: false,
                            timeout_minutes: None,
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
                        workspace_root: Path::new("/tmp/velnor-targets/jackin").into(),
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
                    if is_runner_local_action_reference(&uses) || uses.starts_with("docker://") {
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

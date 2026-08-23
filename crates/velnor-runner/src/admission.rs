//! Transitively-closed action admission.
//!
//! Plan 009 replaces the previous split boundary — a flat top-level capability
//! check plus a local-only recursive preflight, with a redundant post-download
//! re-resolution sharing one bypass switch — with a single typed, read-only
//! admission graph that is completed **before any step side effect**.
//!
//! [`admit_job`] resolves and validates *every* root action (local and remote),
//! recurses through nested local and remote composites, resolves defaults and
//! `${{ inputs.* }}` before validating a child, bounds depth/nodes and guards
//! against cycles, and distinguishes a server-expanded reusable workflow from a
//! runner-side action closure. Metadata reads go through the injectable
//! [`ActionMetadataSource`] (the Contents API in production, a fake in tests) so
//! admission never performs a mutating side effect. Every rejection carries the
//! complete [`Ancestry`] and never the received value.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use serde_json::Value;

use crate::action::{native_action_adapter, ActionMetadata, NATIVE_ACTION_REF};
use crate::job_message::{ActionReferenceType, AgentJobRequestMessage};
use crate::manifest::{self, CapabilityViolation};

/// Maximum composite nesting depth. Matches the removed local preflight bound.
const MAX_COMPOSITE_DEPTH: usize = 10;
/// Hard ceiling on admitted nodes so a pathological or adversarial closure
/// cannot exhaust resources before the depth guard trips.
const MAX_ADMISSION_NODES: usize = 512;

/// A fully-resolved action identity: repository, immutable full-SHA ref, and
/// optional subpath. Local actions carry the workflow repository and workflow
/// SHA as their `repository`/`sha`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionIdentity {
    pub repository: String,
    pub sha: String,
    pub subpath: Option<String>,
}

impl ActionIdentity {
    fn display(&self) -> String {
        match &self.subpath {
            Some(subpath) if !subpath.is_empty() => {
                format!("{}/{}@{}", self.repository, subpath, self.sha)
            }
            _ => format!("{}@{}", self.repository, self.sha),
        }
    }
}

/// The kind of node in the admission graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionNodeKind {
    /// A remote action served by a native Rust adapter (no metadata fetch).
    NativeAction,
    /// A remote composite action whose metadata was fetched and recursed.
    RemoteComposite,
    /// A remote non-composite (JavaScript/Docker) action — rejected on the
    /// production path unless it maps to a native adapter, but represented here
    /// for completeness while recursing.
    RemoteAction,
    /// A local action or composite read from the workflow repository.
    LocalAction,
    /// A server-expanded reusable workflow (`jobs.<id>.uses`).
    ReusableWorkflow,
}

#[derive(Debug, Clone)]
pub struct AdmissionNode {
    pub identity: ActionIdentity,
    pub kind: AdmissionNodeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionEdge {
    pub from: usize,
    pub to: usize,
}

/// The completed admission closure. Planning consumes this graph and never
/// re-resolves an identity.
#[derive(Debug, Clone, Default)]
pub struct AdmissionGraph {
    pub nodes: Vec<AdmissionNode>,
    pub edges: Vec<AdmissionEdge>,
}

impl AdmissionGraph {
    /// Whether a remote action identity was admitted. Planning uses this to
    /// confirm a downloaded action was part of the admitted closure instead of
    /// re-running capability validation.
    pub fn contains_remote_action(
        &self,
        repository: &str,
        sha: &str,
        subpath: Option<&str>,
    ) -> bool {
        let subpath = normalize_subpath(subpath);
        self.nodes.iter().any(|node| {
            !matches!(node.kind, AdmissionNodeKind::LocalAction)
                && node.identity.repository.eq_ignore_ascii_case(repository)
                && node.identity.sha == sha
                && normalize_subpath(node.identity.subpath.as_deref()) == subpath
        })
    }

    fn intern(&mut self, identity: ActionIdentity, kind: AdmissionNodeKind) -> usize {
        if let Some(index) = self.nodes.iter().position(|node| node.identity == identity) {
            return index;
        }
        self.nodes.push(AdmissionNode { identity, kind });
        self.nodes.len() - 1
    }

    fn link(&mut self, from: Option<usize>, to: usize) {
        if let Some(from) = from {
            let edge = AdmissionEdge { from, to };
            if !self.edges.contains(&edge) {
                self.edges.push(edge);
            }
        }
    }
}

fn normalize_subpath(subpath: Option<&str>) -> String {
    subpath
        .map(|value| value.trim().trim_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

/// The complete lineage from a job root down to the offending node. Rendered
/// into diagnostics; it contains action identities only — never a received
/// input value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ancestry(pub Vec<String>);

impl Ancestry {
    fn child(&self, hop: String) -> Ancestry {
        let mut hops = self.0.clone();
        hops.push(hop);
        Ancestry(hops)
    }
}

impl fmt::Display for Ancestry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            write!(formatter, "<job root>")
        } else {
            write!(formatter, "{}", self.0.join(" -> "))
        }
    }
}

/// A redacted admission rejection carrying the complete ancestry. It never
/// stores the received value — only the field, accepted alternatives, a static
/// reason, and the manifest version.
#[derive(Debug, Clone)]
pub struct AdmissionError {
    pub ancestry: Ancestry,
    pub field: String,
    pub accepted: Vec<String>,
    pub reason: String,
    pub manifest_version: u32,
}

impl AdmissionError {
    fn new(
        ancestry: &Ancestry,
        field: impl Into<String>,
        reason: impl Into<String>,
        accepted: Vec<String>,
    ) -> Self {
        Self {
            ancestry: ancestry.clone(),
            field: field.into(),
            accepted,
            reason: reason.into(),
            manifest_version: manifest::MANIFEST_VERSION,
        }
    }

    /// Convert a manifest capability error into a redacted admission error,
    /// preserving the ancestry and dropping the received value.
    fn from_capability(ancestry: &Ancestry, error: anyhow::Error) -> Self {
        if let Some(violation) = error.downcast_ref::<CapabilityViolation>() {
            Self {
                ancestry: ancestry.clone(),
                field: violation.field.clone(),
                accepted: violation.accepted.clone(),
                reason: "unsupported capability".to_string(),
                manifest_version: violation.manifest_version,
            }
        } else {
            // Structural errors (metadata fetch/parse) carry no job input value.
            Self::new(ancestry, "uses", error.to_string(), Vec::new())
        }
    }
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "action admission rejected at {}: field '{}' ({}); accepted: {}; manifest version {}",
            self.ancestry,
            self.field,
            self.reason,
            if self.accepted.is_empty() {
                "none".to_string()
            } else {
                self.accepted.join(", ")
            },
            self.manifest_version
        )
    }
}

impl std::error::Error for AdmissionError {}

/// Read-only source of action metadata. Production wraps the GitHub Contents
/// API; tests inject a fake. Implementations MUST NOT mutate runner state.
pub trait ActionMetadataSource {
    fn fetch_action_metadata(
        &self,
        repository: &str,
        git_ref: &str,
        subpath: Option<&str>,
    ) -> Result<ActionMetadata>;
}

/// Production metadata source backed by the GitHub Contents API. Uses the job
/// repository access token and the advertised API URL. It counts reads so the
/// caller can prove a rejection preceded any metadata fetch.
pub struct ContentsApiMetadataSource {
    client: reqwest::blocking::Client,
    api_url: String,
    token: String,
    reads: AtomicUsize,
}

impl ContentsApiMetadataSource {
    pub fn new(token: impl Into<String>, api_url: impl Into<String>) -> Result<Self> {
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .user_agent("velnor-runner")
                .build()?,
            api_url: api_url.into(),
            token: token.into(),
            reads: AtomicUsize::new(0),
        })
    }

    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }
}

impl ActionMetadataSource for ContentsApiMetadataSource {
    fn fetch_action_metadata(
        &self,
        repository: &str,
        git_ref: &str,
        subpath: Option<&str>,
    ) -> Result<ActionMetadata> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let base = self.api_url.trim_end_matches('/');
        let directory = normalize_subpath(subpath);
        let mut last_status = None;
        for file in ["action.yml", "action.yaml"] {
            let metadata_path = if directory.is_empty() {
                file.to_string()
            } else {
                format!("{directory}/{file}")
            };
            let response = self
                .client
                .get(format!(
                    "{base}/repos/{repository}/contents/{metadata_path}?ref={git_ref}"
                ))
                .bearer_auth(&self.token)
                .header("Accept", "application/vnd.github.raw+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .send()?;
            last_status = Some(response.status());
            if response.status().is_success() {
                return crate::action::parse_action_metadata(&response.text()?)
                    .map_err(|error| anyhow::anyhow!("parse {repository}@{git_ref}: {error:#}"));
            }
            if response.status() != reqwest::StatusCode::NOT_FOUND {
                response.error_for_status()?;
            }
        }
        anyhow::bail!(
            "action metadata not found for {repository}@{git_ref} (last status {})",
            last_status.map_or_else(|| "none".to_string(), |status| status.to_string())
        )
    }
}

/// Recursion state shared across the closure walk.
struct Walk<'a> {
    graph: AdmissionGraph,
    source: &'a dyn ActionMetadataSource,
    visited: BTreeSet<String>,
    context_data: &'a [(String, Value)],
}

/// Admit a job's complete action closure. On success returns the typed graph;
/// on the first rejection returns a redacted [`AdmissionError`] with the full
/// ancestry. This is read-only: it performs no container, checkout, cache,
/// credential, service, or download side effect.
pub fn admit_job(
    job: &AgentJobRequestMessage,
    context_data: &[(String, Value)],
    source: &dyn ActionMetadataSource,
) -> std::result::Result<AdmissionGraph, AdmissionError> {
    let mut walk = Walk {
        graph: AdmissionGraph::default(),
        source,
        visited: BTreeSet::new(),
        context_data,
    };
    let root = Ancestry::default();

    // A server-expanded reusable workflow (jobs.<id>.uses) is resolved by GitHub
    // before the job reaches Velnor. Admit its identity/full-SHA/inputs as the
    // graph root; never parse jobs.<id>.uses as a runner action.
    admit_reusable_workflow(&mut walk, &root)?;

    let workflow = workflow_source(context_data);
    for (index, step) in job
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.enabled)
    {
        if step.reference_type() != Some(ActionReferenceType::Repository) {
            continue;
        }
        let Some(reference) = step.reference.as_ref() else {
            continue;
        };
        let Some(repository) = reference.name.as_deref() else {
            continue;
        };
        let step_label = step
            .display_name_template()
            .or_else(|| step.name.clone())
            .unwrap_or_else(|| format!("step-{index}"));

        if is_local_reference(reference.name.as_deref(), reference.path.as_deref()) {
            let (workflow_repo, workflow_sha) = workflow.as_ref().ok_or_else(|| {
                AdmissionError::new(
                    &root.child(format!("step '{step_label}'")),
                    "uses",
                    "local action requires the exact workflow repository and SHA",
                    Vec::new(),
                )
            })?;
            let subpath = reference
                .path
                .as_deref()
                .or(reference.name.as_deref())
                .map(|value| value.trim_start_matches("./").to_string())
                .unwrap_or_default();
            let ancestry = root.child(format!("step '{step_label}' (local ./{subpath})"));
            admit_local(
                &mut walk,
                &ancestry,
                None,
                workflow_repo,
                workflow_sha,
                &subpath,
                1,
            )?;
            continue;
        }

        // Remote root. Resolve inputs against the full job context, then
        // validate the ref/subpath/inputs before any metadata fetch.
        let action_ref = reference.git_ref.as_deref().unwrap_or(
            if repository.eq_ignore_ascii_case("actions/checkout") {
                NATIVE_ACTION_REF
            } else {
                "<missing>"
            },
        );
        let inputs = resolve_step_inputs(step, context_data);
        let ancestry = root.child(format!("step '{step_label}' ({repository}@{action_ref})"));
        admit_remote(
            &mut walk,
            &ancestry,
            None,
            repository,
            action_ref,
            reference.path.as_deref(),
            &inputs,
            &step_label,
        )?;
    }

    Ok(walk.graph)
}

/// Admit the approved reusable workflow when the job was dispatched by one.
fn admit_reusable_workflow(walk: &mut Walk, root: &Ancestry) -> Result<(), AdmissionError> {
    let Some(job_workflow_ref) = context_string(walk.context_data, "github.job_workflow_ref")
    else {
        return Ok(());
    };
    // A reusable-workflow call is exactly the case where the job's defining
    // workflow (`job_workflow_ref`) differs from the top-level entry workflow
    // (`workflow_ref`). When the two match, or `workflow_ref` is absent so we
    // cannot confirm a reusable call, there is nothing extra to admit here — the
    // job's own steps are still admitted below.
    let Some(top_level) = context_string(walk.context_data, "github.workflow_ref") else {
        return Ok(());
    };
    if workflow_ref_identity(&top_level) == workflow_ref_identity(&job_workflow_ref) {
        return Ok(());
    }
    let Some((repository, path, ref_part)) = split_workflow_ref(&job_workflow_ref) else {
        return Ok(());
    };
    let ancestry = root.child(format!("reusable workflow {repository}/{path}@{ref_part}"));
    // N2: the caller must pin the reusable workflow by immutable full SHA. A
    // branch/tag ref (refs/heads/*, refs/tags/*, or a bare tag) is mutable.
    if !is_full_sha(&ref_part) {
        return Err(AdmissionError::new(
            &ancestry,
            "ref",
            "reusable workflow ref must be an immutable full-SHA",
            vec!["a 40-hex commit SHA".to_string()],
        ));
    }
    // Cross-check the resolved workflow SHA when present.
    if let Some(workflow_sha) = context_string(walk.context_data, "github.job_workflow_sha") {
        if workflow_sha != ref_part {
            return Err(AdmissionError::new(
                &ancestry,
                "sha",
                "reusable workflow ref does not match the resolved workflow SHA",
                vec![ref_part.clone()],
            ));
        }
    }
    let inputs = context_object_strings(walk.context_data, "inputs");
    manifest::validate_reusable_workflow(
        &ancestry.to_string(),
        &repository,
        &path,
        &ref_part,
        &inputs,
    )
    .map_err(|error| AdmissionError::from_capability(&ancestry, error))?;
    walk.graph.intern(
        ActionIdentity {
            repository,
            sha: ref_part,
            subpath: Some(path),
        },
        AdmissionNodeKind::ReusableWorkflow,
    );
    Ok(())
}

/// Admit a remote action: validate identity/subpath/inputs, then (for a
/// non-native action) fetch metadata and recurse.
#[allow(clippy::too_many_arguments)]
fn admit_remote(
    walk: &mut Walk,
    ancestry: &Ancestry,
    parent: Option<usize>,
    repository: &str,
    action_ref: &str,
    subpath: Option<&str>,
    inputs: &BTreeMap<String, String>,
    step_label: &str,
) -> Result<(), AdmissionError> {
    reject_unresolved_capability_inputs(ancestry, repository, inputs)?;
    manifest::validate_resolved_action(step_label, repository, action_ref, subpath, inputs)
        .map_err(|error| AdmissionError::from_capability(ancestry, error))?;

    let normalized = normalize_subpath(subpath);
    let identity = ActionIdentity {
        repository: repository.to_string(),
        sha: action_ref.to_string(),
        subpath: (!normalized.is_empty()).then_some(normalized),
    };

    // A native adapter is authoritative: no metadata fetch, no recursion.
    if native_action_adapter(repository).is_some() {
        let index = walk.graph.intern(identity, AdmissionNodeKind::NativeAction);
        walk.graph.link(parent, index);
        return Ok(());
    }

    let key = identity.display();
    let index = walk
        .graph
        .intern(identity.clone(), AdmissionNodeKind::RemoteAction);
    walk.graph.link(parent, index);
    if !walk.visited.insert(key) {
        return Ok(());
    }
    bound_nodes(walk, ancestry)?;

    let metadata = walk
        .source
        .fetch_action_metadata(repository, action_ref, subpath)
        .map_err(|error| AdmissionError::from_capability(ancestry, error))?;
    if !is_composite(&metadata) {
        return Ok(());
    }
    walk.graph.nodes[index].kind = AdmissionNodeKind::RemoteComposite;
    recurse_composite(
        walk, ancestry, index, repository, action_ref, inputs, &metadata, 1,
    )
}

/// Admit a local action read from the workflow repository at the workflow SHA.
fn admit_local(
    walk: &mut Walk,
    ancestry: &Ancestry,
    parent: Option<usize>,
    repository: &str,
    sha: &str,
    subpath: &str,
    depth: usize,
) -> Result<(), AdmissionError> {
    if subpath.starts_with('/') || subpath.split('/').any(|segment| segment == "..") {
        return Err(AdmissionError::new(
            ancestry,
            "path",
            "local action path escapes the workflow repository",
            Vec::new(),
        ));
    }
    let identity = ActionIdentity {
        repository: repository.to_string(),
        sha: sha.to_string(),
        subpath: Some(subpath.to_string()),
    };
    let index = walk.graph.intern(identity, AdmissionNodeKind::LocalAction);
    walk.graph.link(parent, index);
    let key = format!("local:{repository}@{sha}:{subpath}");
    if !walk.visited.insert(key) {
        return Ok(());
    }
    bound_nodes(walk, ancestry)?;

    let metadata = walk
        .source
        .fetch_action_metadata(repository, sha, Some(subpath))
        .map_err(|error| AdmissionError::from_capability(ancestry, error))?;
    if !is_composite(&metadata) {
        // A local JavaScript/Docker action is trusted workflow-repository code;
        // it is a closure leaf (matches the prior local preflight semantics).
        return Ok(());
    }
    recurse_composite(
        walk,
        ancestry,
        index,
        repository,
        sha,
        &BTreeMap::new(),
        &metadata,
        depth,
    )
}

/// Walk a composite action's steps, resolving each nested `uses`.
#[allow(clippy::too_many_arguments)]
fn recurse_composite(
    walk: &mut Walk,
    ancestry: &Ancestry,
    parent: usize,
    repo_ctx: &str,
    ref_ctx: &str,
    provided_inputs: &BTreeMap<String, String>,
    metadata: &ActionMetadata,
    depth: usize,
) -> Result<(), AdmissionError> {
    if depth > MAX_COMPOSITE_DEPTH {
        return Err(AdmissionError::new(
            ancestry,
            "depth",
            format!("composite action depth exceeded {MAX_COMPOSITE_DEPTH}"),
            Vec::new(),
        ));
    }
    // Resolve this composite's inputs (caller-provided over declared defaults)
    // so nested `${{ inputs.* }}` can be rendered before child validation.
    let composite_inputs = resolve_composite_inputs(metadata, provided_inputs);
    let inputs_context = inputs_context(&composite_inputs);

    for (child_index, step) in metadata.runs.steps.iter().enumerate() {
        let Some(uses) = step.uses.as_deref() else {
            continue;
        };
        let child_inputs = render_inputs(&step.with, &inputs_context);
        let label = step
            .name
            .clone()
            .or_else(|| step.id.clone())
            .unwrap_or_else(|| format!("nested-step-{child_index}"));

        if uses.starts_with("docker://") {
            return Err(AdmissionError::new(
                &ancestry.child(format!("nested '{label}' ({uses})")),
                "uses",
                "nested container action (docker://) is not admitted",
                Vec::new(),
            ));
        }
        if uses.starts_with('.') {
            // A composite-local `uses: ./path` is relative to the current
            // action's repository root; strip only the single `./` prefix.
            let nested_subpath = uses.strip_prefix("./").unwrap_or(uses);
            let ancestry = ancestry.child(format!("nested '{label}' (local ./{nested_subpath})"));
            admit_local(
                walk,
                &ancestry,
                Some(parent),
                repo_ctx,
                ref_ctx,
                nested_subpath,
                depth + 1,
            )?;
            continue;
        }

        let Some((target, target_ref)) = uses.rsplit_once('@') else {
            return Err(AdmissionError::new(
                &ancestry.child(format!("nested '{label}' ({uses})")),
                "uses",
                "nested action reference is missing an @ref",
                Vec::new(),
            ));
        };
        let mut segments = target.split('/');
        let (Some(owner), Some(repo)) = (segments.next(), segments.next()) else {
            return Err(AdmissionError::new(
                &ancestry.child(format!("nested '{label}' ({uses})")),
                "uses",
                "nested action reference is malformed",
                Vec::new(),
            ));
        };
        let target_repository = format!("{owner}/{repo}");
        let target_path = segments.collect::<Vec<_>>().join("/");
        let target_subpath = (!target_path.is_empty()).then_some(target_path.as_str());
        let ancestry = ancestry.child(format!(
            "nested '{label}' ({target_repository}@{target_ref})"
        ));
        admit_remote(
            walk,
            &ancestry,
            Some(parent),
            &target_repository,
            target_ref,
            target_subpath,
            &child_inputs,
            &label,
        )?;
    }
    Ok(())
}

fn bound_nodes(walk: &Walk, ancestry: &Ancestry) -> Result<(), AdmissionError> {
    if walk.graph.nodes.len() > MAX_ADMISSION_NODES {
        return Err(AdmissionError::new(
            ancestry,
            "nodes",
            format!("admission closure exceeded {MAX_ADMISSION_NODES} nodes"),
            Vec::new(),
        ));
    }
    Ok(())
}

/// Reject a capability-affecting input whose value still holds an unresolved
/// `${{ … }}` expression after rendering — it cannot be statically admitted.
fn reject_unresolved_capability_inputs(
    ancestry: &Ancestry,
    repository: &str,
    inputs: &BTreeMap<String, String>,
) -> Result<(), AdmissionError> {
    for (name, value) in inputs {
        if value.contains("${{") && manifest::action_input_is_constrained(repository, name) {
            return Err(AdmissionError::new(
                ancestry,
                format!("with.{name}"),
                "capability-affecting input is a dynamic expression that cannot be resolved before admission",
                vec!["a statically resolvable literal".to_string()],
            ));
        }
    }
    Ok(())
}

fn is_composite(metadata: &ActionMetadata) -> bool {
    metadata.runs.using.eq_ignore_ascii_case("composite")
}

fn resolve_step_inputs(
    step: &crate::job_message::ActionStep,
    context_data: &[(String, Value)],
) -> BTreeMap<String, String> {
    match crate::action::string_inputs(step) {
        Ok(inputs) => inputs
            .into_iter()
            .map(|(name, value)| {
                (
                    name,
                    crate::executor::render_context_expressions(&value, context_data),
                )
            })
            .collect(),
        Err(_) => BTreeMap::new(),
    }
}

fn resolve_composite_inputs(
    metadata: &ActionMetadata,
    provided: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut resolved = BTreeMap::new();
    for (name, input) in &metadata.inputs {
        if let Some(default) = &input.default_value {
            resolved.insert(name.clone(), default.clone());
        }
    }
    for (name, value) in provided {
        resolved.insert(name.clone(), value.clone());
    }
    resolved
}

fn inputs_context(inputs: &BTreeMap<String, String>) -> Vec<(String, Value)> {
    let object = inputs
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect::<serde_json::Map<_, _>>();
    vec![("inputs".to_string(), Value::Object(object))]
}

fn render_inputs(
    with: &BTreeMap<String, String>,
    inputs_context: &[(String, Value)],
) -> BTreeMap<String, String> {
    with.iter()
        .map(|(name, value)| {
            (
                name.clone(),
                crate::executor::render_context_expressions(value, inputs_context),
            )
        })
        .collect()
}

fn is_local_reference(name: Option<&str>, path: Option<&str>) -> bool {
    path.is_some_and(|value| value.starts_with('.'))
        || name.is_some_and(|value| value.starts_with('.'))
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Split a workflow ref `owner/repo/.github/workflows/x.yml@ref` into
/// `(owner/repo, .github/workflows/x.yml, ref)`.
fn split_workflow_ref(workflow_ref: &str) -> Option<(String, String, String)> {
    let (path_part, ref_part) = workflow_ref.rsplit_once('@')?;
    let mut segments = path_part.splitn(3, '/');
    let owner = segments.next()?;
    let repo = segments.next()?;
    let path = segments.next()?;
    if owner.is_empty() || repo.is_empty() || path.is_empty() {
        return None;
    }
    Some((
        format!("{owner}/{repo}"),
        path.to_string(),
        ref_part.to_string(),
    ))
}

/// The repository and workflow-file identity of a workflow ref, ignoring the
/// ref suffix. Used to tell a reusable-workflow call apart from the top-level
/// workflow.
fn workflow_ref_identity(workflow_ref: &str) -> String {
    workflow_ref
        .rsplit_once('@')
        .map(|(path, _)| path)
        .unwrap_or(workflow_ref)
        .to_ascii_lowercase()
}

fn workflow_source(context_data: &[(String, Value)]) -> Option<(String, String)> {
    let sha = context_string(context_data, "github.workflow_sha")
        .or_else(|| context_string(context_data, "github.sha"))?;
    let repository = context_string(context_data, "job.workflow_repository")
        .or_else(|| context_string(context_data, "github.repository"))
        .or_else(|| {
            context_string(context_data, "github.workflow_ref").and_then(|workflow_ref| {
                workflow_ref.split_once('/').map(|(owner, rest)| {
                    let repo = rest.split_once('/').map(|(repo, _)| repo).unwrap_or(rest);
                    format!("{owner}/{repo}")
                })
            })
        })?;
    Some((repository, sha))
}

fn context_string(context_data: &[(String, Value)], path: &str) -> Option<String> {
    let mut parts = path.split('.');
    let first = parts.next()?;
    let mut value = context_data
        .iter()
        .find(|(name, _)| name == first)
        .map(|(_, value)| value)?;
    for part in parts {
        value = value.as_object()?.get(part)?;
    }
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn context_object_strings(context_data: &[(String, Value)], key: &str) -> BTreeMap<String, String> {
    context_data
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| value.as_object())
        .map(|object| {
            object
                .iter()
                .filter_map(|(name, value)| match value {
                    Value::String(value) => Some((name.clone(), value.clone())),
                    Value::Number(value) => Some((name.clone(), value.to_string())),
                    Value::Bool(value) => Some((name.clone(), value.to_string())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory metadata source that counts reads and never touches the
    /// network — proving admission is read-only and metadata-fetch bounded.
    struct FakeMetadataSource {
        entries: BTreeMap<String, String>,
        reads: AtomicUsize,
    }

    impl FakeMetadataSource {
        fn new(entries: &[(&str, &str)]) -> Self {
            Self {
                entries: entries
                    .iter()
                    .map(|(key, yaml)| ((*key).to_string(), (*yaml).to_string()))
                    .collect(),
                reads: AtomicUsize::new(0),
            }
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::Relaxed)
        }
    }

    impl ActionMetadataSource for FakeMetadataSource {
        fn fetch_action_metadata(
            &self,
            repository: &str,
            git_ref: &str,
            subpath: Option<&str>,
        ) -> Result<ActionMetadata> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let key = match normalize_subpath(subpath) {
                subpath if subpath.is_empty() => format!("{repository}@{git_ref}"),
                subpath => format!("{repository}/{subpath}@{git_ref}"),
            };
            let yaml = self
                .entries
                .get(&key)
                .ok_or_else(|| anyhow::anyhow!("no fixture metadata for {key}"))?;
            crate::action::parse_action_metadata(yaml)
        }
    }

    fn job(steps: Value) -> AgentJobRequestMessage {
        serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "admission test",
            "requestId": 1,
            "steps": steps
        }))
        .unwrap()
    }

    fn repo_step(name: &str, git_ref: &str, path: Option<&str>, with: Value) -> Value {
        let mut reference = serde_json::json!({
            "type": "Repository",
            "name": name,
            "ref": git_ref
        });
        if let Some(path) = path {
            reference["path"] = Value::String(path.to_string());
        }
        serde_json::json!({
            "type": "Action",
            "displayName": name,
            "reference": reference,
            "inputs": with
        })
    }

    fn workflow_context() -> Vec<(String, Value)> {
        vec![(
            "github".to_string(),
            serde_json::json!({
                "repository": "acme/repo",
                "workflow_sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
            }),
        )]
    }

    const CACHE_SHA: &str = "55cc8345863c7cc4c66a329aec7e433d2d1c52a9";

    #[test]
    fn positive_root_local_local_remote_closure() {
        let context = workflow_context();
        let job = job(serde_json::json!([repo_step(
            "./.github/actions/outer",
            "",
            Some("./.github/actions/outer"),
            serde_json::json!({})
        )]));
        let source = FakeMetadataSource::new(&[
            (
                "acme/repo/.github/actions/outer@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "runs:\n  using: composite\n  steps:\n    - uses: ./.github/actions/inner\n",
            ),
            (
                "acme/repo/.github/actions/inner@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                &format!("runs:\n  using: composite\n  steps:\n    - uses: actions/cache@{CACHE_SHA}\n      with:\n        path: target\n        key: k\n"),
            ),
        ]);
        let graph = admit_job(&job, &context, &source).unwrap();
        assert!(graph.contains_remote_action("actions/cache", CACHE_SHA, None));
        assert!(graph
            .nodes
            .iter()
            .any(|node| node.kind == AdmissionNodeKind::LocalAction));
    }

    #[test]
    fn remote_root_composite_closure_admits_subaction() {
        let context = workflow_context();
        let job = job(serde_json::json!([repo_step(
            "actions/cache",
            CACHE_SHA,
            Some("restore"),
            serde_json::json!({"path": "target", "key": "k"})
        )]));
        let source = FakeMetadataSource::new(&[]);
        let graph = admit_job(&job, &context, &source).unwrap();
        // Native adapter: admitted without any metadata fetch.
        assert_eq!(source.reads(), 0);
        assert!(graph.contains_remote_action("actions/cache", CACHE_SHA, Some("restore")));
    }

    #[test]
    fn mutable_root_tag_rejected_before_any_metadata_read() {
        let context = workflow_context();
        let job = job(serde_json::json!([repo_step(
            "actions/cache",
            "v4",
            None,
            serde_json::json!({"path": "target", "key": "k"})
        )]));
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        assert_eq!(
            source.reads(),
            0,
            "mutable root must reject before any fetch"
        );
        assert_eq!(error.field, "ref");
        assert!(!error.ancestry.0.is_empty());
    }

    #[test]
    fn unsupported_nested_remote_in_remote_root_rejected_without_fetching_it() {
        let context = workflow_context();
        let outer_sha = "80a1acd07257a23b441c546e6fcad12239ef7626";
        let job = job(serde_json::json!([repo_step(
            "jackin-project/jackin-role-action",
            outer_sha,
            None,
            serde_json::json!({})
        )]));
        let source = FakeMetadataSource::new(&[(
            &format!("jackin-project/jackin-role-action@{outer_sha}"),
            "runs:\n  using: composite\n  steps:\n    - uses: evil/unknown@1111111111111111111111111111111111111111\n",
        )]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        // Only the remote root was fetched; the unsupported nested action was
        // rejected before its own metadata read.
        assert_eq!(source.reads(), 1);
        assert_eq!(error.field, "uses");
        assert!(error
            .ancestry
            .0
            .iter()
            .any(|hop| hop.contains("evil/unknown")));
    }

    #[test]
    fn unknown_subpath_rejected_before_fetch() {
        let context = workflow_context();
        let job = job(serde_json::json!([repo_step(
            "actions/cache",
            CACHE_SHA,
            Some("bogus"),
            serde_json::json!({"path": "target", "key": "k"})
        )]));
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "path");
    }

    #[test]
    fn dynamic_capability_input_rejected() {
        let context = workflow_context();
        let job = job(serde_json::json!([repo_step(
            "actions/cache",
            CACHE_SHA,
            None,
            serde_json::json!({"lookup-only": "${{ steps.probe.outputs.flag }}"})
        )]));
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "with.lookup-only");
    }

    #[test]
    fn ancestry_and_redaction_never_expose_received_value() {
        let context = workflow_context();
        let secret = "ghs_super_secret_value";
        let job = job(serde_json::json!([repo_step(
            "mozilla-actions/sccache-action",
            "9e7fa8a12102821edf02ca5dbea1acd0f89a2696",
            None,
            serde_json::json!({"token": secret})
        )]));
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        let rendered = error.to_string();
        assert!(!rendered.contains(secret), "value must be redacted");
        assert!(!error.ancestry.0.is_empty(), "ancestry must be complete");
        assert_eq!(error.field, "with.token");
    }

    #[test]
    fn reusable_workflow_mutable_ref_rejected() {
        let context = vec![(
            "github".to_string(),
            serde_json::json!({
                "job_workflow_ref": "jackin-project/jackin-role-action/.github/workflows/publish.yml@refs/heads/main",
                "workflow_ref": "acme/repo/.github/workflows/ci.yml@refs/heads/main",
                "repository": "acme/repo",
                "workflow_sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
            }),
        )];
        let job = job(serde_json::json!([]));
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "ref");
        assert!(error
            .ancestry
            .0
            .iter()
            .any(|hop| hop.contains("publish.yml")));
    }

    #[test]
    fn reusable_workflow_pinned_identity_admitted() {
        let publish_sha = "80a1acd07257a23b441c546e6fcad12239ef7626";
        let context = vec![(
            "github".to_string(),
            serde_json::json!({
                "job_workflow_ref": format!("jackin-project/jackin-role-action/.github/workflows/publish.yml@{publish_sha}"),
                "workflow_ref": "acme/repo/.github/workflows/ci.yml@refs/heads/main",
                "job_workflow_sha": publish_sha,
                "repository": "acme/repo",
                "workflow_sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
            }),
        )];
        let job = job(serde_json::json!([]));
        let source = FakeMetadataSource::new(&[]);
        let graph = admit_job(&job, &context, &source).unwrap();
        assert!(graph
            .nodes
            .iter()
            .any(|node| node.kind == AdmissionNodeKind::ReusableWorkflow));
    }

    #[test]
    fn cycle_and_depth_are_bounded() {
        let context = workflow_context();
        let job = job(serde_json::json!([repo_step(
            "./.github/actions/loop",
            "",
            Some("./.github/actions/loop"),
            serde_json::json!({})
        )]));
        // A composite whose nested step points back at itself must terminate via
        // the visited guard rather than recursing forever.
        let source = FakeMetadataSource::new(&[(
            "acme/repo/.github/actions/loop@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "runs:\n  using: composite\n  steps:\n    - uses: ./.github/actions/loop\n",
        )]);
        let graph = admit_job(&job, &context, &source).unwrap();
        assert_eq!(source.reads(), 1, "the self-cycle must be visited once");
        assert!(graph
            .nodes
            .iter()
            .any(|node| node.kind == AdmissionNodeKind::LocalAction));
    }
}

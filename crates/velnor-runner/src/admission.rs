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

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::action::{native_action_adapter, ActionAdapter, ActionMetadata, NATIVE_ACTION_REF};
use crate::job_message::{ActionReferenceType, AgentJobRequestMessage};
use crate::manifest::{self, CapabilityViolation};
use crate::protocol::GitHubScope;

/// Maximum composite nesting depth. Matches the removed local preflight bound.
const MAX_COMPOSITE_DEPTH: usize = 10;
/// Hard ceiling on admitted nodes so a pathological or adversarial closure
/// cannot exhaust resources before the depth guard trips.
const MAX_ADMISSION_NODES: usize = 512;
/// Hard ceiling on unique graph edges in a pathological action closure.
const MAX_ADMISSION_EDGES: usize = 4096;
/// Hard ceiling on steps in one fetched composite metadata document.
const MAX_COMPOSITE_STEPS: usize = 4096;
/// Hard ceiling on input-sensitive composite expansions in one job.
const MAX_ADMISSION_INVOCATIONS: usize = 1024;
/// Hard ceiling on all composite steps inspected during one admission walk.
const MAX_ADMISSION_STEP_VISITS: usize = 1_000_000;
/// Hard ceiling on workflow steps inspected during one admission walk.
const MAX_ADMISSION_ROOT_STEPS: usize = 4096;
/// Hard ceiling on one action metadata response before parsing.
const MAX_ACTION_METADATA_BYTES: usize = 1024 * 1024;
/// Bound immutable metadata retained by one admission walk.
const MAX_ADMISSION_METADATA_BYTES: usize = 8 * 1024 * 1024;
/// Bound copied workflow context used by composite input rendering.
const MAX_ADMISSION_CONTEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_ADMISSION_DURATION: Duration = Duration::from_secs(60);
/// Hard ceiling on one metadata string or map key.
const MAX_METADATA_STRING_BYTES: usize = 64 * 1024;
/// Hard ceiling on all metadata strings retained from one document.
const MAX_METADATA_TOTAL_STRING_BYTES: usize = 512 * 1024;
/// Hard ceiling on entries in one metadata string map.
const MAX_METADATA_MAP_ENTRIES: usize = 256;
/// Hard ceiling on input names in one invocation.
const MAX_ADMISSION_INPUTS: usize = 256;
/// Hard ceiling on total input name/value bytes in one invocation.
const MAX_ADMISSION_INPUT_BYTES: usize = 256 * 1024;
/// Hard ceiling on one input name or value.
const MAX_ADMISSION_INPUT_NAME_BYTES: usize = 256;
const MAX_ADMISSION_INPUT_VALUE_BYTES: usize = 64 * 1024;

/// A fully-resolved action identity: repository, immutable full-SHA ref, and
/// optional subpath. Local actions carry the workflow repository and workflow
/// SHA as their `repository`/`sha`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionIdentity {
    pub repository: String,
    pub sha: String,
    pub subpath: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ActionKey {
    local: bool,
    repository: String,
    sha: String,
    subpath: String,
}

impl ActionKey {
    fn remote(repository: &str, sha: &str, subpath: Option<&str>) -> Self {
        Self::new(false, repository, sha, subpath)
    }

    fn local(repository: &str, sha: &str, subpath: &str) -> Self {
        Self::new(true, repository, sha, Some(subpath))
    }

    fn new(local: bool, repository: &str, sha: &str, subpath: Option<&str>) -> Self {
        Self {
            local,
            repository: repository.trim().to_ascii_lowercase(),
            sha: sha.trim().to_ascii_lowercase(),
            // GitHub repository names and refs are case-insensitive here, but
            // action paths address case-sensitive repository files.
            subpath: normalize_subpath(subpath),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct InvocationKey {
    node_index: usize,
    inputs_digest: [u8; 32],
}

fn input_digest(inputs: &BTreeMap<String, String>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for (name, value) in inputs {
        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update((value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.finalize().into()
}

/// The kind of node in the admission graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionNodeKind {
    /// A remote action served by a native Rust adapter (no metadata fetch).
    NativeAction,
    /// A remote composite action whose metadata was fetched and recursed.
    RemoteComposite,
    /// A remote non-composite (JavaScript/Docker) action. It is admitted as a
    /// leaf; the planner selects its executable adapter from the manifest.
    RemoteAction,
    /// A local action or composite read from the workflow repository.
    LocalAction,
    /// A server-expanded reusable workflow (`jobs.<id>.uses`).
    ReusableWorkflow,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
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
    node_indices: BTreeMap<(String, String, String, u8), usize>,
    edge_indices: BTreeSet<(usize, usize)>,
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
        self.node_indices.contains_key(&(
            repository.trim().to_ascii_lowercase(),
            sha.trim().to_ascii_lowercase(),
            subpath,
            1,
        ))
    }

    fn intern(
        &mut self,
        identity: ActionIdentity,
        kind: AdmissionNodeKind,
        ancestry: &Ancestry,
    ) -> Result<usize, AdmissionError> {
        let key = graph_node_key(&identity, kind);
        if let Some(index) = self.node_indices.get(&key).copied() {
            return Ok(index);
        }
        if self.nodes.len() >= MAX_ADMISSION_NODES {
            return Err(AdmissionError::new(
                ancestry,
                "nodes",
                format!("admission closure exceeded {MAX_ADMISSION_NODES} nodes"),
                Vec::new(),
            ));
        }
        self.nodes.push(AdmissionNode { identity, kind });
        let index = self.nodes.len() - 1;
        self.node_indices.insert(key, index);
        Ok(index)
    }

    fn link(
        &mut self,
        from: Option<usize>,
        to: usize,
        ancestry: &Ancestry,
    ) -> Result<(), AdmissionError> {
        if let Some(from) = from {
            let edge = AdmissionEdge { from, to };
            let edge_key = (from, to);
            if self.edge_indices.contains(&edge_key) {
                return Ok(());
            }
            if self.edges.len() >= MAX_ADMISSION_EDGES {
                return Err(AdmissionError::new(
                    ancestry,
                    "edges",
                    format!("admission closure exceeded {MAX_ADMISSION_EDGES} edges"),
                    Vec::new(),
                ));
            }
            self.edges.push(edge);
            self.edge_indices.insert(edge_key);
        }
        Ok(())
    }
}

fn graph_node_key(
    identity: &ActionIdentity,
    kind: AdmissionNodeKind,
) -> (String, String, String, u8) {
    let class = match kind {
        AdmissionNodeKind::LocalAction => 0,
        AdmissionNodeKind::ReusableWorkflow => 2,
        AdmissionNodeKind::NativeAction
        | AdmissionNodeKind::RemoteComposite
        | AdmissionNodeKind::RemoteAction => 1,
    };
    (
        identity.repository.trim().to_ascii_lowercase(),
        identity.sha.trim().to_ascii_lowercase(),
        normalize_subpath(identity.subpath.as_deref()),
        class,
    )
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
    pub fn new(token: impl Into<String>, scope: &GitHubScope) -> Result<Self> {
        Self::with_api_url(token, scope.api_base_url.as_str())
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        token: impl Into<String>,
        api_url: impl Into<String>,
    ) -> Result<Self> {
        Self::with_api_url(token, api_url)
    }

    fn with_api_url(token: impl Into<String>, api_url: impl Into<String>) -> Result<Self> {
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .user_agent("velnor-runner")
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
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
        let directory = normalize_subpath(subpath);
        let mut last_status = None;
        for file in ["action.yml", "action.yaml"] {
            let metadata_path = if directory.is_empty() {
                file.to_string()
            } else {
                format!("{directory}/{file}")
            };
            let mut url = url::Url::parse(&self.api_url)
                .map_err(|error| anyhow::anyhow!("parse configured GitHub API URL: {error}"))?;
            {
                let mut segments = url
                    .path_segments_mut()
                    .map_err(|_| anyhow::anyhow!("cannot build GitHub contents URL"))?;
                segments.push("repos");
                for segment in repository.split('/') {
                    if !segment.is_empty() {
                        segments.push(segment);
                    }
                }
                segments.push("contents");
                for segment in metadata_path.split('/') {
                    if !segment.is_empty() {
                        segments.push(segment);
                    }
                }
            }
            url.query_pairs_mut().append_pair("ref", git_ref);
            let response = self
                .client
                .get(url)
                .bearer_auth(&self.token)
                .header("Accept", "application/vnd.github.raw+json")
                .header("X-GitHub-Api-Version", "2026-03-10")
                .send()?;
            last_status = Some(response.status());
            if response.status().is_success() {
                let content_length = response.content_length();
                let contents = read_bounded_metadata_body(response, content_length)?;
                return crate::action::parse_action_metadata(&contents)
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

fn read_bounded_metadata_body<R: Read>(reader: R, content_length: Option<u64>) -> Result<String> {
    if content_length.is_some_and(|length| length > MAX_ACTION_METADATA_BYTES as u64) {
        anyhow::bail!("action metadata response exceeds {MAX_ACTION_METADATA_BYTES} bytes");
    }
    let mut body = Vec::with_capacity(
        content_length
            .unwrap_or_default()
            .min(MAX_ACTION_METADATA_BYTES as u64) as usize,
    );
    reader
        .take((MAX_ACTION_METADATA_BYTES as u64).saturating_add(1))
        .read_to_end(&mut body)?;
    if body.len() > MAX_ACTION_METADATA_BYTES {
        anyhow::bail!("action metadata response exceeds {MAX_ACTION_METADATA_BYTES} bytes");
    }
    String::from_utf8(body).map_err(Into::into)
}

/// Recursion state shared across the closure walk.
struct Walk<'a> {
    graph: AdmissionGraph,
    source: &'a dyn ActionMetadataSource,
    metadata_cache: BTreeMap<ActionKey, Arc<ActionMetadata>>,
    metadata_bytes: usize,
    expanded: BTreeSet<InvocationKey>,
    step_visits: usize,
    context_data: &'a [(String, Value)],
    deadline: Instant,
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
    if !context_within_admission_budget(context_data) {
        return Err(AdmissionError::new(
            &Ancestry::default(),
            "context",
            format!("workflow context exceeds {MAX_ADMISSION_CONTEXT_BYTES} bytes"),
            Vec::new(),
        ));
    }
    let mut walk = Walk {
        graph: AdmissionGraph::default(),
        source,
        metadata_cache: BTreeMap::new(),
        metadata_bytes: 0,
        expanded: BTreeSet::new(),
        step_visits: 0,
        context_data,
        deadline: Instant::now() + MAX_ADMISSION_DURATION,
    };
    let root = Ancestry::default();

    // A server-expanded reusable workflow (jobs.<id>.uses) is resolved by GitHub
    // before the job reaches Velnor. Admit its identity/full-SHA/inputs as the
    // graph root; never parse jobs.<id>.uses as a runner action.
    admit_reusable_workflow(&mut walk, &root)?;

    let workflow = workflow_source(context_data);
    if job.steps.len() > MAX_ADMISSION_ROOT_STEPS {
        return Err(AdmissionError::new(
            &root,
            "steps",
            format!("workflow step count exceeds {MAX_ADMISSION_ROOT_STEPS}"),
            Vec::new(),
        ));
    }
    walk.step_visits = job.steps.len();
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
                LocalInputSource::Deferred { step, context_data },
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
        let ancestry = root.child(format!("step '{step_label}' ({repository}@{action_ref})"));
        let inputs = resolve_step_inputs(step, context_data).map_err(|error| {
            AdmissionError::new(&ancestry, "inputs", error.to_string(), Vec::new())
        })?;
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
    // A reusable-workflow call is identified by the presence of the job-defining
    // workflow identity. Once present, all related identity fields are required
    // and validated before the action graph can perform any metadata read.
    let ancestry = root.child("reusable workflow context".to_string());
    let Some(top_level) = context_string(walk.context_data, "github.workflow_ref") else {
        return Err(AdmissionError::new(
            &ancestry,
            "github.workflow_ref",
            "reusable workflow context requires the top-level workflow identity",
            vec!["owner/repository/.github/workflows/name.yml@ref".to_string()],
        ));
    };
    let Some((top_repository, top_path, top_ref)) = split_workflow_ref(&top_level) else {
        return Err(AdmissionError::new(
            &ancestry,
            "github.workflow_ref",
            "top-level workflow identity is malformed",
            vec!["owner/repository/.github/workflows/name.yml@ref".to_string()],
        ));
    };
    let Some((repository, path, ref_part)) = split_workflow_ref(&job_workflow_ref) else {
        return Err(AdmissionError::new(
            &ancestry,
            "github.job_workflow_ref",
            "reusable workflow identity is malformed",
            vec!["owner/repository/.github/workflows/name.yml@<40-hex-SHA>".to_string()],
        ));
    };
    if top_repository == repository && top_path == path {
        return Err(AdmissionError::new(
            &ancestry,
            "github.job_workflow_ref",
            "reusable workflow identity must differ from the top-level workflow",
            vec!["a distinct approved workflow identity".to_string()],
        ));
    }
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
    let Some(top_level_sha) = context_string(walk.context_data, "github.workflow_sha") else {
        return Err(AdmissionError::new(
            &ancestry,
            "github.workflow_sha",
            "reusable workflow context requires the resolved top-level workflow SHA",
            vec!["the exact 40-hex workflow commit SHA".to_string()],
        ));
    };
    if !is_full_sha(&top_level_sha) || (is_full_sha(&top_ref) && top_level_sha != top_ref) {
        return Err(AdmissionError::new(
            &ancestry,
            "github.workflow_sha",
            "top-level workflow ref does not match the resolved workflow SHA",
            vec!["the exact 40-hex workflow commit SHA".to_string()],
        ));
    }
    // Cross-check the resolved workflow SHA when present.
    let Some(workflow_sha) = context_string(walk.context_data, "github.job_workflow_sha") else {
        return Err(AdmissionError::new(
            &ancestry,
            "github.job_workflow_sha",
            "reusable workflow context requires the resolved workflow SHA",
            vec!["the exact 40-hex workflow commit SHA".to_string()],
        ));
    };
    if !is_full_sha(&workflow_sha) || workflow_sha != ref_part {
        return Err(AdmissionError::new(
            &ancestry,
            "sha",
            "reusable workflow ref does not match the resolved workflow SHA",
            vec!["the exact 40-hex workflow commit SHA".to_string()],
        ));
    }
    let inputs = context_object_strings(walk.context_data, "inputs");
    let inputs = canonicalize_admission_inputs(&inputs, &ancestry)?;
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
        &ancestry,
    )?;
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
    let inputs = canonicalize_admission_inputs(inputs, ancestry)?;
    reject_unresolved_capability_inputs(ancestry, repository, &inputs)?;
    manifest::validate_resolved_action(step_label, repository, action_ref, subpath, &inputs)
        .map_err(|error| AdmissionError::from_capability(ancestry, error))?;
    let adapter = manifest::find(repository)
        .map(|capability| capability.adapter)
        .ok_or_else(|| {
            AdmissionError::new(
                ancestry,
                "uses",
                "action capability disappeared during admission",
                Vec::new(),
            )
        })?;

    let normalized = normalize_subpath(subpath);
    let identity = ActionIdentity {
        repository: repository.to_string(),
        sha: action_ref.to_string(),
        subpath: (!normalized.is_empty()).then_some(normalized),
    };

    // A native adapter is authoritative: no metadata fetch, no recursion.
    if let ActionAdapter::Native(expected) = adapter {
        if native_action_adapter(repository) != Some(expected) {
            return Err(AdmissionError::new(
                ancestry,
                "adapter",
                "manifest native adapter does not match the native adapter table",
                vec![format!("{expected:?}")],
            ));
        }
        let index = walk
            .graph
            .intern(identity, AdmissionNodeKind::NativeAction, ancestry)?;
        walk.graph.link(parent, index, ancestry)?;
        return Ok(());
    }

    let action_key = ActionKey::remote(repository, action_ref, subpath);
    let index = walk
        .graph
        .intern(identity.clone(), AdmissionNodeKind::RemoteAction, ancestry)?;
    walk.graph.link(parent, index, ancestry)?;

    let metadata = cached_metadata(walk, &action_key, repository, action_ref, subpath, ancestry)?;
    let runtime = metadata
        .runtime()
        .map_err(|error| AdmissionError::new(ancestry, "runtime", error.to_string(), Vec::new()))?;
    manifest::validate_action_runtime(step_label, repository, action_ref, &runtime)
        .map_err(|error| AdmissionError::from_capability(ancestry, error))?;
    if !matches!(adapter, ActionAdapter::Composite) {
        return Ok(());
    }
    walk.graph.nodes[index].kind = AdmissionNodeKind::RemoteComposite;
    recurse_composite_invocation(
        walk, ancestry, index, repository, action_ref, &inputs, &metadata, 1,
    )
}

/// Admit a local action read from the workflow repository at the workflow SHA.
enum LocalInputSource<'a> {
    Deferred {
        step: &'a crate::job_message::ActionStep,
        context_data: &'a [(String, Value)],
    },
    Resolved(&'a BTreeMap<String, String>),
}

impl<'a> LocalInputSource<'a> {
    fn resolve(self) -> Result<Cow<'a, BTreeMap<String, String>>> {
        match self {
            Self::Deferred { step, context_data } => {
                Ok(Cow::Owned(resolve_step_inputs(step, context_data)?))
            }
            Self::Resolved(inputs) => Ok(Cow::Borrowed(inputs)),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn admit_local(
    walk: &mut Walk,
    ancestry: &Ancestry,
    parent: Option<usize>,
    repository: &str,
    sha: &str,
    subpath: &str,
    provided_inputs: LocalInputSource<'_>,
    depth: usize,
) -> Result<(), AdmissionError> {
    if subpath.starts_with('/')
        || subpath
            .split('/')
            .any(|segment| segment == ".." || segment.is_empty())
    {
        return Err(AdmissionError::new(
            ancestry,
            "path",
            "local action path escapes the workflow repository",
            Vec::new(),
        ));
    }
    if !is_full_sha(sha) {
        return Err(AdmissionError::new(
            ancestry,
            "ref",
            "local action workflow ref must be an immutable full-SHA",
            vec!["a 40-hex commit SHA".to_string()],
        ));
    }
    let identity = ActionIdentity {
        repository: repository.to_string(),
        sha: sha.to_string(),
        subpath: Some(subpath.to_string()),
    };
    let action_key = ActionKey::local(repository, sha, subpath);
    let index = walk
        .graph
        .intern(identity, AdmissionNodeKind::LocalAction, ancestry)?;
    walk.graph.link(parent, index, ancestry)?;

    let metadata = cached_metadata(walk, &action_key, repository, sha, Some(subpath), ancestry)?;
    if !is_composite(&metadata) {
        // A local JavaScript/Docker action is trusted workflow-repository code;
        // it is a closure leaf (matches the prior local preflight semantics).
        return Ok(());
    }
    let provided_inputs = provided_inputs
        .resolve()
        .map_err(|error| AdmissionError::new(ancestry, "inputs", error.to_string(), Vec::new()))?;
    let provided_inputs = canonicalize_admission_inputs(provided_inputs.as_ref(), ancestry)?;
    recurse_composite_invocation(
        walk,
        ancestry,
        index,
        repository,
        sha,
        &provided_inputs,
        &metadata,
        depth,
    )
}

fn cached_metadata(
    walk: &mut Walk,
    key: &ActionKey,
    repository: &str,
    action_ref: &str,
    subpath: Option<&str>,
    ancestry: &Ancestry,
) -> Result<Arc<ActionMetadata>, AdmissionError> {
    if Instant::now() >= walk.deadline {
        return Err(AdmissionError::new(
            ancestry,
            "deadline",
            "action admission exceeded its read-only deadline",
            Vec::new(),
        ));
    }
    if let Some(metadata) = walk.metadata_cache.get(key) {
        return Ok(Arc::clone(metadata));
    }
    let metadata = walk
        .source
        .fetch_action_metadata(repository, action_ref, subpath)
        .map_err(|error| AdmissionError::from_capability(ancestry, error))?;
    validate_metadata_bounds(&metadata).map_err(|error| {
        AdmissionError::new(ancestry, "metadata", error.to_string(), Vec::new())
    })?;
    let retained_bytes = metadata_retained_bytes(&metadata);
    if walk.metadata_bytes.saturating_add(retained_bytes) > MAX_ADMISSION_METADATA_BYTES {
        return Err(AdmissionError::new(
            ancestry,
            "metadata",
            format!("admission metadata exceeds {MAX_ADMISSION_METADATA_BYTES} bytes"),
            Vec::new(),
        ));
    }
    walk.metadata_bytes = walk.metadata_bytes.saturating_add(retained_bytes);
    let metadata = Arc::new(metadata);
    walk.metadata_cache
        .insert(key.clone(), Arc::clone(&metadata));
    Ok(metadata)
}

#[allow(clippy::too_many_arguments)]
fn recurse_composite_invocation(
    walk: &mut Walk,
    ancestry: &Ancestry,
    parent: usize,
    repo_ctx: &str,
    ref_ctx: &str,
    inputs: &BTreeMap<String, String>,
    metadata: &ActionMetadata,
    depth: usize,
) -> Result<(), AdmissionError> {
    let invocation = InvocationKey {
        node_index: parent,
        inputs_digest: input_digest(inputs),
    };
    if walk.expanded.contains(&invocation) {
        return Ok(());
    }
    if walk.expanded.len() >= MAX_ADMISSION_INVOCATIONS {
        return Err(AdmissionError::new(
            ancestry,
            "invocations",
            format!("admission closure exceeded {MAX_ADMISSION_INVOCATIONS} composite invocations"),
            Vec::new(),
        ));
    }
    walk.expanded.insert(invocation.clone());
    recurse_composite(
        walk, ancestry, parent, repo_ctx, ref_ctx, inputs, metadata, depth,
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
    if metadata.runs.steps.len() > MAX_COMPOSITE_STEPS {
        return Err(AdmissionError::new(
            ancestry,
            "steps",
            format!("composite metadata exceeded {MAX_COMPOSITE_STEPS} steps"),
            Vec::new(),
        ));
    }
    walk.step_visits = walk.step_visits.saturating_add(metadata.runs.steps.len());
    if walk.step_visits > MAX_ADMISSION_STEP_VISITS {
        return Err(AdmissionError::new(
            ancestry,
            "steps",
            format!("admission closure exceeded {MAX_ADMISSION_STEP_VISITS} step visits"),
            Vec::new(),
        ));
    }
    // Resolve this composite's inputs (caller-provided over declared defaults)
    // so nested `${{ inputs.* }}` can be rendered before child validation.
    let composite_inputs = resolve_composite_inputs(metadata, provided_inputs);
    let inputs_context = inputs_context(&composite_inputs, walk.context_data);

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
                LocalInputSource::Resolved(&child_inputs),
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
) -> Result<BTreeMap<String, String>> {
    Ok(crate::action::string_inputs(step)?
        .into_iter()
        .map(|(name, value)| (name, render_admission_expression(&value, context_data)))
        .collect())
}

fn resolve_composite_inputs(
    metadata: &ActionMetadata,
    provided: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut resolved = BTreeMap::new();
    for (name, input) in &metadata.inputs {
        if let Some(default) = &input.default_value {
            resolved.insert(name.to_ascii_lowercase(), default.clone());
        }
    }
    for (name, value) in provided {
        resolved.insert(name.clone(), value.clone());
    }
    resolved
}

fn inputs_context(
    inputs: &BTreeMap<String, String>,
    base_context: &[(String, Value)],
) -> Vec<(String, Value)> {
    let mut context = base_context.to_vec();
    context.retain(|(name, _)| !name.eq_ignore_ascii_case("inputs"));
    let object = inputs
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect::<serde_json::Map<_, _>>();
    context.push(("inputs".to_string(), Value::Object(object)));
    context
}

fn render_inputs(
    with: &BTreeMap<String, String>,
    inputs_context: &[(String, Value)],
) -> BTreeMap<String, String> {
    with.iter()
        .map(|(name, value)| {
            (
                name.clone(),
                render_admission_expression(value, inputs_context),
            )
        })
        .collect()
}

fn render_admission_expression(value: &str, context_data: &[(String, Value)]) -> String {
    if contains_runtime_context_expression(value) {
        value.to_string()
    } else {
        crate::executor::render_context_expressions_bounded(value, context_data)
    }
}

fn contains_runtime_context_expression(value: &str) -> bool {
    const RUNTIME_ROOTS: &[&str] = &[
        "steps.",
        "steps[",
        "needs.",
        "needs[",
        "env.",
        "env[",
        "job.",
        "job[",
        "matrix.",
        "matrix[",
        "strategy.",
        "strategy[",
        "secrets.",
        "secrets[",
        "vars.",
        "vars[",
        "runner.",
        "runner[",
        "github.",
        "github.action",
        "github[",
        "github.action_status",
        "github.event.",
    ];
    let mut offset = 0;
    while let Some(start) = value[offset..].find("${{") {
        let start = offset + start + 3;
        let Some(end) = value[start..].find("}}") else {
            return false;
        };
        let expression = &value[start..start + end];
        let lower_expression = expression.to_ascii_lowercase();
        if [
            "hashfiles(",
            "format(",
            "success(",
            "failure(",
            "cancelled(",
            "always(",
        ]
        .iter()
        .any(|function| lower_expression.contains(function))
        {
            return true;
        }
        if RUNTIME_ROOTS.iter().any(|root| {
            let mut search = 0;
            while let Some(found) = lower_expression[search..].find(root) {
                let found = search + found;
                let boundary = found == 0
                    || !lower_expression.as_bytes()[found - 1].is_ascii_alphanumeric()
                        && lower_expression.as_bytes()[found - 1] != b'_'
                        && lower_expression.as_bytes()[found - 1] != b'-';
                if boundary {
                    return true;
                }
                search = found + root.len();
            }
            false
        }) {
            return true;
        }
        offset = start + end + 2;
    }
    false
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
    if ref_part.is_empty() || ref_part.trim() != ref_part {
        return None;
    }
    let mut segments = path_part.splitn(3, '/');
    let owner = segments.next()?;
    let repo = segments.next()?;
    let path = segments.next()?;
    if owner.is_empty()
        || repo.is_empty()
        || path.is_empty()
        || !path.starts_with(".github/workflows/")
        || (!path.ends_with(".yml") && !path.ends_with(".yaml"))
    {
        return None;
    }
    Some((
        format!("{owner}/{repo}"),
        path.to_string(),
        ref_part.to_string(),
    ))
}

fn workflow_source(context_data: &[(String, Value)]) -> Option<(String, String)> {
    let sha = context_string(context_data, "github.workflow_sha")?;
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

fn canonicalize_admission_inputs(
    inputs: &BTreeMap<String, String>,
    ancestry: &Ancestry,
) -> Result<BTreeMap<String, String>, AdmissionError> {
    if inputs.len() > MAX_ADMISSION_INPUTS {
        return Err(AdmissionError::new(
            ancestry,
            "inputs",
            format!("action input count exceeds {MAX_ADMISSION_INPUTS}"),
            Vec::new(),
        ));
    }
    let mut total_bytes = 0usize;
    let mut canonical = BTreeMap::new();
    for (name, value) in inputs {
        if name.len() > MAX_ADMISSION_INPUT_NAME_BYTES {
            return Err(AdmissionError::new(
                ancestry,
                "inputs",
                format!("action input name exceeds {MAX_ADMISSION_INPUT_NAME_BYTES} bytes"),
                Vec::new(),
            ));
        }
        if value.len() > MAX_ADMISSION_INPUT_VALUE_BYTES {
            return Err(AdmissionError::new(
                ancestry,
                "inputs",
                format!("action input value exceeds {MAX_ADMISSION_INPUT_VALUE_BYTES} bytes"),
                Vec::new(),
            ));
        }
        total_bytes = total_bytes
            .saturating_add(name.len())
            .saturating_add(value.len());
        if total_bytes > MAX_ADMISSION_INPUT_BYTES {
            return Err(AdmissionError::new(
                ancestry,
                "inputs",
                format!("action inputs exceed {MAX_ADMISSION_INPUT_BYTES} bytes"),
                Vec::new(),
            ));
        }
        let canonical_name = name.to_ascii_lowercase();
        if canonical.insert(canonical_name, value.clone()).is_some() {
            return Err(AdmissionError::new(
                ancestry,
                "inputs",
                "action input names differ only by ASCII case",
                Vec::new(),
            ));
        }
    }
    Ok(canonical)
}

fn validate_metadata_bounds(metadata: &ActionMetadata) -> Result<()> {
    let mut total_string_bytes = 0usize;
    validate_metadata_text(metadata.name.as_deref(), "name", &mut total_string_bytes)?;
    validate_metadata_text(
        metadata.description.as_deref(),
        "description",
        &mut total_string_bytes,
    )?;
    if metadata.inputs.len() > MAX_METADATA_MAP_ENTRIES {
        anyhow::bail!("metadata input count exceeds {MAX_METADATA_MAP_ENTRIES}");
    }
    let mut input_names = BTreeSet::new();
    for (name, input) in &metadata.inputs {
        validate_metadata_text(Some(name), "inputs.name", &mut total_string_bytes)?;
        if !input_names.insert(name.to_ascii_lowercase()) {
            anyhow::bail!("metadata input names differ only by ASCII case");
        }
        validate_metadata_text(
            input.description.as_deref(),
            "inputs.description",
            &mut total_string_bytes,
        )?;
        validate_metadata_text(
            input.default_value.as_deref(),
            "inputs.default",
            &mut total_string_bytes,
        )?;
    }
    if metadata.outputs.len() > MAX_METADATA_MAP_ENTRIES {
        anyhow::bail!("metadata output count exceeds {MAX_METADATA_MAP_ENTRIES}");
    }
    for (name, output) in &metadata.outputs {
        validate_metadata_text(Some(name), "outputs.name", &mut total_string_bytes)?;
        validate_metadata_text(
            output.description.as_deref(),
            "outputs.description",
            &mut total_string_bytes,
        )?;
        validate_metadata_text(
            output.value.as_deref(),
            "outputs.value",
            &mut total_string_bytes,
        )?;
    }
    validate_metadata_text(
        Some(&metadata.runs.using),
        "runs.using",
        &mut total_string_bytes,
    )?;
    for (field, value) in [
        ("runs.main", metadata.runs.main.as_deref()),
        ("runs.pre", metadata.runs.pre.as_deref()),
        ("runs.pre-if", metadata.runs.pre_if.as_deref()),
        ("runs.post", metadata.runs.post.as_deref()),
        ("runs.post-if", metadata.runs.post_if.as_deref()),
        ("runs.image", metadata.runs.image.as_deref()),
        ("runs.entrypoint", metadata.runs.entrypoint.as_deref()),
    ] {
        validate_metadata_text(value, field, &mut total_string_bytes)?;
    }
    if metadata.runs.args.len() > MAX_METADATA_MAP_ENTRIES {
        anyhow::bail!("metadata argument count exceeds {MAX_METADATA_MAP_ENTRIES}");
    }
    for value in &metadata.runs.args {
        validate_metadata_text(Some(value), "runs.args", &mut total_string_bytes)?;
    }
    if metadata.runs.steps.len() > MAX_COMPOSITE_STEPS {
        anyhow::bail!("metadata step count exceeds {MAX_COMPOSITE_STEPS}");
    }
    for step in &metadata.runs.steps {
        for (field, value) in [
            ("steps.id", step.id.as_deref()),
            ("steps.name", step.name.as_deref()),
            ("steps.shell", step.shell.as_deref()),
            ("steps.run", step.run.as_deref()),
            ("steps.uses", step.uses.as_deref()),
            ("steps.if", step.condition.as_deref()),
            ("steps.working-directory", step.working_directory.as_deref()),
            ("steps.continue-on-error", step.continue_on_error.as_deref()),
        ] {
            validate_metadata_text(value, field, &mut total_string_bytes)?;
        }
        validate_metadata_string_map(&step.with, "steps.with", &mut total_string_bytes)?;
        validate_metadata_string_map(&step.env, "steps.env", &mut total_string_bytes)?;
    }
    Ok(())
}

fn validate_metadata_text(
    value: Option<&str>,
    field: &str,
    total_string_bytes: &mut usize,
) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.len() > MAX_METADATA_STRING_BYTES {
        anyhow::bail!("metadata {field} exceeds {MAX_METADATA_STRING_BYTES} bytes");
    }
    *total_string_bytes = total_string_bytes.saturating_add(value.len());
    if *total_string_bytes > MAX_METADATA_TOTAL_STRING_BYTES {
        anyhow::bail!("metadata strings exceed {MAX_METADATA_TOTAL_STRING_BYTES} bytes in total");
    }
    Ok(())
}

fn validate_metadata_string_map(
    values: &BTreeMap<String, String>,
    field: &str,
    total_string_bytes: &mut usize,
) -> Result<()> {
    if values.len() > MAX_METADATA_MAP_ENTRIES {
        anyhow::bail!("metadata {field} count exceeds {MAX_METADATA_MAP_ENTRIES}");
    }
    for (name, value) in values {
        validate_metadata_text(Some(name), field, total_string_bytes)?;
        validate_metadata_text(Some(value), field, total_string_bytes)?;
    }
    Ok(())
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

fn context_within_admission_budget(context_data: &[(String, Value)]) -> bool {
    let mut total = 0usize;
    let mut pending = context_data
        .iter()
        .map(|(name, value)| (0usize, name.as_str(), value))
        .collect::<Vec<_>>();
    while let Some((depth, name, value)) = pending.pop() {
        total = total.saturating_add(name.len());
        if total > MAX_ADMISSION_CONTEXT_BYTES || depth > 64 {
            return false;
        }
        match value {
            Value::String(value) => total = total.saturating_add(value.len()),
            Value::Array(values) => {
                for value in values {
                    pending.push((depth + 1, "", value));
                }
            }
            Value::Object(values) => {
                for (name, value) in values {
                    pending.push((depth + 1, name, value));
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        if total > MAX_ADMISSION_CONTEXT_BYTES {
            return false;
        }
    }
    true
}

fn metadata_retained_bytes(metadata: &ActionMetadata) -> usize {
    fn add(total: &mut usize, value: Option<&str>) {
        *total = total.saturating_add(value.map_or(0, str::len));
    }
    fn add_map(total: &mut usize, values: &BTreeMap<String, String>) {
        for (name, value) in values {
            *total = total.saturating_add(name.len()).saturating_add(value.len());
        }
    }

    let mut total = 0usize;
    add(&mut total, metadata.name.as_deref());
    add(&mut total, metadata.description.as_deref());
    for (name, input) in &metadata.inputs {
        total = total.saturating_add(name.len());
        add(&mut total, input.description.as_deref());
        add(&mut total, input.default_value.as_deref());
    }
    for (name, output) in &metadata.outputs {
        total = total.saturating_add(name.len());
        add(&mut total, output.description.as_deref());
        add(&mut total, output.value.as_deref());
    }
    add(&mut total, Some(&metadata.runs.using));
    for value in [
        metadata.runs.main.as_deref(),
        metadata.runs.pre.as_deref(),
        metadata.runs.pre_if.as_deref(),
        metadata.runs.post.as_deref(),
        metadata.runs.post_if.as_deref(),
        metadata.runs.image.as_deref(),
        metadata.runs.entrypoint.as_deref(),
    ] {
        add(&mut total, value);
    }
    for value in &metadata.runs.args {
        total = total.saturating_add(value.len());
    }
    for step in &metadata.runs.steps {
        for value in [
            step.id.as_deref(),
            step.name.as_deref(),
            step.shell.as_deref(),
            step.run.as_deref(),
            step.uses.as_deref(),
            step.condition.as_deref(),
            step.working_directory.as_deref(),
            step.continue_on_error.as_deref(),
        ] {
            add(&mut total, value);
        }
        add_map(&mut total, &step.with);
        add_map(&mut total, &step.env);
    }
    total
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
    const REUSE_SHA: &str = "676e2d560c9a403aa252096d99fcab3e1132b0f5";

    #[test]
    fn case_distinct_local_subpaths_are_not_aliases() {
        let context = workflow_context();
        let job = job(serde_json::json!([
            repo_step(
                "./.github/actions/Foo",
                "",
                Some("./.github/actions/Foo"),
                serde_json::json!({})
            ),
            repo_step(
                "./.github/actions/foo",
                "",
                Some("./.github/actions/foo"),
                serde_json::json!({})
            )
        ]));
        let source = FakeMetadataSource::new(&[
            (
                "acme/repo/.github/actions/Foo@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "runs:\n  using: node20\n  main: dist/index.js\n",
            ),
            (
                "acme/repo/.github/actions/foo@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                &format!("runs:\n  using: composite\n  steps:\n    - uses: actions/cache@{CACHE_SHA}\n      with:\n        lookup-only: invalid\n"),
            ),
        ]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        assert_eq!(source.reads(), 2);
        assert_eq!(error.field, "with.lookup-only");
    }

    #[test]
    fn duplicate_case_folded_input_names_rejected_before_admission() {
        let job = job(serde_json::json!([repo_step(
            "actions/cache",
            CACHE_SHA,
            None,
            serde_json::json!({"lookup-only": "true", "LOOKUP-ONLY": "false"})
        )]));
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job, &workflow_context(), &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "inputs");
    }

    #[test]
    fn oversized_metadata_body_rejected_before_parse() {
        let body = vec![b'x'; MAX_ACTION_METADATA_BYTES + 1];
        let error = read_bounded_metadata_body(body.as_slice(), None).unwrap_err();
        assert!(error.to_string().contains("exceeds"));
        let error =
            read_bounded_metadata_body([].as_slice(), Some(MAX_ACTION_METADATA_BYTES as u64 + 1))
                .unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn workflow_sha_is_required_for_local_metadata_identity() {
        let context = vec![(
            "github".to_string(),
            serde_json::json!({
                "repository": "acme/repo",
                "sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
            }),
        )];
        let job = job(serde_json::json!([repo_step(
            "./.github/actions/outer",
            "",
            Some("./.github/actions/outer"),
            serde_json::json!({})
        )]));
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "uses");
    }

    #[test]
    fn positive_root_local_local_remote_closure() {
        let context = workflow_context();
        let job = job(serde_json::json!([repo_step(
            "./.github/actions/outer",
            "",
            Some("./.github/actions/outer"),
            serde_json::json!({"LOOKUP_ONLY": "true"})
        )]));
        let source = FakeMetadataSource::new(&[
            (
                "acme/repo/.github/actions/outer@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "inputs:\n  lookup_only:\n    required: true\nruns:\n  using: composite\n  steps:\n    - uses: ./.github/actions/inner\n      with:\n        lookup_only: ${{ inputs.lookup_only }}\n",
            ),
            (
                "acme/repo/.github/actions/inner@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                &format!("inputs:\n  lookup_only:\n    required: true\nruns:\n  using: composite\n  steps:\n    - uses: actions/cache@{CACHE_SHA}\n      with:\n        path: target\n        key: k\n        lookup-only: ${{{{ inputs.lookup_only }}}}\n"),
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
    fn invalid_nested_capability_input_uses_caller_value() {
        let context = workflow_context();
        let job = job(serde_json::json!([repo_step(
            "./.github/actions/outer",
            "",
            Some("./.github/actions/outer"),
            serde_json::json!({"lookup_only": "invalid"})
        )]));
        let source = FakeMetadataSource::new(&[
            (
                "acme/repo/.github/actions/outer@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "inputs:\n  lookup_only:\n    default: true\nruns:\n  using: composite\n  steps:\n    - uses: ./.github/actions/inner\n      with:\n        lookup_only: ${{ inputs.lookup_only }}\n",
            ),
            (
                "acme/repo/.github/actions/inner@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                &format!("inputs:\n  lookup_only:\n    default: true\nruns:\n  using: composite\n  steps:\n    - uses: actions/cache@{CACHE_SHA}\n      with:\n        path: target\n        key: k\n        lookup-only: ${{{{ inputs.lookup_only }}}}\n"),
            ),
        ]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        assert_eq!(source.reads(), 2);
        assert_eq!(error.field, "with.lookup-only");
        assert_eq!(
            error.accepted,
            vec!["true".to_string(), "false".to_string()]
        );
    }

    #[test]
    fn repeated_local_composite_invocation_cannot_hide_inputs() {
        let context = workflow_context();
        let job = job(serde_json::json!([
            repo_step(
                "./.github/actions/outer",
                "",
                Some("./.github/actions/outer"),
                serde_json::json!({"lookup_only": "true"})
            ),
            repo_step(
                "./.github/actions/outer",
                "",
                Some("./.github/actions/outer"),
                serde_json::json!({"lookup_only": "invalid"})
            )
        ]));
        let source = FakeMetadataSource::new(&[
            (
                "acme/repo/.github/actions/outer@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "inputs:\n  lookup_only:\n    default: true\nruns:\n  using: composite\n  steps:\n    - uses: ./.github/actions/inner\n      with:\n        lookup_only: ${{ inputs.lookup_only }}\n",
            ),
            (
                "acme/repo/.github/actions/inner@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                &format!("inputs:\n  lookup_only:\n    default: true\nruns:\n  using: composite\n  steps:\n    - uses: actions/cache@{CACHE_SHA}\n      with:\n        path: target\n        key: k\n        lookup-only: ${{{{ inputs.lookup_only }}}}\n"),
            ),
        ]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        assert_eq!(source.reads(), 2);
        assert_eq!(error.field, "with.lookup-only");
    }

    #[test]
    fn runtime_dependent_capability_expression_rejected_before_rendering() {
        let context = workflow_context();
        for expression in [
            "${{ steps.probe.outputs.flag != 'true' }}",
            "${{ steps.probe.outputs.flag || 'false' }}",
            "${{ runner.os || 'false' }}",
            "${{ github.event.pull_request.draft }}",
            "${{ github.action || 'docker-container' }}",
            "${{ github['event']['pull_request']['draft'] }}",
            "${{ hashFiles('**/Cargo.lock') }}",
            "${{ format('{0}', 'false') }}",
            "${{ success() && 'true' || 'false' }}",
            "${{ failure() || cancelled() || always() }}",
        ] {
            let job = job(serde_json::json!([repo_step(
                "actions/cache",
                CACHE_SHA,
                None,
                serde_json::json!({"lookup-only": expression})
            )]));
            let source = FakeMetadataSource::new(&[]);
            let error = admit_job(&job, &context, &source).unwrap_err();
            assert_eq!(source.reads(), 0);
            assert_eq!(error.field, "with.lookup-only");
        }
    }

    #[test]
    fn ordinary_github_context_expression_is_rejected_before_rendering() {
        let context = workflow_context();
        let job = job(serde_json::json!([repo_step(
            "actions/cache",
            CACHE_SHA,
            None,
            serde_json::json!({"lookup-only": "${{ github.ref }}"})
        )]));
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "with.lookup-only");
    }

    #[test]
    fn mixed_case_native_subpath_is_rejected_before_metadata_fetch() {
        let context = workflow_context();
        let job = job(serde_json::json!([repo_step(
            "actions/cache",
            CACHE_SHA,
            Some("Restore"),
            serde_json::json!({"key": "k", "path": "target"})
        )]));
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "path");
    }

    #[test]
    fn malformed_local_workflow_ref_rejected_before_metadata_fetch() {
        let context = vec![(
            "github".to_string(),
            serde_json::json!({
                "repository": "acme/repo",
                "workflow_sha": "refs/heads/main"
            }),
        )];
        let job = job(serde_json::json!([repo_step(
            "./.github/actions/outer",
            "",
            Some("./.github/actions/outer"),
            serde_json::json!({})
        )]));
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job, &context, &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "ref");
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
    fn remote_action_runtime_must_match_manifest_dispatch_class() {
        let job = job(serde_json::json!([repo_step(
            "fsfe/reuse-action",
            REUSE_SHA,
            None,
            serde_json::json!({})
        )]));
        let source = FakeMetadataSource::new(&[(
            &format!("fsfe/reuse-action@{REUSE_SHA}"),
            "runs:\n  using: composite\n  steps: []\n",
        )]);
        let error = admit_job(&job, &workflow_context(), &source).unwrap_err();
        assert_eq!(source.reads(), 1);
        assert_eq!(error.field, "runtime");
        assert_eq!(error.accepted, vec!["docker"]);
    }

    #[test]
    fn remote_javascript_action_is_admitted_as_a_leaf() {
        let action_ref = "adc5c234c02592f7edd008bf81d5bc0e9584dc03";
        let repository = "jdx/mr-boxington-action";
        let job = job(serde_json::json!([repo_step(
            repository,
            action_ref,
            None,
            serde_json::json!({"backend": "github"})
        )]));
        let source = FakeMetadataSource::new(&[(
            &format!("{repository}@{action_ref}"),
            "runs:\n  using: node24\n  main: dist/index.js\n",
        )]);

        let graph = admit_job(&job, &workflow_context(), &source).unwrap();

        assert_eq!(source.reads(), 1);
        let node = graph
            .nodes
            .iter()
            .find(|node| node.identity.repository == repository)
            .expect("JavaScript action should be in the admission graph");
        assert_eq!(node.kind, AdmissionNodeKind::RemoteAction);
    }

    #[test]
    fn absolute_remote_subpath_rejected_before_metadata_fetch() {
        let job = job(serde_json::json!([repo_step(
            "actions/cache",
            CACHE_SHA,
            Some("/restore"),
            serde_json::json!({"path": "target", "key": "k"})
        )]));
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job, &workflow_context(), &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "path");
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
    fn reusable_workflow_missing_top_level_identity_rejected() {
        let publish_sha = "80a1acd07257a23b441c546e6fcad12239ef7626";
        let context = vec![(
            "github".to_string(),
            serde_json::json!({
                "job_workflow_ref": format!("jackin-project/jackin-role-action/.github/workflows/publish.yml@{publish_sha}"),
                "job_workflow_sha": publish_sha,
                "repository": "acme/repo",
                "workflow_sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
            }),
        )];
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job(serde_json::json!([])), &context, &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "github.workflow_ref");
    }

    #[test]
    fn reusable_workflow_missing_resolved_sha_rejected() {
        let publish_sha = "80a1acd07257a23b441c546e6fcad12239ef7626";
        let context = vec![(
            "github".to_string(),
            serde_json::json!({
                "job_workflow_ref": format!("jackin-project/jackin-role-action/.github/workflows/publish.yml@{publish_sha}"),
                "workflow_ref": "acme/repo/.github/workflows/ci.yml@refs/heads/main",
                "workflow_sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "repository": "acme/repo"
            }),
        )];
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job(serde_json::json!([])), &context, &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "github.job_workflow_sha");
    }

    #[test]
    fn reusable_workflow_malformed_identity_rejected() {
        let context = vec![(
            "github".to_string(),
            serde_json::json!({
                "job_workflow_ref": "not-a-workflow-ref",
                "workflow_ref": "acme/repo/.github/workflows/ci.yml@refs/heads/main",
                "workflow_sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "repository": "acme/repo"
            }),
        )];
        let source = FakeMetadataSource::new(&[]);
        let error = admit_job(&job(serde_json::json!([])), &context, &source).unwrap_err();
        assert_eq!(source.reads(), 0);
        assert_eq!(error.field, "github.job_workflow_ref");
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
        // the input-sensitive expansion guard rather than recursing forever.
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

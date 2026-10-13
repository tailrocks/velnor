//! Producer-owned inputs for the typed G0 model/workload bindings.
//!
//! GitHub's API cannot provide the effective local agent configuration or a
//! generated workload matrix.  This module captures those inputs at their
//! actual boundaries:
//!
//! * model configuration is read twice through one no-follow file descriptor;
//! * workload bytes are derived from the live, source-bound workflow/scanner
//!   ledger and reviewed manifest;
//! * both typed values are serialized into raw objects and reopened through
//!   the strict mapping wrappers.
//!
//! This is observation of configuration, not a runtime-model attestation.  A
//! runner checkout remains a separate proof obligation and is reported as
//! unavailable until a run/job/attempt-bound artifact exists.

use super::g0_mapping::{CapturedModelSession, CapturedWorkloadArtifact};
use super::live_collector::{LiveCollection, LiveDependency, LiveWorkflow};
use super::{
    content_addressed_storage_ref, sha256_digest, RawObject, RawObjectRef, RawObjectStore,
};
use crate::evidence_check::{ExpectedJobSpec, ManifestDocument, WorkloadPlatform};
use crate::g0_contract::{G0AgentModel, G0ArtifactReference, G0ModelSession};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAX_MODEL_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LocalFdObservation {
    pub source_path: String,
    pub device: u64,
    pub inode: u64,
    pub links: u64,
    pub byte_length: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RawEvidenceSummary {
    pub raw_id: String,
    pub request_id: String,
    pub object_kind: String,
    pub sha256: String,
    pub byte_length: u64,
    pub original_sha256: String,
    pub original_byte_length: u64,
    pub storage_ref: String,
    pub original_storage_ref: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckoutProofStatus {
    pub status: &'static str,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BindingCaptureReport {
    pub schema_version: u32,
    pub observed_at_utc: String,
    pub model_config: LocalFdObservation,
    pub model_session: RawEvidenceSummary,
    pub workload_artifact: RawEvidenceSummary,
    pub workload_source: RawEvidenceSummary,
    pub workload_input_sha256: String,
    pub workflow_count: usize,
    pub scanner_dependency_count: usize,
    pub checkout_proof: CheckoutProofStatus,
    /// Safe references for the separate local-binding ledger. These are not
    /// inserted into the GitHub request ledger: their request IDs are local
    /// FD/derivation observations, not provider API request identities.
    pub raw_objects: Vec<RawObjectRef>,
}

/// Concrete producer result.  The wrappers are constructed only after the
/// store returns and verifies their raw references, so a future live adapter
/// can pass these values directly to `G0MappingBindings`.
#[derive(Debug)]
pub(crate) struct ProducedBindings {
    pub model_session: CapturedModelSession,
    pub workload_artifact: CapturedWorkloadArtifact,
    pub raw_objects: Vec<RawObjectRef>,
    pub report: BindingCaptureReport,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelConfigDocument {
    session_id: String,
    effective: bool,
    orchestrator_model: String,
    orchestrator_effort: String,
    agents: Vec<ModelAgentDocument>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelAgentDocument {
    agent_id: String,
    model: String,
    effort: String,
    effective: bool,
}

#[derive(Debug, Clone, Serialize)]
struct WorkloadDerivationDocument {
    schema_version: u32,
    manifest_id: String,
    reviewed_source: ReviewedSourceDocument,
    repositories: Vec<WorkloadRepositoryDocument>,
}

#[derive(Debug, Clone, Serialize)]
struct ReviewedSourceDocument {
    repository: String,
    revision: String,
    digest: String,
    reviewed_by: String,
}

#[derive(Debug, Clone, Serialize)]
struct WorkloadRepositoryDocument {
    repository: String,
    expected_workload_ids: Vec<String>,
    workload_platform_architecture: Vec<WorkloadPlatform>,
    expected_jobs: Vec<ExpectedJobSpec>,
    workflow_path: String,
    workflow_revision: String,
    workflows: Vec<WorkflowDocument>,
}

#[derive(Debug, Clone, Serialize)]
struct WorkflowDocument {
    path: String,
    revision: String,
    source_sha: String,
    source_url: String,
    source_digest: String,
    source_raw_object_refs: Vec<String>,
    events: Vec<String>,
    source_jobs: Vec<super::live_collector::LiveSourceJob>,
    reusable_workflows: Vec<DependencyDocument>,
    actions: Vec<DependencyDocument>,
    scanners: Vec<DependencyDocument>,
    raw_object_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DependencyDocument {
    kind: String,
    repository: String,
    path: String,
    revision: String,
    resolved_path: Option<String>,
    source_url: Option<String>,
    source_digest: String,
    source_raw_object_refs: Vec<String>,
    raw_object_refs: Vec<String>,
}

struct PreparedModel {
    raw: RawObject,
    observation: LocalFdObservation,
}

struct PreparedWorkload {
    source_raw: RawObject,
    artifact_raw: RawObject,
    source_digest: String,
    workflow_count: usize,
    scanner_dependency_count: usize,
}

/// Capture effective model configuration and derive the workload artifact in
/// one producer-owned operation.  The caller must append `raw_objects` to the
/// live collection before mapping it into G0 evidence.
pub(crate) fn capture_bindings<S: RawObjectStore>(
    store: &mut S,
    live: &LiveCollection,
    manifest: &ManifestDocument,
    model_config_path: impl Into<PathBuf>,
    observed_at_utc: &str,
) -> Result<ProducedBindings> {
    if observed_at_utc.trim().is_empty() {
        bail!("binding capture requires observed_at_utc");
    }
    let prepared_model = prepare_model(model_config_path.into())?;
    let prepared_workload = prepare_workload(live, manifest, observed_at_utc)?;

    let model_ref = store_verified(store, prepared_model.raw)?;
    let source_ref = store_verified(store, prepared_workload.source_raw)?;
    let artifact_ref = store_verified(store, prepared_workload.artifact_raw)?;

    let model_session = CapturedModelSession::from_raw_object(&model_ref)
        .context("reopen captured model session")?;
    let workload_artifact = CapturedWorkloadArtifact::from_raw_object(&artifact_ref)
        .context("reopen derived workload artifact")?;
    let model_summary = summarize_raw(&model_ref);
    let artifact_summary = summarize_raw(&artifact_ref);
    let source_summary = summarize_raw(&source_ref);
    let raw_objects = vec![model_ref, source_ref, artifact_ref];
    let report = BindingCaptureReport {
        schema_version: 1,
        observed_at_utc: observed_at_utc.to_owned(),
        model_config: prepared_model.observation,
        model_session: model_summary,
        workload_artifact: artifact_summary,
        workload_source: source_summary,
        workload_input_sha256: prepared_workload.source_digest,
        workflow_count: prepared_workload.workflow_count,
        scanner_dependency_count: prepared_workload.scanner_dependency_count,
        checkout_proof: CheckoutProofStatus {
            status: "unavailable",
            reason: "GitHub API head_sha is not a runner checkout attestation; no run/job/attempt-bound checkout artifact is captured",
        },
        raw_objects: raw_objects.clone(),
    };
    Ok(ProducedBindings {
        model_session,
        workload_artifact,
        raw_objects,
        report,
    })
}

fn prepare_model(path: PathBuf) -> Result<PreparedModel> {
    let (bytes, observation) = read_observed_file(&path)?;
    let value: ModelConfigDocument = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse observed model config {}", path.display()))?;
    reject_secret_keys(&serde_json::from_slice::<Value>(&bytes)?, &path)?;
    if value.session_id.trim().is_empty()
        || value.orchestrator_model.trim().is_empty()
        || value.orchestrator_effort.trim().is_empty()
        || value.agents.is_empty()
    {
        bail!("observed model config lacks effective session fields");
    }
    if value.agents.iter().any(|agent| {
        agent.agent_id.trim().is_empty()
            || agent.model.trim().is_empty()
            || agent.effort.trim().is_empty()
    }) {
        bail!("observed model config contains an incomplete agent");
    }
    let digest_suffix = digest_suffix(&observation.sha256);
    let raw_id = format!("model-session-{digest_suffix}");
    let model = G0ModelSession {
        session_id: value.session_id,
        effective: value.effective,
        orchestrator_model: value.orchestrator_model,
        orchestrator_effort: value.orchestrator_effort,
        agents: value
            .agents
            .into_iter()
            .map(|agent| G0AgentModel {
                agent_id: agent.agent_id,
                model: agent.model,
                effort: agent.effort,
                effective: agent.effective,
                raw_object_refs: vec![raw_id.clone()],
            })
            .collect(),
        raw_object_refs: vec![raw_id.clone()],
    };
    let safe_bytes = serde_json::to_vec(&model).context("serialize observed model session")?;
    Ok(PreparedModel {
        raw: RawObject {
            raw_id: raw_id.clone(),
            request_id: format!("local-fd-model-{digest_suffix}"),
            object_kind: "model.session".to_owned(),
            canonicalization: "typed-json-v1".to_owned(),
            media_type: "application/json".to_owned(),
            original_bytes: bytes,
            bytes: safe_bytes,
        },
        observation,
    })
}

fn prepare_workload(
    live: &LiveCollection,
    manifest: &ManifestDocument,
    observed_at_utc: &str,
) -> Result<PreparedWorkload> {
    let manifest_repositories = manifest
        .repositories
        .iter()
        .map(|repository| (repository.repository.clone(), repository))
        .collect::<BTreeMap<_, _>>();
    if manifest_repositories.len() != manifest.repositories.len() {
        bail!("reviewed manifest repeats a repository");
    }
    let live_repositories = live
        .repositories
        .iter()
        .map(|repository| (repository.repository.clone(), repository))
        .collect::<BTreeMap<_, _>>();
    if live_repositories.len() != live.repositories.len() {
        bail!("live collection repeats a repository");
    }
    if manifest_repositories.keys().ne(live_repositories.keys()) {
        bail!("workload derivation requires exact manifest/live repository identity");
    }

    let mut repositories = Vec::with_capacity(manifest.repositories.len());
    let mut workflow_count = 0usize;
    let mut scanner_dependency_count = 0usize;
    for manifest_repository in &manifest.repositories {
        let live_repository = live_repositories
            .get(&manifest_repository.repository)
            .copied()
            .ok_or_else(|| {
                anyhow!(
                    "live repository {} is absent",
                    manifest_repository.repository
                )
            })?;
        let mut workflows = live_repository
            .workflows
            .iter()
            .map(|workflow| workflow_document(workflow, live))
            .collect::<Result<Vec<_>>>()?;
        workflows.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.revision.cmp(&right.revision))
        });
        workflow_count += workflows.len();
        scanner_dependency_count += workflows
            .iter()
            .map(|workflow| workflow.scanners.len())
            .sum::<usize>();
        if !manifest_repository.workflow_path.trim().is_empty()
            && !workflows.iter().any(|workflow| {
                workflow.path == manifest_repository.workflow_path
                    && workflow.revision == manifest_repository.workflow_revision
            })
        {
            bail!(
                "reviewed workflow {}/{}@{} is absent from exact live source ledger",
                manifest_repository.repository,
                manifest_repository.workflow_path,
                manifest_repository.workflow_revision
            );
        }
        repositories.push(WorkloadRepositoryDocument {
            repository: manifest_repository.repository.clone(),
            expected_workload_ids: manifest_repository.expected_workload_ids.clone(),
            workload_platform_architecture: manifest_repository
                .workload_platform_architecture
                .clone(),
            expected_jobs: manifest_repository.expected_jobs.clone(),
            workflow_path: manifest_repository.workflow_path.clone(),
            workflow_revision: manifest_repository.workflow_revision.clone(),
            workflows,
        });
    }
    let document = WorkloadDerivationDocument {
        schema_version: 1,
        manifest_id: manifest.manifest_id.clone(),
        reviewed_source: ReviewedSourceDocument {
            repository: manifest.source.repository.clone(),
            revision: manifest.source.revision.clone(),
            digest: manifest.source.digest.clone(),
            reviewed_by: manifest.source.reviewed_by.clone(),
        },
        repositories,
    };
    let source_bytes = serde_json::to_vec(&document).context("serialize workload derivation")?;
    let source_digest = sha256_digest(&source_bytes);
    let source_suffix = digest_suffix(&source_digest);
    let source_raw_id = format!("workload-source-{source_suffix}");
    let source_raw = RawObject {
        raw_id: source_raw_id.clone(),
        request_id: format!("local-workload-source-{source_suffix}"),
        object_kind: "workload.source".to_owned(),
        canonicalization: "canonical-json-v1".to_owned(),
        media_type: "application/json".to_owned(),
        original_bytes: source_bytes.clone(),
        bytes: source_bytes,
    };
    let artifact_raw_id = format!(
        "workload-artifact-{}-{}",
        digest_suffix(&sha256_digest(live.snapshot_id.as_bytes())),
        source_suffix
    );
    let artifact = G0ArtifactReference {
        name: "github-first-dual-lane-workload".to_owned(),
        schema: "github-first-dual-lane.workload-artifact.v1".to_owned(),
        source_url: format!(
            "https://github.com/{}/tree/{}",
            manifest.source.repository, manifest.source.revision
        ),
        sha256: source_digest.clone(),
        storage_ref: content_addressed_storage_ref(&source_digest),
        source_revision: manifest.source.revision.clone(),
        source_digest: source_digest.clone(),
        observed_at_utc: observed_at_utc.to_owned(),
        raw_object_refs: vec![source_raw_id, artifact_raw_id.clone()],
    };
    let artifact_bytes = serde_json::to_vec(&artifact).context("serialize workload artifact")?;
    let artifact_raw = RawObject {
        raw_id: artifact_raw_id,
        request_id: format!("local-workload-artifact-{source_suffix}"),
        object_kind: "workload.artifact".to_owned(),
        canonicalization: "typed-json-v1".to_owned(),
        media_type: "application/json".to_owned(),
        original_bytes: artifact_bytes.clone(),
        bytes: artifact_bytes,
    };
    Ok(PreparedWorkload {
        source_raw,
        artifact_raw,
        source_digest,
        workflow_count,
        scanner_dependency_count,
    })
}

fn workflow_document(workflow: &LiveWorkflow, live: &LiveCollection) -> Result<WorkflowDocument> {
    let source_bytes = BASE64
        .decode(&workflow.source_bytes_base64)
        .context("decode exact workflow source")?;
    if source_bytes.is_empty() || workflow.source_raw_object_refs.is_empty() {
        bail!(
            "workflow {} lacks exact source bytes/raw refs",
            workflow.path
        );
    }
    let source_digest = sha256_digest(&source_bytes);
    require_raw_source(
        live,
        &workflow.source_raw_object_refs,
        "workflow.source",
        &source_digest,
        &workflow.source_bytes_base64,
    )?;
    let reusable_workflows = workflow
        .reusable_workflows
        .iter()
        .map(|dependency| dependency_document(dependency, live))
        .collect::<Result<Vec<_>>>()?;
    let actions = workflow
        .actions
        .iter()
        .map(|dependency| dependency_document(dependency, live))
        .collect::<Result<Vec<_>>>()?;
    let scanners = workflow
        .scanners
        .iter()
        .map(|dependency| dependency_document(dependency, live))
        .collect::<Result<Vec<_>>>()?;
    for source_job in &workflow.source_jobs {
        if source_job.raw_object_refs.is_empty() {
            bail!(
                "workflow source job {} lacks source raw refs",
                source_job.job_id
            );
        }
        require_raw_ids(live, &source_job.raw_object_refs, "workflow.source")?;
    }
    Ok(WorkflowDocument {
        path: workflow.path.clone(),
        revision: workflow.revision.clone(),
        source_sha: workflow.source_sha.clone(),
        source_url: workflow.source_url.clone(),
        source_digest,
        source_raw_object_refs: workflow.source_raw_object_refs.clone(),
        events: workflow.events.clone(),
        source_jobs: workflow.source_jobs.clone(),
        reusable_workflows,
        actions,
        scanners,
        raw_object_refs: workflow.raw_object_refs.clone(),
    })
}

fn dependency_document(
    dependency: &LiveDependency,
    live: &LiveCollection,
) -> Result<DependencyDocument> {
    let bytes_base64 = dependency.source_bytes_base64.as_deref().ok_or_else(|| {
        anyhow!(
            "dependency {}/{} lacks exact source bytes",
            dependency.repository,
            dependency.path
        )
    })?;
    let bytes = BASE64
        .decode(bytes_base64)
        .context("decode dependency source")?;
    if bytes.is_empty() || dependency.source_raw_object_refs.is_empty() {
        bail!(
            "dependency {}/{} lacks exact source evidence",
            dependency.repository,
            dependency.path
        );
    }
    let source_digest = sha256_digest(&bytes);
    require_raw_source(
        live,
        &dependency.source_raw_object_refs,
        "workflow.dependency.source",
        &source_digest,
        bytes_base64,
    )?;
    require_raw_ids(live, &dependency.raw_object_refs, "workflow.dependency")?;
    Ok(DependencyDocument {
        kind: dependency.kind.clone(),
        repository: dependency.repository.clone(),
        path: dependency.path.clone(),
        revision: dependency.revision.clone(),
        resolved_path: dependency.resolved_path.clone(),
        source_url: dependency.source_url.clone(),
        source_digest,
        source_raw_object_refs: dependency.source_raw_object_refs.clone(),
        raw_object_refs: dependency.raw_object_refs.clone(),
    })
}

fn require_raw_source(
    live: &LiveCollection,
    raw_ids: &[String],
    kind: &str,
    digest: &str,
    bytes_base64: &str,
) -> Result<()> {
    if !raw_ids.iter().any(|raw_id| {
        live.raw_objects.iter().any(|raw| {
            raw.raw_id == *raw_id
                && raw.object_kind == kind
                && raw.sha256 == digest
                && raw.bytes_base64 == bytes_base64
        })
    }) {
        bail!("{kind} source bytes are not bound to a live raw object");
    }
    Ok(())
}

fn require_raw_ids(live: &LiveCollection, raw_ids: &[String], kind: &str) -> Result<()> {
    if raw_ids.is_empty()
        || raw_ids.iter().any(|raw_id| {
            live.raw_objects
                .iter()
                .all(|raw| raw.raw_id != *raw_id || raw.object_kind != kind)
        })
    {
        bail!("{kind} references are missing from the live raw ledger");
    }
    Ok(())
}

fn store_verified<S: RawObjectStore>(store: &mut S, object: RawObject) -> Result<RawObjectRef> {
    let expected_raw_id = object.raw_id.clone();
    let expected_request_id = object.request_id.clone();
    let expected_kind = object.object_kind.clone();
    let expected_original_digest = sha256_digest(&object.original_bytes);
    let expected_original_length = object.original_bytes.len() as u64;
    let expected_safe_digest = sha256_digest(&object.bytes);
    let expected_safe_length = object.bytes.len() as u64;
    let reference = store
        .store(object)
        .map_err(|error| anyhow!("store binding raw object: {error:?}"))?;
    if reference.raw_id != expected_raw_id
        || reference.request_id != expected_request_id
        || reference.object_kind != expected_kind
        || reference.original_sha256 != expected_original_digest
        || reference.original_byte_length != expected_original_length
        || reference.sha256 != expected_safe_digest
        || reference.byte_length != expected_safe_length
    {
        bail!("binding raw store returned mismatched measured provenance");
    }
    store
        .verify(&reference)
        .map_err(|error| anyhow!("verify binding raw object: {error:?}"))?;
    Ok(reference)
}

fn summarize_raw(reference: &RawObjectRef) -> RawEvidenceSummary {
    RawEvidenceSummary {
        raw_id: reference.raw_id.clone(),
        request_id: reference.request_id.clone(),
        object_kind: reference.object_kind.clone(),
        sha256: reference.sha256.clone(),
        byte_length: reference.byte_length,
        original_sha256: reference.original_sha256.clone(),
        original_byte_length: reference.original_byte_length,
        storage_ref: reference.storage_ref.clone(),
        original_storage_ref: reference.original_storage_ref.clone(),
    }
}

fn read_observed_file(path: &Path) -> Result<(Vec<u8>, LocalFdObservation)> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("open model config through observed FD {}", path.display()))?;
    let before = file.metadata().context("stat model config FD")?;
    if !before.file_type().is_file() || file_nlinks(&before) != 1 {
        bail!("model config FD is not a regular single-link file");
    }
    let first = read_bounded(&mut file)?;
    file.seek(SeekFrom::Start(0))
        .context("rewind model config FD")?;
    let second = read_bounded(&mut file)?;
    let after = file.metadata().context("restat model config FD")?;
    if first != second || file_identity(&before) != file_identity(&after) {
        bail!("model config changed while captured through its FD");
    }
    if first.len() as u64 != file_size(&after) {
        bail!("model config FD size differs from bytes read");
    }
    let sha256 = sha256_digest(&first);
    Ok((
        first,
        LocalFdObservation {
            source_path: path.to_string_lossy().into_owned(),
            device: file_device(&after),
            inode: file_inode(&after),
            links: file_nlinks(&after),
            byte_length: file_size(&after),
            sha256,
        },
    ))
}

fn read_bounded(file: &mut File) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take((MAX_MODEL_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("read model config FD")?;
    if bytes.len() > MAX_MODEL_CONFIG_BYTES {
        bail!("model config exceeds {} bytes", MAX_MODEL_CONFIG_BYTES);
    }
    Ok(bytes)
}

fn reject_secret_keys(value: &Value, path: &Path) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let normalized = key.to_ascii_lowercase();
                if [
                    "token",
                    "secret",
                    "password",
                    "authorization",
                    "private_key",
                ]
                .iter()
                .any(|part| normalized.contains(part))
                {
                    bail!(
                        "model config {} contains secret-bearing key {}",
                        path.display(),
                        key
                    );
                }
                reject_secret_keys(child, path)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                reject_secret_keys(child, path)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn digest_suffix(digest: &str) -> String {
    digest
        .strip_prefix("sha256:")
        .unwrap_or(digest)
        .chars()
        .take(32)
        .collect()
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> (u64, u64, u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.nlink(),
        metadata.len(),
    )
}

#[cfg(not(unix))]
fn file_identity(metadata: &std::fs::Metadata) -> (u64, u64, u64, u64) {
    (0, 0, 1, metadata.len())
}

fn file_device(metadata: &std::fs::Metadata) -> u64 {
    file_identity(metadata).0
}

fn file_inode(metadata: &std::fs::Metadata) -> u64 {
    file_identity(metadata).1
}

fn file_nlinks(metadata: &std::fs::Metadata) -> u64 {
    file_identity(metadata).2
}

fn file_size(metadata: &std::fs::Metadata) -> u64 {
    file_identity(metadata).3
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::evidence_check::ManifestRepository;
    use crate::github_acquisition::content_addressed_storage_ref;
    use crate::github_acquisition::live_collector::LiveRepository;
    use crate::github_acquisition::RawStorageError;
    use std::collections::BTreeSet;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct MemoryStore {
        references: Vec<RawObjectRef>,
    }

    impl RawObjectStore for MemoryStore {
        fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError> {
            let safe = sha256_digest(&object.bytes);
            let original = sha256_digest(&object.original_bytes);
            let reference = RawObjectRef {
                raw_id: object.raw_id,
                request_id: object.request_id,
                object_kind: object.object_kind,
                canonicalization: object.canonicalization,
                sha256: safe.clone(),
                byte_length: object.bytes.len() as u64,
                original_sha256: original.clone(),
                original_byte_length: object.original_bytes.len() as u64,
                bytes_base64: BASE64.encode(object.bytes),
                media_type: object.media_type,
                storage_ref: content_addressed_storage_ref(&safe),
                original_storage_ref: content_addressed_storage_ref(&original),
            };
            self.references.push(reference.clone());
            Ok(reference)
        }

        fn verify(&self, reference: &RawObjectRef) -> Result<(), RawStorageError> {
            self.references
                .contains(reference)
                .then_some(())
                .ok_or(RawStorageError::Unbound)
        }
    }

    fn temporary_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("velnor-binding-{name}-{nanos}.json"))
    }

    #[test]
    fn model_capture_uses_fd_bytes_and_rejects_secret_fields() {
        let path = temporary_path("model");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "session_id": "session-1",
                "effective": true,
                "orchestrator_model": "gpt-6-astra",
                "orchestrator_effort": "low",
                "agents": [{
                    "agent_id": "agent-1",
                    "model": "gpt-5.6-luna",
                    "effort": "max",
                    "effective": true
                }]
            }))
            .expect("model JSON"),
        )
        .expect("write model config");
        let mut store = MemoryStore::default();
        let prepared = prepare_model(path.clone()).expect("prepare model");
        let reference = store_verified(&mut store, prepared.raw).expect("store model");
        let _captured = CapturedModelSession::from_raw_object(&reference).expect("capture model");
        assert_eq!(
            prepared.observation.byte_length,
            reference.original_byte_length
        );
        fs::remove_file(path).expect("remove model config");

        let secret_path = temporary_path("secret");
        fs::write(
            &secret_path,
            br#"{"session_id":"x","effective":true,"orchestrator_model":"gpt-6-astra","orchestrator_effort":"low","agents":[],"token":"no"}"#,
        )
        .expect("write secret config");
        assert!(prepare_model(secret_path.clone()).is_err());
        fs::remove_file(secret_path).expect("remove secret config");
    }

    #[test]
    fn workload_derivation_requires_source_bound_scanner_objects() {
        let source = b"name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu\n";
        let source_ref = raw_reference("workflow-source", "workflow.source", source.to_vec());
        let workflow = super::super::live_collector::LiveWorkflow {
            path: ".github/workflows/ci.yml".to_owned(),
            revision: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            source_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            source_url: "https://api.github.com/repos/acme/repo/contents/.github/workflows/ci.yml?ref=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            source_bytes_base64: BASE64.encode(source),
            source_raw_object_refs: vec![source_ref.raw_id.clone()],
            events: vec!["push".to_owned()],
            source_jobs: Vec::new(),
            reusable_workflows: Vec::new(),
            actions: Vec::new(),
            scanners: Vec::new(),
            raw_object_refs: vec![source_ref.raw_id.clone()],
        };
        let manifest_repository = ManifestRepository {
            repository: "acme/repo".to_owned(),
            workflow_path: workflow.path.clone(),
            workflow_revision: workflow.revision.clone(),
            ..ManifestRepository::default()
        };
        let manifest = ManifestDocument {
            schema_version: 1,
            manifest_id: "manifest".to_owned(),
            source: crate::evidence_check::SourceIdentity {
                repository: "acme/repo".to_owned(),
                revision: "cccccccccccccccccccccccccccccccccccccccc".to_owned(),
                digest: "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
                    .to_owned(),
                reviewed_by: "reviewer".to_owned(),
            },
            repositories: vec![manifest_repository],
        };
        let live = LiveCollection {
            schema_version: 1,
            manifest_id: "manifest".to_owned(),
            snapshot_id: "snapshot".to_owned(),
            observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
            auth: super::super::AuthIdentity::new(
                "github-viewer",
                "github",
                Some("1".to_owned()),
                Some("viewer".to_owned()),
                BTreeSet::from(["actions:read".to_owned()]),
            ),
            requests: Vec::new(),
            raw_objects: vec![source_ref],
            repositories: vec![LiveRepository {
                repository: "acme/repo".to_owned(),
                repository_id: 1,
                default_branch: "main".to_owned(),
                default_branch_sha: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_owned(),
                rulesets: Vec::new(),
                workflows: vec![workflow],
                open_prs: Vec::new(),
                artifacts: Vec::new(),
                main_executions: Vec::new(),
                main_checks: Vec::new(),
                closing_repository_id: 1,
                closing_default_branch: "main".to_owned(),
                closing_default_branch_sha: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_owned(),
                source_invalidated: false,
                access_state: "unknown".to_owned(),
                access_gaps: vec!["test".to_owned()],
                raw_object_refs: Vec::new(),
            }],
            opening_prs: Vec::new(),
            closing_prs: Vec::new(),
            reconciliation: super::super::IdentityReconciliation {
                opening: Vec::new(),
                closing: Vec::new(),
                changes: Vec::new(),
                duplicate_keys: Vec::new(),
                stable: true,
            },
        };
        let prepared =
            prepare_workload(&live, &manifest, "2026-09-20T00:00:00Z").expect("derive workload");
        assert_eq!(prepared.workflow_count, 1);
        assert_eq!(prepared.scanner_dependency_count, 0);
        let artifact: G0ArtifactReference =
            serde_json::from_slice(&prepared.artifact_raw.bytes).expect("artifact JSON");
        assert_eq!(artifact.raw_object_refs.len(), 2);
        assert_eq!(
            artifact.storage_ref,
            content_addressed_storage_ref(&prepared.source_digest)
        );

        let mut broken = live;
        broken.repositories[0].workflows[0]
            .source_raw_object_refs
            .clear();
        assert!(prepare_workload(&broken, &manifest, "2026-09-20T00:00:00Z").is_err());
    }

    fn raw_reference(raw_id: &str, kind: &str, bytes: Vec<u8>) -> RawObjectRef {
        let digest = sha256_digest(&bytes);
        RawObjectRef {
            raw_id: raw_id.to_owned(),
            request_id: format!("request-{raw_id}"),
            object_kind: kind.to_owned(),
            canonicalization: "raw-utf8".to_owned(),
            sha256: digest.clone(),
            byte_length: bytes.len() as u64,
            original_sha256: digest.clone(),
            original_byte_length: bytes.len() as u64,
            bytes_base64: BASE64.encode(bytes),
            media_type: "text/yaml".to_owned(),
            storage_ref: content_addressed_storage_ref(&digest),
            original_storage_ref: content_addressed_storage_ref(&digest),
        }
    }
}

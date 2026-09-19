//! Deterministic validation for the GitHub-first dual-lane evidence contract.
//!
//! The checker deliberately takes the manifest, the authoritative snapshot,
//! and the evidence envelope as separate inputs. A record cannot make a
//! repository or revision current by repeating it in its own JSON.

use anyhow::{bail, Context, Result};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const REQUIRED_REPOSITORIES: usize = 32;
const SHA_LENGTH: usize = 40;
const DIGEST_LENGTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    G0,
    G1,
    G2,
    G3,
    G4,
    G5,
    G6,
    G7,
}

impl Stage {
    fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_uppercase().as_str() {
            "G0" => Ok(Self::G0),
            "G1" => Ok(Self::G1),
            "G2" => Ok(Self::G2),
            "G3" => Ok(Self::G3),
            "G4" => Ok(Self::G4),
            "G5" => Ok(Self::G5),
            "G6" => Ok(Self::G6),
            "G7" => Ok(Self::G7),
            other => bail!("stage must be one of G0, G1, G2, G3, G4, G5, G6, G7; got {other}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::G0 => "G0",
            Self::G1 => "G1",
            Self::G2 => "G2",
            Self::G3 => "G3",
            Self::G4 => "G4",
            Self::G5 => "G5",
            Self::G6 => "G6",
            Self::G7 => "G7",
        }
    }

    fn needs_execution(self) -> bool {
        self >= Self::G1
    }

    fn needs_release(self) -> bool {
        self >= Self::G2
    }

    fn needs_velnor(self) -> bool {
        matches!(self, Self::G4 | Self::G5)
    }

    fn needs_both_lanes(self) -> bool {
        self >= Self::G6
    }

    fn needs_independent_review(self) -> bool {
        self >= Self::G7
    }
}

#[derive(Debug, Args)]
pub struct EvidenceCheckArgs {
    /// Gate to validate. The gate is explicit so a partial fixture cannot be
    /// reported as a final G7 audit.
    #[arg(long, alias = "gate")]
    pub stage: String,
    /// External exact-scope repository manifest.
    #[arg(long, alias = "manifest-config", alias = "fleet-manifest")]
    pub manifest: PathBuf,
    /// Independently captured default-branch and open-PR snapshot.
    #[arg(long, alias = "authoritative-snapshot")]
    pub snapshot: PathBuf,
    /// Evidence envelope containing records for the snapshot.
    #[arg(long, alias = "records")]
    pub evidence: PathBuf,
    /// Emit the stable JSON report instead of human-readable findings.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct EvidenceCheckInput {
    pub stage: String,
    pub manifest: PathBuf,
    pub snapshot: PathBuf,
    pub evidence: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub schema_version: u32,
    pub stage: String,
    pub status: &'static str,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub code: String,
    pub repository: Option<String>,
    pub field: String,
    pub message: String,
}

impl Finding {
    fn new(
        code: impl Into<String>,
        repository: Option<&str>,
        field: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            repository: repository.map(str::to_owned),
            field: field.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ManifestDocument {
    schema_version: u32,
    #[serde(default)]
    manifest_id: String,
    repositories: Vec<ManifestRepository>,
}

#[derive(Debug, Deserialize)]
struct RawManifestDocument {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    manifest_version: Option<u32>,
    #[serde(default)]
    manifest_id: Option<String>,
    #[serde(default)]
    schema: Option<String>,
    repositories: Vec<RawManifestRepository>,
}

#[derive(Debug, Deserialize)]
struct RawManifestRepository {
    #[serde(alias = "name", default)]
    repository: Option<String>,
    #[serde(default)]
    repository_role: Option<String>,
    #[serde(default)]
    default_branch: Option<String>,
    #[serde(alias = "expected_workloads", alias = "workloads", default)]
    expected_workload_ids: Option<Vec<String>>,
    #[serde(alias = "required_checks", default)]
    required_check_contexts_and_apps: Option<Vec<RequiredContext>>,
    #[serde(alias = "workload_platforms", default)]
    workload_platform_architecture: Option<Vec<WorkloadPlatform>>,
    #[serde(default)]
    provider_eligibility: Option<BTreeMap<String, Eligibility>>,
    #[serde(default)]
    release_applicability: Option<Applicability>,
    #[serde(default)]
    generator_revision: Option<String>,
    #[serde(default)]
    runtime_product_id: Option<String>,
    #[serde(default)]
    generator_artifact_digest: Option<String>,
    #[serde(default)]
    configuration_digest: Option<String>,
    #[serde(default)]
    generated_tree_digest: Option<String>,
    #[serde(default)]
    scan_state_digest: Option<String>,
    #[serde(default)]
    runtime_release_version: Option<String>,
    #[serde(default)]
    runtime_source_sha: Option<String>,
    #[serde(default)]
    job_image_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ManifestRepository {
    #[serde(alias = "name")]
    repository: String,
    #[serde(default)]
    repository_role: String,
    default_branch: String,
    #[serde(alias = "expected_workloads", alias = "workloads")]
    expected_workload_ids: Vec<String>,
    #[serde(alias = "required_checks")]
    required_check_contexts_and_apps: Vec<RequiredContext>,
    #[serde(alias = "workload_platforms")]
    workload_platform_architecture: Vec<WorkloadPlatform>,
    provider_eligibility: BTreeMap<String, Eligibility>,
    #[serde(default)]
    release_applicability: Option<Applicability>,
    #[serde(default)]
    generator_revision: Option<String>,
    #[serde(default)]
    runtime_product_id: Option<String>,
    #[serde(default)]
    generator_artifact_digest: Option<String>,
    #[serde(default)]
    configuration_digest: Option<String>,
    #[serde(default)]
    generated_tree_digest: Option<String>,
    #[serde(default)]
    scan_state_digest: Option<String>,
    #[serde(default)]
    runtime_release_version: Option<String>,
    #[serde(default)]
    runtime_source_sha: Option<String>,
    #[serde(default)]
    job_image_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SnapshotDocument {
    schema_version: u32,
    observed_at_utc: String,
    #[serde(alias = "repository_snapshots", alias = "default_branch_tips")]
    repositories: Vec<SnapshotRepository>,
}

#[derive(Debug, Deserialize)]
struct RawSnapshotDocument {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    manifest_version: Option<u32>,
    #[serde(default)]
    observed_at_utc: String,
    #[serde(alias = "repository_snapshots", alias = "default_branch_tips")]
    repositories: Vec<SnapshotRepository>,
}

#[derive(Debug, Deserialize)]
struct SnapshotRepository {
    #[serde(alias = "name")]
    repository: String,
    default_branch: String,
    default_branch_sha: String,
    #[serde(default)]
    open_prs: Vec<SnapshotPullRequest>,
}

#[derive(Debug, Deserialize)]
struct SnapshotPullRequest {
    number: u64,
    head_sha: String,
    base_sha: String,
    #[serde(default)]
    merge_sha: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EvidenceDocument {
    schema_version: u32,
    manifest_id: String,
    snapshot_observed_at_utc: String,
    #[serde(default)]
    stage: Option<String>,
    records: Vec<EvidenceRecord>,
}

#[derive(Debug, Deserialize)]
struct EvidenceRecord {
    repository: String,
    repository_role: String,
    default_branch: String,
    default_branch_sha: String,
    observed_at_utc: String,
    generator_revision: String,
    runtime_product_id: String,
    generator_artifact_digest: String,
    configuration_digest: String,
    generated_tree_digest: String,
    scan_state_digest: String,
    runtime_release_version: String,
    runtime_source_sha: String,
    job_image_digest: String,
    expected_workload_ids: Vec<String>,
    required_check_contexts_and_apps: Vec<RequiredContext>,
    workload_platform_architecture: Vec<WorkloadPlatform>,
    provider_eligibility: BTreeMap<String, Eligibility>,
    #[serde(default)]
    justified_exclusions: Vec<String>,
    #[serde(rename = "PR_number", alias = "pr_number")]
    pr_number: Option<u64>,
    #[serde(rename = "PR_head_sha", alias = "pr_head_sha")]
    pr_head_sha: Option<String>,
    #[serde(rename = "PR_base_sha", alias = "pr_base_sha")]
    pr_base_sha: Option<String>,
    tested_merge_sha: Option<String>,
    merge_group_sha: Option<String>,
    workflow_path: Option<String>,
    workflow_revision: Option<String>,
    event: Option<String>,
    run_id: Option<u64>,
    run_attempt: Option<u32>,
    run_url: Option<String>,
    trigger_source_sha: Option<String>,
    actual_checkout_sha: Option<String>,
    provider: Option<String>,
    runner_name: Option<String>,
    host_id: Option<String>,
    #[serde(default)]
    expected_jobs: Vec<ExpectedJob>,
    #[serde(default)]
    actual_job_ids: Vec<String>,
    #[serde(default)]
    actual_job_conclusions: BTreeMap<String, String>,
    #[serde(default)]
    logs: Vec<String>,
    #[serde(default)]
    child_run_links: Vec<ChildRunLink>,
    #[serde(default)]
    required_checks: Vec<RequiredCheck>,
    #[serde(default)]
    release: Option<ReleaseEvidence>,
    #[serde(default)]
    install: Option<InstallEvidence>,
    // Flat aliases are accepted for producers that emit the section 10 field
    // list directly. The nested objects remain the documented representation.
    #[serde(default)]
    release_channel: Option<String>,
    #[serde(default)]
    release_version: Option<String>,
    #[serde(default)]
    tag_target_sha: Option<String>,
    #[serde(default)]
    release_id: Option<String>,
    #[serde(default)]
    asset_digests: Option<Value>,
    #[serde(default, alias = "APT_feed_revision_suite_and_candidate")]
    apt_feed_revision_suite_and_candidate: Option<String>,
    #[serde(default, alias = "Homebrew_tap_revision_and_formula")]
    homebrew_tap_revision_and_formula: Option<String>,
    #[serde(default)]
    install_upgrade_test_environment: Option<String>,
    #[serde(default)]
    installed_binary_identity: Option<InstalledBinaryIdentity>,
    #[serde(default)]
    functional_result: Option<String>,
    owner: String,
    reviewer: String,
    gate_status: String,
    #[serde(default)]
    blocker: Option<String>,
    #[serde(default)]
    next_action: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct RequiredContext {
    #[serde(alias = "name")]
    context: String,
    app: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct WorkloadPlatform {
    workload_id: String,
    platform: String,
    architecture: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct ExpectedJob {
    #[serde(alias = "id")]
    job_id: String,
    workload_id: String,
    provider: String,
    platform: String,
    architecture: String,
    #[serde(default)]
    child_run_id: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChildRunLink {
    run_id: Option<u64>,
    run_url: Option<String>,
    repository: String,
    workflow: String,
    source_sha: String,
    provider: String,
    conclusion: String,
    #[serde(default)]
    run_attempt: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct RequiredCheck {
    context: String,
    app: String,
    job_id: String,
    conclusion: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseEvidence {
    applicability: Applicability,
    #[serde(default)]
    justification: Option<String>,
    #[serde(default)]
    release_channel: Option<String>,
    #[serde(default)]
    release_version: Option<String>,
    #[serde(default)]
    tag_target_sha: Option<String>,
    #[serde(default)]
    release_id: Option<String>,
    #[serde(default)]
    asset_digests: Option<Value>,
    #[serde(default, alias = "APT_feed_revision_suite_and_candidate")]
    apt_feed_revision_suite_and_candidate: Option<String>,
    #[serde(default, alias = "Homebrew_tap_revision_and_formula")]
    homebrew_tap_revision_and_formula: Option<String>,
    #[serde(default)]
    manifest: Option<ReleaseManifestEvidence>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallEvidence {
    applicability: Applicability,
    #[serde(default)]
    justification: Option<String>,
    #[serde(default)]
    install_upgrade_test_environment: Option<InstallEnvironmentEvidence>,
    #[serde(default)]
    installed_binary_identity: Option<InstalledBinaryIdentity>,
    #[serde(default)]
    upgrade_from: Option<String>,
    #[serde(default)]
    switch_from: Option<String>,
    #[serde(default)]
    service_manager_result: Option<String>,
    #[serde(default)]
    functional_result: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseManifestEvidence {
    schema: String,
    product_id: String,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    source_repository: Option<String>,
    source_ref: String,
    source_commit: String,
    #[serde(default)]
    release_tag: Option<String>,
    #[serde(default)]
    release_id: Option<String>,
    manifest_sha256: String,
    #[serde(default)]
    artifacts: Vec<ReleaseArtifact>,
    #[serde(default)]
    components: Vec<ReleaseComponent>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseArtifact {
    name: String,
    target: String,
    kind: String,
    sha256: String,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseComponent {
    name: String,
    #[serde(rename = "crate")]
    crate_name: String,
    version: String,
    binary: String,
    targets: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum InstallEnvironmentEvidence {
    Description(String),
    Structured(InstallEnvironment),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallEnvironment {
    os_image: String,
    platform: String,
    architecture: String,
    runner: String,
    workspace: String,
    path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledBinaryIdentity {
    product_id: String,
    channel: String,
    version: String,
    source_sha: String,
    manifest_sha256: String,
    binaries: Vec<InstalledBinary>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledBinary {
    name: String,
    path: String,
    sha256: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Applicability {
    Required,
    Applicable,
    NotApplicable,
    Excluded,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
enum Eligibility {
    Name(String),
    Bool(bool),
}

impl Eligibility {
    fn canonical(&self) -> &'static str {
        match self {
            Self::Name(value) if value == "eligible" => "eligible",
            Self::Name(value) if value == "not-applicable" => "not-applicable",
            Self::Name(value) if value == "excluded" => "excluded",
            Self::Name(_) => "invalid",
            Self::Bool(true) => "eligible",
            Self::Bool(false) => "not-applicable",
        }
    }
}

#[derive(Debug, Clone)]
struct RepositoryIndex<'a> {
    manifest: &'a ManifestRepository,
    snapshot: &'a SnapshotRepository,
    snapshot_observed_at_utc: &'a str,
}

pub fn evidence_check(args: EvidenceCheckArgs) -> Result<()> {
    let stage = Stage::parse(&args.stage)?;
    let report = check_paths(&EvidenceCheckInput {
        stage: stage.as_str().to_owned(),
        manifest: args.manifest,
        snapshot: args.snapshot,
        evidence: args.evidence,
    })?;

    if args.json {
        let output = serde_json::to_string_pretty(&report).context("serialize evidence report")?;
        println!("{output}");
    } else if report.findings.is_empty() {
        println!("{} evidence gate passed", report.stage);
    } else {
        for finding in &report.findings {
            let repository = finding
                .repository
                .as_deref()
                .map(|value| format!(" [{value}]"))
                .unwrap_or_default();
            println!(
                "{}{} {}: {}",
                finding.code, repository, finding.field, finding.message
            );
        }
    }

    if report.status != "pass" {
        bail!(
            "{} evidence gate failed with {} finding(s)",
            report.stage,
            report.findings.len()
        );
    }
    Ok(())
}

pub fn check_paths(input: &EvidenceCheckInput) -> Result<CheckReport> {
    let stage = Stage::parse(&input.stage)?;
    let manifest = read_manifest(&input.manifest)?;
    let snapshot = read_snapshot(&input.snapshot)?;
    let evidence = read_evidence(&input.evidence)?;
    Ok(check_documents(stage, &manifest, &snapshot, &evidence))
}

fn read_snapshot(path: &Path) -> Result<SnapshotDocument> {
    let raw = read_json::<RawSnapshotDocument>(path, "snapshot")?;
    Ok(SnapshotDocument {
        schema_version: raw
            .schema_version
            .or(raw.manifest_version)
            .unwrap_or_default(),
        observed_at_utc: raw.observed_at_utc,
        repositories: raw.repositories,
    })
}

fn read_manifest(path: &Path) -> Result<ManifestDocument> {
    let raw = read_json::<RawManifestDocument>(path, "manifest")?;
    let schema_version = raw
        .schema_version
        .or(raw.manifest_version)
        .unwrap_or_default();
    let manifest_id = raw.manifest_id.or(raw.schema).unwrap_or_default();
    let repositories = raw
        .repositories
        .into_iter()
        .map(|repository| ManifestRepository {
            repository: repository.repository.unwrap_or_default(),
            repository_role: repository.repository_role.unwrap_or_default(),
            default_branch: repository.default_branch.unwrap_or_default(),
            expected_workload_ids: repository.expected_workload_ids.unwrap_or_default(),
            required_check_contexts_and_apps: repository
                .required_check_contexts_and_apps
                .unwrap_or_default(),
            workload_platform_architecture: repository
                .workload_platform_architecture
                .unwrap_or_default(),
            provider_eligibility: repository.provider_eligibility.unwrap_or_default(),
            release_applicability: repository.release_applicability,
            generator_revision: repository.generator_revision,
            runtime_product_id: repository.runtime_product_id,
            generator_artifact_digest: repository.generator_artifact_digest,
            configuration_digest: repository.configuration_digest,
            generated_tree_digest: repository.generated_tree_digest,
            scan_state_digest: repository.scan_state_digest,
            runtime_release_version: repository.runtime_release_version,
            runtime_source_sha: repository.runtime_source_sha,
            job_image_digest: repository.job_image_digest,
        })
        .collect();
    Ok(ManifestDocument {
        schema_version,
        manifest_id,
        repositories,
    })
}

fn read_evidence(path: &Path) -> Result<EvidenceDocument> {
    let bytes = fs::read(path).with_context(|| format!("read evidence {}", path.display()))?;
    let mut value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse evidence JSON {}", path.display()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("evidence JSON must be an object"))?;

    // The records workstream's fleet.json is also a useful G0 input. Accept
    // its repository rows as an envelope, while replacing null claims with
    // empty typed values so the checker reports missing evidence rather than
    // treating parse failure as an opaque success/failure shortcut.
    if !object.contains_key("records")
        && let Some(repositories) = object.get("repositories").cloned()
    {
        object.insert("records".to_owned(), repositories);
    }
    if !object.contains_key("manifest_id")
        && let Some(schema) = object.get("schema").cloned()
    {
        object.insert("manifest_id".to_owned(), schema);
    }
    if !object.contains_key("snapshot_observed_at_utc")
        && let Some(observed) = object.get("observed_at_utc").cloned()
    {
        object.insert("snapshot_observed_at_utc".to_owned(), observed);
    }
    if !object.contains_key("stage")
        && let Some(gate) = object
            .get("source")
            .and_then(Value::as_object)
            .and_then(|source| source.get("current_gate"))
            .cloned()
    {
        object.insert("stage".to_owned(), gate);
    }
    if let Some(records) = object.get_mut("records").and_then(Value::as_array_mut) {
        for record in records {
            normalize_nullable_record(record);
        }
    }
    serde_json::from_value(value)
        .with_context(|| format!("parse evidence envelope {}", path.display()))
}

fn normalize_nullable_record(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for key in [
        "repository",
        "repository_role",
        "default_branch",
        "default_branch_sha",
        "observed_at_utc",
        "generator_revision",
        "runtime_product_id",
        "generator_artifact_digest",
        "configuration_digest",
        "generated_tree_digest",
        "scan_state_digest",
        "runtime_release_version",
        "runtime_source_sha",
        "job_image_digest",
        "owner",
        "reviewer",
        "gate_status",
    ] {
        if object.get(key).map(Value::is_null).unwrap_or(true) {
            object.insert(key.to_owned(), Value::String(String::new()));
        }
    }
    for key in [
        "expected_workload_ids",
        "required_check_contexts_and_apps",
        "workload_platform_architecture",
        "justified_exclusions",
        "expected_jobs",
        "actual_job_ids",
        "logs",
        "child_run_links",
        "required_checks",
    ] {
        if object.get(key).map(Value::is_null).unwrap_or(true) {
            object.insert(key.to_owned(), Value::Array(Vec::new()));
        }
    }
    if object
        .get("provider_eligibility")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        object.insert(
            "provider_eligibility".to_owned(),
            Value::Object(serde_json::Map::new()),
        );
    }
    if object
        .get("actual_job_conclusions")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        object.insert(
            "actual_job_conclusions".to_owned(),
            Value::Object(serde_json::Map::new()),
        );
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read {label} {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {label} JSON {}", path.display()))
}

fn check_documents(
    stage: Stage,
    manifest: &ManifestDocument,
    snapshot: &SnapshotDocument,
    evidence: &EvidenceDocument,
) -> CheckReport {
    let mut findings = Vec::new();
    check_document_headers(stage, manifest, snapshot, evidence, &mut findings);
    check_manifest(manifest, &mut findings);
    check_snapshot(snapshot, &mut findings);

    let manifest_by_repo = index_manifest(manifest, &mut findings);
    let snapshot_by_repo = index_snapshot(snapshot, &mut findings);
    check_manifest_snapshot_coverage(&manifest_by_repo, &snapshot_by_repo, &mut findings);

    if evidence.records.is_empty() {
        findings.push(Finding::new(
            "missing-evidence",
            None,
            "records",
            "evidence envelope has no records",
        ));
    }

    let mut seen_keys = BTreeSet::new();
    let mut records_by_repo_provider: BTreeMap<(String, String), usize> = BTreeMap::new();
    for record in &evidence.records {
        let key = (
            record.repository.clone(),
            record
                .provider
                .clone()
                .unwrap_or_else(|| "<missing>".to_owned()),
        );
        if !seen_keys.insert(key.clone()) {
            findings.push(Finding::new(
                "duplicate-evidence",
                Some(&record.repository),
                "records",
                format!("duplicate repository/provider record for {}", key.1),
            ));
        }
        *records_by_repo_provider.entry(key).or_default() += 1;

        match (
            manifest_by_repo.get(&record.repository),
            snapshot_by_repo.get(&record.repository),
        ) {
            (Some(manifest_repo), Some(snapshot_repo)) => {
                let index = RepositoryIndex {
                    manifest: manifest_repo,
                    snapshot: snapshot_repo,
                    snapshot_observed_at_utc: &snapshot.observed_at_utc,
                };
                check_record(stage, &index, record, &mut findings);
            }
            (None, _) => findings.push(Finding::new(
                "unknown-repository",
                Some(&record.repository),
                "repository",
                "record repository is absent from the external manifest",
            )),
            (_, None) => findings.push(Finding::new(
                "missing-snapshot-repository",
                Some(&record.repository),
                "repository",
                "record repository is absent from the authoritative snapshot",
            )),
        }
    }

    check_record_coverage(
        stage,
        &manifest_by_repo,
        &records_by_repo_provider,
        &mut findings,
    );
    sort_findings(&mut findings);
    CheckReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        stage: stage.as_str().to_owned(),
        status: if findings.is_empty() { "pass" } else { "fail" },
        findings,
    }
}

fn check_document_headers(
    stage: Stage,
    manifest: &ManifestDocument,
    snapshot: &SnapshotDocument,
    evidence: &EvidenceDocument,
    findings: &mut Vec<Finding>,
) {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        findings.push(Finding::new(
            "schema-version",
            None,
            "manifest.schema_version",
            format!(
                "expected {}, got {}",
                MANIFEST_SCHEMA_VERSION, manifest.schema_version
            ),
        ));
    }
    if snapshot.schema_version != EVIDENCE_SCHEMA_VERSION {
        findings.push(Finding::new(
            "schema-version",
            None,
            "snapshot.schema_version",
            format!(
                "expected {}, got {}",
                EVIDENCE_SCHEMA_VERSION, snapshot.schema_version
            ),
        ));
    }
    if evidence.schema_version != EVIDENCE_SCHEMA_VERSION {
        findings.push(Finding::new(
            "schema-version",
            None,
            "evidence.schema_version",
            format!(
                "expected {}, got {}",
                EVIDENCE_SCHEMA_VERSION, evidence.schema_version
            ),
        ));
    }
    if manifest.manifest_id.is_empty() {
        findings.push(Finding::new(
            "missing-manifest-id",
            None,
            "manifest.manifest_id",
            "manifest_id is required",
        ));
    }
    if evidence.manifest_id != manifest.manifest_id {
        findings.push(Finding::new(
            "manifest-mismatch",
            None,
            "evidence.manifest_id",
            format!(
                "evidence manifest_id '{}' does not match manifest '{}'",
                evidence.manifest_id, manifest.manifest_id
            ),
        ));
    }
    if !valid_timestamp(&snapshot.observed_at_utc) {
        findings.push(Finding::new(
            "invalid-timestamp",
            None,
            "snapshot.observed_at_utc",
            "expected an RFC3339 UTC timestamp",
        ));
    }
    if evidence.snapshot_observed_at_utc != snapshot.observed_at_utc {
        findings.push(Finding::new(
            "snapshot-mismatch",
            None,
            "evidence.snapshot_observed_at_utc",
            "evidence does not bind to the supplied snapshot timestamp",
        ));
    }
    if let Some(evidence_stage) = evidence.stage.as_deref() {
        match Stage::parse(evidence_stage) {
            Ok(value) if value == stage => {}
            Ok(value) => findings.push(Finding::new(
                "stage-mismatch",
                None,
                "evidence.stage",
                format!(
                    "evidence declares {} but the checker was invoked for {}",
                    value.as_str(),
                    stage.as_str()
                ),
            )),
            Err(error) => findings.push(Finding::new(
                "stage-mismatch",
                None,
                "evidence.stage",
                error.to_string(),
            )),
        }
    }
}

fn check_manifest(manifest: &ManifestDocument, findings: &mut Vec<Finding>) {
    if manifest.repositories.len() != REQUIRED_REPOSITORIES {
        findings.push(Finding::new(
            "manifest-count",
            None,
            "manifest.repositories",
            format!(
                "expected exactly {REQUIRED_REPOSITORIES} repositories, got {}",
                manifest.repositories.len()
            ),
        ));
    }
    let mut seen = BTreeSet::new();
    for repo in &manifest.repositories {
        if !seen.insert(repo.repository.clone()) {
            findings.push(Finding::new(
                "manifest-duplicate",
                Some(&repo.repository),
                "manifest.repositories",
                "repository appears more than once",
            ));
        }
        validate_repository_name(&repo.repository, None, "manifest.repositories", findings);
        if repo.repository_role.trim().is_empty() {
            findings.push(Finding::new(
                "manifest-field",
                Some(&repo.repository),
                "repository_role",
                "repository_role is required",
            ));
        }
        if repo.default_branch.trim().is_empty() {
            findings.push(Finding::new(
                "manifest-field",
                Some(&repo.repository),
                "default_branch",
                "default_branch is required",
            ));
        }
        validate_workloads(
            &repo.repository,
            &repo.expected_workload_ids,
            &repo.workload_platform_architecture,
            findings,
        );
        validate_contexts(
            &repo.repository,
            &repo.required_check_contexts_and_apps,
            findings,
        );
        validate_eligibility(&repo.repository, &repo.provider_eligibility, findings);
        validate_manifest_identity(repo, findings);
    }
}

fn validate_manifest_identity(repo: &ManifestRepository, findings: &mut Vec<Finding>) {
    for (field, value) in [
        ("generator_revision", repo.generator_revision.as_deref()),
        ("runtime_source_sha", repo.runtime_source_sha.as_deref()),
    ] {
        if let Some(value) = value {
            validate_sha(&repo.repository, field, value, findings);
        }
    }
    for (field, value) in [
        (
            "generator_artifact_digest",
            repo.generator_artifact_digest.as_deref(),
        ),
        ("configuration_digest", repo.configuration_digest.as_deref()),
        (
            "generated_tree_digest",
            repo.generated_tree_digest.as_deref(),
        ),
        ("scan_state_digest", repo.scan_state_digest.as_deref()),
        ("job_image_digest", repo.job_image_digest.as_deref()),
    ] {
        if let Some(value) = value {
            validate_digest(&repo.repository, field, value, findings);
        }
    }
    if repo
        .runtime_product_id
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        finding(
            findings,
            "manifest-field",
            &repo.repository,
            "runtime_product_id",
            "manifest runtime product identity cannot be empty",
        );
    }
    if repo
        .runtime_release_version
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        finding(
            findings,
            "manifest-field",
            &repo.repository,
            "runtime_release_version",
            "manifest runtime release version cannot be empty",
        );
    }
}

fn check_snapshot(snapshot: &SnapshotDocument, findings: &mut Vec<Finding>) {
    let mut seen = BTreeSet::new();
    for repo in &snapshot.repositories {
        if !seen.insert(repo.repository.clone()) {
            findings.push(Finding::new(
                "snapshot-duplicate",
                Some(&repo.repository),
                "snapshot.repositories",
                "repository appears more than once",
            ));
        }
        validate_repository_name(&repo.repository, None, "snapshot.repositories", findings);
        validate_sha(
            &repo.repository,
            "default_branch_sha",
            &repo.default_branch_sha,
            findings,
        );
        if repo.default_branch.trim().is_empty() {
            findings.push(Finding::new(
                "snapshot-field",
                Some(&repo.repository),
                "default_branch",
                "default_branch is required",
            ));
        }
        let mut prs = BTreeSet::new();
        for pr in &repo.open_prs {
            if !prs.insert(pr.number) {
                findings.push(Finding::new(
                    "snapshot-duplicate-pr",
                    Some(&repo.repository),
                    "open_prs",
                    format!("PR #{} appears more than once", pr.number),
                ));
            }
            if pr.number == 0 {
                findings.push(Finding::new(
                    "snapshot-pr-field",
                    Some(&repo.repository),
                    "open_prs.number",
                    "PR number must be positive",
                ));
            }
            validate_sha(
                &repo.repository,
                "open_prs.head_sha",
                &pr.head_sha,
                findings,
            );
            validate_sha(
                &repo.repository,
                "open_prs.base_sha",
                &pr.base_sha,
                findings,
            );
            if let Some(merge_sha) = &pr.merge_sha {
                validate_sha(&repo.repository, "open_prs.merge_sha", merge_sha, findings);
            }
        }
    }
}

fn index_manifest<'a>(
    manifest: &'a ManifestDocument,
    findings: &mut Vec<Finding>,
) -> BTreeMap<String, &'a ManifestRepository> {
    let mut result = BTreeMap::new();
    for repo in &manifest.repositories {
        if result.insert(repo.repository.clone(), repo).is_some() {
            findings.push(Finding::new(
                "manifest-duplicate",
                Some(&repo.repository),
                "manifest.repositories",
                "duplicate prevents deterministic indexing",
            ));
        }
    }
    result
}

fn index_snapshot<'a>(
    snapshot: &'a SnapshotDocument,
    findings: &mut Vec<Finding>,
) -> BTreeMap<String, &'a SnapshotRepository> {
    let mut result = BTreeMap::new();
    for repo in &snapshot.repositories {
        if result.insert(repo.repository.clone(), repo).is_some() {
            findings.push(Finding::new(
                "snapshot-duplicate",
                Some(&repo.repository),
                "snapshot.repositories",
                "duplicate prevents deterministic indexing",
            ));
        }
    }
    result
}

fn check_manifest_snapshot_coverage(
    manifest: &BTreeMap<String, &ManifestRepository>,
    snapshot: &BTreeMap<String, &SnapshotRepository>,
    findings: &mut Vec<Finding>,
) {
    for name in manifest.keys() {
        if !snapshot.contains_key(name) {
            findings.push(Finding::new(
                "missing-snapshot-repository",
                Some(name),
                "snapshot.repositories",
                "manifest repository has no authoritative snapshot row",
            ));
        }
    }
    for name in snapshot.keys() {
        if !manifest.contains_key(name) {
            findings.push(Finding::new(
                "snapshot-out-of-scope",
                Some(name),
                "snapshot.repositories",
                "snapshot row is absent from the external manifest",
            ));
        }
    }
}

fn check_record_coverage(
    stage: Stage,
    manifest: &BTreeMap<String, &ManifestRepository>,
    records: &BTreeMap<(String, String), usize>,
    findings: &mut Vec<Finding>,
) {
    for (name, repo) in manifest {
        let provider_names = required_providers(stage, repo);
        for provider in provider_names {
            if !records.contains_key(&(name.clone(), provider.to_owned())) {
                findings.push(Finding::new(
                    "missing-provider-evidence",
                    Some(name),
                    "records",
                    format!("missing {provider} evidence for {}", stage.as_str()),
                ));
            }
        }
        if stage == Stage::G0 && !records.keys().any(|(record_repo, _)| record_repo == name) {
            findings.push(Finding::new(
                "missing-repository",
                Some(name),
                "records",
                "manifest repository has no evidence record",
            ));
        }
    }
    for (name, provider) in records.keys() {
        if !manifest.contains_key(name) {
            continue;
        }
        if provider != "<missing>" && !["github", "velnor"].contains(&provider.as_str()) {
            findings.push(Finding::new(
                "unknown-provider",
                Some(name),
                "provider",
                format!("unsupported provider '{provider}'"),
            ));
        }
    }
}

fn required_providers(stage: Stage, repo: &ManifestRepository) -> Vec<&str> {
    let mut providers = Vec::new();
    let github = repo
        .provider_eligibility
        .get("github")
        .map(Eligibility::canonical)
        == Some("eligible");
    let velnor = repo
        .provider_eligibility
        .get("velnor")
        .map(Eligibility::canonical)
        == Some("eligible");
    if matches!(stage, Stage::G0 | Stage::G1 | Stage::G2 | Stage::G3) {
        if github {
            providers.push("github");
        }
    } else if stage.needs_both_lanes() {
        if github {
            providers.push("github");
        }
        if velnor {
            providers.push("velnor");
        }
    } else if stage.needs_velnor() && velnor {
        providers.push("velnor");
    }
    providers
}

fn check_record(
    stage: Stage,
    index: &RepositoryIndex<'_>,
    record: &EvidenceRecord,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    if record.repository_role != index.manifest.repository_role {
        finding(
            findings,
            "manifest-mismatch",
            repo,
            "repository_role",
            "record role does not match manifest",
        );
    }
    if record.default_branch != index.manifest.default_branch
        || record.default_branch != index.snapshot.default_branch
    {
        finding(
            findings,
            "snapshot-mismatch",
            repo,
            "default_branch",
            "record branch does not match manifest and snapshot",
        );
    }
    if record.default_branch_sha != index.snapshot.default_branch_sha {
        finding(
            findings,
            "stale-sha",
            repo,
            "default_branch_sha",
            "record branch SHA differs from the authoritative snapshot",
        );
    }
    if !valid_timestamp(&record.observed_at_utc) {
        finding(
            findings,
            "invalid-timestamp",
            repo,
            "observed_at_utc",
            "expected an RFC3339 UTC timestamp",
        );
    }
    if let (Ok(record_time), Ok(snapshot_time)) = (
        OffsetDateTime::parse(&record.observed_at_utc, &Rfc3339),
        OffsetDateTime::parse(index.snapshot_observed_at_utc, &Rfc3339),
    ) && record_time < snapshot_time
    {
        finding(
            findings,
            "stale-evidence",
            repo,
            "observed_at_utc",
            "record was captured before the authoritative snapshot",
        );
    }
    validate_sha(
        repo,
        "generator_revision",
        &record.generator_revision,
        findings,
    );
    validate_sha(
        repo,
        "runtime_source_sha",
        &record.runtime_source_sha,
        findings,
    );
    validate_digest(
        repo,
        "generator_artifact_digest",
        &record.generator_artifact_digest,
        findings,
    );
    validate_digest(
        repo,
        "configuration_digest",
        &record.configuration_digest,
        findings,
    );
    validate_digest(
        repo,
        "generated_tree_digest",
        &record.generated_tree_digest,
        findings,
    );
    validate_digest(
        repo,
        "scan_state_digest",
        &record.scan_state_digest,
        findings,
    );
    validate_digest(repo, "job_image_digest", &record.job_image_digest, findings);
    if record.runtime_product_id.trim().is_empty() {
        finding(
            findings,
            "missing-identity",
            repo,
            "runtime_product_id",
            "runtime product identity is required",
        );
    }
    if record.runtime_release_version.trim().is_empty() {
        finding(
            findings,
            "missing-identity",
            repo,
            "runtime_release_version",
            "runtime release version is required",
        );
    }
    validate_workloads(
        repo,
        &record.expected_workload_ids,
        &record.workload_platform_architecture,
        findings,
    );
    if sorted_strings(&record.expected_workload_ids)
        != sorted_strings(&index.manifest.expected_workload_ids)
    {
        finding(
            findings,
            "workload-mismatch",
            repo,
            "expected_workload_ids",
            "record workload inventory differs from manifest",
        );
    }
    if record.workload_platform_architecture != index.manifest.workload_platform_architecture {
        finding(
            findings,
            "platform-mismatch",
            repo,
            "workload_platform_architecture",
            "record workload platform/architecture differs from manifest",
        );
    }
    if record.required_check_contexts_and_apps != index.manifest.required_check_contexts_and_apps {
        finding(
            findings,
            "check-context-mismatch",
            repo,
            "required_check_contexts_and_apps",
            "record required check contract differs from manifest",
        );
    }
    check_record_eligibility(index.manifest, record, findings);
    check_manifest_identity_match(index.manifest, record, findings);

    if !stage.needs_execution() {
        if record.gate_status != "pass" {
            finding(
                findings,
                "gate-status",
                repo,
                "gate_status",
                "inventory evidence must explicitly pass G0",
            );
        }
        return;
    }

    check_execution(stage, index, record, findings);
    if stage.needs_release() {
        check_release_and_install(index, record, findings);
    }
    if stage.needs_independent_review() {
        if record.owner.trim().is_empty() || record.reviewer.trim().is_empty() {
            finding(
                findings,
                "missing-review",
                repo,
                "owner/reviewer",
                "G7 requires both owner and independent reviewer",
            );
        } else if record.owner == record.reviewer {
            finding(
                findings,
                "self-review",
                repo,
                "reviewer",
                "G7 reviewer must differ from owner",
            );
        }
    }
    if record.gate_status != "pass" {
        finding(
            findings,
            "gate-status",
            repo,
            "gate_status",
            "record gate_status must be pass after all evidence checks",
        );
    }
    if record.blocker.is_some() {
        finding(
            findings,
            "blocker-present",
            repo,
            "blocker",
            "a passing record cannot contain a blocker",
        );
    }
    if record.next_action.is_some() {
        finding(
            findings,
            "next-action-present",
            repo,
            "next_action",
            "a passing record cannot contain a next action",
        );
    }
}

fn check_manifest_identity_match(
    manifest: &ManifestRepository,
    record: &EvidenceRecord,
    findings: &mut Vec<Finding>,
) {
    for (field, expected, actual) in [
        (
            "generator_revision",
            manifest.generator_revision.as_deref(),
            Some(record.generator_revision.as_str()),
        ),
        (
            "runtime_product_id",
            manifest.runtime_product_id.as_deref(),
            Some(record.runtime_product_id.as_str()),
        ),
        (
            "generator_artifact_digest",
            manifest.generator_artifact_digest.as_deref(),
            Some(record.generator_artifact_digest.as_str()),
        ),
        (
            "configuration_digest",
            manifest.configuration_digest.as_deref(),
            Some(record.configuration_digest.as_str()),
        ),
        (
            "generated_tree_digest",
            manifest.generated_tree_digest.as_deref(),
            Some(record.generated_tree_digest.as_str()),
        ),
        (
            "scan_state_digest",
            manifest.scan_state_digest.as_deref(),
            Some(record.scan_state_digest.as_str()),
        ),
        (
            "runtime_release_version",
            manifest.runtime_release_version.as_deref(),
            Some(record.runtime_release_version.as_str()),
        ),
        (
            "runtime_source_sha",
            manifest.runtime_source_sha.as_deref(),
            Some(record.runtime_source_sha.as_str()),
        ),
        (
            "job_image_digest",
            manifest.job_image_digest.as_deref(),
            Some(record.job_image_digest.as_str()),
        ),
    ] {
        let Some(expected) = expected else {
            continue;
        };
        let actual = actual.unwrap_or_default();
        let equal = if field.ends_with("digest") {
            digest_equal(expected, actual)
        } else {
            expected == actual
        };
        if !equal {
            finding(
                findings,
                "manifest-mismatch",
                &record.repository,
                field,
                format!("record {field} differs from the external manifest pin"),
            );
        }
    }
}

fn check_record_eligibility(
    manifest: &ManifestRepository,
    record: &EvidenceRecord,
    findings: &mut Vec<Finding>,
) {
    if record.provider_eligibility != manifest.provider_eligibility {
        finding(
            findings,
            "eligibility-mismatch",
            &record.repository,
            "provider_eligibility",
            "record eligibility differs from manifest",
        );
    }
    for (provider, eligibility) in &manifest.provider_eligibility {
        if eligibility.canonical() == "invalid" {
            finding(
                findings,
                "manifest-provider",
                &record.repository,
                "provider_eligibility",
                format!("invalid eligibility value for {provider}"),
            );
        }
        if eligibility.canonical() == "excluded"
            && !record
                .justified_exclusions
                .iter()
                .any(|value| value.starts_with(&format!("{provider}:")))
        {
            finding(
                findings,
                "missing-exclusion",
                &record.repository,
                "justified_exclusions",
                format!("excluded provider {provider} needs a justification"),
            );
        }
    }
    if let Some(provider) = &record.provider {
        match manifest.provider_eligibility.get(provider) {
            Some(eligibility) if eligibility.canonical() == "eligible" => {}
            Some(eligibility) => finding(
                findings,
                "ineligible-provider",
                &record.repository,
                "provider",
                format!(
                    "provider {provider} is {} in manifest",
                    eligibility.canonical()
                ),
            ),
            None => finding(
                findings,
                "unknown-provider",
                &record.repository,
                "provider",
                format!("provider {provider} has no manifest policy"),
            ),
        }
    }
}

fn check_execution(
    stage: Stage,
    index: &RepositoryIndex<'_>,
    record: &EvidenceRecord,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let provider = match record.provider.as_deref() {
        Some(value) if value == "github" || value == "velnor" => value,
        Some(value) => {
            finding(
                findings,
                "wrong-provider",
                repo,
                "provider",
                format!("unsupported provider {value}"),
            );
            return;
        }
        None => {
            finding(
                findings,
                "missing-evidence",
                repo,
                "provider",
                "execution record has no provider",
            );
            return;
        }
    };
    if stage.needs_velnor() && provider != "velnor" {
        finding(
            findings,
            "wrong-provider",
            repo,
            "provider",
            format!("{} requires Velnor provider evidence", stage.as_str()),
        );
    }
    if (stage == Stage::G1 || stage == Stage::G3) && provider != "github" {
        finding(
            findings,
            "wrong-provider",
            repo,
            "provider",
            format!(
                "{} requires GitHub-hosted recovery evidence",
                stage.as_str()
            ),
        );
    }
    if record
        .workflow_path
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        finding(
            findings,
            "missing-evidence",
            repo,
            "workflow_path",
            "workflow path is required",
        );
    }
    if record
        .workflow_revision
        .as_deref()
        .map(|value| !valid_sha(value))
        .unwrap_or(true)
    {
        finding(
            findings,
            "missing-pin",
            repo,
            "workflow_revision",
            "workflow revision must be a 40-hex SHA",
        );
    }
    if record.run_id.unwrap_or(0) == 0 {
        finding(
            findings,
            "missing-evidence",
            repo,
            "run_id",
            "run_id must be positive",
        );
    }
    if record.run_attempt.unwrap_or(0) == 0 {
        finding(
            findings,
            "missing-evidence",
            repo,
            "run_attempt",
            "run_attempt must be positive",
        );
    }
    if !nonempty_url(record.run_url.as_deref()) {
        finding(
            findings,
            "missing-evidence",
            repo,
            "run_url",
            "run_url must be an https URL",
        );
    }
    let trigger = match record.trigger_source_sha.as_deref() {
        Some(value) if valid_sha(value) => value,
        _ => {
            finding(
                findings,
                "missing-source",
                repo,
                "trigger_source_sha",
                "trigger source SHA is required",
            );
            ""
        }
    };
    let checkout = match record.actual_checkout_sha.as_deref() {
        Some(value) if valid_sha(value) => value,
        _ => {
            finding(
                findings,
                "missing-source",
                repo,
                "actual_checkout_sha",
                "actual checkout SHA is required",
            );
            ""
        }
    };
    check_event_source(index, record, trigger, checkout, findings);
    if record
        .runner_name
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
        || record.host_id.as_deref().unwrap_or("").trim().is_empty()
    {
        finding(
            findings,
            "missing-runtime-identity",
            repo,
            "runner_name/host_id",
            "runner and host identities are required",
        );
    }
    check_jobs(record, findings);
    check_required_checks(index.manifest, record, findings);
    check_child_runs(record, trigger, checkout, findings);
}

fn check_event_source(
    index: &RepositoryIndex<'_>,
    record: &EvidenceRecord,
    trigger: &str,
    checkout: &str,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let event = match record.event.as_deref() {
        Some(value)
            if ["push", "pull_request", "merge_group", "workflow_dispatch"].contains(&value) =>
        {
            value
        }
        Some(value) => {
            finding(
                findings,
                "event",
                repo,
                "event",
                format!("unsupported event {value}"),
            );
            return;
        }
        None => {
            finding(
                findings,
                "missing-evidence",
                repo,
                "event",
                "event is required",
            );
            return;
        }
    };
    match event {
        "push" | "workflow_dispatch" => {
            if trigger != index.snapshot.default_branch_sha || checkout != trigger {
                finding(
                    findings,
                    "stale-sha",
                    repo,
                    "trigger_source_sha/actual_checkout_sha",
                    "main execution does not correspond to the current snapshot tip",
                );
            }
        }
        "pull_request" => {
            let number = record.pr_number;
            let snapshot_pr = number.and_then(|value| {
                index
                    .snapshot
                    .open_prs
                    .iter()
                    .find(|candidate| candidate.number == value)
            });
            let Some(snapshot_pr) = snapshot_pr else {
                finding(
                    findings,
                    "snapshot-mismatch",
                    repo,
                    "PR_number",
                    "PR is absent from the authoritative open-PR snapshot",
                );
                return;
            };
            if record.pr_head_sha.as_deref() != Some(snapshot_pr.head_sha.as_str())
                || record.pr_base_sha.as_deref() != Some(snapshot_pr.base_sha.as_str())
            {
                finding(
                    findings,
                    "stale-sha",
                    repo,
                    "PR_head_sha/PR_base_sha",
                    "PR identity differs from the authoritative snapshot",
                );
            }
            if trigger != snapshot_pr.head_sha {
                finding(
                    findings,
                    "source-mismatch",
                    repo,
                    "trigger_source_sha",
                    "pull_request trigger source must equal the contributor head SHA",
                );
            }
            if let Some(tested) = record.tested_merge_sha.as_deref() {
                if !valid_sha(tested) || checkout != tested {
                    finding(
                        findings,
                        "source-mismatch",
                        repo,
                        "tested_merge_sha/actual_checkout_sha",
                        "PR checkout must equal the tested merge SHA",
                    );
                }
                if let Some(snapshot_merge) = snapshot_pr.merge_sha.as_deref()
                    && tested != snapshot_merge
                {
                    finding(
                        findings,
                        "stale-sha",
                        repo,
                        "tested_merge_sha",
                        "tested merge SHA differs from the snapshot merge candidate",
                    );
                }
            } else {
                finding(
                    findings,
                    "missing-source",
                    repo,
                    "tested_merge_sha",
                    "pull_request evidence must identify its tested merge candidate",
                );
            }
        }
        "merge_group" => {
            let merge_group = record.merge_group_sha.as_deref();
            if merge_group != Some(trigger) || merge_group != Some(checkout) {
                finding(
                    findings,
                    "source-mismatch",
                    repo,
                    "merge_group_sha/trigger_source_sha/actual_checkout_sha",
                    "merge_group evidence must bind one immutable merge-group SHA",
                );
            }
        }
        _ => {}
    }
}

fn check_jobs(record: &EvidenceRecord, findings: &mut Vec<Finding>) {
    let repo = &record.repository;
    if record.expected_jobs.is_empty() {
        finding(
            findings,
            "empty-workloads",
            repo,
            "expected_jobs",
            "expected job inventory must be non-empty",
        );
        return;
    }
    let mut expected_ids = BTreeSet::new();
    let mut expected_workloads = BTreeSet::new();
    for job in &record.expected_jobs {
        if !expected_ids.insert(job.job_id.clone()) {
            finding(
                findings,
                "duplicate-job",
                repo,
                "expected_jobs",
                format!("duplicate job {}", job.job_id),
            );
        }
        expected_workloads.insert(job.workload_id.clone());
        if !["github", "velnor"].contains(&job.provider.as_str()) {
            finding(
                findings,
                "wrong-provider",
                repo,
                "expected_jobs.provider",
                format!("unsupported job provider {}", job.provider),
            );
        }
        if Some(job.provider.as_str()) != record.provider.as_deref() {
            finding(
                findings,
                "wrong-provider",
                repo,
                "expected_jobs.provider",
                format!("job {} provider differs from record provider", job.job_id),
            );
        }
        match record
            .workload_platform_architecture
            .iter()
            .find(|platform| platform.workload_id == job.workload_id)
        {
            Some(platform)
                if platform.platform == job.platform
                    && platform.architecture == job.architecture => {}
            Some(_) => finding(
                findings,
                "platform-mismatch",
                repo,
                "expected_jobs.platform/architecture",
                format!("job {} target differs from its workload target", job.job_id),
            ),
            None => finding(
                findings,
                "platform-mismatch",
                repo,
                "expected_jobs.workload_id",
                format!("job {} names an undeclared workload", job.job_id),
            ),
        }
    }
    if expected_workloads != record.expected_workload_ids.iter().cloned().collect() {
        finding(
            findings,
            "workload-mismatch",
            repo,
            "expected_jobs",
            "expected jobs do not cover exactly the declared workload IDs",
        );
    }
    let actual_ids = record
        .actual_job_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if actual_ids.len() != record.actual_job_ids.len() {
        finding(
            findings,
            "duplicate-job",
            repo,
            "actual_job_ids",
            "actual job IDs must be unique",
        );
    }
    if actual_ids != expected_ids {
        finding(
            findings,
            "job-inventory-mismatch",
            repo,
            "actual_job_ids",
            "actual job IDs must equal the expected job inventory",
        );
    }
    for expected in &expected_ids {
        if !actual_ids.contains(expected) {
            finding(
                findings,
                "missing-job",
                repo,
                "actual_job_ids",
                format!("expected job {expected} did not complete"),
            );
        }
        match record.actual_job_conclusions.get(expected) {
            Some(conclusion) if conclusion == "success" => {}
            Some(conclusion) => finding(
                findings,
                "job-conclusion",
                repo,
                "actual_job_conclusions",
                format!("job {expected} concluded {conclusion}; only success passes"),
            ),
            None => finding(
                findings,
                "missing-job-conclusion",
                repo,
                "actual_job_conclusions",
                format!("job {expected} has no conclusion"),
            ),
        }
    }
    if record.logs.is_empty() {
        finding(
            findings,
            "missing-evidence",
            repo,
            "logs",
            "at least one immutable job/run log link is required",
        );
    } else if record
        .logs
        .iter()
        .any(|log| !nonempty_url(Some(log.as_str())))
    {
        finding(
            findings,
            "missing-evidence",
            repo,
            "logs",
            "every job/run log link must be an https URL",
        );
    }
}

fn check_required_checks(
    manifest: &ManifestRepository,
    record: &EvidenceRecord,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let expected = manifest
        .required_check_contexts_and_apps
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let actual = record
        .required_checks
        .iter()
        .map(|check| RequiredContext {
            context: check.context.clone(),
            app: check.app.clone(),
        })
        .collect::<BTreeSet<_>>();
    if expected != actual {
        finding(
            findings,
            "check-context-mismatch",
            repo,
            "required_checks",
            "required check contexts/apps do not match the manifest",
        );
    }
    if record.required_checks.len() != actual.len() {
        finding(
            findings,
            "check-context-mismatch",
            repo,
            "required_checks",
            "required check context/app pairs must be unique",
        );
    }
    let jobs = record
        .expected_jobs
        .iter()
        .map(|job| job.job_id.as_str())
        .collect::<BTreeSet<_>>();
    for check in &record.required_checks {
        if !jobs.contains(check.job_id.as_str()) {
            finding(
                findings,
                "check-context-mismatch",
                repo,
                "required_checks.job_id",
                format!("check {} names unknown job {}", check.context, check.job_id),
            );
        }
        if check.conclusion != "success" {
            finding(
                findings,
                "check-conclusion",
                repo,
                "required_checks.conclusion",
                format!(
                    "required check {} concluded {}; only success passes",
                    check.context, check.conclusion
                ),
            );
        }
    }
}

fn check_child_runs(
    record: &EvidenceRecord,
    trigger: &str,
    checkout: &str,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let expected_child_ids = record
        .expected_jobs
        .iter()
        .filter_map(|job| job.child_run_id)
        .collect::<BTreeSet<_>>();
    let actual_child_ids = record
        .child_run_links
        .iter()
        .filter_map(|child| child.run_id)
        .collect::<BTreeSet<_>>();
    if actual_child_ids.len() != record.child_run_links.len() {
        finding(
            findings,
            "duplicate-child-run",
            repo,
            "child_run_links",
            "child run IDs must be unique",
        );
    }
    for expected in &expected_child_ids {
        if !actual_child_ids.contains(expected) {
            finding(
                findings,
                "missing-child-run",
                repo,
                "child_run_links",
                format!("expected child run {expected} is absent"),
            );
        }
    }
    for child in &record.child_run_links {
        if child.run_id.unwrap_or(0) == 0
            || !nonempty_url(child.run_url.as_deref())
            || child.workflow.trim().is_empty()
            || !valid_sha(&child.source_sha)
        {
            finding(
                findings,
                "missing-child-run",
                repo,
                "child_run_links",
                "child run link lacks identity, workflow, source SHA, or URL",
            );
        }
        if child.repository != *repo {
            finding(
                findings,
                "child-run-identity",
                repo,
                "child_run_links.repository",
                "child run repository differs from parent record",
            );
        }
        if child.provider != record.provider.as_deref().unwrap_or("") {
            finding(
                findings,
                "wrong-provider",
                repo,
                "child_run_links.provider",
                "child run provider differs from parent record",
            );
        }
        if child.source_sha != trigger && child.source_sha != checkout {
            finding(
                findings,
                "child-run-identity",
                repo,
                "child_run_links.source_sha",
                "child run source SHA does not match the parent source",
            );
        }
        if child.conclusion != "success" {
            finding(
                findings,
                "child-run-conclusion",
                repo,
                "child_run_links.conclusion",
                format!(
                    "child run concluded {}; only success passes",
                    child.conclusion
                ),
            );
        }
        if child.run_attempt.unwrap_or(0) == 0 {
            finding(
                findings,
                "missing-child-run",
                repo,
                "child_run_links.run_attempt",
                "child run attempt must be positive",
            );
        }
    }
}

fn check_release_and_install(
    index: &RepositoryIndex<'_>,
    record: &EvidenceRecord,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let release = merged_release(record);
    let install = merged_install(record);
    let manifest_applicability = index
        .manifest
        .release_applicability
        .unwrap_or(Applicability::Required);
    let Some(release) = release else {
        finding(
            findings,
            "missing-release-evidence",
            repo,
            "release",
            "G2+ requires an explicit release object, including not-applicable justification",
        );
        return;
    };
    if manifest_applicability == Applicability::Required
        && release.applicability != Applicability::Required
    {
        finding(
            findings,
            "release-applicability",
            repo,
            "release.applicability",
            "manifest requires release evidence; record cannot downgrade it",
        );
    }
    match release.applicability {
        Applicability::NotApplicable | Applicability::Excluded => {
            if release
                .justification
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                finding(
                    findings,
                    "missing-justification",
                    repo,
                    "release.justification",
                    "non-applicable release needs a justification",
                );
            }
        }
        Applicability::Required | Applicability::Applicable => {
            check_required_release(repo, record, &release, findings);
        }
    }
    let Some(install) = install else {
        finding(
            findings,
            "missing-install-evidence",
            repo,
            "install",
            "G2+ requires an explicit install object, including not-applicable justification",
        );
        return;
    };
    match install.applicability {
        Applicability::NotApplicable | Applicability::Excluded => {
            if install
                .justification
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                finding(
                    findings,
                    "missing-justification",
                    repo,
                    "install.justification",
                    "non-applicable install needs a justification",
                );
            }
        }
        Applicability::Required | Applicability::Applicable => {
            check_required_install(repo, record, &release, &install, findings);
        }
    }
}

fn check_required_release(
    repo: &str,
    record: &EvidenceRecord,
    release: &ReleaseEvidence,
    findings: &mut Vec<Finding>,
) {
    let channel = release.release_channel.as_deref().unwrap_or("");
    if !["preview", "stable"].contains(&channel) {
        finding(
            findings,
            "release-identity",
            repo,
            "release.release_channel",
            "release channel must be preview or stable",
        );
    }
    let version = release.release_version.as_deref().unwrap_or("");
    if version.trim().is_empty() {
        finding(
            findings,
            "release-identity",
            repo,
            "release.release_version",
            "release version is required",
        );
    }
    let source = artifact_source(record);
    if release.tag_target_sha.as_deref() != Some(source) {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "release.tag_target_sha",
            "release tag target does not match the executed source SHA",
        );
    }
    if release
        .release_id
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        finding(
            findings,
            "release-identity",
            repo,
            "release.release_id",
            "release ID is required",
        );
    }
    let manifest = release.manifest.as_ref();
    if manifest.is_none() {
        finding(
            findings,
            "missing-release-manifest",
            repo,
            "release.manifest",
            "required release evidence must include the canonical application manifest",
        );
    }
    if let Some(manifest) = manifest {
        check_release_manifest(repo, record, release, manifest, findings);
    }
    match release.asset_digests.as_ref() {
        Some(Value::Object(value)) if !value.is_empty() => {
            for digest in value.values() {
                if digest.as_str().map(valid_digest).unwrap_or(false) {
                    continue;
                }
                finding(
                    findings,
                    "mismatched-artifact",
                    repo,
                    "release.asset_digests",
                    "every release asset digest must be a valid sha256 digest",
                );
                break;
            }
        }
        _ => finding(
            findings,
            "mismatched-artifact",
            repo,
            "release.asset_digests",
            "release must contain a named asset-to-sha256 digest map",
        ),
    }
    for (field, value) in [
        (
            "release.apt_feed_revision_suite_and_candidate",
            release.apt_feed_revision_suite_and_candidate.as_deref(),
        ),
        (
            "release.homebrew_tap_revision_and_formula",
            release.homebrew_tap_revision_and_formula.as_deref(),
        ),
    ] {
        if value.unwrap_or("").trim().is_empty() {
            finding(
                findings,
                "missing-release-evidence",
                repo,
                field,
                "publication evidence is required for both APT and Homebrew",
            );
        } else if !version.is_empty() && !value.unwrap_or("").contains(version) {
            finding(
                findings,
                "mismatched-artifact",
                repo,
                field,
                "publication evidence does not identify the release version",
            );
        } else if let Some(manifest) = manifest
            && !manifest.manifest_sha256.is_empty()
            && !value.unwrap_or("").contains(&manifest.manifest_sha256)
        {
            finding(
                findings,
                "mismatched-artifact",
                repo,
                field,
                "publication evidence does not identify the canonical manifest digest",
            );
        }
    }
}

fn check_release_manifest(
    repo: &str,
    record: &EvidenceRecord,
    release: &ReleaseEvidence,
    manifest: &ReleaseManifestEvidence,
    findings: &mut Vec<Finding>,
) {
    if manifest.schema.trim().is_empty() {
        finding(
            findings,
            "release-manifest",
            repo,
            "release.manifest.schema",
            "canonical manifest schema is required",
        );
    }
    if manifest.product_id.trim().is_empty() {
        finding(
            findings,
            "release-manifest",
            repo,
            "release.manifest.product_id",
            "canonical manifest product identity is required",
        );
    } else if manifest.product_id != record.runtime_product_id {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "release.manifest.product_id",
            "canonical manifest product differs from runtime_product_id",
        );
    }
    if let Some(channel) = manifest.channel.as_deref()
        && Some(channel) != release.release_channel.as_deref()
    {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "release.manifest.channel",
            "canonical manifest channel differs from release channel",
        );
    }
    if let Some(version) = manifest.version.as_deref()
        && Some(version) != release.release_version.as_deref()
    {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "release.manifest.version",
            "canonical manifest version differs from release version",
        );
    }
    if let Some(source_repository) = manifest.source_repository.as_deref() {
        validate_repository_name(
            source_repository,
            Some(repo),
            "release.manifest.source_repository",
            findings,
        );
    }
    if manifest
        .release_tag
        .as_deref()
        .is_some_and(|release_tag| release_tag.trim().is_empty())
    {
        finding(
            findings,
            "release-manifest",
            repo,
            "release.manifest.release_tag",
            "release tag cannot be empty when supplied",
        );
    }
    if let Some(release_id) = manifest.release_id.as_deref()
        && Some(release_id) != release.release_id.as_deref()
    {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "release.manifest.release_id",
            "canonical manifest release ID differs from release evidence",
        );
    }
    if !manifest.source_ref.starts_with("refs/heads/")
        && !manifest.source_ref.starts_with("refs/tags/")
    {
        finding(
            findings,
            "release-manifest",
            repo,
            "release.manifest.source_ref",
            "source_ref must be an immutable tag or an explicit branch ref",
        );
    }
    validate_sha(
        repo,
        "release.manifest.source_commit",
        &manifest.source_commit,
        findings,
    );
    if manifest.source_commit != artifact_source(record) {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "release.manifest.source_commit",
            "canonical manifest source commit differs from executed artifact source",
        );
    }
    validate_digest(
        repo,
        "release.manifest.manifest_sha256",
        &manifest.manifest_sha256,
        findings,
    );
    let asset_digests = release.asset_digests.as_ref().and_then(Value::as_object);
    if manifest.artifacts.is_empty() {
        finding(
            findings,
            "release-manifest",
            repo,
            "release.manifest.artifacts",
            "canonical manifest must list one or more immutable artifacts",
        );
    }
    let mut artifact_names = BTreeSet::new();
    for artifact in &manifest.artifacts {
        if !artifact_names.insert(artifact.name.clone()) {
            finding(
                findings,
                "release-manifest",
                repo,
                "release.manifest.artifacts",
                format!("duplicate artifact {}", artifact.name),
            );
        }
        if artifact.name.trim().is_empty()
            || artifact.target.trim().is_empty()
            || artifact.kind.trim().is_empty()
        {
            finding(
                findings,
                "release-manifest",
                repo,
                "release.manifest.artifacts",
                "artifact name, target, and kind are required",
            );
        }
        if !valid_target(&artifact.target) {
            finding(
                findings,
                "unsupported-target",
                repo,
                "release.manifest.artifacts.target",
                format!("unsupported artifact target {}", artifact.target),
            );
        }
        if artifact.size == Some(0) {
            finding(
                findings,
                "release-manifest",
                repo,
                "release.manifest.artifacts.size",
                format!("artifact {} must have a positive size", artifact.name),
            );
        }
        validate_digest(
            repo,
            "release.manifest.artifacts.sha256",
            &artifact.sha256,
            findings,
        );
        if let Some(asset_digests) = asset_digests {
            match asset_digests.get(&artifact.name).and_then(Value::as_str) {
                Some(value) if digest_equal(value, &artifact.sha256) => {}
                _ => finding(
                    findings,
                    "mismatched-artifact",
                    repo,
                    "release.asset_digests",
                    format!(
                        "asset digest for {} differs from canonical manifest",
                        artifact.name
                    ),
                ),
            }
        }
    }
    if manifest.components.is_empty() {
        finding(
            findings,
            "release-manifest",
            repo,
            "release.manifest.components",
            "canonical manifest must list one or more product components",
        );
    }
    let mut component_names = BTreeSet::new();
    let mut component_binaries = BTreeSet::new();
    for component in &manifest.components {
        if !component_names.insert(component.name.clone()) {
            finding(
                findings,
                "release-manifest",
                repo,
                "release.manifest.components",
                format!("duplicate component {}", component.name),
            );
        }
        if component.name.trim().is_empty()
            || component.crate_name.trim().is_empty()
            || component.version.trim().is_empty()
            || component.binary.trim().is_empty()
        {
            finding(
                findings,
                "release-manifest",
                repo,
                "release.manifest.components",
                "component name, crate, version, and binary are required",
            );
        }
        if !component_binaries.insert(component.binary.clone()) {
            finding(
                findings,
                "release-manifest",
                repo,
                "release.manifest.components.binary",
                format!("duplicate component binary {}", component.binary),
            );
        }
        if component.targets.is_empty() {
            finding(
                findings,
                "release-manifest",
                repo,
                "release.manifest.components.targets",
                format!("component {} has no target", component.name),
            );
        }
        for target in &component.targets {
            if !valid_target(target) {
                finding(
                    findings,
                    "unsupported-target",
                    repo,
                    "release.manifest.components.targets",
                    format!("unsupported component target {target}"),
                );
            }
        }
    }
}

fn check_required_install(
    repo: &str,
    record: &EvidenceRecord,
    release: &ReleaseEvidence,
    install: &InstallEvidence,
    findings: &mut Vec<Finding>,
) {
    check_install_environment(repo, install, findings);
    for (field, value) in [
        ("install.upgrade_from", install.upgrade_from.as_deref()),
        ("install.switch_from", install.switch_from.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            finding(
                findings,
                "install-provenance",
                repo,
                field,
                "an operation identity must be null or non-empty",
            );
        }
    }
    match install.service_manager_result.as_deref() {
        Some(value) if valid_service_result(value) => {}
        Some(_) => finding(
            findings,
            "install-provenance",
            repo,
            "install.service_manager_result",
            "service manager result must be 'systemd success' or justified not-applicable",
        ),
        None => finding(
            findings,
            "missing-install-evidence",
            repo,
            "install.service_manager_result",
            "service manager result is required",
        ),
    }
    if install.functional_result.as_deref() != Some("success") {
        finding(
            findings,
            "install-result",
            repo,
            "install.functional_result",
            "functional installation result must be success",
        );
    }
    let Some(identity) = install.installed_binary_identity.as_ref() else {
        finding(
            findings,
            "missing-install-evidence",
            repo,
            "install.installed_binary_identity",
            "installed binary identity is required",
        );
        return;
    };
    if identity.product_id != record.runtime_product_id {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "install.installed_binary_identity.product_id",
            "installed product identity differs from runtime_product_id",
        );
    }
    if identity.channel != release.release_channel.as_deref().unwrap_or("") {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "install.installed_binary_identity.channel",
            "installed channel differs from release channel",
        );
    }
    if Some(identity.version.as_str()) != release.release_version.as_deref() {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "install.installed_binary_identity.version",
            "installed version differs from release version",
        );
    }
    if !valid_sha(&identity.source_sha) || identity.source_sha != artifact_source(record) {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "install.installed_binary_identity.source_sha",
            "installed source identity differs from executed artifact source",
        );
    }
    let release_manifest_digest = release
        .manifest
        .as_ref()
        .map(|manifest| manifest.manifest_sha256.as_str())
        .unwrap_or("");
    if !digest_equal(&identity.manifest_sha256, release_manifest_digest) {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "install.installed_binary_identity.manifest_sha256",
            "installed identity does not bind the canonical release manifest",
        );
    }
    if identity.binaries.is_empty() {
        finding(
            findings,
            "missing-install-evidence",
            repo,
            "install.installed_binary_identity.binaries",
            "installed identity must list one or more binaries",
        );
    }
    let mut binary_names = BTreeSet::new();
    for binary in &identity.binaries {
        if !binary_names.insert(binary.name.clone()) {
            finding(
                findings,
                "install-provenance",
                repo,
                "install.installed_binary_identity.binaries",
                format!("duplicate installed binary {}", binary.name),
            );
        }
        if binary.name.trim().is_empty() || binary.path.trim().is_empty() {
            finding(
                findings,
                "missing-install-evidence",
                repo,
                "install.installed_binary_identity.binaries",
                "installed binary name and path are required",
            );
        }
        if !binary.path.starts_with('/') {
            finding(
                findings,
                "install-provenance",
                repo,
                "install.installed_binary_identity.binaries.path",
                format!("binary {} path must be absolute and installed", binary.name),
            );
        }
        validate_digest(
            repo,
            "install.installed_binary_identity.binaries.sha256",
            &binary.sha256,
            findings,
        );
    }
    if let Some(manifest) = release.manifest.as_ref() {
        let expected = manifest
            .components
            .iter()
            .map(|component| component.binary.clone())
            .collect::<BTreeSet<_>>();
        if expected != binary_names {
            finding(
                findings,
                "mismatched-artifact",
                repo,
                "install.installed_binary_identity.binaries",
                "installed binary inventory differs from canonical component inventory",
            );
        }
    }
}

fn check_install_environment(repo: &str, install: &InstallEvidence, findings: &mut Vec<Finding>) {
    let Some(environment) = install.install_upgrade_test_environment.as_ref() else {
        finding(
            findings,
            "missing-install-evidence",
            repo,
            "install.install_upgrade_test_environment",
            "clean install/upgrade environment is required",
        );
        return;
    };
    match environment {
        InstallEnvironmentEvidence::Description(value) => {
            let normalized = value.to_ascii_lowercase();
            if value.trim().is_empty()
                || !normalized.contains("clean")
                || !normalized.contains("path")
            {
                finding(
                    findings,
                    "install-provenance",
                    repo,
                    "install.install_upgrade_test_environment",
                    "environment description must identify a clean workspace and PATH",
                );
            }
        }
        InstallEnvironmentEvidence::Structured(environment) => {
            if environment.os_image.trim().is_empty()
                || environment.platform.trim().is_empty()
                || environment.architecture.trim().is_empty()
                || environment.runner.trim().is_empty()
                || environment.workspace.trim().is_empty()
                || environment.path.trim().is_empty()
            {
                finding(
                    findings,
                    "install-provenance",
                    repo,
                    "install.install_upgrade_test_environment",
                    "environment requires image, platform, architecture, runner, workspace, and PATH",
                );
            }
            if !valid_target(&format!(
                "{}-{}",
                environment.platform, environment.architecture
            )) {
                finding(
                    findings,
                    "unsupported-target",
                    repo,
                    "install.install_upgrade_test_environment.architecture",
                    "install environment has an unsupported platform/architecture",
                );
            }
        }
    }
}

fn valid_service_result(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized == "systemd success"
        || normalized
            .strip_prefix("not-applicable")
            .is_some_and(|reason| !reason.trim_matches([' ', ':', ';']).trim().is_empty())
}

fn merged_release(record: &EvidenceRecord) -> Option<ReleaseEvidence> {
    if let Some(release) = record.release.clone() {
        return Some(release);
    }
    let any = record.release_channel.is_some()
        || record.release_version.is_some()
        || record.tag_target_sha.is_some()
        || record.release_id.is_some()
        || record.asset_digests.is_some()
        || record.apt_feed_revision_suite_and_candidate.is_some()
        || record.homebrew_tap_revision_and_formula.is_some();
    any.then(|| ReleaseEvidence {
        applicability: Applicability::Required,
        justification: None,
        release_channel: record.release_channel.clone(),
        release_version: record.release_version.clone(),
        tag_target_sha: record.tag_target_sha.clone(),
        release_id: record.release_id.clone(),
        asset_digests: record.asset_digests.clone(),
        apt_feed_revision_suite_and_candidate: record.apt_feed_revision_suite_and_candidate.clone(),
        homebrew_tap_revision_and_formula: record.homebrew_tap_revision_and_formula.clone(),
        manifest: None,
    })
}

fn merged_install(record: &EvidenceRecord) -> Option<InstallEvidence> {
    if let Some(install) = record.install.clone() {
        return Some(install);
    }
    let any = record.install_upgrade_test_environment.is_some()
        || record.installed_binary_identity.is_some()
        || record.functional_result.is_some();
    any.then(|| InstallEvidence {
        applicability: Applicability::Required,
        justification: None,
        install_upgrade_test_environment: record
            .install_upgrade_test_environment
            .clone()
            .map(InstallEnvironmentEvidence::Description),
        installed_binary_identity: record.installed_binary_identity.clone(),
        upgrade_from: None,
        switch_from: None,
        service_manager_result: None,
        functional_result: record.functional_result.clone(),
    })
}

fn artifact_source(record: &EvidenceRecord) -> &str {
    record
        .actual_checkout_sha
        .as_deref()
        .unwrap_or(&record.default_branch_sha)
}

fn validate_workloads(
    repository: &str,
    workload_ids: &[String],
    platforms: &[WorkloadPlatform],
    findings: &mut Vec<Finding>,
) {
    if workload_ids.is_empty() {
        finding(
            findings,
            "empty-workloads",
            repository,
            "expected_workload_ids",
            "expected workload inventory must be non-empty",
        );
    }
    let unique = workload_ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != workload_ids.len() {
        finding(
            findings,
            "duplicate-workload",
            repository,
            "expected_workload_ids",
            "workload IDs must be unique",
        );
    }
    let platform_ids = platforms
        .iter()
        .map(|platform| platform.workload_id.as_str())
        .collect::<BTreeSet<_>>();
    let workload_ids = workload_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if platform_ids != workload_ids {
        finding(
            findings,
            "platform-mismatch",
            repository,
            "workload_platform_architecture",
            "every workload needs exactly one platform/architecture row",
        );
    }
    for platform in platforms {
        if platform.platform.trim().is_empty() || platform.architecture.trim().is_empty() {
            finding(
                findings,
                "platform-mismatch",
                repository,
                "workload_platform_architecture",
                format!(
                    "workload {} has an empty platform or architecture",
                    platform.workload_id
                ),
            );
        }
    }
}

fn validate_contexts(repository: &str, contexts: &[RequiredContext], findings: &mut Vec<Finding>) {
    if contexts.is_empty() {
        finding(
            findings,
            "empty-check-contract",
            repository,
            "required_check_contexts_and_apps",
            "at least one required check context and supplying app is required",
        );
    }
    let mut seen = BTreeSet::new();
    for context in contexts {
        if context.context.trim().is_empty() || context.app.trim().is_empty() {
            finding(
                findings,
                "check-context",
                repository,
                "required_check_contexts_and_apps",
                "required check context and app must be non-empty",
            );
        }
        if !seen.insert((context.context.clone(), context.app.clone())) {
            finding(
                findings,
                "check-context",
                repository,
                "required_check_contexts_and_apps",
                format!("duplicate required check {}", context.context),
            );
        }
    }
}

fn validate_eligibility(
    repository: &str,
    eligibility: &BTreeMap<String, Eligibility>,
    findings: &mut Vec<Finding>,
) {
    for provider in ["github", "velnor"] {
        if !eligibility.contains_key(provider) {
            finding(
                findings,
                "manifest-provider",
                repository,
                "provider_eligibility",
                format!("missing {provider} eligibility"),
            );
        }
    }
    for (provider, value) in eligibility {
        if value.canonical() == "invalid" {
            finding(
                findings,
                "manifest-provider",
                repository,
                "provider_eligibility",
                format!("invalid eligibility value for {provider}"),
            );
        }
    }
}

fn validate_repository_name(
    repository: &str,
    record_repository: Option<&str>,
    field: &str,
    findings: &mut Vec<Finding>,
) {
    let parts = repository.split('/').collect::<Vec<_>>();
    if parts.len() != 2 || parts.iter().any(|part| part.trim().is_empty()) {
        findings.push(Finding::new(
            "repository-name",
            record_repository.or(Some(repository)),
            field,
            "repository must be owner/name",
        ));
    }
}

fn validate_sha(repository: &str, field: &str, value: &str, findings: &mut Vec<Finding>) {
    if !valid_sha(value) {
        finding(
            findings,
            "invalid-sha",
            repository,
            field,
            "expected a 40-hex Git SHA",
        );
    }
}

fn validate_digest(repository: &str, field: &str, value: &str, findings: &mut Vec<Finding>) {
    if !valid_digest(value) {
        finding(
            findings,
            "invalid-digest",
            repository,
            field,
            "expected sha256:<64 hex characters>",
        );
    }
}

fn finding(
    findings: &mut Vec<Finding>,
    code: &str,
    repository: &str,
    field: &str,
    message: impl Into<String>,
) {
    findings.push(Finding::new(code, Some(repository), field, message));
}

fn sorted_strings(values: &[String]) -> Vec<String> {
    let mut values = values.to_vec();
    values.sort();
    values
}

fn valid_sha(value: &str) -> bool {
    value.len() == SHA_LENGTH && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_digest(value: &str) -> bool {
    let hex = value.strip_prefix("sha256:").unwrap_or(value);
    hex.len() == DIGEST_LENGTH && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn digest_equal(left: &str, right: &str) -> bool {
    let left = left.strip_prefix("sha256:").unwrap_or(left);
    let right = right.strip_prefix("sha256:").unwrap_or(right);
    valid_digest(left) && valid_digest(right) && left.eq_ignore_ascii_case(right)
}

fn valid_target(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase().replace(['_', '.'], "-");
    matches!(
        normalized.as_str(),
        "linux-x64"
            | "linux-amd64"
            | "linux-x86-64"
            | "linux-arm64"
            | "linux-aarch64"
            | "macos-arm64"
            | "macos-aarch64"
            | "darwin-arm64"
            | "macos-x64"
            | "macos-amd64"
            | "macos-x86-64"
            | "darwin-x64"
            | "darwin-x86-64"
    )
}

fn valid_timestamp(value: &str) -> bool {
    value.ends_with('Z') && OffsetDateTime::parse(value, &Rfc3339).is_ok()
}

fn nonempty_url(value: Option<&str>) -> bool {
    value
        .map(|value| value.starts_with("https://") && value.len() > "https://".len())
        .unwrap_or(false)
}

fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|left, right| {
        (
            left.repository.as_deref().unwrap_or(""),
            left.code.as_str(),
            left.field.as_str(),
            left.message.as_str(),
        )
            .cmp(&(
                right.repository.as_deref().unwrap_or(""),
                right.code.as_str(),
                right.field.as_str(),
                right.message.as_str(),
            ))
    });
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

    fn sha(seed: char) -> String {
        std::iter::repeat_n(seed, SHA_LENGTH).collect()
    }

    fn digest(seed: char) -> String {
        format!(
            "sha256:{}",
            std::iter::repeat_n(seed, DIGEST_LENGTH).collect::<String>()
        )
    }

    fn manifest_json() -> ManifestDocument {
        let repositories = (0..REQUIRED_REPOSITORIES)
            .map(|index| ManifestRepository {
                repository: format!("owner/repo-{index}"),
                repository_role: "library".to_owned(),
                default_branch: "main".to_owned(),
                expected_workload_ids: vec!["ci".to_owned()],
                required_check_contexts_and_apps: vec![RequiredContext {
                    context: "ci / required".to_owned(),
                    app: "github-actions".to_owned(),
                }],
                workload_platform_architecture: vec![WorkloadPlatform {
                    workload_id: "ci".to_owned(),
                    platform: "linux".to_owned(),
                    architecture: "amd64".to_owned(),
                }],
                provider_eligibility: BTreeMap::from([
                    (
                        "github".to_owned(),
                        Eligibility::Name("eligible".to_owned()),
                    ),
                    (
                        "velnor".to_owned(),
                        Eligibility::Name("eligible".to_owned()),
                    ),
                ]),
                release_applicability: Some(Applicability::NotApplicable),
                generator_revision: None,
                runtime_product_id: None,
                generator_artifact_digest: None,
                configuration_digest: None,
                generated_tree_digest: None,
                scan_state_digest: None,
                runtime_release_version: None,
                runtime_source_sha: None,
                job_image_digest: None,
            })
            .collect();
        ManifestDocument {
            schema_version: 1,
            manifest_id: "fixture".to_owned(),
            repositories,
        }
    }

    fn snapshot_json(manifest: &ManifestDocument) -> SnapshotDocument {
        SnapshotDocument {
            schema_version: 1,
            observed_at_utc: "2026-09-19T12:00:00Z".to_owned(),
            repositories: manifest
                .repositories
                .iter()
                .map(|repo| SnapshotRepository {
                    repository: repo.repository.clone(),
                    default_branch: repo.default_branch.clone(),
                    default_branch_sha: sha('a'),
                    open_prs: Vec::new(),
                })
                .collect(),
        }
    }

    fn record_json(repo: &ManifestRepository, snapshot: &SnapshotRepository) -> EvidenceRecord {
        EvidenceRecord {
            repository: repo.repository.clone(),
            repository_role: repo.repository_role.clone(),
            default_branch: repo.default_branch.clone(),
            default_branch_sha: snapshot.default_branch_sha.clone(),
            observed_at_utc: "2026-09-19T12:01:00Z".to_owned(),
            generator_revision: sha('b'),
            runtime_product_id: "velnor".to_owned(),
            generator_artifact_digest: digest('a'),
            configuration_digest: digest('b'),
            generated_tree_digest: digest('c'),
            scan_state_digest: digest('d'),
            runtime_release_version: "0.1.1".to_owned(),
            runtime_source_sha: sha('c'),
            job_image_digest: digest('e'),
            expected_workload_ids: repo.expected_workload_ids.clone(),
            required_check_contexts_and_apps: repo.required_check_contexts_and_apps.clone(),
            workload_platform_architecture: repo.workload_platform_architecture.clone(),
            provider_eligibility: repo.provider_eligibility.clone(),
            justified_exclusions: Vec::new(),
            pr_number: None,
            pr_head_sha: None,
            pr_base_sha: None,
            tested_merge_sha: None,
            merge_group_sha: None,
            workflow_path: Some(".github/workflows/ci.yml".to_owned()),
            workflow_revision: Some(sha('d')),
            event: Some("push".to_owned()),
            run_id: Some(1),
            run_attempt: Some(1),
            run_url: Some("https://github.com/owner/repo/actions/runs/1".to_owned()),
            trigger_source_sha: Some(snapshot.default_branch_sha.clone()),
            actual_checkout_sha: Some(snapshot.default_branch_sha.clone()),
            provider: Some("github".to_owned()),
            runner_name: Some("ubuntu-24.04".to_owned()),
            host_id: Some("github-hosted".to_owned()),
            expected_jobs: vec![ExpectedJob {
                job_id: "ci".to_owned(),
                workload_id: "ci".to_owned(),
                provider: "github".to_owned(),
                platform: "linux".to_owned(),
                architecture: "amd64".to_owned(),
                child_run_id: None,
            }],
            actual_job_ids: vec!["ci".to_owned()],
            actual_job_conclusions: BTreeMap::from([("ci".to_owned(), "success".to_owned())]),
            logs: vec!["https://github.com/owner/repo/actions/runs/1".to_owned()],
            child_run_links: Vec::new(),
            required_checks: vec![RequiredCheck {
                context: "ci / required".to_owned(),
                app: "github-actions".to_owned(),
                job_id: "ci".to_owned(),
                conclusion: "success".to_owned(),
            }],
            release: Some(ReleaseEvidence {
                applicability: Applicability::NotApplicable,
                justification: Some("library has no release surface".to_owned()),
                release_channel: None,
                release_version: None,
                tag_target_sha: None,
                release_id: None,
                asset_digests: None,
                apt_feed_revision_suite_and_candidate: None,
                homebrew_tap_revision_and_formula: None,
                manifest: None,
            }),
            install: Some(InstallEvidence {
                applicability: Applicability::NotApplicable,
                justification: Some("library has no install surface".to_owned()),
                install_upgrade_test_environment: None,
                installed_binary_identity: None,
                upgrade_from: None,
                switch_from: None,
                service_manager_result: None,
                functional_result: None,
            }),
            release_channel: None,
            release_version: None,
            tag_target_sha: None,
            release_id: None,
            asset_digests: None,
            apt_feed_revision_suite_and_candidate: None,
            homebrew_tap_revision_and_formula: None,
            install_upgrade_test_environment: None,
            installed_binary_identity: None,
            functional_result: None,
            owner: "owner".to_owned(),
            reviewer: "reviewer".to_owned(),
            gate_status: "pass".to_owned(),
            blocker: None,
            next_action: None,
        }
    }

    fn envelope(manifest: &ManifestDocument, snapshot: &SnapshotDocument) -> EvidenceDocument {
        EvidenceDocument {
            schema_version: 1,
            manifest_id: manifest.manifest_id.clone(),
            snapshot_observed_at_utc: snapshot.observed_at_utc.clone(),
            stage: None,
            records: manifest
                .repositories
                .iter()
                .zip(snapshot.repositories.iter())
                .map(|(manifest, snapshot)| record_json(manifest, snapshot))
                .collect(),
        }
    }

    #[test]
    fn g1_fixture_passes_with_all_32_manifest_rows() {
        let manifest = manifest_json();
        let snapshot = snapshot_json(&manifest);
        let evidence = envelope(&manifest, &snapshot);
        let report = check_documents(Stage::G1, &manifest, &snapshot, &evidence);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
    }

    #[test]
    fn g2_fixture_requires_nested_release_and_install_provenance() {
        let mut manifest = manifest_json();
        manifest.repositories[0].release_applicability = Some(Applicability::Required);
        let snapshot = snapshot_json(&manifest);
        let mut evidence = envelope(&manifest, &snapshot);
        let source = snapshot.repositories[0].default_branch_sha.clone();
        let manifest_digest = digest('f');
        evidence.records[0].release = Some(ReleaseEvidence {
            applicability: Applicability::Required,
            justification: None,
            release_channel: Some("stable".to_owned()),
            release_version: Some("0.1.1".to_owned()),
            tag_target_sha: Some(source.clone()),
            release_id: Some("release-1".to_owned()),
            asset_digests: Some(serde_json::json!({"velnorctl-linux": digest('a')})),
            apt_feed_revision_suite_and_candidate: Some(format!(
                "feed stable 0.1.1 {manifest_digest}"
            )),
            homebrew_tap_revision_and_formula: Some(format!("tap stable 0.1.1 {manifest_digest}")),
            manifest: Some(ReleaseManifestEvidence {
                schema: "velnor.application-manifest.v1".to_owned(),
                product_id: "velnor".to_owned(),
                channel: Some("stable".to_owned()),
                version: Some("0.1.1".to_owned()),
                source_repository: Some("owner/repo-0".to_owned()),
                source_ref: "refs/tags/v0.1.1".to_owned(),
                source_commit: source.clone(),
                release_tag: Some("v0.1.1".to_owned()),
                release_id: Some("release-1".to_owned()),
                manifest_sha256: manifest_digest.clone(),
                artifacts: vec![ReleaseArtifact {
                    name: "velnorctl-linux".to_owned(),
                    target: "linux-amd64".to_owned(),
                    kind: "archive".to_owned(),
                    sha256: digest('a'),
                    size: Some(1),
                }],
                components: vec![ReleaseComponent {
                    name: "velnorctl".to_owned(),
                    crate_name: "velnorctl".to_owned(),
                    version: "0.1.1".to_owned(),
                    binary: "velnorctl".to_owned(),
                    targets: vec!["linux-amd64".to_owned()],
                }],
            }),
        });
        evidence.records[0].install = Some(InstallEvidence {
            applicability: Applicability::Required,
            justification: None,
            install_upgrade_test_environment: Some(InstallEnvironmentEvidence::Description(
                "ubuntu-24.04; linux amd64; runner-1; clean workspace/PATH".to_owned(),
            )),
            installed_binary_identity: Some(InstalledBinaryIdentity {
                product_id: "velnor".to_owned(),
                channel: "stable".to_owned(),
                version: "0.1.1".to_owned(),
                source_sha: source,
                manifest_sha256: manifest_digest,
                binaries: vec![InstalledBinary {
                    name: "velnorctl".to_owned(),
                    path: "/usr/bin/velnorctl".to_owned(),
                    sha256: digest('b'),
                }],
            }),
            upgrade_from: None,
            switch_from: None,
            service_manager_result: Some("systemd success".to_owned()),
            functional_result: Some("success".to_owned()),
        });
        let report = check_documents(Stage::G2, &manifest, &snapshot, &evidence);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
    }

    #[test]
    fn stale_sha_is_rejected() {
        let manifest = manifest_json();
        let snapshot = snapshot_json(&manifest);
        let mut evidence = envelope(&manifest, &snapshot);
        evidence.records[0].default_branch_sha = sha('f');
        let report = check_documents(Stage::G1, &manifest, &snapshot, &evidence);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "stale-sha"));
    }

    #[test]
    fn skipped_job_is_rejected() {
        let manifest = manifest_json();
        let snapshot = snapshot_json(&manifest);
        let mut evidence = envelope(&manifest, &snapshot);
        evidence.records[0]
            .actual_job_conclusions
            .insert("ci".to_owned(), "skipped".to_owned());
        let report = check_documents(Stage::G1, &manifest, &snapshot, &evidence);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "job-conclusion"));
    }

    #[test]
    fn missing_repository_is_rejected() {
        let mut manifest = manifest_json();
        let snapshot = snapshot_json(&manifest);
        manifest.repositories[0].repository = "owner/missing".to_owned();
        let evidence = envelope(&manifest, &snapshot);
        let report = check_documents(Stage::G1, &manifest, &snapshot, &evidence);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "missing-snapshot-repository"));
    }

    #[test]
    fn wrong_provider_is_rejected() {
        let manifest = manifest_json();
        let snapshot = snapshot_json(&manifest);
        let mut evidence = envelope(&manifest, &snapshot);
        evidence.records[0].provider = Some("velnor".to_owned());
        let report = check_documents(Stage::G1, &manifest, &snapshot, &evidence);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "wrong-provider"));
    }

    #[test]
    fn failed_child_is_rejected() {
        let manifest = manifest_json();
        let snapshot = snapshot_json(&manifest);
        let mut evidence = envelope(&manifest, &snapshot);
        evidence.records[0].expected_jobs[0].child_run_id = Some(9);
        let repository = evidence.records[0].repository.clone();
        evidence.records[0].child_run_links.push(ChildRunLink {
            run_id: Some(9),
            run_url: Some("https://github.com/owner/repo/actions/runs/9".to_owned()),
            repository,
            workflow: "child.yml".to_owned(),
            source_sha: snapshot.repositories[0].default_branch_sha.clone(),
            provider: "github".to_owned(),
            conclusion: "failure".to_owned(),
            run_attempt: Some(1),
        });
        let report = check_documents(Stage::G1, &manifest, &snapshot, &evidence);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "child-run-conclusion"));
    }

    #[test]
    fn mismatched_artifact_is_rejected() {
        let mut manifest = manifest_json();
        manifest.repositories[0].release_applicability = Some(Applicability::Required);
        let snapshot = snapshot_json(&manifest);
        let mut evidence = envelope(&manifest, &snapshot);
        evidence.records[0].release = Some(ReleaseEvidence {
            applicability: Applicability::Required,
            justification: None,
            release_channel: Some("stable".to_owned()),
            release_version: Some("0.1.1".to_owned()),
            tag_target_sha: Some(sha('f')),
            release_id: Some("release-1".to_owned()),
            asset_digests: Some(Value::Array(vec![Value::String(digest('a'))])),
            apt_feed_revision_suite_and_candidate: Some("feed stable 0.1.1".to_owned()),
            homebrew_tap_revision_and_formula: Some("tap stable 0.1.1".to_owned()),
            manifest: None,
        });
        evidence.records[0].install = Some(InstallEvidence {
            applicability: Applicability::NotApplicable,
            justification: Some("fixture".to_owned()),
            install_upgrade_test_environment: None,
            installed_binary_identity: None,
            upgrade_from: None,
            switch_from: None,
            service_manager_result: None,
            functional_result: None,
        });
        let report = check_documents(Stage::G2, &manifest, &snapshot, &evidence);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "mismatched-artifact"));
    }

    #[test]
    fn manifest_must_have_exactly_32_rows() {
        let mut manifest = manifest_json();
        manifest.repositories.pop();
        let snapshot = snapshot_json(&manifest);
        let evidence = envelope(&manifest, &snapshot);
        let report = check_documents(Stage::G0, &manifest, &snapshot, &evidence);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "manifest-count"));
    }
}

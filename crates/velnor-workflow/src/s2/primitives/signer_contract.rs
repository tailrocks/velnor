//! Typed, transport-only contract for native-product signer evidence.
//!
//! This module deliberately stops before workflow permission/DAG wiring.  It
//! owns the bytes and identity invariants that every future caller and
//! publisher must share: strict JSON objects, one record per subject cell,
//! basename-only sidecars, and the admitted source pair.

#![expect(
    dead_code,
    reason = "the typed transport contract is consumed when the privileged signer DAG is wired"
)]

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

pub(crate) const SUBJECT_INVENTORY_SCHEMA: &str = "velnor.subject-inventory/v1";
pub(crate) const ATTESTATION_RECORD_SCHEMA: &str = "velnor.attestation-record/v1";
pub(crate) const APT_HANDOFF_SCHEMA: &str = "velnor.release-admission/v1";
pub(crate) const PRODUCT_MANIFEST_FILE: &str = "product-manifest.json";
pub(crate) const APT_HANDOFF_FILE: &str = "release-admission.json";
pub(crate) const APT_HANDOFF_CHECKSUM_FILE: &str = "release-admission.json.sha256";
pub(crate) const ATTESTATION_RECORD_FILE: &str = "attestation-record.json";
pub(crate) const ATTESTATION_RECORD_CHECKSUM_FILE: &str = "attestation-record.json.sha256";
pub(crate) const SHARED_SIGNER_WORKFLOW: &str = ".github/workflows/ci-release-package-signer.yml";
pub(crate) const OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";
pub(crate) const SLSA_PREDICATE: &str = "https://slsa.dev/provenance/v1";

/// The only source identity pair accepted by producer callers and signers.
/// Every field is a string because GitHub job outputs are string transport;
/// consumer-owned numeric IDs are parsed only at the APT boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdmissionOutputs {
    pub(crate) provider_repository_id: String,
    pub(crate) source_repository: String,
    pub(crate) release_id: String,
    pub(crate) release_tag: String,
    pub(crate) source_ref: String,
    pub(crate) source_commit: String,
    pub(crate) target_commitish: String,
    pub(crate) release_url: String,
    pub(crate) assets_url: String,
    pub(crate) phase: String,
}

impl AdmissionOutputs {
    pub(crate) fn validate(
        &self,
        channel: &str,
        expected_repository: &str,
        default_branch: &str,
    ) -> Result<(), String> {
        if channel != "stable" && channel != "preview" {
            return Err("admission channel must be stable or preview".to_owned());
        }
        if self.source_repository != expected_repository || !valid_repository(expected_repository) {
            return Err("admission source repository is not the configured repository".to_owned());
        }
        if !positive_decimal(&self.provider_repository_id) || !positive_decimal(&self.release_id) {
            return Err(
                "admission provider/release IDs must be canonical positive decimals".to_owned(),
            );
        }
        if !safe_tag(&self.release_tag) {
            return Err("admission release tag is unsafe".to_owned());
        }
        if !full_sha(&self.source_commit) {
            return Err("admission source commit must be lowercase 40-hex".to_owned());
        }
        let expected_ref = if channel == "stable" {
            format!("refs/tags/{}", self.release_tag)
        } else {
            format!("refs/heads/{default_branch}")
        };
        if self.source_ref != expected_ref || !safe_ref(&self.source_ref) {
            return Err(
                "admission source ref is not the resolved immutable channel ref".to_owned(),
            );
        }
        if self.target_commitish.is_empty()
            || self
                .target_commitish
                .bytes()
                .any(|byte| byte.is_ascii_whitespace())
        {
            return Err("admission target_commitish is empty or contains whitespace".to_owned());
        }
        let expected_release_url = format!(
            "https://github.com/{}/releases/tag/{}",
            self.source_repository, self.release_tag
        );
        let expected_assets_url = format!(
            "https://api.github.com/repos/{}/releases/{}/assets",
            self.source_repository, self.release_id
        );
        if self.release_url != expected_release_url || self.assets_url != expected_assets_url {
            return Err("admission provider URLs are not canonical".to_owned());
        }
        if self.phase != "draft" && self.phase != "published" {
            return Err("admission phase must be draft or published".to_owned());
        }
        Ok(())
    }
}

/// The source-owned row used by every producer subject inventory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubjectDescriptor {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) source_repository: String,
    pub(crate) source_ref: String,
    pub(crate) source_commit: String,
}

impl SubjectDescriptor {
    fn validate(&self, admission: &AdmissionOutputs) -> Result<(), String> {
        if !safe_basename(&self.name) {
            return Err(format!(
                "subject name is not a safe basename: {}",
                self.name
            ));
        }
        if !matches!(
            self.kind.as_str(),
            "binary" | "archive" | "homebrew-archive" | "apt-package" | "runtime" | "image"
        ) {
            return Err(format!("subject kind is unsupported: {}", self.kind));
        }
        if !sha256(&self.sha256) {
            return Err(format!(
                "subject digest is not lowercase SHA-256: {}",
                self.name
            ));
        }
        if self.source_repository != admission.source_repository
            || self.source_ref != admission.source_ref
            || self.source_commit != admission.source_commit
        {
            return Err(format!("subject source identity differs: {}", self.name));
        }
        Ok(())
    }
}

/// One exact source artifact transport.  `subjects` is sorted by basename and
/// never inferred from a lossy matrix job output.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubjectInventory {
    pub(crate) schema: String,
    pub(crate) lane: String,
    pub(crate) artifact_name: String,
    pub(crate) source_repository: String,
    pub(crate) source_ref: String,
    pub(crate) source_commit: String,
    pub(crate) subjects: Vec<SubjectDescriptor>,
}

impl SubjectInventory {
    pub(crate) fn validate(
        &self,
        lane: SignerLane,
        admission: &AdmissionOutputs,
    ) -> Result<(), String> {
        if self.schema != SUBJECT_INVENTORY_SCHEMA || self.lane != lane.as_str() {
            return Err("subject inventory schema/lane mismatch".to_owned());
        }
        if self.artifact_name != lane.subject_inventory_artifact() {
            return Err("subject inventory artifact name mismatch".to_owned());
        }
        if self.source_repository != admission.source_repository
            || self.source_ref != admission.source_ref
            || self.source_commit != admission.source_commit
        {
            return Err("subject inventory source identity differs from admission".to_owned());
        }
        if self.subjects.is_empty() {
            return Err("subject inventory must contain at least one subject".to_owned());
        }
        let mut previous: Option<&str> = None;
        for subject in &self.subjects {
            subject.validate(admission)?;
            if previous.is_some_and(|name| name >= subject.name.as_str()) {
                return Err("subject inventory must be strictly sorted and unique".to_owned());
            }
            previous = Some(&subject.name);
        }
        Ok(())
    }
}

/// One matrix-cell signer result.  The fixed record path plus a basename-only
/// sidecar makes each cell independently downloadable and content-addressed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttestationRecord {
    pub(crate) schema: String,
    pub(crate) lane: String,
    pub(crate) artifact_name: String,
    pub(crate) subject: String,
    pub(crate) subject_sha256: String,
    pub(crate) subject_size: u64,
    pub(crate) source_repository: String,
    pub(crate) source_ref: String,
    pub(crate) source_commit: String,
    pub(crate) signer_workflow: String,
    pub(crate) oidc_issuer: String,
    pub(crate) predicate_type: String,
}

impl AttestationRecord {
    fn validate(&self, lane: SignerLane, admission: &AdmissionOutputs) -> Result<(), String> {
        if self.schema != ATTESTATION_RECORD_SCHEMA || self.lane != lane.as_str() {
            return Err("attestation record schema/lane mismatch".to_owned());
        }
        if !safe_basename(&self.subject)
            || self.artifact_name != signer_record_artifact_name(lane, &self.subject)
        {
            return Err("attestation record transport name/subject mismatch".to_owned());
        }
        if !sha256(&self.subject_sha256) {
            return Err("attestation record subject digest is not lowercase SHA-256".to_owned());
        }
        if self.source_repository != admission.source_repository
            || self.source_ref != admission.source_ref
            || self.source_commit != admission.source_commit
        {
            return Err("attestation record source identity differs from admission".to_owned());
        }
        let workflow_identity_ok = if lane == SignerLane::Image {
            safe_workflow_path(&self.signer_workflow)
        } else {
            self.signer_workflow == SHARED_SIGNER_WORKFLOW
        };
        if !workflow_identity_ok
            || self.oidc_issuer != OIDC_ISSUER
            || self.predicate_type != SLSA_PREDICATE
        {
            return Err("attestation record signer identity is not the shared contract".to_owned());
        }
        Ok(())
    }
}

/// A cross-cell aggregation check.  It rejects missing, duplicate, foreign,
/// stale, or digest/size-mismatched records before any publisher can upload.
pub(crate) fn validate_attestation_records(
    inventory: &SubjectInventory,
    lane: SignerLane,
    admission: &AdmissionOutputs,
    records: &[AttestationRecord],
) -> Result<(), String> {
    inventory.validate(lane, admission)?;
    if records.len() != inventory.subjects.len() {
        return Err("attestation record count does not equal subject inventory".to_owned());
    }
    let mut seen = std::collections::BTreeSet::new();
    for record in records {
        record.validate(lane, admission)?;
        if !seen.insert(record.subject.as_str()) {
            return Err(format!(
                "duplicate attestation record subject: {}",
                record.subject
            ));
        }
        let expected = inventory
            .subjects
            .iter()
            .find(|subject| subject.name == record.subject)
            .ok_or_else(|| format!("foreign attestation record subject: {}", record.subject))?;
        if expected.sha256 != record.subject_sha256 || expected.size != record.subject_size {
            return Err(format!(
                "attestation record digest/size mismatch: {}",
                record.subject
            ));
        }
    }
    Ok(())
}

/// The non-manifest cross-repository APT handoff.  It points back to the
/// producer manifest by digest and never hashes itself.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AptAdmissionHandoff {
    pub(crate) schema: String,
    pub(crate) source_repository: String,
    pub(crate) source_ref: String,
    pub(crate) source_commit: String,
    pub(crate) release_tag: String,
    pub(crate) release_id: String,
    pub(crate) provider_repository_id: String,
    pub(crate) parent_manifest_sha256: String,
    pub(crate) manifest_asset: String,
}

impl AptAdmissionHandoff {
    pub(crate) fn validate(
        &self,
        admission: &AdmissionOutputs,
        expected_manifest_sha256: &str,
    ) -> Result<(), String> {
        if self.schema != APT_HANDOFF_SCHEMA
            || self.manifest_asset != PRODUCT_MANIFEST_FILE
            || !sha256(&self.parent_manifest_sha256)
            || self.parent_manifest_sha256 != expected_manifest_sha256
        {
            return Err("APT handoff schema/manifest digest is invalid".to_owned());
        }
        if self.source_repository != admission.source_repository
            || self.source_ref != admission.source_ref
            || self.source_commit != admission.source_commit
            || self.release_tag != admission.release_tag
            || self.release_id != admission.release_id
            || self.provider_repository_id != admission.provider_repository_id
        {
            return Err("APT handoff source/provider identity differs from admission".to_owned());
        }
        if !positive_decimal(&self.provider_repository_id)
            || self.provider_repository_id.parse::<u64>().is_err()
        {
            return Err("APT provider repository ID overflows consumer u64".to_owned());
        }
        Ok(())
    }
}

/// Exact one-line checksum transport for every standalone JSON sidecar.
pub(crate) fn validate_checksum_sidecar(
    sidecar: &[u8],
    expected_digest: &str,
    expected_basename: &str,
) -> Result<(), String> {
    if !sha256(expected_digest) || !safe_basename(expected_basename) {
        return Err("checksum sidecar expectation is invalid".to_owned());
    }
    let text = std::str::from_utf8(sidecar).map_err(|_| "checksum sidecar is not UTF-8")?;
    let line = text
        .strip_suffix('\n')
        .ok_or_else(|| "checksum sidecar must end with one newline".to_owned())?;
    let mut fields = line.split("  ");
    let digest = fields.next().unwrap_or_default();
    let basename = fields.next().unwrap_or_default();
    if fields.next().is_some()
        || digest != expected_digest
        || basename != expected_basename
        || !sha256(digest)
    {
        return Err("checksum sidecar is not an exact digest/basename pair".to_owned());
    }
    Ok(())
}

/// Validate one matrix-cell record and its exact basename-only transport.
/// The caller supplies paths obtained from the downloaded workflow artifact so
/// a later publisher cannot silently accept a renamed or nested record.
pub(crate) fn validate_attestation_record_transport(
    record_path: &str,
    checksum_path: &str,
    record_bytes: &[u8],
    checksum_bytes: &[u8],
    lane: SignerLane,
    admission: &AdmissionOutputs,
) -> Result<AttestationRecord, String> {
    if record_path != ATTESTATION_RECORD_FILE || checksum_path != ATTESTATION_RECORD_CHECKSUM_FILE {
        return Err("attestation record transport path is not canonical".to_owned());
    }
    let record = parse_canonical::<AttestationRecord>(record_bytes)?;
    record.validate(lane, admission)?;
    validate_checksum_sidecar(
        checksum_bytes,
        &sha256_bytes(record_bytes),
        ATTESTATION_RECORD_FILE,
    )?;
    Ok(record)
}

/// Validate the acyclic APT handoff bytes and its checksum sidecar.  The
/// handoff's parent digest points to the separately fetched product manifest;
/// it never hashes itself.
pub(crate) fn validate_apt_handoff_transport(
    handoff_path: &str,
    checksum_path: &str,
    handoff_bytes: &[u8],
    checksum_bytes: &[u8],
    admission: &AdmissionOutputs,
    expected_manifest_sha256: &str,
) -> Result<AptAdmissionHandoff, String> {
    if handoff_path != APT_HANDOFF_FILE || checksum_path != APT_HANDOFF_CHECKSUM_FILE {
        return Err("APT handoff transport path is not canonical".to_owned());
    }
    let handoff = parse_canonical::<AptAdmissionHandoff>(handoff_bytes)?;
    handoff.validate(admission, expected_manifest_sha256)?;
    validate_checksum_sidecar(
        checksum_bytes,
        &sha256_bytes(handoff_bytes),
        APT_HANDOFF_FILE,
    )?;
    Ok(handoff)
}

/// The four reusable-signer inputs.  Rendering this value is pure; workflow
/// permission and DAG wiring remain a later integration unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SignerCallInputs {
    pub(crate) artifact_name: String,
    pub(crate) subject_path: String,
    pub(crate) source_ref: String,
    pub(crate) source_digest: String,
}

impl SignerCallInputs {
    pub(crate) fn admitted(artifact_name: &str, subject_path: &str) -> Result<Self, String> {
        if !safe_basename(artifact_name) || !safe_basename(subject_path) {
            return Err("signer input artifact/subject must be basenames".to_owned());
        }
        Ok(Self::admitted_unchecked_subject(
            artifact_name,
            subject_path,
        ))
    }

    /// Render a matrix-cell subject path.  The static portion remains a
    /// basename; the only dynamic token accepted is a GitHub expression in
    /// the basename itself.  Callers cannot smuggle a path or a second source
    /// authority through this escape hatch.
    pub(crate) fn admitted_expression(
        artifact_name: &str,
        subject_path: &str,
    ) -> Result<Self, String> {
        let normalized = subject_path
            .replace("${{ matrix.arch }}", "arch")
            .replace("${{ matrix.subject }}", "subject")
            .replace("${{ needs.verify.outputs.version }}", "version")
            .replace("${{ needs.identity.outputs.version }}", "version");
        if !safe_basename(artifact_name)
            || !safe_basename(&normalized)
            || subject_path.is_empty()
            || subject_path.contains('/')
            || subject_path.contains('\\')
            || subject_path.matches("${{ matrix.arch }}").count() > 1
            || subject_path.matches("${{ matrix.subject }}").count() > 1
            || subject_path
                .matches("${{ needs.verify.outputs.version }}")
                .count()
                > 1
            || subject_path
                .matches("${{ needs.identity.outputs.version }}")
                .count()
                > 1
        {
            return Err("signer input artifact/subject must be safe basenames".to_owned());
        }
        Ok(Self::admitted_unchecked_subject(
            artifact_name,
            subject_path,
        ))
    }

    fn admitted_unchecked_subject(artifact_name: &str, subject_path: &str) -> Self {
        Self {
            artifact_name: artifact_name.to_owned(),
            subject_path: subject_path.to_owned(),
            source_ref: "${{ needs.admit-product-release.outputs.source_ref }}".to_owned(),
            source_digest: "${{ needs.admit-product-release.outputs.source_commit }}".to_owned(),
        }
    }

    pub(crate) fn render_yaml(&self) -> String {
        format!(
            "artifact-name: {}\nsubject-path: {}\nsource-ref: {}\nsource-digest: {}\n",
            self.artifact_name, self.subject_path, self.source_ref, self.source_digest
        )
    }

    /// Render the complete reusable-signer call, including the typed record
    /// transport. The source repository is an admitted identity input, not a
    /// value inferred by the signer from its checkout context.
    pub(crate) fn render_yaml_for_lane(
        &self,
        lane: SignerLane,
        source_repository: &str,
    ) -> Result<String, String> {
        if !valid_repository(source_repository) {
            return Err("signer source repository is not a safe owner/repository".to_owned());
        }
        let record_artifact_name = signer_record_artifact_name(lane, &self.subject_path);
        Ok(format!(
            "{}lane: {}\nsource-repository: {}\nrecord-artifact-name: {}\n",
            self.render_yaml(),
            lane.as_str(),
            source_repository,
            record_artifact_name
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SignerLane {
    Native,
    Debian,
    Runtime,
    Image,
    Apt,
}

impl SignerLane {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Debian => "debian",
            Self::Runtime => "runtime",
            Self::Image => "image",
            Self::Apt => "apt",
        }
    }

    const fn subject_inventory_artifact(self) -> &'static str {
        match self {
            Self::Native => "native-subjects.json",
            Self::Debian => "debian-subjects.json",
            Self::Runtime => "runtime-subjects.json",
            Self::Image => "image-digests.json",
            Self::Apt => APT_HANDOFF_FILE,
        }
    }
}

pub(crate) fn signer_record_artifact_name(lane: SignerLane, subject: &str) -> String {
    format!("{}-{}", lane.record_prefix(), subject)
}

impl SignerLane {
    const fn record_prefix(self) -> &'static str {
        match self {
            Self::Native => "native-attestation-record",
            Self::Debian => "debian-attestation-record",
            Self::Runtime => "runtime-attestation-record",
            Self::Image => "image-attestation-record",
            Self::Apt => "apt-attestation-record",
        }
    }
}

/// Parse one canonical object. Serde rejects duplicate keys and
/// `deny_unknown_fields` rejects schema drift before typed validation.
fn parse_canonical<T>(bytes: &[u8]) -> Result<T, String>
where
    T: DeserializeOwned + Serialize,
{
    let value = serde_json::from_slice::<T>(bytes).map_err(|error| error.to_string())?;
    let canonical = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    if canonical != bytes {
        return Err("JSON is not the contract's canonical byte representation".to_owned());
    }
    Ok(value)
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|error| error.to_string())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn valid_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(repository), None)
            if safe_component(owner) && safe_component(repository)
    )
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
        && !value.starts_with(['.', '-', '_'])
        && !value.ends_with(['.', '-', '_'])
}

fn safe_basename(value: &str) -> bool {
    safe_component(value) && !value.contains("..")
}

fn safe_tag(value: &str) -> bool {
    safe_basename(value) && value.starts_with(['v', 'p'])
}

fn safe_ref(value: &str) -> bool {
    (value.starts_with("refs/tags/") || value.starts_with("refs/heads/"))
        && value
            .strip_prefix("refs/tags/")
            .or_else(|| value.strip_prefix("refs/heads/"))
            .is_some_and(|tail| safe_tag(tail) || safe_basename(tail))
}

fn safe_workflow_path(value: &str) -> bool {
    value.starts_with(".github/workflows/")
        && std::path::Path::new(value)
            .extension()
            .is_some_and(|extension| extension == "yml")
        && !value.contains("..")
        && value
            .strip_prefix(".github/workflows/")
            .is_some_and(safe_basename)
}

fn positive_decimal(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('0') && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn full_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_REPOSITORY: &str = "tailrocks/velnor";
    const SOURCE_REF: &str = "refs/tags/v1.2.3";
    const SOURCE_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const MANIFEST_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn admission() -> AdmissionOutputs {
        AdmissionOutputs {
            provider_repository_id: "1255367013".to_owned(),
            source_repository: SOURCE_REPOSITORY.to_owned(),
            release_id: "12345".to_owned(),
            release_tag: "v1.2.3".to_owned(),
            source_ref: SOURCE_REF.to_owned(),
            source_commit: SOURCE_COMMIT.to_owned(),
            target_commitish: SOURCE_COMMIT.to_owned(),
            release_url: "https://github.com/tailrocks/velnor/releases/tag/v1.2.3".to_owned(),
            assets_url: "https://api.github.com/repos/tailrocks/velnor/releases/12345/assets"
                .to_owned(),
            phase: "draft".to_owned(),
        }
    }

    fn subject(name: &str, kind: &str, digest: &str, size: u64) -> SubjectDescriptor {
        SubjectDescriptor {
            name: name.to_owned(),
            kind: kind.to_owned(),
            sha256: digest.to_owned(),
            size,
            source_repository: SOURCE_REPOSITORY.to_owned(),
            source_ref: SOURCE_REF.to_owned(),
            source_commit: SOURCE_COMMIT.to_owned(),
        }
    }

    fn inventory() -> SubjectInventory {
        SubjectInventory {
            schema: SUBJECT_INVENTORY_SCHEMA.to_owned(),
            lane: "native".to_owned(),
            artifact_name: "native-subjects.json".to_owned(),
            source_repository: SOURCE_REPOSITORY.to_owned(),
            source_ref: SOURCE_REF.to_owned(),
            source_commit: SOURCE_COMMIT.to_owned(),
            subjects: vec![
                subject(
                    "product-manifest.json",
                    "binary",
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    11,
                ),
                subject(
                    "velnorctl-v1.2.3-aarch64-apple-darwin.tar.gz",
                    "homebrew-archive",
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    22,
                ),
            ],
        }
    }

    fn record(subject: &SubjectDescriptor) -> AttestationRecord {
        AttestationRecord {
            schema: ATTESTATION_RECORD_SCHEMA.to_owned(),
            lane: "native".to_owned(),
            artifact_name: signer_record_artifact_name(SignerLane::Native, &subject.name),
            subject: subject.name.clone(),
            subject_sha256: subject.sha256.clone(),
            subject_size: subject.size,
            source_repository: SOURCE_REPOSITORY.to_owned(),
            source_ref: SOURCE_REF.to_owned(),
            source_commit: SOURCE_COMMIT.to_owned(),
            signer_workflow: SHARED_SIGNER_WORKFLOW.to_owned(),
            oidc_issuer: OIDC_ISSUER.to_owned(),
            predicate_type: SLSA_PREDICATE.to_owned(),
        }
    }

    #[test]
    fn admission_json_is_strict_and_canonical() {
        let value = admission();
        let bytes = canonical_json(&value).unwrap_or_default();
        assert_eq!(
            parse_canonical::<AdmissionOutputs>(&bytes),
            Ok(value.clone())
        );
        assert!(value.validate("stable", SOURCE_REPOSITORY, "main").is_ok());

        let duplicate = String::from_utf8(bytes.clone())
            .unwrap_or_default()
            .replacen(
                "\"source_ref\":\"refs/tags/v1.2.3\"",
                "\"source_ref\":\"refs/tags/v1.2.3\",\"source_ref\":\"refs/tags/v1.2.3\"",
                1,
            );
        assert!(parse_canonical::<AdmissionOutputs>(duplicate.as_bytes()).is_err());
        let unknown =
            String::from_utf8(bytes)
                .unwrap_or_default()
                .replacen('{', "{\"unexpected\":true,", 1);
        assert!(parse_canonical::<AdmissionOutputs>(unknown.as_bytes()).is_err());
        let numeric = String::from_utf8(canonical_json(&value).unwrap_or_default())
            .unwrap_or_default()
            .replacen("\"release_id\":\"12345\"", "\"release_id\":12345", 1);
        assert!(parse_canonical::<AdmissionOutputs>(numeric.as_bytes()).is_err());
    }

    #[test]
    fn record_aggregation_requires_every_typed_matrix_cell() {
        let admission = admission();
        let inventory = inventory();
        let records = inventory.subjects.iter().map(record).collect::<Vec<_>>();
        assert!(
            validate_attestation_records(&inventory, SignerLane::Native, &admission, &records)
                .is_ok()
        );
        let record_bytes = canonical_json(&records[0]).unwrap_or_default();
        let record_checksum = format!(
            "{}  {ATTESTATION_RECORD_FILE}\n",
            sha256_bytes(&record_bytes)
        );
        assert!(validate_attestation_record_transport(
            ATTESTATION_RECORD_FILE,
            ATTESTATION_RECORD_CHECKSUM_FILE,
            &record_bytes,
            record_checksum.as_bytes(),
            SignerLane::Native,
            &admission,
        )
        .is_ok());
        assert!(validate_attestation_record_transport(
            "nested/attestation-record.json",
            ATTESTATION_RECORD_CHECKSUM_FILE,
            &record_bytes,
            record_checksum.as_bytes(),
            SignerLane::Native,
            &admission,
        )
        .is_err());
        let nested_duplicate = String::from_utf8(canonical_json(&inventory).unwrap_or_default())
            .unwrap_or_default()
            .replacen(
                "\"name\":\"product-manifest.json\"",
                "\"name\":\"product-manifest.json\",\"name\":\"product-manifest.json\"",
                1,
            );
        assert!(parse_canonical::<SubjectInventory>(nested_duplicate.as_bytes()).is_err());

        assert!(validate_attestation_records(
            &inventory,
            SignerLane::Native,
            &admission,
            &records[..1]
        )
        .is_err());
        let mut duplicate = records.clone();
        duplicate[1].subject = duplicate[0].subject.clone();
        duplicate[1].artifact_name =
            signer_record_artifact_name(SignerLane::Native, &duplicate[1].subject);
        assert!(validate_attestation_records(
            &inventory,
            SignerLane::Native,
            &admission,
            &duplicate
        )
        .is_err());
        let mut foreign = records;
        foreign[0].subject = "foreign.json".to_owned();
        foreign[0].artifact_name = signer_record_artifact_name(SignerLane::Native, "foreign.json");
        assert!(
            validate_attestation_records(&inventory, SignerLane::Native, &admission, &foreign)
                .is_err()
        );
    }

    #[test]
    fn apt_handoff_is_acyclic_and_identity_bound() {
        let admission = admission();
        let handoff = AptAdmissionHandoff {
            schema: APT_HANDOFF_SCHEMA.to_owned(),
            source_repository: SOURCE_REPOSITORY.to_owned(),
            source_ref: SOURCE_REF.to_owned(),
            source_commit: SOURCE_COMMIT.to_owned(),
            release_tag: "v1.2.3".to_owned(),
            release_id: "12345".to_owned(),
            provider_repository_id: "1255367013".to_owned(),
            parent_manifest_sha256: MANIFEST_SHA.to_owned(),
            manifest_asset: PRODUCT_MANIFEST_FILE.to_owned(),
        };
        assert!(handoff.validate(&admission, MANIFEST_SHA).is_ok());
        assert!(validate_checksum_sidecar(
            format!("{MANIFEST_SHA}  {APT_HANDOFF_FILE}\n").as_bytes(),
            MANIFEST_SHA,
            APT_HANDOFF_FILE
        )
        .is_ok());
        assert!(validate_checksum_sidecar(
            format!("{MANIFEST_SHA}  /tmp/{APT_HANDOFF_FILE}\n").as_bytes(),
            MANIFEST_SHA,
            APT_HANDOFF_FILE
        )
        .is_err());
        let mut overflow = handoff.clone();
        overflow.provider_repository_id = "18446744073709551616".to_owned();
        assert!(overflow.validate(&admission, MANIFEST_SHA).is_err());
        let handoff_bytes = canonical_json(&handoff).unwrap_or_default();
        let handoff_checksum = format!("{}  {APT_HANDOFF_FILE}\n", sha256_bytes(&handoff_bytes));
        assert!(validate_apt_handoff_transport(
            APT_HANDOFF_FILE,
            APT_HANDOFF_CHECKSUM_FILE,
            &handoff_bytes,
            handoff_checksum.as_bytes(),
            &admission,
            MANIFEST_SHA,
        )
        .is_ok());

        let bytes = canonical_json(&handoff).unwrap_or_default();
        let duplicate = String::from_utf8(bytes.clone())
            .unwrap_or_default()
            .replacen(
                "\"release_id\":\"12345\"",
                "\"release_id\":\"12345\",\"release_id\":\"12345\"",
                1,
            );
        assert!(parse_canonical::<AptAdmissionHandoff>(duplicate.as_bytes()).is_err());
        let unknown = String::from_utf8(bytes).unwrap_or_default().replacen(
            '{',
            "{\"provider_repository_id_number\":1,",
            1,
        );
        assert!(parse_canonical::<AptAdmissionHandoff>(unknown.as_bytes()).is_err());
    }

    #[test]
    fn rendered_signer_inputs_bind_the_admitted_pair() {
        let rendered =
            SignerCallInputs::admitted("native-product-assets", "record.json").map(|inputs| {
                let yaml = inputs.render_yaml();
                assert_eq!(yaml.matches("source-ref:").count(), 1);
                assert_eq!(yaml.matches("source-digest:").count(), 1);
                assert!(yaml
                    .contains("source-ref: ${{ needs.admit-product-release.outputs.source_ref }}"));
                assert!(yaml.contains(
                    "source-digest: ${{ needs.admit-product-release.outputs.source_commit }}"
                ));
                assert!(!yaml.contains("github.sha"));
            });
        assert!(rendered.is_ok());
        assert!(SignerCallInputs::admitted("native/product", "record.json").is_err());

        let matrix = SignerCallInputs::admitted_expression(
            "debian-packages",
            "runner-v1.2.3-${{ matrix.arch }}.deb",
        )
        .expect("matrix subject remains a basename expression");
        let rendered = matrix
            .render_yaml_for_lane(SignerLane::Debian, SOURCE_REPOSITORY)
            .expect("complete signer input is valid");
        assert!(rendered.contains("subject-path: runner-v1.2.3-${{ matrix.arch }}.deb"));
        assert!(rendered.contains("lane: debian"));
        assert!(rendered.contains(
            "source-repository: tailrocks/velnor\nrecord-artifact-name: debian-attestation-record-runner-v1.2.3-${{ matrix.arch }}.deb"
        ));
        assert!(
            SignerCallInputs::admitted_expression("debian-packages", "nested/runner.deb").is_err()
        );
    }
}

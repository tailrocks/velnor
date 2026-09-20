//! Strict offline verification for a future trusted checkout proof.
//!
//! This module is deliberately not connected to the live collector or to a
//! production raw store.  It verifies bounded provider fixtures, snapshots,
//! and archive-member digests, then returns `checkout_only` evidence.  A later
//! producer may implement [`ImmutableCasReader`] after the canonical store is
//! independently approved.

use serde::de::{DeserializeOwned, Error as DeError};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::io::{Cursor, Read, Write};
use url::Url;
use zip::ZipArchive;

pub const MAX_PROOF_SUBJECT_BYTES: usize = 64 * 1024;
pub const MAX_JSON_DEPTH: usize = 16;
pub const MAX_JSON_OBJECT_MEMBERS: usize = 64;
pub const MAX_JSON_ARRAY_MEMBERS: usize = 128;
pub const MAX_SNAPSHOT_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_ARCHIVE_MEMBERS: usize = 4096;
pub const MAX_ARCHIVE_MEMBER_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ARCHIVE_TOTAL_MEMBER_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckoutProofError {
    InputTooLarge,
    JsonDepthExceeded,
    JsonObjectTooLarge,
    JsonArrayTooLarge,
    InvalidJson,
    InvalidIdentifier,
    InvalidSha1,
    InvalidSha256,
    InvalidMemberName,
    InvalidProviderUrl,
    InvalidProviderIdentity,
    ExpiredSnapshot,
    ArchiveTooLarge,
    ArchiveInvalid,
    ArchiveMemberLimit,
    ArchiveMemberTooLarge,
    ArchiveTotalTooLarge,
    ArchiveDuplicateMember,
    ArchiveMemberMissing,
    ArchiveTargetDirectory,
    SnapshotDigestMismatch,
    SnapshotLengthMismatch,
    SnapshotMemberDigestMismatch,
    SnapshotMemberLengthMismatch,
    CasStorageRefMismatch,
    CasReadFailed,
    CasDigestMismatch,
}

impl fmt::Display for CheckoutProofError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InputTooLarge => "proof input exceeds the fixed byte limit",
            Self::JsonDepthExceeded => "proof JSON exceeds the fixed depth limit",
            Self::JsonObjectTooLarge => "proof JSON object exceeds the fixed member limit",
            Self::JsonArrayTooLarge => "proof JSON array exceeds the fixed member limit",
            Self::InvalidJson => "proof JSON is invalid or non-canonical",
            Self::InvalidIdentifier => "provider identifier is not canonical",
            Self::InvalidSha1 => "SHA-1 value is not 40 lowercase hexadecimal characters",
            Self::InvalidSha256 => "SHA-256 value is not 64 lowercase hexadecimal characters",
            Self::InvalidMemberName => "archive member name is not canonical",
            Self::InvalidProviderUrl => "provider archive URL is not the fixed API URL",
            Self::InvalidProviderIdentity => "provider identity does not match the proof",
            Self::ExpiredSnapshot => "snapshot artifact is expired",
            Self::ArchiveTooLarge => "snapshot archive exceeds the fixed byte limit",
            Self::ArchiveInvalid => "snapshot archive is invalid",
            Self::ArchiveMemberLimit => "snapshot archive has too many members",
            Self::ArchiveMemberTooLarge => "snapshot member exceeds the fixed byte limit",
            Self::ArchiveTotalTooLarge => "snapshot members exceed the fixed total limit",
            Self::ArchiveDuplicateMember => "snapshot archive contains a duplicate member",
            Self::ArchiveMemberMissing => "required snapshot member is missing",
            Self::ArchiveTargetDirectory => "required snapshot member is a directory",
            Self::SnapshotDigestMismatch => "snapshot archive digest does not match the provider",
            Self::SnapshotLengthMismatch => "snapshot archive length does not match the provider",
            Self::SnapshotMemberDigestMismatch => "snapshot member digest does not match the proof",
            Self::SnapshotMemberLengthMismatch => "snapshot member length does not match the proof",
            Self::CasStorageRefMismatch => "CAS storage reference is not the canonical digest URI",
            Self::CasReadFailed => "CAS original-byte read failed",
            Self::CasDigestMismatch => "CAS original bytes do not match the expected digest",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CheckoutProofError {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub(crate) struct DecimalId(String);

impl DecimalId {
    fn parse(value: String) -> Result<Self, CheckoutProofError> {
        if value.is_empty()
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || value.parse::<u64>().is_err()
        {
            return Err(CheckoutProofError::InvalidIdentifier);
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DecimalId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub(crate) struct Sha1Hex(String);

impl Sha1Hex {
    fn parse(value: String) -> Result<Self, CheckoutProofError> {
        if value.len() != 40
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CheckoutProofError::InvalidSha1);
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Sha1Hex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub(crate) struct Sha256Hex(String);

impl Sha256Hex {
    fn parse(value: String) -> Result<Self, CheckoutProofError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CheckoutProofError::InvalidSha256);
        }
        Ok(Self(value))
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let value = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        Self(value)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Sha256Hex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub(crate) struct SnapshotMemberName(String);

impl SnapshotMemberName {
    fn parse(value: String) -> Result<Self, CheckoutProofError> {
        if value.is_empty()
            || value.starts_with('/')
            || value.contains('\\')
            || value.contains('\0')
            || value.as_bytes().get(1) == Some(&b':')
            || value.bytes().any(|byte| byte.is_ascii_control())
            || value
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(CheckoutProofError::InvalidMemberName);
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SnapshotMemberName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SnapshotService {
    GithubActions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckoutEvidenceStatus {
    CheckoutOnly,
    BuiltFrom,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderArtifactDto {
    service: SnapshotService,
    artifact_id: DecimalId,
    repository: String,
    repository_id: DecimalId,
    run_id: DecimalId,
    run_attempt: DecimalId,
    archive_url: String,
    archive_sha256: Sha256Hex,
    archive_byte_length: u64,
    expired: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderJobDto {
    service: SnapshotService,
    repository: String,
    repository_id: DecimalId,
    run_id: DecimalId,
    run_attempt: DecimalId,
    check_run_id: DecimalId,
    job_id: DecimalId,
    logical_job_id: String,
    api_head_sha: Sha1Hex,
    workflow_path: String,
    workflow_sha: Sha1Hex,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotReferenceDto {
    service: SnapshotService,
    artifact_id: DecimalId,
    archive_sha256: Sha256Hex,
    archive_byte_length: u64,
    member_name: SnapshotMemberName,
    member_sha256: Sha256Hex,
    member_byte_length: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProofSubjectDto {
    schema: String,
    repository: String,
    repository_id: DecimalId,
    run_id: DecimalId,
    run_attempt: DecimalId,
    job_logical_id: String,
    check_run_id: DecimalId,
    job_id: DecimalId,
    api_head_sha: Sha1Hex,
    checkout_sha: Sha1Hex,
    tree_sha: Sha1Hex,
    workflow_path: String,
    workflow_sha: Sha1Hex,
    wrapper_revision: Sha1Hex,
    snapshot: SnapshotReferenceDto,
    observed_at_utc: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotArtifactDescriptor {
    service: SnapshotService,
    artifact_id: DecimalId,
    repository: String,
    repository_id: DecimalId,
    run_id: DecimalId,
    run_attempt: DecimalId,
    archive_sha256: Sha256Hex,
    archive_byte_length: u64,
}

impl SnapshotArtifactDescriptor {
    pub(crate) fn service(&self) -> SnapshotService {
        self.service
    }

    pub(crate) fn artifact_id(&self) -> &DecimalId {
        &self.artifact_id
    }

    pub(crate) fn archive_sha256(&self) -> &Sha256Hex {
        &self.archive_sha256
    }

    pub(crate) fn archive_byte_length(&self) -> u64 {
        self.archive_byte_length
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArchiveMemberCensus {
    name: SnapshotMemberName,
    directory: bool,
    byte_length: u64,
    sha256: Sha256Hex,
}

impl ArchiveMemberCensus {
    pub(crate) fn name(&self) -> &SnapshotMemberName {
        &self.name
    }

    pub(crate) fn is_directory(&self) -> bool {
        self.directory
    }

    pub(crate) fn byte_length(&self) -> u64 {
        self.byte_length
    }

    pub(crate) fn sha256(&self) -> &Sha256Hex {
        &self.sha256
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotArchiveCensus {
    archive_sha256: Sha256Hex,
    archive_byte_length: u64,
    total_member_bytes: u64,
    members: Vec<ArchiveMemberCensus>,
    target: ArchiveMemberCensus,
}

impl SnapshotArchiveCensus {
    pub(crate) fn archive_sha256(&self) -> &Sha256Hex {
        &self.archive_sha256
    }

    pub(crate) fn archive_byte_length(&self) -> u64 {
        self.archive_byte_length
    }

    pub(crate) fn total_member_bytes(&self) -> u64 {
        self.total_member_bytes
    }

    pub(crate) fn members(&self) -> &[ArchiveMemberCensus] {
        &self.members
    }

    pub(crate) fn target(&self) -> &ArchiveMemberCensus {
        &self.target
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckoutOnlyFixtureObservation {
    subject_sha256: Sha256Hex,
    subject_byte_length: u64,
    status: CheckoutEvidenceStatus,
    snapshot: SnapshotArtifactDescriptor,
    archive: SnapshotArchiveCensus,
}

impl CheckoutOnlyFixtureObservation {
    pub(crate) fn subject_sha256(&self) -> &Sha256Hex {
        &self.subject_sha256
    }

    pub(crate) fn subject_byte_length(&self) -> u64 {
        self.subject_byte_length
    }

    pub(crate) fn status(&self) -> CheckoutEvidenceStatus {
        self.status
    }

    pub(crate) fn snapshot(&self) -> &SnapshotArtifactDescriptor {
        &self.snapshot
    }

    pub(crate) fn archive(&self) -> &SnapshotArchiveCensus {
        &self.archive
    }

    fn from_fixture(
        subject_sha256: Sha256Hex,
        subject_byte_length: u64,
        snapshot: SnapshotArtifactDescriptor,
        archive: SnapshotArchiveCensus,
    ) -> Self {
        Self {
            subject_sha256,
            subject_byte_length,
            status: CheckoutEvidenceStatus::CheckoutOnly,
            snapshot,
            archive,
        }
    }
}

/// A later producer-owned CAS adapter.  The reader returns original bytes;
/// the verifier computes the digest and length before any JSON parser runs.
pub(crate) trait ImmutableCasReader {
    fn open_original<'a>(
        &'a self,
        storage_ref: &str,
    ) -> Result<Box<dyn Read + 'a>, CheckoutProofError>;
}

pub(crate) struct MeasuredOriginalBytes {
    digest: Sha256Hex,
    byte_length: u64,
    bytes: Vec<u8>,
}

impl MeasuredOriginalBytes {
    pub(crate) fn digest(&self) -> &Sha256Hex {
        &self.digest
    }

    pub(crate) fn byte_length(&self) -> u64 {
        self.byte_length
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Read one canonical original-byte object without wiring a concrete store.
/// The returned bytes are measured before a caller may parse them.
#[derive(Debug, Clone, Copy)]
enum CasObjectKind {
    Json,
    Archive,
}

impl CasObjectKind {
    fn max_bytes(self) -> usize {
        match self {
            Self::Json => MAX_PROOF_SUBJECT_BYTES,
            Self::Archive => MAX_SNAPSHOT_ARCHIVE_BYTES,
        }
    }
}

struct BoundedOriginalBytes {
    limit: usize,
    bytes: Vec<u8>,
    exceeded: bool,
}

impl BoundedOriginalBytes {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            bytes: Vec::new(),
            exceeded: false,
        }
    }
}

impl Write for BoundedOriginalBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::other("bounded original-byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn read_original_from_cas<C: ImmutableCasReader>(
    store: &C,
    storage_ref: &str,
    expected_digest: &Sha256Hex,
    kind: CasObjectKind,
) -> Result<MeasuredOriginalBytes, CheckoutProofError> {
    let expected_ref = format!("sha256://{}", expected_digest.as_str());
    if storage_ref != expected_ref {
        return Err(CheckoutProofError::CasStorageRefMismatch);
    }
    let mut reader = store
        .open_original(storage_ref)
        .map_err(|_| CheckoutProofError::CasReadFailed)?;
    let mut bounded = BoundedOriginalBytes::new(kind.max_bytes());
    let copy_result = std::io::copy(&mut reader, &mut bounded);
    if bounded.exceeded {
        return Err(CheckoutProofError::InputTooLarge);
    }
    if copy_result.is_err() {
        return Err(CheckoutProofError::CasReadFailed);
    }
    let bytes = bounded.bytes;
    let digest = Sha256Hex::from_bytes(&bytes);
    if &digest != expected_digest {
        return Err(CheckoutProofError::CasDigestMismatch);
    }
    Ok(MeasuredOriginalBytes {
        digest,
        byte_length: bytes.len() as u64,
        bytes,
    })
}

#[derive(Debug)]
pub(crate) struct CheckoutProofCasRefs {
    subject_storage_ref: String,
    subject_sha256: Sha256Hex,
    provider_job_storage_ref: String,
    provider_job_sha256: Sha256Hex,
    provider_artifact_storage_ref: String,
    provider_artifact_sha256: Sha256Hex,
    archive_storage_ref: String,
    archive_sha256: Sha256Hex,
}

pub(crate) fn verify_checkout_proof_from_cas<C: ImmutableCasReader>(
    store: &C,
    refs: &CheckoutProofCasRefs,
) -> Result<CheckoutOnlyFixtureObservation, CheckoutProofError> {
    let subject = read_original_from_cas(
        store,
        &refs.subject_storage_ref,
        &refs.subject_sha256,
        CasObjectKind::Json,
    )?;
    let provider_job = read_original_from_cas(
        store,
        &refs.provider_job_storage_ref,
        &refs.provider_job_sha256,
        CasObjectKind::Json,
    )?;
    let provider_artifact = read_original_from_cas(
        store,
        &refs.provider_artifact_storage_ref,
        &refs.provider_artifact_sha256,
        CasObjectKind::Json,
    )?;
    let archive = read_original_from_cas(
        store,
        &refs.archive_storage_ref,
        &refs.archive_sha256,
        CasObjectKind::Archive,
    )?;
    verify_checkout_proof_bytes(
        subject.bytes(),
        provider_job.bytes(),
        provider_artifact.bytes(),
        archive.bytes(),
    )
}

fn verify_checkout_proof_bytes(
    subject_bytes: &[u8],
    provider_job_bytes: &[u8],
    provider_artifact_bytes: &[u8],
    archive_bytes: &[u8],
) -> Result<CheckoutOnlyFixtureObservation, CheckoutProofError> {
    let (subject_digest, subject_length) = measure_json_bytes(subject_bytes)?;
    let provider_job: ProviderJobDto = parse_strict_json(provider_job_bytes)?;
    let provider_artifact: ProviderArtifactDto = parse_strict_json(provider_artifact_bytes)?;
    let snapshot = descriptor_from_provider(provider_artifact)?;
    let subject: ProofSubjectDto = parse_strict_json(subject_bytes)?;
    let target_name = subject.snapshot.member_name.clone();
    let archive = census_archive(archive_bytes, &target_name)?;

    if archive.archive_sha256 != snapshot.archive_sha256 {
        return Err(CheckoutProofError::SnapshotDigestMismatch);
    }
    if archive.archive_byte_length != snapshot.archive_byte_length {
        return Err(CheckoutProofError::SnapshotLengthMismatch);
    }
    verify_provider_identity(&provider_job, &snapshot)?;
    verify_subject_identity(&subject, &provider_job, &snapshot, &archive)?;

    Ok(CheckoutOnlyFixtureObservation::from_fixture(
        subject_digest,
        subject_length,
        snapshot,
        archive,
    ))
}

/// Test-only raw fixture entry point. Production callers must use the CAS
/// adapter above so no caller-owned byte slice can mint a typed result.
#[cfg(test)]
fn verify_checkout_proof_fixture(
    subject_bytes: &[u8],
    provider_job_bytes: &[u8],
    provider_artifact_bytes: &[u8],
    archive_bytes: &[u8],
) -> Result<CheckoutOnlyFixtureObservation, CheckoutProofError> {
    verify_checkout_proof_bytes(
        subject_bytes,
        provider_job_bytes,
        provider_artifact_bytes,
        archive_bytes,
    )
}

fn descriptor_from_provider(
    provider: ProviderArtifactDto,
) -> Result<SnapshotArtifactDescriptor, CheckoutProofError> {
    if !valid_repository(&provider.repository) {
        return Err(CheckoutProofError::InvalidProviderIdentity);
    }
    if provider.expired {
        return Err(CheckoutProofError::ExpiredSnapshot);
    }
    let expected_path = format!(
        "/repos/{}/actions/artifacts/{}/zip",
        provider.repository,
        provider.artifact_id.as_str()
    );
    let url =
        Url::parse(&provider.archive_url).map_err(|_| CheckoutProofError::InvalidProviderUrl)?;
    if url.scheme() != "https"
        || url.host_str() != Some("api.github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || has_explicit_port(&provider.archive_url)
        || url.path() != expected_path
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(CheckoutProofError::InvalidProviderUrl);
    }
    Ok(SnapshotArtifactDescriptor {
        service: provider.service,
        artifact_id: provider.artifact_id,
        repository: provider.repository,
        repository_id: provider.repository_id,
        run_id: provider.run_id,
        run_attempt: provider.run_attempt,
        archive_sha256: provider.archive_sha256,
        archive_byte_length: provider.archive_byte_length,
    })
}

fn verify_provider_identity(
    job: &ProviderJobDto,
    snapshot: &SnapshotArtifactDescriptor,
) -> Result<(), CheckoutProofError> {
    if job.service != snapshot.service
        || job.repository != snapshot.repository
        || job.repository_id != snapshot.repository_id
        || job.run_id != snapshot.run_id
        || job.run_attempt != snapshot.run_attempt
        || !valid_text(&job.logical_job_id)
        || !valid_relative_name(&job.workflow_path)
    {
        return Err(CheckoutProofError::InvalidProviderIdentity);
    }
    Ok(())
}

fn verify_subject_identity(
    subject: &ProofSubjectDto,
    job: &ProviderJobDto,
    snapshot: &SnapshotArtifactDescriptor,
    archive: &SnapshotArchiveCensus,
) -> Result<(), CheckoutProofError> {
    if subject.schema != "velnor.checkout-proof.v1"
        || subject.repository != job.repository
        || subject.repository_id != job.repository_id
        || subject.run_id != job.run_id
        || subject.run_attempt != job.run_attempt
        || subject.job_logical_id != job.logical_job_id
        || subject.check_run_id != job.check_run_id
        || subject.job_id != job.job_id
        || subject.api_head_sha != job.api_head_sha
        || subject.workflow_path != job.workflow_path
        || subject.workflow_sha != job.workflow_sha
        || !valid_relative_name(&subject.workflow_path)
        || !valid_text(&subject.observed_at_utc)
    {
        return Err(CheckoutProofError::InvalidProviderIdentity);
    }
    if subject.snapshot.service != snapshot.service
        || subject.snapshot.artifact_id != snapshot.artifact_id
        || subject.snapshot.archive_sha256 != snapshot.archive_sha256
        || subject.snapshot.archive_byte_length != snapshot.archive_byte_length
        || subject.snapshot.member_name != archive.target.name
    {
        return Err(CheckoutProofError::InvalidProviderIdentity);
    }
    if subject.snapshot.member_sha256 != archive.target.sha256 {
        return Err(CheckoutProofError::SnapshotMemberDigestMismatch);
    }
    if subject.snapshot.member_byte_length != archive.target.byte_length {
        return Err(CheckoutProofError::SnapshotMemberLengthMismatch);
    }
    Ok(())
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value == value.trim() && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn valid_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(name) = parts.next() else {
        return false;
    };
    parts.next().is_none() && valid_slug(owner) && valid_slug(name)
}

fn valid_slug(value: &str) -> bool {
    valid_text(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_relative_name(value: &str) -> bool {
    SnapshotMemberName::parse(value.to_owned()).is_ok()
}

fn has_explicit_port(raw_url: &str) -> bool {
    let Some(authority) = raw_url
        .strip_prefix("https://")
        .and_then(|remaining| remaining.split('/').next())
    else {
        return false;
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    host.contains(':')
}

fn census_archive(
    archive_bytes: &[u8],
    target_name: &SnapshotMemberName,
) -> Result<SnapshotArchiveCensus, CheckoutProofError> {
    if archive_bytes.len() > MAX_SNAPSHOT_ARCHIVE_BYTES {
        return Err(CheckoutProofError::ArchiveTooLarge);
    }
    let archive_sha256 = Sha256Hex::from_bytes(archive_bytes);
    let archive_byte_length = archive_bytes.len() as u64;
    validate_zip_central_directory(archive_bytes)?;
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes))
        .map_err(|_| CheckoutProofError::ArchiveInvalid)?;
    if archive.len() > MAX_ARCHIVE_MEMBERS {
        return Err(CheckoutProofError::ArchiveMemberLimit);
    }
    let mut names = BTreeSet::new();
    let mut members = Vec::with_capacity(archive.len());
    let mut total_member_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|_| CheckoutProofError::ArchiveInvalid)?;
        if file.is_symlink() {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        let raw_name = file.name().to_owned();
        let name = SnapshotMemberName::parse(raw_name.trim_end_matches('/').to_owned())?;
        insert_member_name(&mut names, &name)?;
        let byte_length = file.size();
        if byte_length > MAX_ARCHIVE_MEMBER_BYTES {
            return Err(CheckoutProofError::ArchiveMemberTooLarge);
        }
        let bytes = read_archive_member_bounded(&mut file, MAX_ARCHIVE_MEMBER_BYTES)?;
        if bytes.len() as u64 != byte_length {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        total_member_bytes = total_member_bytes
            .checked_add(bytes.len() as u64)
            .ok_or(CheckoutProofError::ArchiveTotalTooLarge)?;
        if total_member_bytes > MAX_ARCHIVE_TOTAL_MEMBER_BYTES {
            return Err(CheckoutProofError::ArchiveTotalTooLarge);
        }
        members.push(ArchiveMemberCensus {
            name,
            directory: file.is_dir(),
            byte_length,
            sha256: Sha256Hex::from_bytes(&bytes),
        });
    }
    let target = members
        .iter()
        .find(|member| member.name == *target_name)
        .cloned()
        .ok_or(CheckoutProofError::ArchiveMemberMissing)?;
    if target.directory {
        return Err(CheckoutProofError::ArchiveTargetDirectory);
    }
    Ok(SnapshotArchiveCensus {
        archive_sha256,
        archive_byte_length,
        total_member_bytes,
        members,
        target,
    })
}

fn read_archive_member_bounded<R: Read>(
    reader: &mut R,
    limit: u64,
) -> Result<Vec<u8>, CheckoutProofError> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| CheckoutProofError::ArchiveInvalid)?;
        if read == 0 {
            break;
        }
        if (bytes.len() as u64)
            .checked_add(read as u64)
            .is_none_or(|length| length > limit)
        {
            return Err(CheckoutProofError::ArchiveMemberTooLarge);
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    Ok(bytes)
}

#[derive(Debug, Clone, Copy)]
struct LocalZipRecord {
    offset: usize,
    end: usize,
}

fn validate_zip_central_directory(bytes: &[u8]) -> Result<(), CheckoutProofError> {
    const EOCD_SIGNATURE: u32 = 0x0605_4b50;
    const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
    const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
    const DATA_DESCRIPTOR_FLAG: u16 = 0x0008;
    const ENCRYPTED_FLAG: u16 = 0x0001;
    if bytes.len() < 22 {
        return Err(CheckoutProofError::ArchiveInvalid);
    }
    let search_start = bytes.len().saturating_sub(65_557);
    let eocd = (search_start..=bytes.len() - 22)
        .rev()
        .find(|offset| read_u32(bytes, *offset) == Some(EOCD_SIGNATURE))
        .ok_or(CheckoutProofError::ArchiveInvalid)?;
    let comment_length =
        usize::from(read_u16(bytes, eocd + 20).ok_or(CheckoutProofError::ArchiveInvalid)?);
    let eocd_end = eocd
        .checked_add(22)
        .and_then(|value| value.checked_add(comment_length))
        .ok_or(CheckoutProofError::ArchiveInvalid)?;
    if eocd_end != bytes.len()
        || read_u16(bytes, eocd + 4) != Some(0)
        || read_u16(bytes, eocd + 6) != Some(0)
    {
        return Err(CheckoutProofError::ArchiveInvalid);
    }
    let entries_on_disk =
        usize::from(read_u16(bytes, eocd + 8).ok_or(CheckoutProofError::ArchiveInvalid)?);
    let entries =
        usize::from(read_u16(bytes, eocd + 10).ok_or(CheckoutProofError::ArchiveInvalid)?);
    if entries_on_disk != entries || entries > MAX_ARCHIVE_MEMBERS {
        return Err(CheckoutProofError::ArchiveMemberLimit);
    }
    let central_size =
        usize::try_from(read_u32(bytes, eocd + 12).ok_or(CheckoutProofError::ArchiveInvalid)?)
            .map_err(|_| CheckoutProofError::ArchiveInvalid)?;
    let central_offset =
        usize::try_from(read_u32(bytes, eocd + 16).ok_or(CheckoutProofError::ArchiveInvalid)?)
            .map_err(|_| CheckoutProofError::ArchiveInvalid)?;
    if central_offset
        .checked_add(central_size)
        .ok_or(CheckoutProofError::ArchiveInvalid)?
        != eocd
    {
        return Err(CheckoutProofError::ArchiveInvalid);
    }

    let mut cursor = central_offset;
    let mut names = BTreeSet::new();
    let mut local_offsets = BTreeSet::new();
    let mut local_records = Vec::with_capacity(entries);
    for _ in 0..entries {
        if read_u32(bytes, cursor) != Some(CENTRAL_SIGNATURE) {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        let header_end = cursor
            .checked_add(46)
            .ok_or(CheckoutProofError::ArchiveInvalid)?;
        if header_end > eocd {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        let version_made = read_u16(bytes, cursor + 4).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let version_needed =
            read_u16(bytes, cursor + 6).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let flags = read_u16(bytes, cursor + 8).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let method = read_u16(bytes, cursor + 10).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let modified_time =
            read_u16(bytes, cursor + 12).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let modified_date =
            read_u16(bytes, cursor + 14).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let crc32 = read_u32(bytes, cursor + 16).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let compressed_size =
            u64::from(read_u32(bytes, cursor + 20).ok_or(CheckoutProofError::ArchiveInvalid)?);
        let uncompressed_size =
            u64::from(read_u32(bytes, cursor + 24).ok_or(CheckoutProofError::ArchiveInvalid)?);
        if flags & (DATA_DESCRIPTOR_FLAG | ENCRYPTED_FLAG) != 0 {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        if uncompressed_size > MAX_ARCHIVE_MEMBER_BYTES {
            return Err(CheckoutProofError::ArchiveMemberTooLarge);
        }
        let name_length =
            usize::from(read_u16(bytes, cursor + 28).ok_or(CheckoutProofError::ArchiveInvalid)?);
        let extra_length =
            usize::from(read_u16(bytes, cursor + 30).ok_or(CheckoutProofError::ArchiveInvalid)?);
        let comment_length =
            usize::from(read_u16(bytes, cursor + 32).ok_or(CheckoutProofError::ArchiveInvalid)?);
        let name_start = header_end;
        let name_end = name_start
            .checked_add(name_length)
            .ok_or(CheckoutProofError::ArchiveInvalid)?;
        let extra_end = name_end
            .checked_add(extra_length)
            .ok_or(CheckoutProofError::ArchiveInvalid)?;
        let record_end = extra_end
            .checked_add(comment_length)
            .ok_or(CheckoutProofError::ArchiveInvalid)?;
        if record_end > eocd {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        let raw_name = std::str::from_utf8(&bytes[name_start..name_end])
            .map_err(|_| CheckoutProofError::InvalidMemberName)?;
        let name = SnapshotMemberName::parse(raw_name.trim_end_matches('/').to_owned())?;
        insert_member_name(&mut names, &name)?;

        let local_offset = usize::try_from(
            read_u32(bytes, cursor + 42).ok_or(CheckoutProofError::ArchiveInvalid)?,
        )
        .map_err(|_| CheckoutProofError::ArchiveInvalid)?;
        if !local_offsets.insert(local_offset)
            || read_u32(bytes, local_offset) != Some(LOCAL_SIGNATURE)
        {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        let local_header_end = local_offset
            .checked_add(30)
            .ok_or(CheckoutProofError::ArchiveInvalid)?;
        if local_header_end > central_offset {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        let local_version_needed =
            read_u16(bytes, local_offset + 4).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let local_flags =
            read_u16(bytes, local_offset + 6).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let local_method =
            read_u16(bytes, local_offset + 8).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let local_modified_time =
            read_u16(bytes, local_offset + 10).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let local_modified_date =
            read_u16(bytes, local_offset + 12).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let local_crc32 =
            read_u32(bytes, local_offset + 14).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let local_compressed_size = u64::from(
            read_u32(bytes, local_offset + 18).ok_or(CheckoutProofError::ArchiveInvalid)?,
        );
        let local_uncompressed_size = u64::from(
            read_u32(bytes, local_offset + 22).ok_or(CheckoutProofError::ArchiveInvalid)?,
        );
        let local_name_length = usize::from(
            read_u16(bytes, local_offset + 26).ok_or(CheckoutProofError::ArchiveInvalid)?,
        );
        let local_extra_length = usize::from(
            read_u16(bytes, local_offset + 28).ok_or(CheckoutProofError::ArchiveInvalid)?,
        );
        let local_name_start = local_header_end;
        let local_name_end = local_name_start
            .checked_add(local_name_length)
            .ok_or(CheckoutProofError::ArchiveInvalid)?;
        let local_extra_end = local_name_end
            .checked_add(local_extra_length)
            .ok_or(CheckoutProofError::ArchiveInvalid)?;
        let data_end = local_extra_end
            .checked_add(
                usize::try_from(compressed_size).map_err(|_| CheckoutProofError::ArchiveInvalid)?,
            )
            .ok_or(CheckoutProofError::ArchiveInvalid)?;
        if local_extra_end > central_offset || data_end > central_offset {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        if local_name_length != name_length
            || local_extra_length != extra_length
            || bytes.get(local_name_start..local_name_end) != Some(&bytes[name_start..name_end])
            || bytes.get(local_extra_end - local_extra_length..local_extra_end)
                != Some(&bytes[name_end..extra_end])
        {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        if local_version_needed != version_needed
            || local_flags != flags
            || local_method != method
            || local_modified_time != modified_time
            || local_modified_date != modified_date
            || local_crc32 != crc32
            || local_compressed_size != compressed_size
            || local_uncompressed_size != uncompressed_size
        {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        let made_on_unix = version_made >> 8 == 3;
        let external_attributes =
            read_u32(bytes, cursor + 38).ok_or(CheckoutProofError::ArchiveInvalid)?;
        let unix_mode = external_attributes >> 16;
        if made_on_unix && unix_mode & 0xf000 == 0xa000 {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
        local_records.push(LocalZipRecord {
            offset: local_offset,
            end: data_end,
        });
        cursor = record_end;
    }
    if cursor != eocd {
        return Err(CheckoutProofError::ArchiveInvalid);
    }
    local_records.sort_unstable_by_key(|record| record.offset);
    if entries == 0 {
        if central_offset != 0 {
            return Err(CheckoutProofError::ArchiveInvalid);
        }
    } else if local_records.first().map(|record| record.offset) != Some(0)
        || local_records.last().map(|record| record.end) != Some(central_offset)
        || local_records
            .windows(2)
            .any(|records| records[0].end != records[1].offset)
    {
        return Err(CheckoutProofError::ArchiveInvalid);
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    Some(u16::from_le_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    Some(u32::from_le_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn insert_member_name(
    names: &mut BTreeSet<SnapshotMemberName>,
    name: &SnapshotMemberName,
) -> Result<(), CheckoutProofError> {
    if !names.insert(name.clone()) {
        return Err(CheckoutProofError::ArchiveDuplicateMember);
    }
    Ok(())
}

fn measure_json_bytes(bytes: &[u8]) -> Result<(Sha256Hex, u64), CheckoutProofError> {
    if bytes.len() > MAX_PROOF_SUBJECT_BYTES {
        return Err(CheckoutProofError::InputTooLarge);
    }
    check_nesting(bytes)?;
    Ok((Sha256Hex::from_bytes(bytes), bytes.len() as u64))
}

fn parse_strict_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CheckoutProofError> {
    measure_json_bytes(bytes)?;
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| CheckoutProofError::InvalidJson)?;
    enforce_value_limits(&value, 0)?;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let parsed = T::deserialize(&mut deserializer).map_err(|_| CheckoutProofError::InvalidJson)?;
    deserializer
        .end()
        .map_err(|_| CheckoutProofError::InvalidJson)?;
    Ok(parsed)
}

fn check_nesting(bytes: &[u8]) -> Result<(), CheckoutProofError> {
    let mut depth = 0_usize;
    let mut escaped = false;
    let mut in_string = false;
    for byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return Err(CheckoutProofError::JsonDepthExceeded);
                }
            }
            b'}' | b']' => {
                if depth == 0 {
                    return Err(CheckoutProofError::InvalidJson);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    if in_string || depth != 0 {
        return Err(CheckoutProofError::InvalidJson);
    }
    Ok(())
}

fn enforce_value_limits(value: &Value, depth: usize) -> Result<(), CheckoutProofError> {
    if depth > MAX_JSON_DEPTH {
        return Err(CheckoutProofError::JsonDepthExceeded);
    }
    match value {
        Value::Array(values) => {
            if values.len() > MAX_JSON_ARRAY_MEMBERS {
                return Err(CheckoutProofError::JsonArrayTooLarge);
            }
            for value in values {
                enforce_value_limits(value, depth + 1)?;
            }
        }
        Value::Object(values) => {
            if values.len() > MAX_JSON_OBJECT_MEMBERS {
                return Err(CheckoutProofError::JsonObjectTooLarge);
            }
            for value in values.values() {
                enforce_value_limits(value, depth + 1)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive_fixture() -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::FileOptions::<()>::default();
        writer.start_file("source/manifest.json", options).unwrap();
        writer.write_all(br#"{"source":"fixture"}"#).unwrap();
        writer.start_file("source/readme.txt", options).unwrap();
        writer.write_all(b"fixture").unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn fixture_json(archive: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let archive_sha256 = Sha256Hex::from_bytes(archive);
        let member = br#"{"source":"fixture"}"#;
        let member_sha256 = Sha256Hex::from_bytes(member);
        let artifact = serde_json::json!({
            "service": "github_actions",
            "artifact_id": "42",
            "repository": "tailrocks/example",
            "repository_id": "7",
            "run_id": "9",
            "run_attempt": "1",
            "archive_url": "https://api.github.com/repos/tailrocks/example/actions/artifacts/42/zip",
            "archive_sha256": archive_sha256.as_str(),
            "archive_byte_length": archive.len(),
            "expired": false
        });
        let job = serde_json::json!({
            "service": "github_actions",
            "repository": "tailrocks/example",
            "repository_id": "7",
            "run_id": "9",
            "run_attempt": "1",
            "check_run_id": "17",
            "job_id": "18",
            "logical_job_id": "proof",
            "api_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "workflow_path": ".github/workflows/proof.yml",
            "workflow_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        });
        let subject = serde_json::json!({
            "schema": "velnor.checkout-proof.v1",
            "repository": "tailrocks/example",
            "repository_id": "7",
            "run_id": "9",
            "run_attempt": "1",
            "job_logical_id": "proof",
            "check_run_id": "17",
            "job_id": "18",
            "api_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "checkout_sha": "cccccccccccccccccccccccccccccccccccccccc",
            "tree_sha": "dddddddddddddddddddddddddddddddddddddddd",
            "workflow_path": ".github/workflows/proof.yml",
            "workflow_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "wrapper_revision": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "snapshot": {
                "service": "github_actions",
                "artifact_id": "42",
                "archive_sha256": archive_sha256.as_str(),
                "archive_byte_length": archive.len(),
                "member_name": "source/manifest.json",
                "member_sha256": member_sha256.as_str(),
                "member_byte_length": member.len()
            },
            "observed_at_utc": "2026-09-20T00:00:00Z"
        });
        (
            serde_json::to_vec(&subject).unwrap(),
            serde_json::to_vec(&job).unwrap(),
            serde_json::to_vec(&artifact).unwrap(),
        )
    }

    #[test]
    fn provider_fixture_census_returns_checkout_only() {
        let archive = archive_fixture();
        let (subject, job, artifact) = fixture_json(&archive);
        let proof = verify_checkout_proof_fixture(&subject, &job, &artifact, &archive).unwrap();
        assert_eq!(proof.status(), CheckoutEvidenceStatus::CheckoutOnly);
        assert_eq!(proof.archive().members().len(), 2);
        assert_eq!(
            proof.archive().target().name().as_str(),
            "source/manifest.json"
        );
        assert_eq!(proof.snapshot().artifact_id().as_str(), "42");
    }

    #[test]
    fn strict_subject_rejects_unknown_and_duplicate_fields() {
        let archive = archive_fixture();
        let (subject, _, _) = fixture_json(&archive);
        let mut value: Value = serde_json::from_slice(&subject).unwrap();
        value["future_attestation_artifact_id"] = Value::String("99".to_owned());
        let unknown = serde_json::to_vec(&value).unwrap();
        assert!(parse_strict_json::<ProofSubjectDto>(&unknown).is_err());

        let duplicate = br#"{"schema":"velnor.checkout-proof.v1","schema":"evil"}"#;
        assert!(parse_strict_json::<ProofSubjectDto>(duplicate).is_err());
    }

    #[test]
    fn strict_ids_reject_json_numbers_and_leading_zeroes() {
        let numeric = br#"{"artifact_id":42}"#;
        assert!(parse_strict_json::<SnapshotReferenceDto>(numeric).is_err());
        let leading_zero = br#"{"artifact_id":"042"}"#;
        assert!(parse_strict_json::<SnapshotReferenceDto>(leading_zero).is_err());
    }

    #[test]
    fn strict_parser_rejects_depth_and_size() {
        let deep = format!(
            "{}0{}",
            "[".repeat(MAX_JSON_DEPTH + 1),
            "]".repeat(MAX_JSON_DEPTH + 1)
        );
        assert_eq!(
            parse_strict_json::<Value>(deep.as_bytes()),
            Err(CheckoutProofError::JsonDepthExceeded)
        );
        let huge = vec![b' '; MAX_PROOF_SUBJECT_BYTES + 1];
        assert_eq!(
            parse_strict_json::<Value>(&huge),
            Err(CheckoutProofError::InputTooLarge)
        );
    }

    #[test]
    fn archive_census_rejects_traversal_and_duplicate_members() {
        let mut traversal = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::FileOptions::<()>::default();
        traversal.start_file("../escape", options).unwrap();
        traversal.write_all(b"x").unwrap();
        let traversal = traversal.finish().unwrap().into_inner();
        let target = SnapshotMemberName::parse("source/manifest.json".to_owned()).unwrap();
        assert_eq!(
            census_archive(&traversal, &target),
            Err(CheckoutProofError::InvalidMemberName)
        );

        let mut names = BTreeSet::new();
        assert!(insert_member_name(&mut names, &target).is_ok());
        assert_eq!(
            insert_member_name(&mut names, &target),
            Err(CheckoutProofError::ArchiveDuplicateMember)
        );
    }

    fn central_offset_for_test(archive: &[u8]) -> usize {
        usize::try_from(read_u32(archive, archive.len() - 22 + 16).unwrap()).unwrap()
    }

    fn central_record_length_for_test(archive: &[u8], offset: usize) -> usize {
        46 + usize::from(read_u16(archive, offset + 28).unwrap())
            + usize::from(read_u16(archive, offset + 30).unwrap())
            + usize::from(read_u16(archive, offset + 32).unwrap())
    }

    fn write_u16_for_test(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32_for_test(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn archive_census_rejects_local_metadata_mismatch() {
        let mut archive = archive_fixture();
        let central = central_offset_for_test(&archive);
        let local = usize::try_from(read_u32(&archive, central + 42).unwrap()).unwrap();
        let modified_time = read_u16(&archive, local + 10).unwrap();
        write_u16_for_test(&mut archive, local + 10, modified_time.wrapping_add(1));
        let target = SnapshotMemberName::parse("source/manifest.json".to_owned()).unwrap();
        assert_eq!(
            census_archive(&archive, &target),
            Err(CheckoutProofError::ArchiveInvalid)
        );
    }

    #[test]
    fn archive_census_rejects_symlink_and_shared_local_records() {
        let target = SnapshotMemberName::parse("source/manifest.json".to_owned()).unwrap();

        let mut symlink = archive_fixture();
        let central = central_offset_for_test(&symlink);
        write_u16_for_test(&mut symlink, central + 4, 0x0314);
        write_u32_for_test(&mut symlink, central + 38, 0xa000_0000);
        assert_eq!(
            census_archive(&symlink, &target),
            Err(CheckoutProofError::ArchiveInvalid)
        );

        let mut shared = archive_fixture();
        let central = central_offset_for_test(&shared);
        let second = central + central_record_length_for_test(&shared, central);
        let first_local = read_u32(&shared, central + 42).unwrap();
        write_u32_for_test(&mut shared, second + 42, first_local);
        assert_eq!(
            census_archive(&shared, &target),
            Err(CheckoutProofError::ArchiveInvalid)
        );
    }

    #[test]
    fn archive_census_rejects_unlisted_local_records() {
        let archive = archive_fixture();
        let central = central_offset_for_test(&archive);
        let mut hidden = vec![0_u8; 30];
        write_u32_for_test(&mut hidden, 0, 0x0403_4b50);
        write_u16_for_test(&mut hidden, 26, 6);
        hidden.extend_from_slice(b"hidden");

        let mut mutated = Vec::with_capacity(archive.len() + hidden.len());
        mutated.extend_from_slice(&archive[..central]);
        mutated.extend_from_slice(&hidden);
        mutated.extend_from_slice(&archive[central..]);
        let new_eocd = archive.len() - 22 + hidden.len();
        write_u32_for_test(&mut mutated, new_eocd + 16, (central + hidden.len()) as u32);

        let target = SnapshotMemberName::parse("source/manifest.json".to_owned()).unwrap();
        assert_eq!(
            census_archive(&mutated, &target),
            Err(CheckoutProofError::ArchiveInvalid)
        );
    }

    #[test]
    fn archive_member_limit_applies_to_actual_reader_output() {
        let mut reader = Cursor::new(b"three".to_vec());
        assert_eq!(
            read_archive_member_bounded(&mut reader, 2),
            Err(CheckoutProofError::ArchiveMemberTooLarge)
        );
    }

    #[test]
    fn provider_url_rejects_userinfo_and_explicit_ports() {
        let archive = archive_fixture();
        let (subject, job, _) = fixture_json(&archive);
        for url in [
            "https://token@api.github.com/repos/tailrocks/example/actions/artifacts/42/zip",
            "https://api.github.com:443/repos/tailrocks/example/actions/artifacts/42/zip",
        ] {
            let (_, _, artifact) = fixture_json(&archive);
            let mut value: Value = serde_json::from_slice(&artifact).unwrap();
            value["archive_url"] = Value::String(url.to_owned());
            let artifact = serde_json::to_vec(&value).unwrap();
            assert!(matches!(
                verify_checkout_proof_fixture(&subject, &job, &artifact, &archive),
                Err(CheckoutProofError::InvalidProviderUrl)
            ));
        }
    }

    struct MemoryCas {
        objects: std::collections::BTreeMap<String, Vec<u8>>,
    }

    impl ImmutableCasReader for MemoryCas {
        fn open_original<'a>(
            &'a self,
            storage_ref: &str,
        ) -> Result<Box<dyn Read + 'a>, CheckoutProofError> {
            self.objects
                .get(storage_ref)
                .cloned()
                .map(|bytes| Box::new(Cursor::new(bytes)) as Box<dyn Read>)
                .ok_or(CheckoutProofError::CasReadFailed)
        }
    }

    fn cas_entry(bytes: &[u8]) -> (String, Sha256Hex, Vec<u8>) {
        let digest = Sha256Hex::from_bytes(bytes);
        (
            format!("sha256://{}", digest.as_str()),
            digest,
            bytes.to_vec(),
        )
    }

    #[test]
    fn provider_fixture_cas_path_is_the_only_production_verifier_path() {
        let archive = archive_fixture();
        let (subject, job, artifact) = fixture_json(&archive);
        let (subject_ref, subject_sha256, subject_bytes) = cas_entry(&subject);
        let (job_ref, job_sha256, job_bytes) = cas_entry(&job);
        let (artifact_ref, artifact_sha256, artifact_bytes) = cas_entry(&artifact);
        let (archive_ref, archive_sha256, archive_bytes) = cas_entry(&archive);
        let store = MemoryCas {
            objects: [
                (subject_ref.clone(), subject_bytes),
                (job_ref.clone(), job_bytes),
                (artifact_ref.clone(), artifact_bytes),
                (archive_ref.clone(), archive_bytes),
            ]
            .into_iter()
            .collect(),
        };
        let refs = CheckoutProofCasRefs {
            subject_storage_ref: subject_ref,
            subject_sha256,
            provider_job_storage_ref: job_ref,
            provider_job_sha256: job_sha256,
            provider_artifact_storage_ref: artifact_ref,
            provider_artifact_sha256: artifact_sha256,
            archive_storage_ref: archive_ref,
            archive_sha256,
        };
        let proof = verify_checkout_proof_from_cas(&store, &refs).unwrap();
        assert_eq!(proof.status(), CheckoutEvidenceStatus::CheckoutOnly);
    }

    #[test]
    fn cas_adapter_measures_before_parse_and_rejects_tamper() {
        let bytes = b"{}".to_vec();
        let expected = Sha256Hex::from_bytes(&bytes);
        let storage_ref = format!("sha256://{}", expected.as_str());
        let store = MemoryCas {
            objects: [(storage_ref.clone(), bytes.clone())].into_iter().collect(),
        };
        let measured =
            read_original_from_cas(&store, &storage_ref, &expected, CasObjectKind::Json).unwrap();
        assert_eq!(measured.digest(), &expected);
        assert_eq!(measured.byte_length(), 2);

        let tampered = MemoryCas {
            objects: [(storage_ref.clone(), b"{\"tampered\":true}".to_vec())]
                .into_iter()
                .collect(),
        };
        assert!(matches!(
            read_original_from_cas(&tampered, &storage_ref, &expected, CasObjectKind::Json),
            Err(CheckoutProofError::CasDigestMismatch)
        ));
    }
}

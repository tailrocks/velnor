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
use std::io::{Cursor, Read};
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
pub enum CheckoutProofError {
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
pub struct DecimalId(String);

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

    pub fn as_str(&self) -> &str {
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
pub struct Sha1Hex(String);

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

    pub fn as_str(&self) -> &str {
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
pub struct Sha256Hex(String);

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

    pub fn as_str(&self) -> &str {
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
pub struct SnapshotMemberName(String);

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

    pub fn as_str(&self) -> &str {
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
pub enum SnapshotService {
    GithubActions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutEvidenceStatus {
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
pub struct SnapshotArtifactDescriptor {
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
    pub fn service(&self) -> SnapshotService {
        self.service
    }

    pub fn artifact_id(&self) -> &DecimalId {
        &self.artifact_id
    }

    pub fn archive_sha256(&self) -> &Sha256Hex {
        &self.archive_sha256
    }

    pub fn archive_byte_length(&self) -> u64 {
        self.archive_byte_length
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveMemberCensus {
    name: SnapshotMemberName,
    directory: bool,
    byte_length: u64,
    sha256: Sha256Hex,
}

impl ArchiveMemberCensus {
    pub fn name(&self) -> &SnapshotMemberName {
        &self.name
    }

    pub fn is_directory(&self) -> bool {
        self.directory
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }

    pub fn sha256(&self) -> &Sha256Hex {
        &self.sha256
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotArchiveCensus {
    archive_sha256: Sha256Hex,
    archive_byte_length: u64,
    total_member_bytes: u64,
    members: Vec<ArchiveMemberCensus>,
    target: ArchiveMemberCensus,
}

impl SnapshotArchiveCensus {
    pub fn archive_sha256(&self) -> &Sha256Hex {
        &self.archive_sha256
    }

    pub fn archive_byte_length(&self) -> u64 {
        self.archive_byte_length
    }

    pub fn total_member_bytes(&self) -> u64 {
        self.total_member_bytes
    }

    pub fn members(&self) -> &[ArchiveMemberCensus] {
        &self.members
    }

    pub fn target(&self) -> &ArchiveMemberCensus {
        &self.target
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCheckoutProof {
    subject_sha256: Sha256Hex,
    subject_byte_length: u64,
    status: CheckoutEvidenceStatus,
    snapshot: SnapshotArtifactDescriptor,
    archive: SnapshotArchiveCensus,
}

impl VerifiedCheckoutProof {
    pub fn subject_sha256(&self) -> &Sha256Hex {
        &self.subject_sha256
    }

    pub fn subject_byte_length(&self) -> u64 {
        self.subject_byte_length
    }

    pub fn status(&self) -> CheckoutEvidenceStatus {
        self.status
    }

    pub fn snapshot(&self) -> &SnapshotArtifactDescriptor {
        &self.snapshot
    }

    pub fn archive(&self) -> &SnapshotArchiveCensus {
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
pub trait ImmutableCasReader {
    fn read_original(&self, storage_ref: &str) -> Result<Vec<u8>, CheckoutProofError>;
}

pub struct MeasuredOriginalBytes {
    digest: Sha256Hex,
    byte_length: u64,
    bytes: Vec<u8>,
}

impl MeasuredOriginalBytes {
    pub fn digest(&self) -> &Sha256Hex {
        &self.digest
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Read one canonical original-byte object without wiring a concrete store.
/// The returned bytes are measured before a caller may parse them.
pub fn read_original_from_cas<C: ImmutableCasReader>(
    store: &C,
    storage_ref: &str,
    expected_digest: &Sha256Hex,
    max_bytes: usize,
) -> Result<MeasuredOriginalBytes, CheckoutProofError> {
    let expected_ref = format!("sha256://{}", expected_digest.as_str());
    if storage_ref != expected_ref {
        return Err(CheckoutProofError::CasStorageRefMismatch);
    }
    let bytes = store
        .read_original(storage_ref)
        .map_err(|_| CheckoutProofError::CasReadFailed)?;
    if bytes.len() > max_bytes {
        return Err(CheckoutProofError::InputTooLarge);
    }
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

/// Verify provider API fixture facts, the immutable ZIP snapshot, and a
/// checkout-proof subject.  No signature or builder-consumption authority is
/// inferred here; successful output remains `checkout_only`.
pub fn verify_checkout_proof_fixture(
    subject_bytes: &[u8],
    provider_job_bytes: &[u8],
    provider_artifact_bytes: &[u8],
    archive_bytes: &[u8],
) -> Result<VerifiedCheckoutProof, CheckoutProofError> {
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

    Ok(VerifiedCheckoutProof::from_fixture(
        subject_digest,
        subject_length,
        snapshot,
        archive,
    ))
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

fn census_archive(
    archive_bytes: &[u8],
    target_name: &SnapshotMemberName,
) -> Result<SnapshotArchiveCensus, CheckoutProofError> {
    if archive_bytes.len() > MAX_SNAPSHOT_ARCHIVE_BYTES {
        return Err(CheckoutProofError::ArchiveTooLarge);
    }
    let archive_sha256 = Sha256Hex::from_bytes(archive_bytes);
    let archive_byte_length = archive_bytes.len() as u64;
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
        let name = SnapshotMemberName::parse(file.name().to_owned())?;
        insert_member_name(&mut names, &name)?;
        let byte_length = file.size();
        if byte_length > MAX_ARCHIVE_MEMBER_BYTES {
            return Err(CheckoutProofError::ArchiveMemberTooLarge);
        }
        total_member_bytes = total_member_bytes
            .checked_add(byte_length)
            .ok_or(CheckoutProofError::ArchiveTotalTooLarge)?;
        if total_member_bytes > MAX_ARCHIVE_TOTAL_MEMBER_BYTES {
            return Err(CheckoutProofError::ArchiveTotalTooLarge);
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|_| CheckoutProofError::ArchiveInvalid)?;
        if bytes.len() as u64 != byte_length {
            return Err(CheckoutProofError::ArchiveInvalid);
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

    struct MemoryCas {
        storage_ref: String,
        bytes: Vec<u8>,
    }

    impl ImmutableCasReader for MemoryCas {
        fn read_original(&self, storage_ref: &str) -> Result<Vec<u8>, CheckoutProofError> {
            if storage_ref != self.storage_ref {
                return Err(CheckoutProofError::CasReadFailed);
            }
            Ok(self.bytes.clone())
        }
    }

    #[test]
    fn cas_adapter_measures_before_parse_and_rejects_tamper() {
        let bytes = b"{}".to_vec();
        let expected = Sha256Hex::from_bytes(&bytes);
        let storage_ref = format!("sha256://{}", expected.as_str());
        let store = MemoryCas {
            storage_ref: storage_ref.clone(),
            bytes: bytes.clone(),
        };
        let measured =
            read_original_from_cas(&store, &storage_ref, &expected, MAX_PROOF_SUBJECT_BYTES)
                .unwrap();
        assert_eq!(measured.digest(), &expected);
        assert_eq!(measured.byte_length(), 2);

        let tampered = MemoryCas {
            storage_ref: storage_ref.clone(),
            bytes: b"{\"tampered\":true}".to_vec(),
        };
        assert!(matches!(
            read_original_from_cas(&tampered, &storage_ref, &expected, MAX_PROOF_SUBJECT_BYTES),
            Err(CheckoutProofError::CasDigestMismatch)
        ));
    }
}
